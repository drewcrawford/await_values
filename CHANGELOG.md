# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Breaking Changes

- **Simplified error handling**: Removed `ObserverError` type entirely. `Observer::current_value()` now returns `Option<T>` instead of `Result<T, ObserverError>`. When a value is dropped, observers gracefully receive `None` rather than an error—because sometimes silence speaks louder than exceptions.

- **Streamlined API**: Removed `Observer::next()` and `AggregateObserver::next()` methods. Both types now implement `futures_core::Stream`, so you can use all your favorite stream combinators directly. Less API surface, more composability—everybody wins!

- **Type constraints tightened**: `Value<T>` and `Observer<T>` now require `T: Clone`. This keeps the implementation lock-free and happy. Also added `Clone` requirement to `Observer::is_dirty()` for consistency.

### Added

- **Stream implementations**: Both `Observer<T>` and `AggregateObserver` now implement `futures_core::Stream`. Poll to your heart's content, or use them with `StreamExt`—the async ecosystem is your oyster.

- **Display trait**: Added `Display` implementations for better debugging and logging. Because sometimes you just want to print what's going on.

- **Equality and hashing**: `Value<T>` now implements `Eq`, `PartialEq`, and `Hash` when `T` supports them. Store your values in hash maps or compare them directly—we won't judge.

- **New FlipCard traits**: Added additional trait implementations for the internal `FlipCard` type to improve flexibility and composability.

### Fixed

- **Race condition squashed**: Fixed a subtle race in the FlipCard double-buffer implementation. Your concurrent reads and writes should now play nicely together, even under heavy load.

- **Increased ID capacity**: Bumped internal ID field from narrower type to `u64`. If you were planning to create 18 quintillion observers, we've got you covered now.

- **Memory ordering improvements**: Switched to release memory ordering in FlipCard operations for better performance on architectures that care about such things.

### Changed

- **CI/CD overhaul**: Complete refresh of the continuous integration pipeline. Tests run faster, checks are more thorough, and everything's a bit more pleasant to maintain.

- **Documentation polish**: Rewrote and expanded documentation throughout. The examples are clearer, the explanations are friendlier, and we fixed those typos nobody mentioned.

- **Test coverage expansion**: Added more test cases, especially around edge conditions and WASM compatibility. Because finding bugs in production is so last season.

## [0.1.0] - 2025-08-12

Initial release of await_values—your friendly neighborhood async observable value library!

### Features

- **`Value<T>`**: Lock-free observable value holder with interior mutability
- **`Observer<T>`**: Stream-based observer that tracks changes and only notifies on actual updates
- **`AggregateObserver`**: Type-erased container for heterogeneous observers
- **`FlipCard<T>`**: Lock-free double-buffer supporting up to 127 concurrent readers per slot
- **Executor-agnostic**: Works with any async runtime or no runtime at all
- **WASM support**: Runs great in browsers thanks to careful synchronization primitives

[Unreleased]: https://github.com/drewcrawford/await_values/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/drewcrawford/await_values/releases/tag/v0.1.0
