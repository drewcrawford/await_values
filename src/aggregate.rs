//SPDX-License-Identifier: MIT OR Apache-2.0
//! Support for aggregating multiple heterogeneous observers into a single observer.
//!
//! This module provides [`AggregateObserver`], which can hold multiple observers of different types
//! and wait for any of them to produce a new value.

use crate::Observer;
use crate::active_observation::ActiveObservation;
use std::fmt::Debug;

trait ErasedObserver: Debug + Send {
    fn clone_box(&self) -> Box<dyn ErasedObserver>;
    fn aggregate_poll(&mut self, observation: ActiveObservation) -> Result<ActiveObservation, ()>;
    fn observe_if_distinct(&mut self) -> bool;

    fn is_dirty(&self) -> bool;
}
impl<T> ErasedObserver for Observer<T>
where
    T: PartialEq + Clone + Debug + Send + 'static,
{
    fn clone_box(&self) -> Box<dyn ErasedObserver> {
        Box::new(self.clone())
    }

    fn aggregate_poll(&mut self, observation: ActiveObservation) -> Result<ActiveObservation, ()> {
        match self.aggregate_poll_impl(observation) {
            Ok(f) => Ok(f.0), //extract the nongeneric part
            Err(_) => Err(()),
        }
    }

    fn observe_if_distinct(&mut self) -> bool {
        self.observe_if_distinct()
    }

    fn is_dirty(&self) -> bool {
        self.is_dirty()
    }
}

/// An aggregate, heterogeneous observer that can hold multiple observers of different types.
///
/// `AggregateObserver` allows you to wait for changes on multiple [`Observer`]s simultaneously,
/// even when they observe values of different types. This is useful when you need to react to
/// changes from multiple sources without knowing which one will change first.
///
/// # Examples
///
/// ```
/// # fn setup() -> (await_values::aggregate::AggregateObserver, await_values::Value<i32>, await_values::Value<&'static str>) {
/// use await_values::{Value, aggregate::AggregateObserver};
///
/// // Create values of different types
/// let int_value = Value::new(42);
/// let str_value = Value::new("hello");
///
/// // Create an aggregate observer
/// let mut aggregate = AggregateObserver::new();
/// aggregate.add_observer(int_value.observe());
/// aggregate.add_observer(str_value.observe());
/// # (aggregate, int_value, str_value)
/// # }
///
/// # test_executors::sleep_on(async {
/// # let (mut aggregate, int_value, str_value) = setup();
/// // Get initial values
/// let index = aggregate.next().await;
/// assert!(index == 0 || index == 1);
///
/// // Change one of the values
/// int_value.set(100);
///
/// // Wait for the change
/// let changed_index = aggregate.next().await;
/// assert_eq!(changed_index, 0); // The integer value changed
/// # });
/// ```
#[derive(Debug)]
pub struct AggregateObserver {
    observers: Vec<Box<dyn ErasedObserver>>,
}

impl AggregateObserver {
    /// Creates a new empty `AggregateObserver`.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::aggregate::AggregateObserver;
    ///
    /// let aggregate = AggregateObserver::new();
    /// ```
    pub fn new() -> Self {
        AggregateObserver {
            observers: Vec::new(),
        }
    }

    /// Adds an observer to the aggregate.
    ///
    /// The observer can be of any type `T` that implements the required traits.
    /// Once added, the aggregate will monitor this observer for changes.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::{Value, aggregate::AggregateObserver};
    ///
    /// let value = Value::new(42);
    /// let mut aggregate = AggregateObserver::new();
    /// aggregate.add_observer(value.observe());
    /// ```
    pub fn add_observer<T>(&mut self, observer: Observer<T>)
    where
        T: 'static + PartialEq + Clone + Debug + Send,
    {
        // Store the observer as a boxed trait object to erase the type
        self.observers.push(Box::new(observer));
    }

