//SPDX-License-Identifier: MIT OR Apache-2.0
/*!
Primitives for subscribing to / notifying about changes to values.

![logo](../../../art/logo.png)

This library provides a simple way to create observable values that can notify multiple
observers when they change. It's particularly useful for GUI applications, state management,
and reactive programming patterns.

# Core Concepts

This library primarily imagines your value type is:
* `Clone` - so that it can be cloned for observers.
* `PartialEq` - so that we can diff values and only notify observers when the value changes.

Our cast of characters includes:
* [`Value`] - Allocates storage for a value that can be observed.
* [`Observer`] - A handle to a value that can be used to observe when the value changes remotely.
* [`aggregate::AggregateObserver`] - A handle to multiple heterogeneous values that can be used to observe when any of the values change.

This library uses asynchronous functions and is executor-agnostic. It does not depend on tokio.

# Quick Start

```
use await_values::{Value, Observer};

# test_executors::sleep_on(async {
// Create an observable value
let value = Value::new(42);

// Create an observer
let mut observer = value.observe();

// Get the current value
assert_eq!(observer.next().await.unwrap(), 42);

// Update the value
value.set(100);

// Observe the change
assert_eq!(observer.next().await.unwrap(), 100);
# });
```

# Advanced Usage

## Observing Multiple Values

You can observe multiple values of different types using `AggregateObserver`:

```
use await_values::{Value, aggregate::AggregateObserver};

# test_executors::sleep_on(async {
let temperature = Value::new(20.5);
let status = Value::new("OK");

let mut aggregate = AggregateObserver::new();
aggregate.add_observer(temperature.observe());
aggregate.add_observer(status.observe());

// Wait for initial values
let index = aggregate.next().await;
assert!(index == 0 || index == 1);

// Change a value
temperature.set(25.0);

// See which observer changed
let changed_index = aggregate.next().await;
assert_eq!(changed_index, 0); // temperature changed
# });
```

# Thread Safety

All types in this library are thread-safe and can be shared across threads.
`Value` uses interior mutability with proper synchronization, making it safe to use from multiple threads.

```
use await_values::Value;
use std::sync::Arc;
use std::thread;

// Wrap Value in Arc to share between threads
let value = Arc::new(Value::new(0));
let value_clone = Arc::clone(&value);

let handle = thread::spawn(move || {
    value_clone.set(42);
});

handle.join().unwrap();
assert_eq!(value.get(), 42);
```
*/

pub mod aggregate;
pub mod flip_card;

use crate::flip_card::FlipCard;
use atomic_waker::AtomicWaker;
use std::ffi::c_void;
use std::fmt::{Debug, Display};
use std::pin::Pin;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicPtr, AtomicU8};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};

struct ActiveObservation {
    id: u8,
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
    next_observer_id: AtomicU8,
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
    pub fn new(value: T) -> Self {
        Self {
            shared: Arc::new(Shared {
                value: FlipCard::new(Some(value)),
                active_observations: treiber_stack::TreiberStack::default(),
                next_observer_id: AtomicU8::new(0),
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
        self.shared.value.flip_to(None);
        self.notify();
    }
}

/// Errors that can occur when observing values.
#[derive(Debug)]
#[non_exhaustive]
pub enum ObserverError {
    /// Indicates that the value has been hung up, meaning the value is no longer available
    /// and no updates will be made.
    ///
    /// This occurs when the [`Value`] is dropped while observers still exist.
    Hungup,
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
///
/// # test_executors::sleep_on(async {
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
    observer_id: u8,
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
        assert!(
            observer_id != u8::MAX,
            "Too many observers created, maximum is 255"
        );
        let active = Arc::new(ActiveObservation {
            id: observer_id,
            notify: AtomicWaker::new(),
        });
        self.shared
            .active_observations
            .push(Arc::downgrade(&active));
        Self {
            active_observation: active,
            shared: self.shared.clone(),
            observed: self.observed.clone(),
            observer_id,
        }
    }
}

pub struct Observation<'a, T> {
    observer: &'a mut Observer<T>,
}

impl<'a, T> Observation<'a, T> {
    /// Creates a new observation for the given observer.
    fn new(observer: &'a mut Observer<T>) -> Self {
        Self { observer }
    }
}

impl<'a, T> Future for Observation<'a, T>
where
    T: PartialEq + Clone,
{
    type Output = Result<T, ObserverError>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.observer.active_observation.register(cx.waker());
        // Check if the observer has a distinct value available
        match self.get_mut().observer.next_when_immediately_available() {
            Ok(v) => Poll::Ready(v),
            Err(_) => Poll::Pending,
        }
    }
}

impl<T> Observer<T> {
    /// Creates a new observer for the given `Value`.
    ///
    /// The observer starts with no observed value, meaning the first call to
    /// [`next`](Self::next) or [`current_value`](Self::current_value) will
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
        assert!(
            observer_id != u8::MAX,
            "Too many observers created, maximum is 255"
        );
        let active = Arc::new(ActiveObservation {
            id: observer_id,
            notify: AtomicWaker::new(),
        });
        value
            .shared
            .active_observations
            .push(Arc::downgrade(&active));
        let shared = value.shared.clone();
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
    /// # Errors
    ///
    /// Returns [`ObserverError::Hungup`] if the underlying [`Value`] has been dropped.
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
    pub fn current_value(&mut self) -> Result<T, ObserverError>
    where
        T: Clone,
    {
        let observed = self.shared.value.read();
        if let Some(obs) = observed {
            self.observed = Some(obs.clone());
            Ok(obs)
        } else {
            Err(ObserverError::Hungup)
        }
    }

