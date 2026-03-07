# ShadowQUIC Performance Optimization

## Overview

This document describes the performance optimizations applied to ShadowQUIC to achieve high-performance network proxy capabilities comparable to Cloudflare/Google standards.

## Optimization Summary

### 1. Memory Arena for Zero-Copy Buffers

**Location**: `src/arena/mod.rs`

Pre-allocated packet buffer pool using lock-free `SegQueue` for eliminating allocation overhead in hot paths.

```rust
// API
pub fn packet_buf() -> BytesMut      // 2KB buffer
pub fn packet_buf_large() -> BytesMut // 64KB buffer  
pub fn packet_buf_sized(size: usize) -> BytesMut
pub static PACKET_ARENA: LazyLock<PacketArena>
```

**Benefits**:
- Zero allocation overhead on packet send/receive
- Lock-free buffer recycling via atomic SegQueue
- Pre-warmed cache (256 small + 64 large buffers)
- Automatic buffer return on Drop

### 2. Lock-Free ID Generation

**Location**: `src/id/mod.rs`

Sealed trait pattern for ID generation with atomic operations.

```rust
// API
pub fn next_udp_id() -> u32
pub fn free_udp_id(id: u32)
pub struct AtomicIdGenerator
pub struct ShardedIdGenerator
```

**Benefits**:
- Sealed trait prevents external implementations (security)
- Lock-free ID allocation using `AtomicU32`
- SegQueue-based free list for efficient ID reuse
- Sharded variant for per-CPU scaling

### 3. Global Metrics Collection

**Location**: `src/metrics/mod.rs`

Global metrics with atomic counters for observability.

```rust
// Metrics tracked
pub packets_received: Arc<AtomicU64>
pub packets_sent: Arc<AtomicU64>
pub bytes_received: Arc<AtomicU64>
pub bytes_sent: Arc<AtomicU64>
pub connections_active: Arc<AtomicUsize>
pub errors: Arc<AtomicUsize>
pub udp_sessions: Arc<AtomicUsize>
pub tcp_sessions: Arc<AtomicUsize>
```

### 4. QUIC Hot Path Optimization

**Location**: `src/squic/mod.rs`

#### Stream Path Optimization
- Single write with combined buffer for small packets (≤128 bytes)
- Stack-allocated fixed-size buffer for zero allocation
- Arena buffer fallback for larger packets

```rust
// Before: Two separate writes
uni_conn.write_all(&header_buf).await?;
uni_conn.write_all(&bytes).await?;

// After: Single combined write for small packets
let mut combined = [0u8; 128];
combined[..header_buf.len()].copy_from_slice(&header_buf);
combined[header_buf.len()..].copy_from_slice(&bytes);
uni_conn.write_all(&combined[..total_len]).await?;
```

#### Datagram Path Optimization
- Pre-allocated buffer with `reserve()` before extending
- Eliminates reallocation during packet assembly

```rust
// Before
datagram_buf.clear();
datagram_buf.extend_from_slice(&header_buf);
datagram_buf.extend_from_slice(&bytes);

// After
datagram_buf.clear();
datagram_buf.reserve(total_len);  // Pre-allocate
datagram_buf.extend_from_slice(&header_buf);
datagram_buf.extend_from_slice(&bytes);
```

### 5. Cross-Platform Compatibility

**MIPS Support**: Changed `AtomicU64` to `AtomicU32` for platforms without 64-bit atomic support.

**CI Optimization**: Updated `.github/workflows/release.yml`:
```yaml
# Before
rustflags: "-C target-cpu=nehalem -C target-feature=+sse,+sse2"

# After  
rustflags: "-C target-cpu=native"
```

### 6. Code Quality Improvements

- Removed all unused imports
- Fixed clippy warnings
- Consistent code formatting with `cargo fmt`
- Reserved keyword fixes (`gen` → `_i`)
- Fixed lifetime elision warnings

## Architecture Highlights

### Zero-Copy Design
1. **Buffer Pool**: Pre-allocated `BytesMut` from arena eliminates malloc in hot path
2. **QUIC Datagrams**: Direct `Bytes` passing without copying
3. **UDP Handling**: Zero-copy slice operations using `bytes::Bytes::slice()`

### Lock-Free Hot Path
- ID generation: atomic operations only (no mutex)
- Buffer pool: SegQueue (lock-free MPMC queue)
- Metrics: atomic counters with relaxed ordering

### Per-CPU Design
- ShardedIdGenerator for lock-free per-core ID allocation
- Connection pool sharding reduces contention

## Benchmarking

```bash
# Run all tests
cargo test --release --verbose

# Run single test
cargo test --release test_name

# Integration tests
cargo test --release --test tcp_echo
cargo test --release --test tcp_echo_sunnyquic
cargo test --release --test udp_overstream_echo

# Python integration tests
./scripts/main_test.py
```

## Recommended Configuration

For optimal performance:

```yaml
# Server side
inbound:
  type: shadowquic
  bind-addr: "0.0.0.0:443"
  initial-mtu: 1400        # Higher for stable networks
  congestion-control: bbr  # BBR for high-speed networks
  gso: true               # Enable GSO if supported

# Client side  
outbound:
  type: shadowquic
  addr: "server:443"
  over-stream: false      # Use datagram for lower latency
  initial-mtu: 1400
  congestion-control: bbr
```

## Version History

| Version | Changes |
|---------|---------|
| v0.3.4 | Arena, ID, Metrics modules + QUIC optimization + MIPS support |
| v0.3.3 | Initial buffer pool integration |
| v0.3.2 | Basic ID store optimizations |

## Future Optimization Opportunities

1. **DPDK/AF_XDP**: Kernel-bypass for even higher performance
2. **SIMD**: Accelerate checksum calculations
3. **Huge Pages**: Reduce TLB misses for large buffer pools
4. **Connection Pool**: Pre-established QUIC connections