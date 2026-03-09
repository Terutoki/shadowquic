# ShadowQUIC Extreme Performance Optimization Guide

## Overview
This document provides comprehensive guidance for achieving maximum performance with ShadowQUIC in cross-border (China ↔ United States) networking scenarios.

---

## 1. Batch UDP I/O (recvmmsg/sendmmsg)

### Implementation
```rust
use shadowquic::utils::batch_udp::{BatchUdpSocket, DEFAULT_BATCH_SIZE};

// Create batch UDP socket
let socket = BatchUdpSocket::new("0.0.0.0:443".parse()?, 64)?;
socket.set_udp_optimizations()?;

// Batch receive
let mut buffer = RecvMmsgBuffer::new(64);
let count = socket.recv_mmsg(&mut buffer);

// Batch send
let packets = vec![SendPacket { data, addr }];
socket.send_mmsg(&packets);
```

### Expected Performance Gains
- **PPS Increase**: 3-5x (from ~500K to 2-3M PPS)
- **Syscall Reduction**: 90%+ reduction
- **Latency**: Sub-100μs per batch operation

### Recommended Batch Sizes
| Network Speed | Batch Size |
|--------------|------------|
| 1Gbps        | 16-32      |
| 5Gbps        | 32-64      |
| 10Gbps+      | 64-128     |

---

## 2. UDP GSO (Generic Segmentation Offload)

### Configuration
Already enabled via quinn: `tp_cfg.enable_segmentation_offload(cfg.gso)`

### Manual Socket Configuration
```rust
unsafe {
    let gso_size: libc::c_int = 65535;
    libc::setsockopt(
        fd,
        libc::IPPROTO_UDP,
        libc::UDP_SEGMENT,
        &gso_size as *const _ as *const libc::c_void,
        std::mem::size_of::<libc::c_int>() as libc::socklen_t,
    );
}
```

### Expected Performance
- **Throughput**: 2-3x improvement for large packets
- **CPU Usage**: 30-50% reduction

---

## 3. BBR Congestion Control

### Configuration
```rust
use quinn::congestion::BbrConfig;

let bbr = BbrConfig::default()
    .with_initial_window(8 * 1024 * 1024)
    .with_max_window(32 * 1024 * 1024);
tp_cfg.congestion_controller_factory(Arc::new(bbr));
```

### Linux System Configuration
```bash
sysctl -w net.ipv4.tcp_congestion_control=bbr
sysctl -w net.core.default_qdisc=fq
```

### Expected Performance
- **Cross-border RTT**: 30-50% improvement
- **Packet Loss**: Better resilience
- **Throughput**: 2-4x in high-latency scenarios

---

## 4. CPU Affinity

### Implementation
```rust
use shadowquic::utils::per_core_worker::{PerCoreWorker, set_cpu_affinity};

let worker = PerCoreWorker::new(0, 0); // worker_id=0, cpu_id=0
worker.spawn(|local| {
    set_cpu_affinity(&[local.cpu_id()])?;
    // Work loop
})?;
```

### Recommended CPU Layout (8-core)
```
Core 0-3: Inbound UDP polling (interrupt driven)
Core 4-7: QUIC workers + outbound
```

### Expected Performance
- **Cache Misses**: 60-80% reduction
- **Latency**: 20-30% improvement
- **Jitter**: Significant reduction

---

## 5. Busy Polling

### Implementation
```rust
use shadowquic::utils::udp_batch::SocketOptimizer;

SocketOptimizer::set_udp_optimizations(&socket)?;
```

### Linux Configuration
```bash
# Set busy poll time (microseconds)
sysctl -w net.core.busy_poll=50
sysctl -w net.core.busy_read=50
```

### Socket Options
```rust
let enable: libc::c_int = 50; // microseconds
libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_BUSY_POLL, 
    &enable, std::mem::size_of::<libc::c_int>() as libc::socklen_t);
```

### Expected Performance
- **Latency**: 50-100μs reduction
- **Wakeup Overhead**: Near-zero for active sockets

---

## 6. Lock-Free Queue

### Implementation
```rust
use shadowquic::utils::lock_free_ring::{LockFreeRingBuffer, RingEntry};

let ring = LockFreeRingBuffer::new(4096);

// Producer
ring.push(RingEntry { data, src_addr, dst_addr });

// Consumer  
if let Some(entry) = ring.pop() {
    process(entry);
}
```

### Expected Performance
- **Throughput**: 10M+ ops/sec
- **Latency**: Sub-microsecond per operation
- **Contention**: Near-zero

---

## 7. NUMA Awareness

### Implementation
```rust
use shadowquic::utils::numa_allocator::{NumaAwareAllocator, bind_thread_to_numa_node};

// Bind to NUMA node 0
bind_thread_to_numa_node(0)?;

let allocator = NumaAwareAllocator::new(2, 16 * 1024 * 1024, 4);
// 2 nodes, 16MB slabs, 4 slabs per node
```

