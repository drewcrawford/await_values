# await_values

Primitives for subscribing to / notifying about changes to values.

![logo](art/logo.png)

A [`Value<T>`](Value) holds a `T`. Any number of `Observer`s — each a
[`Stream`](futures_core::Stream) — can await changes to it, from any thread, on
any executor. Set the value and every observer wakes and yields the new value;
drop the `Value` and every observer's stream ends.

This is a primitive for *state*, not events: a connection status, a window
size, a config struct. Observers always see the current value. One that polls
slowly skips intermediate values rather than queueing them, and setting a value
equal (by `PartialEq`) to what an observer last saw does not re-deliver it. If
every message matters, you want a channel, not this crate (see
[Alternatives](#alternatives)).

# Core Concepts

Your value type needs to be:
* `Clone` — observers receive copies, so readers never hold a lock on the storage.
* `PartialEq` — observers diff against the last value they yielded and skip duplicates.

The cast of characters:
* `Value` — owns the storage. Not `Clone`; wrap it in an `Arc` to share
  (which is why [`set`](Value::set) takes `&self`). [`get`](Value::get) and
  `set` also work directly, with no observer involved.
* `Observer` — a `Stream` of distinct values, created by
  `observe`. The first poll yields the current value immediately;
  later polls wait for a change. [`current_value`](Observer::current_value)
  and [`is_dirty`](Observer::is_dirty) cover the non-async cases.
* `aggregate::AggregateObserver` — bundles observers of *different* types
  into one stream that yields the index of whichever changed.

The crate is executor-agnostic: the only async dependency is the
`futures_core::Stream` trait, so it runs under tokio, smol, a hand-rolled
`block_on`, or the browser event loop, unchanged. wasm32 is a first-class
target — the test suite runs in a real headless browser, and nothing here
pulls in wasm-bindgen.

# Quick Start

Both `Observer` and `aggregate::AggregateObserver` implement the `futures_core::Stream` trait,
which is the primary way to consume values from observers. The `Stream` trait provides the `next()`
method (via `StreamExt`) that returns `Option<T>`, where `None` indicates the underlying value has
been dropped.

```rust
use await_values::{Value, Observer};
use futures_util::StreamExt;

wasm_lite_std::async_doctest!(async {
// Create an observable value
let value = Value::new(42);

// Create an observer
let mut observer = value.observe();

// Get the current value (using Stream trait's next() method)
assert_eq!(observer.next().await.unwrap(), 42);

// Update the value
value.set(100);

// Observe the change
assert_eq!(observer.next().await.unwrap(), 100);
});
```

# Observing Multiple Values

`aggregate::AggregateObserver` waits on many values at once, even when their
types differ. Instead of yielding values (they'd have different types), it
yields the index of the observer that changed; you then read the value from
the matching observer or `Value`. It polls in index order and yields the
lowest ready index, so put chatty values last. When an observed `Value` is
dropped, its index is yielded one final time; when all of them are gone, the
stream ends.

```rust
use await_values::{Value, aggregate::AggregateObserver};
use futures_util::StreamExt;

wasm_lite_std::async_doctest!(async {
let temperature = Value::new(20.5);
let status = Value::new("OK");

let mut aggregate = AggregateObserver::new();
aggregate.add_observer(temperature.observe());
aggregate.add_observer(status.observe());

// Wait for initial values
let index = aggregate.next().await;
assert!(index == Some(0) || index == Some(1));

// Change a value
temperature.set(25.0);

// See which observer changed
let changed_index = aggregate.next().await;
assert_eq!(changed_index, Some(0)); // temperature changed
});
```

# Thread Safety

Every type here is safe to share across threads. `set` takes `&self`, so the
usual pattern is `Arc<Value<T>>` with writers and observers on whatever
threads you like.

Sharing requires `T: Send + Sync`, not just `Send`: concurrent readers clone
out of the same storage at the same time, so `T::clone` must tolerate being
called from several threads at once. A `Send + !Sync` type such as
`RefCell<T>` still works in a `Value` on a single thread; the `Value` just
can't be shared.

(The example spawns with `wasm_lite_std::spawn` rather than `std::thread`
only so that it also runs in the browser test suite, where `std::thread` and
blocking `join` on the main thread don't exist.)

```rust
use await_values::Value;
use std::sync::Arc;

wasm_lite_std::worker_doctest!(|| {
// Wrap Value in Arc to share between threads
let value = Arc::new(Value::new(0));
let value_clone = Arc::clone(&value);

let handle = wasm_lite_std::spawn(move || {
    value_clone.set(42);
});

handle.join().unwrap();
assert_eq!(value.get(), 42);
});
```

# How It Works

Storage is a double buffer of two slots that alternate roles. Readers clone
out of the front slot under a shared lock that admits up to 127 concurrent
readers; a writer fills the back slot and then flips a pointer. Readers never
block writers, writers never block readers, and only writers serialize with
each other. Wakeups go through a lock-free Treiber stack of registered wakers,
ordered so that a `set` racing a poll can't lose a notification.

# Alternatives

The same problem is solved several ways across the ecosystem. The near
neighbors, and where this crate differs:

**[`tokio::sync::watch`](https://docs.rs/tokio/latest/tokio/sync/watch/)** is
the same shape: one slot, many waiting readers, latest-value-only delivery.
It differs in the details. `send` marks every receiver changed whether or not
the value actually differs (you opt out with `send_if_modified`), where
`await_values` compares with `PartialEq` before yielding. Reads hand out a
lock guard, and a `borrow()` held too long — across an `.await`, say — blocks
the writer; here reads clone the value out, so there is no guard to hold
wrong. And `Receiver` isn't a `Stream` without the `tokio-stream` wrapper
crate. If you already depend on tokio and none of that bites, `watch` is a
fine choice.

**[`futures-signals`](https://crates.io/crates/futures-signals)** is a full
FRP toolkit: `Mutable`, a `Signal` algebra of `map`/`dedupe`/etc., plus
signal vectors and maps. Reach for it when you want *derived* state — values
computed from other values and kept current. `await_values` stops at the
primitive: one cell, one `Stream`, and ordinary `StreamExt` combinators for
everything downstream. (Note that its signals deliver duplicates unless you
add `.dedupe()`; here dedup is the default.)

**[`postage`](https://crates.io/crates/postage)** and
**[`async-watch`](https://crates.io/crates/async-watch)** offer
executor-agnostic watch channels with tokio-watch-style semantics —
sender/receiver split, borrow-guard reads, no equality filtering. Closer in
spirit to this crate than tokio itself, with the same behavioral differences
as above.

**Broadcast channels**
([`tokio::sync::broadcast`](https://docs.rs/tokio/latest/tokio/sync/broadcast/),
[`async-broadcast`](https://crates.io/crates/async-broadcast)) deliver every
message to every receiver, and a receiver that falls behind lags or errors.
That's the right tool when each message is an *event* that must be handled.
`await_values` is deliberately lossy in exactly that case: an observer that
wakes late sees only the newest value.

**[`arc-swap`](https://crates.io/crates/arc-swap)** shares the "readers grab
the latest value without blocking" goal and its reads are faster, but it has
no notification side — you poll. Pair it with
[`event-listener`](https://crates.io/crates/event-listener) and some dedup
logic and you have roughly rebuilt this crate.

Reasons to pick something else: your type can't be `Clone + PartialEq`; every
read here is a clone, so large payloads want an `Arc<T>` inside the `Value`;
there is no history, no `send`-style backpressure, and no derived-value
algebra. Reasons to pick this: watch-style state observation as a plain
`Stream`, equality dedup by default, four small dependencies with no runtime
attached, and a test suite that runs on native *and* inside real browsers on
wasm32.

# Feature Flags

`exfiltrate` (off by default) maintains a process-wide registry of live
`Value`s and exposes it through the `exfiltrate` debugging tool's `snapshot`
command. It costs a registry update per `set`.
