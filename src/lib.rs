// SPDX-License-Identifier: MIT OR Apache-2.0
/*!
# await_values

Primitives for subscribing to / notifying about changes to values.

![logo](https://github.com/drewcrawford/await_values/raw/main/art/logo.png)

A [`Value<T>`](Value) holds a `T`. Any number of [`Observer`]s — each a
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
* [`Value`] — owns the storage. Not `Clone`; wrap it in an `Arc` to share
  (which is why [`set`](Value::set) takes `&self`). [`get`](Value::get) and
  `set` also work directly, with no observer involved.
* [`Observer`] — a `Stream` of distinct values, created by
  [`Value::observe`]. The first poll yields the current value immediately;
  later polls wait for a change. [`current_value`](Observer::current_value)
  and [`is_dirty`](Observer::is_dirty) cover the non-async cases.
* [`aggregate::AggregateObserver`] — bundles observers of *different* types
  into one stream that yields the index of whichever changed.

The crate is executor-agnostic: the only async dependency is the
`futures_core::Stream` trait, so it runs under tokio, smol, a hand-rolled
`block_on`, or the browser event loop, unchanged. wasm32 is a first-class
target — the test suite runs in a real headless browser, and nothing here
pulls in wasm-bindgen.

# Quick Start

Both [`Observer`] and [`aggregate::AggregateObserver`] implement the `futures_core::Stream` trait,
which is the primary way to consume values from observers. The `Stream` trait provides the `next()`
method (via `StreamExt`) that returns `Option<T>`, where `None` indicates the underlying value has
been dropped.

```
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

[`aggregate::AggregateObserver`] waits on many values at once, even when their
types differ. Instead of yielding values (they'd have different types), it
yields the index of the observer that changed; you then read the value from
the matching observer or `Value`. It polls in index order and yields the
lowest ready index, so put chatty values last. When an observed `Value` is
dropped, its index is yielded one final time; when all of them are gone, the
stream ends.

```
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

```
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
*/

#[cfg(feature = "exfiltrate")]
pub mod exfiltrate_provider;
#[cfg(feature = "exfiltrate")]
pub mod registry;

pub mod aggregate;
pub(crate) mod flip_card;

use crate::flip_card::FlipCard;
use atomic_waker::AtomicWaker;
use std::fmt::{Debug, Display};
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};

/// Result of a non-blocking observation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Observation {
    /// A new, distinct value was observed.
    NewValue,
    /// The underlying [`Value`] has been dropped.
    Hangup,
    /// No new value is available.
    Unchanged,
}

struct ActiveObservation {
    id: u64,
    notify: AtomicWaker,
}

impl ActiveObservation {
    fn notify(&self) {
        self.notify.wake();
    }
    fn register(&self, waker: &Waker) {
        self.notify.register(waker);
    }
}

impl Debug for ActiveObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ActiveObservation(id: {})", self.id)
    }
}

#[derive(Debug)]
struct Shared<T> {
    /// This value's row in the debug registry. See [`registry`].
    #[cfg(feature = "exfiltrate")]
    registry_id: u64,
    next_observer_id: AtomicU64,
    value: FlipCard<Option<T>>,
    active_observations: treiber_stack::TreiberStack<Weak<ActiveObservation>>,
}

impl<T> Shared<T> {
    fn notify(&self) {
        for orig in self.active_observations.drain() {
            if let Some(active) = orig.upgrade() {
                self.active_observations.push_arc(orig);
                active.notify();
            } else {
                // If the active observation has been dropped, we don't need to notify it
                // and can safely ignore it.
            }
        }
    }
}