### Expected Performance
- **Memory Access**: 40-60% improvement for local access
- **Cross-node Traffic**: Eliminated

---

## 8. Zero-Copy Optimization

### Buffer Pool Usage
```rust
use shadowquic::pool::BufferPool;

let pool = BufferPool::new(1024);

// Allocate
let buf = pool.alloc();

// Reuse
pool.free(buf);
```

### Zero-Copy Send
```rust
// Send directly from buffer without copy
socket.send_to(buf.freeze(), addr).await?;
```

### Expected Performance
- **Memory Copies**: Near eliminated
- **Throughput**: 20-30% improvement

---

## 9. SO_REUSEPORT

### Implementation
```rust
use socket2::{Socket, Type, Domain, Protocol};

let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
socket.set_reuse_port(true)?;
```

### Expected Performance
- **Multi-core Scaling**: Linear up to # of cores
- **Connection Distribution**: Kernel-level balancing

---

## 10. NIC RSS/RPS/XPS

### Configuration Script
```bash
# Set IRQ affinity
echo 1 > /proc/irq/<irq_num>/smp_affinity

# Configure RPS
echo ffffffff > /sys/class/net/eth0/queues/rx-0/rps_cpus

# Configure XPS  
echo ffffffff > /sys/class/net/eth0/queues/tx-0/xps_cpus
```

### Rust Implementation
```rust
use shadowquic::utils::NicConfig;

NicConfig::set_rps_cpu("eth0", 0, 0)?;
NicConfig::set_xps_tx("eth0", 0, 0)?;
NicConfig::enable_rss("eth0")?;
```

### Expected Performance
- **CPU Utilization**: Even distribution
- **Interrupt Load**: Balanced across cores

---

## System Tuning Commands

### Kernel Parameters
```bash
# Network buffers
net.core.rmem_max=134217728
net.core.wmem_max=134217728
net.core.netdev_max_backlog=250000
net.core.somaxconn=65535

# TCP
net.ipv4.tcp_rmem=16777216 16777216 134217728
net.ipv4.tcp_wmem=16777216 16777216 134217728
net.ipv4.udp_rmem_min=16777216
net.ipv4.udp_wmem_min=16777216
net.ipv4.tcp_fastopen=3
net.ipv4.tcp_congestion_control=bbr
net.core.default_qdisc=fq
```

### NIC Settings
```bash
# Disable coalescing for latency
ethtool -C eth0 rx-usecs 0 tx-usecs 0

# Enable RSS
ethtool -K eth0 rx on tx on

# Increase ring buffer
ethtool -G eth0 rx 4096 tx 4096
```

---

## Benchmarking Methodology

### 1. Latency Test
```bash
# Ping with microsecond precision
ping -i 0.001 -c 10000 <target>

# Or use UDP ping
./perf/bin/udp_ping -s <server> -c 10000
```

### 2. Throughput Test
```bash
# UDP throughput
iperf3 -u -s -p 5201
iperf3 -u -c <server> -b 10G -t 60 -P 8

# QUIC throughput  
quicly perf --connection-count 100 --streams-per-connection 10
```

### 3. PPS Test
```bash
# Small packet UDP
./perf/bin/udp_throughput -s -p 5001 -S 64
./perf/bin/udp_throughput -c <server> -p 5001 -S 64 -t 10
```

### 4. CPU Usage Test
```bash
# Monitor with perf
perf stat -e cycles,instructions,cache-misses ./shadowquic

# Or use top/htop
htop
```

### Performance Metrics Collection
```bash
# Network stats
netstat -s

# Socket buffers
ss -s

# NIC stats
ethtool -S eth0
```

---

## Expected Results

### Baseline (Current)
- Latency: 100-200ms (cross-border)
- Throughput: 100-200 Mbps
- PPS: ~500K
- CPU: High

### Optimized Target
- Latency: 50-100ms (cross-border)
- Throughput: 1-5 Gbps
- PPS: 2-5M
- CPU: 30-50% reduction

### Optimization Priority
1. Batch UDP I/O (highest impact)
2. BBR + GSO
3. CPU Affinity
4. NUMA (if multi-socket)
5. Lock-free queues

---

## Recommended Crates

| Crate | Purpose | Already Used |
|-------|---------|--------------|
| socket2 | Advanced socket options | ✓ |
| bytes | Zero-copy buffers | ✓ |
| crossbeam | Lock-free structures | ✓ |
| quinn | QUIC protocol | ✓ |
| libc | Raw syscalls | ✓ |

### Additional Recommended Crates
```toml
# Add to Cargo.toml
[dependencies]
ahash = "0.8"           # Faster hashing
rustc-hash = "2.0"      # FxHasher
parking_lot = "0.12"    # Faster mutex
jemallocator = "0.5"    # Optimized allocator
```
