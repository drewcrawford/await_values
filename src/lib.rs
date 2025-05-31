/*!
Primitives for subscribing to / notifying about changes to values.

This library primarily imagines your value type is:
* `Clone` - so that it can be cloned for observers.
* `PartialEq` - so that we can diff values and only notify observers when the value changes.

Our cast of characters includes:
* [Value] - Allocates storage for a value.
* [Observer] - A handle to a value that can be used to observe when the value changes remotely.
* [aggregate::AggregateObserver] - A handle to multiple heterogeneous values that can be used to observe when any of the values change.

This library uses asynchronous functions and is executor-agnostic.  It does not depend on tokio.
*/

mod active_observation;
pub mod aggregate;

use std::fmt::Display;
use std::sync::{Arc, Mutex, MutexGuard};
use crate::active_observation::ActiveObservation;

#[derive(Debug)]
struct Shared<T> {
    value: Option<T>,// None on hangup
    active_observations: Vec<ActiveObservation>,
}

/**
Allocates storage for a value that can be observed.
*/

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
    pub fn new(value: T) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared { value: Some(value) , active_observations: Vec::new() })),
        }
    }

    /// Returns a copy of the current value.
    pub fn get(&self) -> T where T: Clone {
        self.shared.lock().unwrap().value.clone().expect("Value is hungup")
    }

    /// Sets a new value and returns the old value.
    pub fn set(&self, value: T) -> T {
        let mut lock = self.shared.lock().unwrap();
        let old = std::mem::replace(&mut lock.value, Some(value));
        let observers = std::mem::take(&mut lock.active_observations);
        drop(lock); // Explicitly drop the lock before notifying observers
        Self::notify(observers);
        old.expect("Value is hungup")
    }

    fn notify(who: Vec<ActiveObservation>) {
        drop(who);
    }

    /// Returns a new `Observer` for this `Value`.
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

#[derive(Debug)]
#[non_exhaustive]
pub enum ObserverError {
    /// Indicates that the value has been hung up, meaning the value is no longer available and no updates will be made.
    Hungup,
}

/**
A handle to a value that can be used to observe when the value changes remotely.

Observers have an internal 'state' that tracks the last observed value.
This allows them to return the current value immediately, and then wait for the next value to change.
*/
#[derive(Debug,Clone)]
pub struct Observer<T> {
    shared: Arc<Mutex<Shared<T>>>,
    //The value last observed.
    observed: Option<T>,
}

impl<T> Observer<T> {
    /// Creates a new observer for the given `Value`.
    pub fn new(value: &Value<T>) -> Self  {
        let shared = value.shared.clone();
        Self { shared, observed: None }
    }

    /// Returns the current value observed.
    pub fn current_value(&mut self) -> Result<T, ObserverError> where T: Clone {
        let observed = self.shared.lock().unwrap().value.clone();
        if let Some(obs) = observed {
            self.observed = Some(obs.clone());
            Ok(obs)
        }
        else {
            Err(ObserverError::Hungup)
        }
    }


    /**
    Returns the next value observed.
    * If no values have been observed yet, it will return the current value.
    * Subsequent calls will yield until the value changes, at which point it will return the new value.
    */
    pub async fn next(&mut self) -> Result<T,ObserverError> where T: Clone + PartialEq {
        let mut r = self.next_when_immediately_available();
        let future = match r {
            Ok(Ok(value)) => {
                return Ok(value)
            },
            Ok(Err(ObserverError::Hungup)) => {
                return Err(ObserverError::Hungup)
            },
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
        }
        else {
            self.current_value()
        }

    }

    /**
    Returns the next value observed, but only if it is immediately available.
    For this purpose, the next value is considered immediately available if the value hang up.
    * If no values have been observed yet, it will return the current value.
    * If the value has not changed since the last observation, it will return an error.
*/
    fn next_when_immediately_available(&mut self) -> Result<Result<T,ObserverError>,MutexGuard<Shared<T>>> where T: PartialEq + Clone {
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
            }
            else {
                Err(lock) // hold the lock for the caller to use
            }
        }
        else {
            Ok(self.current_value())
        }
    }


    /**
    Returns either the underling value if available, along with the unused ActiveObservationFuture.
    Or else installs the observer and returns a vacuous Err.
*/
    pub(crate) fn aggregate_poll_impl(&mut self, observer: ActiveObservation) -> Result<(ActiveObservation,Result<T,ObserverError>),()> where T: PartialEq + Clone {
        match self.next_when_immediately_available() {
            Ok(answer) => {
                Ok((observer,answer))
            }
            Err(mut lock) => {
                lock.active_observations.push(observer);
                Err(())
            }
        }
    }
    
    ///Determines if the observer has a value available without blocking.
    pub(crate) fn observe_if_distinct(&mut self) -> bool  where T: PartialEq + Clone {
        let r = self.next_when_immediately_available();
        match r {
            Ok(..) => true, // Value is available and distinct
            Err(_) => false, // No value available
        }
    }
    
    ///Determines if a new value can be read, without blocking or changing the internal state.
    /// 
    /// For this purpose, a hungup value is considered dirty.
    pub fn is_dirty(&self) -> bool where T: PartialEq {
        let lock = self.shared.lock().unwrap();
        match &lock.value {
            Some(value) => {
                // If the value is not equal to the last observed value, it's dirty
                self.observed.as_ref().map_or(true, |obs| obs != value)
            },
            None => true, // If the value is None (hung up), it's considered dirty
            
        }
    }



}

//boilerplates

impl <T> Default for Value<T> where T: Default {
    fn default() -> Self {
        Self::new(T::default())
    }

}
impl <T> Display for Value<T> where T: Display + Clone {
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
impl std::error::Error for ObserverError {

}

impl<T> From<Value<T>> for Observer<T> {
    fn from(value: Value<T>) -> Self {
        value.observe()
    }
}

#[cfg(test)] mod tests {
    use test_executors::async_test;

    #[test] fn test_value() {
        let mut value = super::Value::new(42);
        assert_eq!(value.get(), 42);

        let old_value = value.set(100);
        assert_eq!(old_value, 42);
        assert_eq!(value.get(), 100);
    }

    #[test] fn test_observer() {
        let mut value = super::Value::new(42);
        let mut observer = value.observe();
        assert_eq!(observer.current_value().unwrap(), 42);
        value.set(100);
        assert_eq!(observer.current_value().unwrap(), 100);
    }

    #[async_test] async fn test_observer_next() {
        let mut value = super::Value::new(42);
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

    #[async_test] async fn drop_value() {
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
        assert!(result2.is_err(), "Expected error after value drop, got: {:?}", result2);
    }
}