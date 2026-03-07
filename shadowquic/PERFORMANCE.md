# Performance Optimization Guide

This document describes the performance optimizations applied to ShadowQUIC.

## Version History

### v0.3.4 (Current)

#### 1. Memory Arena for Packet Buffers
- **Location**: `src/arena/mod.rs`
- **Description**: Pre-allocated packet buffer pool using lock-free `SegQueue`
- **Benefits**:
  - Zero allocation overhead for hot path buffers
  - Lock-free buffer recycling via atomic operations
  - Pre-warmed buffer cache (256 small + 64 large buffers)
- **API**:
  ```rust
  pub fn packet_buf() -> BytesMut      // 2KB buffer
  pub fn packet_buf_large() -> BytesMut // 64KB buffer  
  pub fn packet_buf_sized(size: usize) -> BytesMut
  pub fn packet_buf_put(buf: BytesMut) // Return to pool
  ```

#### 2. Lock-Free ID Generation
- **Location**: `src/id/mod.rs`
- **Description**: Sealed trait pattern for ID generation with atomic operations
- **Benefits**:
  - Sealed trait prevents external implementations
  - Lock-free ID allocation using `AtomicU32`
  - SegQueue-based free list for ID reuse
  - Sharded variant for per-CPU scaling
- **API**:
  ```rust
  pub fn next_udp_id() -> u32
  pub fn free_udp_id(id: u32)
  ```

#### 3. Global Metrics Collection
- **Location**: `src/metrics/mod.rs`
- **Description**: Global metrics with atomic counters
- **Metrics**:
  - Packets/bytes sent/received
  - Active connections
  - Error counts
  - UDP/TCP session counts

#### 4. QUIC Hot Path Optimization
- **Location**: `src/squic/mod.rs`
- **Description**: Zero-copy improvements for datagram and stream paths

**Stream Path**:
- Single write with combined buffer for small packets (≤128 bytes)
- Arena buffer for larger packets
- Reduced syscall overhead

**Datagram Path**:
- Pre-allocated buffer with `reserve()` before extending
- Eliminated unnecessary reallocations

#### 5. Cross-Platform Compatibility
- **MIPS Support**: Changed `AtomicU64` to `AtomicU32` for MIPS platforms
- **CI Update**: Changed to `target-cpu=native` for x86_64

#### 6. Code Quality Improvements
- Removed all unused imports
- Fixed clippy warnings
- Consistent code formatting with `cargo fmt`
- Reserved keyword fixes (`gen` → `id_gen`)

### v0.3.3
- Initial buffer pool integration
- Basic ID store optimizations

## Benchmarking

Run benchmarks with:
```bash
cargo test --release
./scripts/main_test.py
```

## Configuration

For optimal performance, use:
```yaml
# Server side
inbound:
  type: shadowquic
  initial-mtu: 1400  # Higher for stable networks
  congestion-control: bbr
  gso: true

# Client side  
outbound:
  type: shadowquic
  over-stream: false  # Use datagram for lower latency
  initial-mtu: 1400
  congestion-control: bbr
```

## Architecture Notes

### Zero-Copy Design
1. **Buffer Pool**: Pre-allocated `BytesMut` from arena
2. **QUIC Datagrams**: Direct `Bytes` passing without copying
3. **UDP Handling**: Zero-copy slice operations on received packets

### Lock-Free Hot Path
- ID generation uses atomic operations only
- Buffer pool uses SegQueue (lock-free MPMC)
- Metrics use atomic counters with relaxed ordering