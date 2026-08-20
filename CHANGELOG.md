# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2026-08-20

### Added

- **A `Value` can now be asked whether anyone is listening.** Behind the
  optional `exfiltrate` feature, `snapshot --subsystem values` answers two
  questions nothing else in the process could. A value with zero observers is a
  publisher nobody subscribed to — a wiring mistake that raises no error and
  produces no output, just silence. And an observer that has not caught up to
  the current generation is stale; one that stays stale has stopped polling.

  It never reports a value. Rows carry the generation and when the value last
  changed, never what it holds — enough to answer staleness, and unable to leak
  application data. That is also what the types allow: `T` is only `Clone`, not
  `Debug`, so rendering one would mean widening a bound on every user of the
  crate to serve a debugging feature.

  `never_changed` and `since_change_ms` are mutually exclusive, so "never set
  since construction" cannot be misread as "unchanged for a while".

## [Unreleased]

### Fixed

- **Stream consumers now count as caught up in value diagnostics.** Polling an
  `Observer` (directly or through `AggregateObserver`) updated its local value
  but not the optional `exfiltrate` registry, leaving an actively consumed
  stream looking permanently stale. Normal stream polling now advances the
  observed generation just like `current_value()` does.

- **Cloned observers no longer disappear from diagnostics.** Each clone is an
  independent subscriber, but the registry only counted observers made
  directly from a `Value`; dropping a clone then decremented a count it had
  never incremented. Clone and drop accounting now balance, so
  `live_observers` stays honest.

## [0.3.0] - 2026-08-17

### Breaking Changes

- **`Send` and `Sync` now ask for `T: Sync` as well**: sharing a `Value<T>` across threads used to require only `T: Send`, which was unsound. Readers clone straight out of the shared slot, so up to 127 threads can be sitting inside `T::clone` on the same value at once — and safe code could hand a `Value<RefCell<i32>>` to two threads and race on the borrow flag. Miri agrees, loudly.

  `Value<T>` and `Observer<T>` are now `Send`/`Sync` only when `T: Send + Sync`, and `AggregateObserver::add_observer` (with `From<Observer<T>>`) picked up the same bound. Note that this bites *moving* one between threads too, not just sharing it. Every ordinary `T` — `i32`, `String`, `Vec<_>`, `Arc<_>` — sails through untouched; the types this turns away are precisely the ones that were never safe to share in the first place. `Send + !Sync` values are still perfectly welcome on a single thread.

- **MSRV is now 1.95.0**, up from 1.85.1. Not our doing, exactly: `wasm_lite_std` 0.1.2 sets its own floor at 1.95.0, and it is a regular dependency, so ours cannot sit below it. Nothing in this crate needs anything past 1.85 on its own.

### Fixed

- **A panicking `Clone` no longer wedges the buffer**: if `T::clone` unwound while a slot lock was held, the lock leaked — and a leaked read lock left every later write spinning at 100% CPU forever, `Value`'s own destructor included. A leaked write lock did the same to readers. Both now release on the way out, and a write that falls over mid-clone leaves the old value exactly where it was.

  One caveat, and it is a real one: this rests on unwinding, and `wasm32-unknown-unknown` is `panic-strategy = "abort"`. A panicking clone traps there instead, no destructor runs, and the lock leaks just as it did before. Worse, in fact — a wasm panic is local to one instance, so the trapped worker dies alone while shared memory and every sibling thread carry on, spinning on a `FlipCard` that outlived the thread that broke it. If a `panic = "unwind"` mode ever lands, the guards start working there with no change on our end.

- **The wasm32 build compiles on current nightly again**: `AtomicU8::fetch_update` was renamed to `try_update` in 1.95, and since the wasm32 leg builds with `-D warnings`, a polite deprecation notice turned into a hard error. Now that the MSRV is 1.95 we simply call `try_update`.

- **The shipped docs match the code again**: the crate-doc logo was a relative path, which resolves against `target/doc` locally and against nothing at all on docs.rs — it is an absolute URL now. `README.md` had drifted from `lib.rs` besides: it never picked up the `T: Send + Sync` requirement above, and still called the library lock-free a while after writers started serializing on a mutex. Both are back in sync, and `release_prep`'s `readme-sync` check keeps them there.

  The examples changed shape slightly in the process. The executor call each one needs is now written out (`wasm_lite_std::async_doctest!(…)`) rather than hidden behind rustdoc's `#`, so the README shows exactly the code the doctest runs — and the thread-safety example reaches for `wasm_lite_std::spawn`, the one spelling that works on both targets, instead of quietly swapping `std::thread` out on wasm32.

- **Unflaked two tests**: the pair covering the panic-safety fix above shared a global "make the next clone panic" flag *and* the process-wide panic hook, while libtest happily ran them in parallel — so one could disarm the flag mid-way through the other's panicking clone, failing an assertion about code that was working fine. They take a gate now.

  Separately, `test_repeat_values` started its stopwatch *after* spawning the thread it was timing. A child that got off the line first already had some of its own sleeping behind it, so the elapsed time could land just under the threshold with nothing actually wrong. The clock now starts before the spawn.

### Changed

- **Traded `test_executors` for wasm_lite's test support**: `#[test_executors::async_test]` becomes `#[wasm_lite::wasm_lite_test]` — one attribute that is a plain libtest `#[test]` off wasm32 and a browser test on it — and the `test_executors::sleep_on(…)` wrapper in doctests becomes `wasm_lite_std::async_doctest!(…)`. wasm-bindgen has left the dependency graph entirely.

  This gets **wasm32 tests running again**, which `test_executors` was quietly blocking: its `#[async_test]` still expands to `#[wasm_bindgen_test]`, and nothing here depends on `wasm_bindgen_test` any more. Tests and doctests now run in a real browser by way of the `wasm_lite` runner, which the repo's `.cargo/config.toml` is pointed at.

  Test names shed the old prefix along the way — `async_test_test_observer_next` is now simply `test_observer_next`.

- **Nearly the whole suite runs on wasm32 now**: 23 of 25 unit tests run in a browser, up from 6. The contention tests (`test_concurrent_reads`, `test_concurrent_read_write`, `test_concurrent_writers`, `reproduce_panic`) run on genuine Web Workers via `#[wasm_lite_test(worker)]`, with thread and iteration counts scaled down there to fit the runner's page deadline. `wasm_lite_std` has no `std::sync::Barrier`, so they line up on a small spin-based `StartGate` instead.

  The two holdouts are the panicking-`Clone` tests, which exist to prove the slot guards release during an unwind — and as noted above, wasm32 does not unwind. They stay native-only until it can.

- **`web-time` out, `wasm_lite_std` in** (`0.1.2`): the wasm32 clock, threading and synchronization now come from `wasm_lite_std`, which is also what backs the internal `FlipCard` write lock. `test_executors` is gone from the dev-dependencies in favour of `wasm_lite`, and `wasm-bindgen` is no longer anywhere in the graph — on any target.

- **Slimmed down the wasm32 link flags**: added the `__stack_pointer` export the wasm_lite runner needs, and dropped `__tls_align`, `__tls_base`, `__heap_base` and `-Csymbol-mangling-version=legacy`, which were only ever there to work around wasm-bindgen issues that no longer apply to us.

## [0.2.0] - 2025-11-26

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

[0.3.0]: https://github.com/drewcrawford/await_values/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/drewcrawford/await_values/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/drewcrawford/await_values/releases/tag/v0.1.0