    /// Waits for the next value change from any of the observers and returns its index.
    ///
    /// This method will:
    /// - Return immediately if any observer has an unobserved value change
    /// - Otherwise, wait until any observer's value changes
    /// - Return the index (0-based) of the first observer that has a new value
    ///
    /// # Returns
    ///
    /// The index of the observer that changed. Indices correspond to the order in which
    /// observers were added via [`add_observer`](Self::add_observer).
    ///
    /// # Notes
    ///
    /// - If multiple observers have changed, only the index of the first one is returned
    /// - The actual value cannot be retrieved through the aggregate; you'll need to keep
    ///   a separate reference to the original [`Value`](crate::Value) or [`Observer`]
    /// - This method handles "repeat" values correctly - if an observer is set to the same
    ///   value multiple times, it won't be considered as having a new value
    ///
    /// # Examples
    ///
    /// ```
    /// # use await_values::{Value, aggregate::AggregateObserver};
    /// # test_executors::sleep_on(async {
    /// let value1 = Value::new(1);
    /// let value2 = Value::new("hello");
    ///
    /// let mut aggregate = AggregateObserver::new();
    /// aggregate.add_observer(value1.observe());
    /// aggregate.add_observer(value2.observe());
    ///
    /// // Wait for either value to change
    /// let changed_index = aggregate.next().await;
    /// println!("Observer {} changed", changed_index);
    /// # });
    /// ```
    pub async fn next(&mut self) -> usize {
        loop {
            let (active_observation, active_future) = crate::active_observation::observation();
            for (o, observer) in &mut self.observers.iter_mut().enumerate() {
                let r = observer.aggregate_poll(active_observation.clone());
                match r {
                    Ok(future) => {
                        //future is being returned to us
                        drop(future);
                        return o; // Return the index of the first observer that is ready
                    }
                    Err(_) => continue, // If the observer is not ready, continue to the next one
                }
            }

            _ = active_future.await;
            //look for the first observer that is ready
            for (o, observer) in &mut self.observers.iter_mut().enumerate() {
                if observer.observe_if_distinct() {
                    return o; // Return the index of the first observer that is ready
                }
            }
            //in "repeat" situations we may not have any observers that are ready
            //so try again!
        }
    }

    /// Checks if any observer has a new value available without blocking.
    ///
    /// This method does not consume the value or change any internal state.
    /// It's useful for checking if calling [`next`](Self::next) would return immediately.
    ///
    /// # Returns
    ///
    /// - `true` if at least one observer has a new value ready or is hung up
    /// - `false` if all observers are up-to-date
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::{Value, aggregate::AggregateObserver};
    ///
    /// let value = Value::new(42);
    /// let mut observer = value.observe();
    /// let mut aggregate = AggregateObserver::new();
    /// aggregate.add_observer(observer);
    ///
    /// // Initially dirty (observer hasn't observed initial value yet)
    /// assert!(aggregate.is_dirty());
    ///
    /// # test_executors::sleep_on(async {
    /// // After observing the value, it's no longer dirty
    /// aggregate.next().await;
    /// assert!(!aggregate.is_dirty());
    ///
    /// // After setting a new value, it becomes dirty again
    /// value.set(100);
    /// assert!(aggregate.is_dirty());
    /// # });
    /// ```
    pub fn is_dirty(&self) -> bool {
        self.observers.iter().any(|e| e.is_dirty())
    }
}

//boilerplates

// Send/Sync: AggregateObserver is automatically Send since it contains Vec<Box<dyn ErasedObserver>>
// where ErasedObserver: Send. It is not Sync due to &mut self methods like next() and add_observer().

impl Clone for AggregateObserver {
    fn clone(&self) -> Self {
        Self {
            observers: self.observers.iter().map(|obs| obs.clone_box()).collect(),
        }
    }
}

impl Default for AggregateObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> From<Observer<T>> for AggregateObserver
where
    T: 'static + PartialEq + Clone + Debug + Send,
{
    /// Creates an `AggregateObserver` from a single `Observer`.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::{Value, aggregate::AggregateObserver};
    ///
    /// let value = Value::new(42);
    /// let observer = value.observe();
    /// let aggregate = AggregateObserver::from(observer);
    /// ```
    fn from(observer: Observer<T>) -> Self {
        let mut aggregate = AggregateObserver::new();
        aggregate.add_observer(observer);
        aggregate
    }
}

#[cfg(test)]
mod tests {
    use super::AggregateObserver;
    use crate::Value;
    use test_executors::async_test;

    #[async_test]
    async fn test_aggregate_observer() {
        let value = Value::new(2);
        let value2 = Value::new(0.3);

        let mut o = AggregateObserver::new();
        o.add_observer(value.observe());
        o.add_observer(value2.observe());

        let _ = o.next().await;
        let _ = o.next().await;

        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let value = value;
            value.set(3);
            //don't hangup
            std::mem::forget(value);
        });
        _ = o.next().await;
    }

    #[async_test]
    async fn test_repeat_values() {
        let v = Value::new(0);
        let mut o = AggregateObserver::new();
        o.add_observer(v.observe());
        let o1 = o.next().await;
        assert_eq!(o1, 0);

        std::thread::spawn(move || {
            let v = v;
            for _ in 0..5 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                v.set(0);
            }
            v.set(1);
            //don't hangup
            std::mem::forget(v);
        });

        let begin = std::time::Instant::now();
        let o2 = o.next().await;
        assert!(
            begin.elapsed().as_millis() > 49,
            "Should have waited for the next value"
        );
        assert_eq!(o2, 0);
    }
}