    /// Returns the next value observed.
    ///
    /// This method implements the core observation logic:
    /// * If no values have been observed yet, it will return the current value immediately.
    /// * If the value has changed since the last observation, it returns the new value immediately.
    /// * If the value hasn't changed, it waits until the value changes, then returns the new value.
    ///
    /// # Errors
    ///
    /// Returns [`ObserverError::Hungup`] if the underlying [`Value`] has been dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::Value;
    /// use std::thread;
    /// use std::time::Duration;
    ///
    /// # test_executors::sleep_on(async {
    /// let value = Value::new(1);
    /// let mut observer = value.observe();
    ///
    /// // First call returns immediately
    /// assert_eq!(observer.next().await.unwrap(), 1);
    ///
    /// // Spawn a thread to update the value
    /// thread::spawn(move || {
    ///     thread::sleep(Duration::from_millis(10));
    ///     value.set(2);
    ///     # std::mem::forget(value); // Prevent hangup in test
    /// });
    ///
    /// // This call waits for the change
    /// assert_eq!(observer.next().await.unwrap(), 2);
    /// # });
    /// ```
    pub fn next(&mut self) -> impl Future<Output = Result<T, ObserverError>>
    where
        T: Clone + PartialEq,
    {
        Observation { observer: self }
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
    /// - `Ok(Ok(T))` - A new value is available
    /// - `Ok(Err(ObserverError::Hungup))` - The value has been dropped
    /// - `Err(()))` - No new value is available.
    fn next_when_immediately_available(&mut self) -> Result<Result<T, ObserverError>, ()>
    where
        T: PartialEq + Clone,
    {
        let observe = self.shared.value.read();
        if let Some(observe) = observe {
            //determine if new or not
            if let Some(last) = &self.observed {
                if &observe == last {
                    // If the value is the same as the last observed value, we return an error
                    return Err(());
                } else {
                    // If the value is different, we update the observed value and return it
                    self.observed = Some(observe.clone());
                    return Ok(Ok(observe));
                }
            } else {
                // If this is the first observation, we set the observed value and return it
                self.observed = Some(observe.clone());
                return Ok(Ok(observe));
            }
        } else {
            // If the value is None, it means the value has been dropped (hungup)
            return Ok(Err(ObserverError::Hungup));
        }
    }

    /// Determines if the observer has a distinct value available without blocking.
    ///
    /// This is an internal method that checks if a new, different value can be read.
    /// It updates the observer's state if a new value is available.
    pub(crate) fn observe_if_distinct(&mut self) -> bool
    where
        T: PartialEq + Clone,
    {
        let r = self.next_when_immediately_available();
        match r {
            Ok(..) => true,  // Value is available and distinct
            Err(_) => false, // No value available
        }
    }

    /// Determines if a new value can be read without blocking or changing the internal state.
    ///
    /// A value is considered "dirty" if:
    /// - The observer has never observed any value
    /// - The current value differs from the last observed value
    /// - The underlying [`Value`] has been dropped (hungup)
    ///
    /// This method is useful for checking if calling [`next`](Self::next) would
    /// return immediately without waiting.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::Value;
    ///
    /// let value = Value::new("hello");
    /// let mut observer = value.observe();
    ///
    /// // Initially dirty (no value observed yet)
    /// assert!(observer.is_dirty());
    ///
    /// # test_executors::sleep_on(async {
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
        for ((orig, active)) in extra {
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

impl Display for ObserverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObserverError::Hungup => write!(f, "Observer hung up"),
        }
    }
}
impl std::error::Error for ObserverError {}

impl<T: Clone> From<Value<T>> for Observer<T> {
    fn from(value: Value<T>) -> Self {
        value.observe()
    }
}

#[cfg(test)]
mod tests {
    use test_executors::async_test;

    #[test]
    fn test_value() {
        let value = super::Value::new(42);
        assert_eq!(value.get(), 42);

        let old_value = value.set(100);
        assert_eq!(old_value, 42);
        assert_eq!(value.get(), 100);
    }

    #[test]
    fn test_observer() {
        let value = super::Value::new(42);
        let mut observer = value.observe();
        assert_eq!(observer.current_value().unwrap(), 42);
        value.set(100);
        assert_eq!(observer.current_value().unwrap(), 100);
    }

    #[async_test]
    async fn test_observer_next() {
        let value = super::Value::new(42);
        let mut observer = value.observe();
        assert_eq!(observer.current_value().unwrap(), 42);

        //push first
        value.set(100);
        let next_value = observer.next().await.unwrap();
        assert_eq!(next_value, 100);

        //read first
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            value.set(200);
            std::mem::forget(value); //don't hangup
        });
        //wait for next
        let next_value = observer.next().await.unwrap();
        assert_eq!(next_value, 200);
    }

    #[async_test]
    async fn drop_value() {
        let value = super::Value::new(42);
        let mut observer = value.observe();
        assert_eq!(observer.current_value().unwrap(), 42);

        // Spawn a task that will drop the value after some time
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(value);
        });

        // Wait for the next value, which should return an error since the value is dropped
        let result = observer.next().await;
        assert!(result.is_err());

        //should work again back to back
        let result2 = observer.next().await;
        assert!(
            result2.is_err(),
            "Expected error after value drop, got: {:?}",
            result2
        );
    }
    #[test]
    fn test_observer_clone_drop_loop() {
        let value = super::Value::new(42);
        let observer = value.observe();
        for _ in 0..300 {
            let clone = observer.clone();
            drop(clone);
        }
    }
}
