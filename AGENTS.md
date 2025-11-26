# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
# Full validation (format, check, clippy, docs, tests)
scripts/check_all

# Individual commands
scripts/fmt                # Format code
scripts/check              # cargo check (native + wasm32)
scripts/clippy             # Clippy lints (native + wasm32)
scripts/docs               # Build docs
scripts/tests              # Run tests (native + wasm32)

# Run a single test
cargo test test_name

# Run tests on wasm32 target only
cargo +nightly test --target=wasm32-unknown-unknown
```

All checks use `RUSTFLAGS="-D warnings"` to treat warnings as errors.

## Architecture

This is an executor-agnostic async observable value library with three main types:

- **`Value<T>`** (`src/lib.rs`): The observable value holder. Uses interior mutability, does not implement `Clone` (wrap in `Arc` for sharing). Dropping notifies all observers with `None`.

- **`Observer<T>`** (`src/lib.rs`): Implements `futures_core::Stream`. Maintains per-observer state to track last seen value. Only yields when value differs from last observation (uses `PartialEq`).

- **`AggregateObserver`** (`src/aggregate.rs`): Type-erased container for multiple heterogeneous `Observer`s. Stream yields the index of whichever observer changed.

**Internal `FlipCard<T>`** (`src/flip_card.rs`): Lock-free double-buffer that powers the library. Two slots alternate roles - readers access "front" while writers update "back", then atomically flip. Supports up to 127 concurrent readers per slot via packed atomic u8 state.

## Key Dependencies

- `futures-core`: Stream trait (no runtime dependency)
- `atomic-waker`: Waker registration
- `treiber_stack`: Lock-free stack for observer tracking
- `wasm_safe_mutex`: WASM-compatible synchronization
- `test_executors`: Test runtime (dev dependency)