/// Allocates storage for a value that can be observed.
///
/// `Value<T>` is the primary way to create observable values in this library.
/// It holds a value of type `T` and allows multiple [`Observer`]s to watch for changes.
///
/// # Thread Safety
///
/// `Value` is thread-safe and can be used from multiple threads. All operations
/// use interior mutability with proper synchronization.
///
/// # Examples
///
/// ```
/// use await_values::Value;
///
/// // Create a value
/// let value = Value::new(42);
///
/// // Read the current value
/// assert_eq!(value.get(), 42);
///
/// // Update the value
/// let old = value.set(100);
/// assert_eq!(old, 42);
/// assert_eq!(value.get(), 100);
/// ```
///
/// # Design Note
///
/// `Value` does not implement `Clone` because it also implements `Drop`, which would require
/// reference counting to ensure that the value is not dropped while there are still observers.
/// If you need to share a `Value` across multiple owners, wrap it in `Arc`.

/*
Design note - the problem with making this Clone is that it also implements Drop, which would require
reference counting to ensure that the value is not dropped while there are still observers.

It's probably easiest to wrap this in Arc, which is why set is not &mut self.
 */
#[derive(Debug)]
pub struct Value<T: Clone> {
    shared: Arc<Shared<T>>,
}

impl<T: Clone> Value<T> {
    /// Creates a new `Value` with the given initial value.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::Value;
    ///
    /// let value = Value::new("Hello, world!");
    /// assert_eq!(value.get(), "Hello, world!");
    /// ```
    #[cfg_attr(feature = "exfiltrate", track_caller)]
    pub fn new(value: T) -> Self {
        #[cfg(feature = "exfiltrate")]
        let registry_id = registry::record_created(std::panic::Location::caller());
        Self {
            shared: Arc::new(Shared {
                #[cfg(feature = "exfiltrate")]
                registry_id,
                value: FlipCard::new(Some(value)),
                active_observations: treiber_stack::TreiberStack::default(),
                next_observer_id: AtomicU64::new(0),
            }),
        }
    }

    /// Returns a copy of the current value.
    ///
    /// # Panics
    ///
    /// Panics if the value has been dropped (hungup).
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::Value;
    ///
    /// let value = Value::new(vec![1, 2, 3]);
    /// let data = value.get();
    /// assert_eq!(data, vec![1, 2, 3]);
    /// ```
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.shared.value.read().expect("Value is hungup")
    }

    /// Sets a new value and returns the old value.
    ///
    /// This method will notify all active observers that the value has changed,
    /// even if the new value equals the old value.
    ///
    /// # Panics
    ///
    /// Panics if the value has been dropped (hungup).
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::Value;
    ///
    /// let value = Value::new(10);
    /// let old = value.set(20);
    /// assert_eq!(old, 10);
    /// assert_eq!(value.get(), 20);
    /// ```
    pub fn set(&self, value: T) -> T
    where
        T: Clone,
    {
        let old = self.shared.value.flip_to(Some(value));
        #[cfg(feature = "exfiltrate")]
        registry::record_set(self.shared.registry_id);
        self.notify();
        old.expect("Value is hungup")
    }

    fn notify(&self) {
        self.shared.notify();
    }

    /// Returns a new `Observer` for this `Value`.
    ///
    /// Each observer maintains its own state tracking which values it has seen,
    /// allowing multiple independent observers to watch the same value.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::Value;
    ///
    /// let value = Value::new(42);
    /// let mut observer1 = value.observe();
    /// let mut observer2 = value.observe();
    ///
    /// // Both observers can independently track changes
    /// assert_eq!(observer1.current_value().unwrap(), 42);
    /// assert_eq!(observer2.current_value().unwrap(), 42);
    /// ```
    pub fn observe(&self) -> Observer<T> {
        Observer::new(self)
    }
}

impl<T: Clone> Drop for Value<T> {
    fn drop(&mut self) {
        // When the value is dropped, we need to notify all observers that the value is hung up.
        // This is done by setting the value to None, which indicates that the value is no
        // longer available.
        let old = self.shared.value.flip_to(None);
        #[cfg(feature = "exfiltrate")]
        registry::record_value_dropped(self.shared.registry_id);
        self.notify();
        // Destructors are user code and may panic. Run them only after hangup
        // is recorded and every pending observer has been woken.
        drop(old);
    }
}

