# CI/CD Performance Optimizations

This document describes how performance optimizations are applied in GitHub Actions CI/CD.

## Optimizations Applied in CI

### 1. LTO (Link-Time Optimization)

**Status:** ✅ Enabled by default in `Cargo.toml`

```toml
[profile.release]
lto = "fat"
```

**Performance gain:** 5-10%

### 2. Codegen Units

**Status:** ✅ Enabled in `Cargo.toml`

```toml
[profile.release]
codegen-units = 1
```

**Performance gain:** 2-5%

### 3. Optimization Level

**Status:** ✅ Maximum optimization

```toml
[profile.release]
opt-level = 3
```

### 4. Strip Symbols

**Status:** ✅ Enabled for smaller binary

```toml
[profile.release]
strip = true
```

**Binary size reduction:** 30-50%

### 5. Panic Strategy

**Status:** ✅ Abort on panic

```toml
[profile.release]
panic = "abort"
```

**Binary size reduction:** 5-10%

### 6. Target CPU Optimizations

**Status:** ✅ Enabled via RUSTFLAGS in CI

```yaml
rustflags: "-C target-cpu=x86-64-v3"  # For x86_64
rustflags: "-C target-cpu=apple-m1"    # For M1/M2
```

**CPU features enabled:**
- x86-64-v3: AVX, AVX2, BMI1, BMI2, FMA
- apple-m1: ARM NEON, specific Apple Silicon optimizations

**Performance gain:** 3-8%

### 7. jemalloc Memory Allocator

**Status:** ✅ Enabled by default

```toml
[features]
default = ["jemalloc"]
```

**Performance gain:** 2-5%

### 8. Fast Hash (AHash)

**Status:** ✅ Already in use

**Performance gain:** 2-5%

## Advanced Optimizations (Manual)

### PGO (Profile-Guided Optimization)

**Status:** ⚠️ Manual workflow

PGO requires actual workload execution and is not suitable for automated CI.

**How to use:**

1. Go to Actions → "Optimized Release (PGO)"
2. Click "Run workflow"
3. Select target platform
4. Enable "profile_workload" if you want to run profiling
5. Wait for build to complete
6. Download optimized binary

**Performance gain:** 5-15%

**Note:** Profile data is hardware-specific. Don't reuse across different CPUs.

### BOLT (Binary Optimization and Layout Tool)

**Status:** ⚠️ Disabled by default

BOLT requires:
1. Actual hardware execution
2. Performance profiling
3. Binary reconstruction

**How to use locally:**

```bash
# See scripts/bolt.sh for automated script
./scripts/bolt.sh
```

**Performance gain:** 2-8% (on top of PGO)

## CI Workflow Files

### 1. `release.yml` - Standard Release

**Optimizations:**
- ✅ LTO (fat)
- ✅ target-cpu optimizations
- ✅ jemalloc
- ✅ AHash
- ✅ All Cargo.toml optimizations

**When to use:** Production releases, all tags

**Estimated performance:** Good (80-90% of max)

### 2. `optimized-release.yml` - PGO Build

**Optimizations:**
- ✅ Everything from standard release
- ✅ PGO (Profile-Guided Optimization)

**When to use:** Manual builds for specific hardware

**Estimated performance:** Excellent (95-100% of max)

### 3. Local Build (Maximum Performance)

```bash
# Maximum optimization with PGO + BOLT
./scripts/pgo.sh
./scripts/bolt.sh
```

**Estimated performance:** Maximum (100%)

## Build Commands Reference

### Quick Build (Development)

```bash
cargo build --release
```

### Optimized Build (Standard)

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

### PGO Build

```bash
# Instrumented build
RUSTFLAGS="-C target-cpu=native -C profile-generate=./profdata" \
  cargo build --release

# Run workload
./target/release/shadowquic

# Optimized build
RUSTFLAGS="-C target-cpu=native -C profile-use=./profdata/merged.profdata" \
  cargo build --release
```

### BOLT Build

```bash
# See scripts/bolt.sh
./scripts/bolt.sh
```

## Performance Comparison Table

| Build Type | LTO | CPU | PGO | BOLT | Performance |
|-----------|-----|-----|-----|------|-------------|
| Debug | ❌ | ❌ | ❌ | ❌ | Baseline |
| Release | ✅ | ❌ | ❌ | ❌ | +20-30% |
| Optimized | ✅ | ✅ | ❌ | ❌ | +25-40% |
| PGO | ✅ | ✅ | ✅ | ❌ | +35-55% |
| PGO+BOLT | ✅ | ✅ | ✅ | ✅ | +40-65% |

## Notes

1. **PGO is hardware-specific**: Don't distribute PGO binaries built on different hardware
2. **BOLT is optional**: Requires Linux and special setup
3. **CI uses generic optimizations**: For maximum performance, build locally with `target-cpu=native`
4. **Profile data expires**: Rebuild PGO binaries periodically with new workloads

## Future Improvements

- [ ] Automated PGO with benchmark suite
- [ ] Multi-stage PGO (multiple workloads)
- [ ] Cross-platform BOLT support
- [ ] Automated performance regression testing
