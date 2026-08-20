# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`CLAUDE.md` is a symlink to `AGENTS.md` — edit `AGENTS.md`.

## Build Commands

```bash
scripts/check_all          # everything below, in order

scripts/fmt                # rustfmt
scripts/check              # cargo check  (native + wasm32)
scripts/clippy             # clippy       (native + wasm32)
scripts/docs               # rustdoc      (native + wasm32)
scripts/tests              # tests        (native + wasm32)
scripts/miri test --lib    # Miri         (native; disables wasm build-std)
```

Each of `check`/`clippy`/`docs`/`tests` is a thin wrapper over `scripts/native/<x>`
and `scripts/wasm32/<x>`; run those directly to isolate one target. All of them
set `-D warnings`.

```bash
cargo test test_name                        # single native test
cargo test --doc "Value<T>::get"            # single doctest
```

Use `scripts/miri`, not a bare `cargo +nightly miri`, inside this repository.
Miri supplies its own custom sysroot; the wrapper prevents the unconditional
wasm32 `build-std` config from also rebuilding `core` during Miri setup, which
otherwise fails with duplicate `rmeta` candidates on a clean Miri cache.

## wasm32 is a real browser

`.cargo/config.toml` sets `runner = ["wasm_lite", "run"]`, so wasm32 tests and
doctests are compiled, served, and driven in a headless browser by the
`wasm_lite` CLI — which must be on `PATH` (`cargo install wasm_lite`). Each test
gets a fresh page load, so a wasm32 run is far slower than the native one.

wasm32 needs nightly (`build-std` for atomics) and the right link flags. The
flags live in `.cargo/config.toml`, but rustdoc does not read `rustflags`, so
they are duplicated under `[target.wasm32-unknown-unknown].rustdocflags` — keep
the two lists in sync. `scripts/wasm32/_env` reads them back out into
`$WASM32_RUSTFLAGS`, which is how to run one wasm32 test by hand:

```bash
source scripts/wasm32/_env
RUSTFLAGS="$WASM32_RUSTFLAGS" cargo +nightly test --target=wasm32-unknown-unknown --lib test_name
```

`WASM_LITE_BROWSER=chrome` picks a browser (default firefox);
`WASM_LITE_TIMEOUT_SECS` raises the per-page deadline.

## Architecture

An executor-agnostic observable-value library. Three public types plus the
lock-free buffer underneath them:

- **`Value<T>`** (`src/lib.rs`) — owns the storage. Not `Clone` (it has a `Drop`
  that would need refcounting); wrap in `Arc` to share, which is why `set` takes
  `&self`. Internally holds `Arc<Shared<T>>`.

- **`Observer<T>`** (`src/lib.rs`) — a `futures_core::Stream`. Keeps its own
  `observed: Option<T>` and only yields when the current value differs by
  `PartialEq`. Registers an `AtomicWaker` in `Shared`'s Treiber stack.

- **`AggregateObserver`** (`src/aggregate.rs`) — type-erased `Vec` of observers;
  the stream yields the *index* that changed. Returns the lowest ready index, so
  a hot observer can starve later ones. `hangup_reported` makes a dropped value
  yield its index exactly once instead of forever.

- **`FlipCard<T>`** (`src/flip_card.rs`, `pub(crate)`) — two slots that alternate
  roles. Readers clone out of the "front" slot under a shared read lock (up to
  127 of them, tracked in a packed `AtomicU8`); a writer fills the "back" slot
  then flips a pointer.

`Shared<T>` stores a `FlipCard<Option<T>>`: the inner `None` is the hangup
signal, written by `Value::drop`, and it is what turns into `Stream::poll_next`
returning `Ready(None)`.

### Invariants worth not breaking

These are spread across files and are easy to violate by accident:

- **Exactly one slot holds `Some`** at a time. `flip_to` relies on it
  (`.expect("Prior value")`), and it only holds because writers serialize on
  `FlipCard::write_lock`. Two concurrent writers would both fill the same back
  slot and both `take` the front.

- **The wakeup protocol is three orderings that compose.** `poll_next` registers
  its waker *before* reading; `Value::set` notifies *after* flipping; and
  `Shared::notify`/`Observer::drop` restore every entry to the Treiber stack
  *before* waking any of them. Drop any one and a notification can be lost:
  `notify` drains the whole stack, so an entry is briefly absent, and only the
  restore-then-wake order guarantees the observer re-reads after the write that
  missed it. Waker panics are deferred until all entries are restored and all
  wakers have had their turn.

- **`FlipCard: Sync` requires `T: Send + Sync`**, not just `Send`. Readers clone
  from a *shared* slot, so many threads can be inside `T::clone` at once.
  Requiring only `Send` was a soundness hole (safe code could race a
  `Value<RefCell<i32>>`); Miri catches it if it regresses.

- **`ReadGuard` releases the slot's read lock on unwind**, so a panicking
  `T::clone` cannot leave a lock held and spin every later writer forever.
  Writes move their owned input and run no user code while the slot is locked;
  `WriteGuard` remains defense against a future unwind point. Unwind guards do
  nothing on wasm32, which is `panic-strategy = "abort"`; see the note on
  `ReadGuard`.

## Test conventions

- `#[wasm_lite_test]` replaces `#[test]` for anything that should run on both
  targets: a libtest `#[test]` off wasm32, a browser test on it. Plain `#[test]`
  means native-only.
- Use `#[wasm_lite_test(worker)]` when the body **blocks** — `JoinHandle::join`,
  `park`, `recv_block`, `lock_block`. Those trap on the browser main thread, so
  the body has to run on a dedicated worker.
- Doctests that await use `wasm_lite_std::async_doctest!(async { … })`; doctests
  that block use `worker_doctest!(|| { … })`. Both are no-ops-with-a-`block_on`
  natively. Hide them behind `#` so rendered docs stay clean.
- `std::thread` does not work on wasm32. Test modules alias
  `use wasm_lite_std as thread;` there, so bodies read the same on both targets.
  `wasm_lite_std` has no `Barrier` — `flip_card.rs` uses a local `StartGate`.
- Browser runs are slow, so thread and iteration counts are cfg-scaled
  (`THREADS`, `STRESS` in `flip_card.rs`). Keep native counts high.
- Tests that swap the process-wide panic hook or a global flag must take
  `fussy::gate()` — libtest runs tests in parallel and they will race otherwise.

## Dependencies

`futures-core` (Stream trait only, no runtime), `atomic-waker`, `treiber_stack`
(observer registrations), and `wasm_lite_std` (the `FlipCard` write lock, plus
the wasm32 clock/threading/sync). `wasm_lite` is a dev-dependency on **all**
targets for the test macro. Nothing pulls in wasm-bindgen.

MSRV is 1.95.0, inherited from `wasm_lite_std` — nothing here needs past 1.85.