/// A handle to a value that can be used to observe when the value changes remotely.
///
/// Observers have an internal 'state' that tracks the last observed value.
/// This allows them to return the current value immediately, and then wait for the next value to change.
///
/// # Cloning
///
/// `Observer` implements `Clone`, allowing you to create multiple independent observers
/// from a single observer. Each clone maintains its own observation state.
///
/// # Examples
///
/// ```
/// use await_values::Value;
/// use futures_util::StreamExt;
///
/// # wasm_lite_std::async_doctest!(async {
/// let value = Value::new("initial");
/// let mut observer = value.observe();
///
/// // First call returns the current value
/// assert_eq!(observer.next().await.unwrap(), "initial");
///
/// // Update the value
/// value.set("updated");
///
/// // Next call returns the new value
/// assert_eq!(observer.next().await.unwrap(), "updated");
/// # });
/// ```
#[derive(Debug)]
pub struct Observer<T> {
    active_observation: Arc<ActiveObservation>,
    shared: Arc<Shared<T>>,
    //The value last observed.
    observed: Option<T>,
    observer_id: u64,
}

impl<T: Clone> Clone for Observer<T> {
    /**
        Cloning an observer creates a new instance that
        a) Observes the same Value
        b) Copies (but does not share) the last observed value
        c) Creates a new active observation with a new ID
    */
    fn clone(&self) -> Self {
        // Cloning an observer creates a new instance with the same shared state,
        // but a new active observation ID.
        let observer_id = self.shared.next_observer_id.fetch_add(1, Relaxed);
        let active = Arc::new(ActiveObservation {
            id: observer_id,
            notify: AtomicWaker::new(),
        });
        self.shared
            .active_observations
            .push(Arc::downgrade(&active));
        #[cfg(feature = "exfiltrate")]
        registry::record_observer_created(self.shared.registry_id);
        Self {
            active_observation: active,
            shared: self.shared.clone(),
            observed: self.observed.clone(),
            observer_id,
        }
    }
}

impl<T> futures_core::Stream for Observer<T>
where
    T: PartialEq + Clone + Unpin,
{
    type Item = T;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.active_observation.register(cx.waker());
        // Check if the observer has a distinct value available
        match self.get_mut().next_when_immediately_available() {
            Ok(v) => Poll::Ready(v),
            Err(_) => Poll::Pending,
        }
    }
}

impl<T> Observer<T> {
    /// Creates a new observer for the given `Value`.
    ///
    /// The observer starts with no observed value, meaning the first call to
    /// `next()` (from the `Stream` trait) or [`current_value`](Self::current_value) will
    /// return the current value immediately.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::{Value, Observer};
    ///
    /// let value = Value::new(42);
    /// let observer = Observer::new(&value);
    /// ```
    pub fn new(value: &Value<T>) -> Self
    where
        T: Clone,
    {
        let observer_id = value.shared.next_observer_id.fetch_add(1, Relaxed);
        let active = Arc::new(ActiveObservation {
            id: observer_id,
            notify: AtomicWaker::new(),
        });
        value
            .shared
            .active_observations
            .push(Arc::downgrade(&active));
        let shared = value.shared.clone();
        #[cfg(feature = "exfiltrate")]
        registry::record_observer_created(shared.registry_id);
        Self {
            shared,
            observed: None,
            observer_id,
            active_observation: active,
        }
    }

    /// Returns the current value observed.
    ///
    /// This method always returns the current value from the underlying [`Value`],
    /// updating the observer's internal state. It does not wait for changes.
    ///
    /// # Returns
    ///
    /// Returns `None` if the underlying [`Value`] has been dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::Value;
    ///
    /// let value = Value::new(vec![1, 2, 3]);
    /// let mut observer = value.observe();
    ///
    /// // Get current value
    /// assert_eq!(observer.current_value().unwrap(), vec![1, 2, 3]);
    ///
    /// // Update and get new value
    /// value.set(vec![4, 5, 6]);
    /// assert_eq!(observer.current_value().unwrap(), vec![4, 5, 6]);
    /// ```
    pub fn current_value(&mut self) -> Option<T>
    where
        T: Clone,
    {
        let observed = self.shared.value.read();
        #[cfg(feature = "exfiltrate")]
        registry::record_observed(self.shared.registry_id);
        if let Some(obs) = observed {
            self.observed = Some(obs.clone());
            Some(obs)
        } else {
            None
        }
    }

