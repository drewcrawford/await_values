/*!
Primitives for subscribing to updates to value changes.
*/

mod active_observation;

use std::sync::{Arc, Mutex};
use crate::active_observation::ActiveObservation;

struct Shared<T> {
    value: Option<T>,// None on hangup
    active_observations: Vec<ActiveObservation>,
}
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
    pub fn set(&mut self, value: T) -> T {
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
pub enum ObserverError {
    Hungup,
}

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
        if let Some(last_observed) = &self.observed {
            let mut lock = self.shared.lock().unwrap();
            //take a new observation
            if (lock.value.as_ref()) != Some(last_observed) {
                let new_value = lock.value.clone();
                if let Some(new_value) = new_value {
                    // If the value has changed, update the observed value
                    self.observed = Some(new_value.clone());
                    Ok(new_value)
                } else {
                    // If the value is None, it means the value has been dropped
                    Err(ObserverError::Hungup)
                }
            }
            else {
                //value the same as last time.  In this case we need to wait for a change
                let (observation, future) = active_observation::observation();
                lock.active_observations.push(observation);
                drop(lock); // Explicitly drop the lock before awaiting
                future.await?;
                // After the future resolves, we can check the value again
                self.current_value()
            }
        }
        else {
            self.current_value()
        }
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
    }
}