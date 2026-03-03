# AGENTS.md - ShadowQUIC Developer Guide

## Build Commands

```bash
cd shadowquic
 build --release
cargo build
cargo```

## Lint Commands

```bash
cargo fmt --check --verbose
cargo fmt --verbose
cargo clippy --all-targets
```

## Test Commands

```bash
# Run all tests
cargo test --release --verbose

# Run single test
cargo test --release test_name
cargo test --release --test tcp_echo

# Integration tests
cargo test --release --test tcp_echo
cargo test --release --test tcp_echo_sunnyquic
cargo test --release --test udp_overstream_echo
cargo test --release --test socks_out

# Python integration tests
./scripts/main_test.py
```

## Code Style

### Imports Order
1. `std` imports
2. External crates (`bytes`, `tokio`, `tracing`)
3. Local `crate` imports

### Naming
- Modules: `snake_case`
- Structs/Enums: `PascalCase`
- Functions/Variables: `snake_case`
- Traits: `PascalCase` (e.g., `Inbound`, `Outbound`)

### Error Handling
- Use `thiserror` for error enums
- Define errors in `src/error.rs` with `SError` enum
- Use `SResult<T> = Result<T, SError>` alias
- Log with `tracing::error!` before returning

```rust
#[derive(Error, Debug)]
pub enum SError {
    #[error("IO Error:{0}")]
    Io(#[from] io::Error),
    #[error("Outbound unavailable")]
    OutboundUnavailable,
}
pub type SResult<T> = Result<T, SError>;
```

### Async Patterns
- Use `async_trait` for async traits
- Use `tokio::spawn` for background tasks
- Use `tokio::try_join!` for concurrency
- Use `tracing::Instrument` for span propagation

### Configuration
- Use `serde` with `Deserialize` for YAML
- Use `#[serde(rename_all = "kebab-case")]`
- Use `#[serde(tag = "type")]` for tagged enums
- Provide `Default` implementations

### Logging
- Use `tracing` crate
- `trace!` - detailed debug, `debug!` - general debug
- `info!` - operational events, `warn!` - unexpected but handled
- `error!` - failures

### Testing
- `#[tokio::test]` for async, `#[test]` for sync
- Unit tests in `src/module.rs` with `#[cfg(test)]`
- Integration tests in `tests/`

## Project Structure

```
shadowquic/
├── Cargo.toml           # Workspace
├── shadowquic/          # Main crate
│   ├── src/
│   │   ├── lib.rs      # Public API/traits
│   │   ├── error.rs    # SError enum
│   │   ├── config/     # Configuration
│   │   ├── direct/     # Direct outbound
│   │   ├── shadowquic/ # ShadowQUIC protocol
│   │   ├── sunnyquic/  # SunnyQUIC protocol
│   │   └── socks/      # SOCKS protocol
│   ├── tests/         # Integration tests
│   └── examples/      # Example programs
└── shadowquic-macros/ # Proc macros
```

## Key Conventions

- Use `Arc<T>` for shared ownership across async tasks
- Use `Box<dyn Trait>` for trait objects
- Document public APIs with doc comments (`///`)
- Use `anyhow::Result` at application layer
- Run `cargo fmt` before committing