    /// Returns the next value observed, but only if it is immediately available.
    ///
    /// For this purpose, the next value is considered immediately available if:
    /// - The observer has never observed a value before
    /// - The value has changed since the last observation
    /// - The value has been hung up (dropped)
    ///
    /// # Returns
    ///
    /// - `Ok(Some(T))` - A new value is available
    /// - `Ok(None)` - The value has been dropped
    /// - `Err(())` - No new value is available.
    fn next_when_immediately_available(&mut self) -> Result<Option<T>, ()>
    where
        T: PartialEq + Clone,
    {
        let observe = self.shared.value.read();
        #[cfg(feature = "exfiltrate")]
        registry::record_observed(self.shared.registry_id);
        if let Some(observe) = observe {
            //determine if new or not
            if let Some(last) = &self.observed {
                if &observe == last {
                    // If the value is the same as the last observed value, we return an error
                    Err(())
                } else {
                    // If the value is different, we update the observed value and return it
                    self.observed = Some(observe.clone());
                    Ok(Some(observe))
                }
            } else {
                // If this is the first observation, we set the observed value and return it
                self.observed = Some(observe.clone());
                Ok(Some(observe))
            }
        } else {
            // If the value is None, it means the value has been dropped (hungup)
            Ok(None)
        }
    }

    /// Determines if the observer has a distinct value available without blocking.
    ///
    /// This is an internal method that checks if a new, different value can be read.
    /// It updates the observer's state if a new value is available.
    pub(crate) fn observe_if_distinct(&mut self) -> Observation
    where
        T: PartialEq + Clone,
    {
        match self.next_when_immediately_available() {
            Ok(Some(_)) => Observation::NewValue,
            Ok(None) => Observation::Hangup,
            Err(()) => Observation::Unchanged,
        }
    }

    /// Determines if a new value can be read without blocking or changing the internal state.
    ///
    /// A value is considered "dirty" if:
    /// - The observer has never observed any value
    /// - The current value differs from the last observed value
    /// - The underlying [`Value`] has been dropped (hungup)
    ///
    /// This method is useful for checking if calling `next()` (from the `Stream` trait) would
    /// return immediately without waiting.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::Value;
    /// use futures_util::StreamExt;
    ///
    /// let value = Value::new("hello");
    /// let mut observer = value.observe();
    ///
    /// // Initially dirty (no value observed yet)
    /// assert!(observer.is_dirty());
    ///
    /// # wasm_lite_std::async_doctest!(async {
    /// // After observing, no longer dirty
    /// observer.next().await.unwrap();
    /// assert!(!observer.is_dirty());
    ///
    /// // After value change, dirty again
    /// value.set("world");
    /// assert!(observer.is_dirty());
    /// # });
    /// ```
    pub fn is_dirty(&self) -> bool
    where
        T: PartialEq + Clone,
    {
        match &self.shared.value.read() {
            Some(value) => {
                // If the value is not equal to the last observed value, it's dirty
                self.observed.as_ref() != Some(value)
            }
            None => true, // If the value is None (hung up), it's considered dirty
        }
    }
}

impl<T> Drop for Observer<T> {
    fn drop(&mut self) {
        #[cfg(feature = "exfiltrate")]
        registry::record_observer_dropped(self.shared.registry_id);
        // When the observer is dropped, we need to remove it from the active observations.
        // This ensures that we don't keep references to dropped observers.
        let mut extra = Vec::new();
        while let Some(orig) = self.shared.active_observations.pop() {
            if let Some(active) = orig.upgrade() {
                if active.id == self.observer_id {
                    // Found the active observation for this observer, remove it
                    break;
                } else {
                    extra.push((orig, active));
                }
            }
        }
        // Push back any extra active observations that were popped
        for (orig, active) in extra {
            self.shared.active_observations.push_arc(orig);
            active.notify();
        }
    }
}

