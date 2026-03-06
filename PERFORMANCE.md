# ShadowQUIC Performance Optimization Guide

This document describes the performance optimizations applied to ShadowQUIC.

## Build Optimizations

### 1. Release Profile Optimizations

The following optimizations are enabled in `Cargo.toml`:

```toml
[profile.release]
lto = "fat"              # Full Link-Time Optimization
codegen-units = 1        # Better optimizations, slower compile
opt-level = 3            # Maximum optimization level
strip = true             # Strip symbols for smaller binary
panic = "abort"          # Remove panic unwinding code
```

### 2. CPU-Specific Optimizations

Build with native CPU optimizations:

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

This enables:
- CPU-specific instruction sets (AVX, AVX2, AVX-512, NEON, etc.)
- Better instruction scheduling
- Architecture-specific optimizations

### 3. jemalloc Memory Allocator

ShadowQUIC uses jemalloc by default for better memory allocation performance:

```toml
[features]
default = ["jemalloc"]
jemalloc = ["dep:jemallocator"]
```

Benefits:
- Lower memory fragmentation
- Better multi-threaded performance
- Reduced memory usage

To disable: `cargo build --release --no-default-features`

### 4. Fast Hash Functions

ShadowQUIC uses `ahash` (AHash) instead of the standard hash:

```rust
use ahash::AHashMap;
```

Benefits:
- 3-10x faster hashing
- Better performance for hash maps
- Lower CPU usage

## Advanced Optimizations

### Profile-Guided Optimization (PGO)

PGO optimizes the binary based on runtime profiling data.

**Steps:**

1. Run the PGO script:
   ```bash
   ./scripts/pgo.sh
   ```

2. Or manually:
   ```bash
   # Build instrumented binary
   RUSTFLAGS="-C target-cpu=native -C profile-generate=./target/profdata" \
       cargo build --release
   
   # Run your workload
   ./target/release/shadowquic
   
   # Merge profile data
   llvm-profdata merge -o ./target/profdata/merged.profdata ./target/profdata/*.profraw
   
   # Build optimized binary
   RUSTFLAGS="-C target-cpu=native -C profile-use=./target/profdata/merged.profdata" \
       cargo build --release
   ```

**Expected improvement:** 5-15% performance gain

### BOLT (Binary Optimization and Layout Tool)

BOLT optimizes binary layout for better cache performance.

**Prerequisites:**
```bash
# Ubuntu/Debian
sudo apt install llvm-bolt

# macOS
brew install llvm-bolt
```

**Steps:**

1. Run the BOLT script:
   ```bash
   ./scripts/bolt.sh
   ```

2. Or manually:
   ```bash
   # Build release binary
   RUSTFLAGS="-C target-cpu=native" cargo build --release
   
   # Profile with perf
   perf record -e cycles:u -j any,u -o perf.data -- ./target/release/shadowquic
   
   # Convert to BOLT format
   perf2bolt -p perf.data -o shadowquic.fdata ./target/release/shadowquic
   
   # Optimize with BOLT
   llvm-bolt ./target/release/shadowquic \
       -o ./target/release/shadowquic.bolt \
       -data shadowquic.fdata \
       -reorder-blocks=cache+ \
       -reorder-functions=hfsort+ \
       -split-functions=3 \
       -split-all-cold \
       -dyno-stats \
       -icf=1 \
       -use-gnu-stack
   ```

**Expected improvement:** 2-8% additional gain on top of PGO

## Code Optimizations

### Zero-Copy Operations

- Datagram buffers use `.split()` instead of `.clone()`
- Buffers are pre-allocated and reused in hot paths
- Minimal data copying in packet processing

### Efficient Data Structures

- HashMaps use `entry` API to avoid double lookups
- Pre-allocated capacity for known sizes
- Fast hash function (AHash)

### Inline Optimizations

- Hot path functions are marked with `#[inline]`
- Small functions are inlined for better performance

### Memory Management

- Buffer reuse in UDP/TCP loops
- Reduced allocations in error paths
- Efficient Drop implementations

## Performance Comparison

| Optimization | Expected Gain |
|-------------|---------------|
| Release profile | Baseline |
| LTO (fat) | 5-10% |
| target-cpu=native | 3-8% |
| jemalloc | 2-5% |
| Fast hash (AHash) | 2-5% |
| PGO | 5-15% |
| BOLT | 2-8% |
| **Total** | **20-50%** |

## Recommended Build Command

For maximum performance:

```bash
# Step 1: Build with PGO
./scripts/pgo.sh

# Step 2: Apply BOLT (optional)
./scripts/bolt.sh
```

Or for a quick optimized build:

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

## Benchmarking

Use the tools in `perf/` directory to benchmark:

```bash
# TCP benchmark
cargo run --release --bin tcp_shadowquic

# UDP benchmark
cargo run --release --bin udp_shadowquic
```

## Notes

- PGO and BOLT require running representative workloads
- Benefits vary based on hardware and workload characteristics
- Profile data is hardware-specific, don't reuse across different CPUs
