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

mod active_observation;
pub mod aggregate;

use crate::active_observation::ActiveObservation;
use std::fmt::Display;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug)]
struct Shared<T> {
    value: Option<T>, // None on hangup
    active_observations: Vec<ActiveObservation>,
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
pub struct Value<T> {
    shared: Arc<Mutex<Shared<T>>>,
}

impl<T> Value<T> {
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
            shared: Arc::new(Mutex::new(Shared {
                value: Some(value),
                active_observations: Vec::new(),
            })),
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
        self.shared
            .lock()
            .unwrap()
            .value
            .clone()
            .expect("Value is hungup")
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
    pub fn set(&self, value: T) -> T {
        let mut lock = self.shared.lock().unwrap();
        let old = lock.value.replace(value);
        let observers = std::mem::take(&mut lock.active_observations);
        drop(lock); // Explicitly drop the lock before notifying observers
        Self::notify(observers);
        old.expect("Value is hungup")
    }

    fn notify(who: Vec<ActiveObservation>) {
        drop(who);
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

impl<T> Drop for Value<T> {
    fn drop(&mut self) {
        let mut lock = self.shared.lock().unwrap();
        // Set the value to None to indicate that the value is hung up
        lock.value = None;
        //take the active observations
        let observers = std::mem::take(&mut lock.active_observations);
        drop(lock); // Explicitly drop the lock before notifying observers
        drop(observers); // Notify observers that the value is hung up
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
#[derive(Debug, Clone)]
pub struct Observer<T> {
    shared: Arc<Mutex<Shared<T>>>,
    //The value last observed.
    observed: Option<T>,
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
    pub fn new(value: &Value<T>) -> Self {
        let shared = value.shared.clone();
        Self {
            shared,
            observed: None,
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
        let observed = self.shared.lock().unwrap().value.clone();
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
    pub async fn next(&mut self) -> Result<T, ObserverError>
    where
        T: Clone + PartialEq,
    {
        let mut r = self.next_when_immediately_available();
        let future = match r {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(ObserverError::Hungup)) => return Err(ObserverError::Hungup),
            Err(ref mut lock) => {
                // We need to wait for a change
                let (observation, future) = active_observation::observation();
                lock.active_observations.push(observation);
                future
            }
        };
        drop(r);
        let r = future.await;
        if let Err(e) = r {
            Err(e)
        } else {
            self.current_value()
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
    /// - `Ok(Ok(T))` - A new value is available
    /// - `Ok(Err(ObserverError::Hungup))` - The value has been dropped
    /// - `Err(MutexGuard)` - No new value is available; returns the lock for the caller to use
    fn next_when_immediately_available(
        &mut self,
    ) -> Result<Result<T, ObserverError>, MutexGuard<Shared<T>>>
    where
        T: PartialEq + Clone,
    {
        if let Some(last_observed) = &self.observed {
            let lock = self.shared.lock().unwrap();
            //take a new observation
            if (lock.value.as_ref()) != Some(last_observed) {
                let new_value = lock.value.clone();
                if let Some(new_value) = new_value {
                    // If the value has changed, update the observed value
                    self.observed = Some(new_value.clone());
                    Ok(Ok(new_value))
                } else {
                    // If the value is None, it means the value has been dropped
                    Ok(Err(ObserverError::Hungup))
                }
            } else {
                Err(lock) // hold the lock for the caller to use
            }
        } else {
            Ok(self.current_value())
        }
    }

    /// Returns either the underlying value if available, along with the unused ActiveObservation,
    /// or else installs the observer and returns a vacuous Err.
    ///
    /// This is an internal method used by [`AggregateObserver`](crate::aggregate::AggregateObserver)
    /// to efficiently poll multiple observers.
    pub(crate) fn aggregate_poll_impl(
        &mut self,
        observer: ActiveObservation,
    ) -> Result<(ActiveObservation, Result<T, ObserverError>), ()>
    where
        T: PartialEq + Clone,
    {
        match self.next_when_immediately_available() {
            Ok(answer) => Ok((observer, answer)),
            Err(mut lock) => {
                lock.active_observations.push(observer);
                Err(())
            }
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
        T: PartialEq,
    {
        let lock = self.shared.lock().unwrap();
        match &lock.value {
            Some(value) => {
                // If the value is not equal to the last observed value, it's dirty
                self.observed.as_ref() != Some(value)
            }
            None => true, // If the value is None (hung up), it's considered dirty
        }
    }
}

//boilerplates

impl<T> Default for Value<T>
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

impl<T> From<T> for Value<T> {
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

impl<T> From<Value<T>> for Observer<T> {
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
}