//boilerplates

impl<T: Clone> Default for Value<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}
impl<T> Display for Value<T>
where
    T: Display + Clone,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Value({})", self.get())
    }
}

impl<T: Clone> From<T> for Value<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Clone> From<Value<T>> for Observer<T> {
    fn from(value: Value<T>) -> Self {
        value.observe()
    }
}

impl<T> Display for Observer<T>
where
    T: Clone,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Observer(id: {})", self.observer_id)
    }
}

impl<T> PartialEq for Value<T>
where
    T: PartialEq + Clone,
{
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl<T> Eq for Value<T> where T: Eq + Clone {}

impl<T> std::hash::Hash for Value<T>
where
    T: std::hash::Hash + Clone,
{
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.get().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use futures_core::Stream;
    use futures_util::StreamExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};
    use wasm_lite::wasm_lite_test;

    #[cfg(not(target_arch = "wasm32"))]
    use std::thread;
    #[cfg(target_arch = "wasm32")]
    use wasm_lite_std as thread;

    #[wasm_lite_test]
    fn test_value() {
        let value = super::Value::new(42);
        assert_eq!(value.get(), 42);

        let old_value = value.set(100);
        assert_eq!(old_value, 42);
        assert_eq!(value.get(), 100);
    }

    #[derive(Debug)]
    struct DropBomb {
        value: u8,
        bomb: bool,
        armed: Arc<AtomicBool>,
    }

    impl Clone for DropBomb {
        fn clone(&self) -> Self {
            Self {
                value: self.value,
                // Only the caller-owned instance is explosive. This exposes
                // an implementation that clones and then drops `set`'s owned
                // argument before it has notified observers.
                bomb: false,
                armed: Arc::clone(&self.armed),
            }
        }
    }

    impl PartialEq for DropBomb {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }

    impl Drop for DropBomb {
        fn drop(&mut self) {
            if self.bomb && self.armed.load(Ordering::Relaxed) {
                panic!("set dropped its owned input");
            }
        }
    }

    #[derive(Default)]
    struct WakeCount(AtomicUsize);

    impl Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `set` owns its argument. Moving it into storage avoids a panicking
    /// destructor between the visible state change and observer notification.
    #[wasm_lite_test]
    fn set_moves_its_input_before_notifying() {
        let armed = Arc::new(AtomicBool::new(true));
        let value = super::Value::new(DropBomb {
            value: 0,
            bomb: false,
            armed: Arc::clone(&armed),
        });
        let mut observer = value.observe();

        let wakes = Arc::new(WakeCount::default());
        let waker = Waker::from(Arc::clone(&wakes));
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(
            Stream::poll_next(std::pin::Pin::new(&mut observer), &mut cx),
            Poll::Ready(Some(_))
        ));
        assert!(matches!(
            Stream::poll_next(std::pin::Pin::new(&mut observer), &mut cx),
            Poll::Pending
        ));

        let old = value.set(DropBomb {
            value: 1,
            bomb: true,
            armed: Arc::clone(&armed),
        });

        // The bomb is now the stored value; disarm it before assertions so a
        // failing assertion cannot cause a second panic during unwinding.
        armed.store(false, Ordering::Relaxed);
        assert_eq!(old.value, 0);
        assert_eq!(wakes.0.load(Ordering::Relaxed), 1);
    }

    /// Hangup must become observable before the stored value's destructor can
    /// unwind out of `Value::drop`.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn drop_notifies_before_dropping_the_payload() {
        let armed = Arc::new(AtomicBool::new(true));
        let value = super::Value::new(DropBomb {
            value: 0,
            bomb: true,
            armed,
        });
        let mut observer = value.observe();

        let wakes = Arc::new(WakeCount::default());
        let waker = Waker::from(Arc::clone(&wakes));
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(
            Stream::poll_next(std::pin::Pin::new(&mut observer), &mut cx),
            Poll::Ready(Some(_))
        ));
        assert!(matches!(
            Stream::poll_next(std::pin::Pin::new(&mut observer), &mut cx),
            Poll::Pending
        ));

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value)));
        assert!(result.is_err(), "the payload destructor should still panic");
        assert_eq!(
            wakes.0.load(Ordering::Relaxed),
            1,
            "the pending observer must be notified before payload destruction"
        );
    }

    #[wasm_lite_test]
    fn test_observer() {
        let value = super::Value::new(42);
        let mut observer = value.observe();
        assert_eq!(observer.current_value().unwrap(), 42);
        value.set(100);
        assert_eq!(observer.current_value().unwrap(), 100);
    }

    #[wasm_lite_test]
    async fn test_observer_next() {
        let value = super::Value::new(42);
        let mut observer = value.observe();
        assert_eq!(observer.current_value().unwrap(), 42);

        //push first
        value.set(100);
        let next_value = observer.next().await.unwrap();
        assert_eq!(next_value, 100);

        //read first
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(100));
            value.set(200);
            std::mem::forget(value); //don't hangup
        });
        //wait for next
        let next_value = observer.next().await.unwrap();
        assert_eq!(next_value, 200);
    }

    #[wasm_lite_test]
    async fn drop_value() {
        let value = super::Value::new(42);
        let mut observer = value.observe();
        assert_eq!(observer.current_value().unwrap(), 42);

        // Spawn a task that will drop the value after some time
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(100));
            drop(value);
        });

        // Wait for the next value, which should return an error since the value is dropped
        let result = observer.next().await;
        assert!(result.is_none());

        //should work again back to back
        let result2 = observer.next().await;
        assert!(
            result2.is_none(),
            "Expected error after value drop, got: {:?}",
            result2
        );
    }
    #[wasm_lite_test]
    fn test_observer_clone_drop_loop() {
        let value = super::Value::new(42);
        let observer = value.observe();
        for _ in 0..300 {
            let clone = observer.clone();
            drop(clone);
        }
    }

    #[wasm_lite_test]
    fn test_value_partialeq() {
        let value1 = super::Value::new(42);
        let value2 = super::Value::new(42);
        let value3 = super::Value::new(100);

        assert_eq!(value1, value2);
        assert_ne!(value1, value3);

        value2.set(100);
        assert_eq!(value2, value3);
        assert_ne!(value1, value2);
    }

    #[wasm_lite_test]
    fn test_value_hash() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let value1 = super::Value::new(42);
        let value2 = super::Value::new(42);
        let value3 = super::Value::new(100);

        let mut hasher1 = DefaultHasher::new();
        value1.hash(&mut hasher1);
        let hash1 = hasher1.finish();

        let mut hasher2 = DefaultHasher::new();
        value2.hash(&mut hasher2);
        let hash2 = hasher2.finish();

        let mut hasher3 = DefaultHasher::new();
        value3.hash(&mut hasher3);
        let hash3 = hasher3.finish();

        assert_eq!(hash1, hash2, "Equal values should have equal hashes");
        assert_ne!(
            hash1, hash3,
            "Different values should have different hashes"
        );

        // Test that hash changes when value changes
        value2.set(100);
        let mut hasher4 = DefaultHasher::new();
        value2.hash(&mut hasher4);
        let hash4 = hasher4.finish();

        assert_eq!(
            hash3, hash4,
            "Value with same content should have same hash"
        );
        assert_ne!(
            hash1, hash4,
            "Value after update should have different hash"
        );
    }

    #[wasm_lite_test]
    fn test_observer_display() {
        let value = super::Value::new(42);
        let observer = value.observe();
        let display_str = format!("{}", observer);
        assert!(display_str.starts_with("Observer(id:"));
    }
}
