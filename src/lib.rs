/*!
Primitives for subscribing to updates to value changes.
*/

mod active_observation;

use std::sync::{Arc, Mutex};
use crate::active_observation::ActiveObservation;

struct Shared<T> {
    value: T,
    active_observations: Vec<ActiveObservation>,
}
pub struct Value<T> {
    shared: Arc<Mutex<Shared<T>>>,
}

impl<T> Value<T> {
    /// Creates a new `Value` with the given initial value.
    pub fn new(value: T) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared { value , active_observations: Vec::new() })),
        }
    }

    /// Returns a copy of the current value.
    pub fn get(&self) -> T where T: Clone {
        self.shared.lock().unwrap().value.clone()
    }

    /// Sets a new value and returns the old value.
    pub fn set(&mut self, value: T) -> T {
        let mut lock = self.shared.lock().unwrap();
        let old = std::mem::replace(&mut lock.value, value);
        let observers = std::mem::take(&mut lock.active_observations);
        drop(lock); // Explicitly drop the lock before notifying observers
        Self::notify(observers);
        old
    }

    fn notify(who: Vec<ActiveObservation>) {
        drop(who);
    }

    /// Returns a new `Observer` for this `Value`.
    pub fn observe(&self) -> Observer<T> {
        Observer::new(self)
    }


}

#[derive(Debug)]
enum ObserverError {
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
    pub fn current_value(&mut self) -> T where T: Clone {
        let observed = self.shared.lock().unwrap().value.clone();
        self.observed = Some(observed.clone());
        observed
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
            if lock.value != *last_observed {
                let new_value = lock.value.clone();
                self.observed = Some(new_value.clone());
                Ok(new_value)
            }
            else {
                //value the same as last time.  In this case we need to wait for a change
                let (observation, future) = active_observation::observation();
                lock.active_observations.push(observation);
                drop(lock); // Explicitly drop the lock before awaiting
                future.await;
                // After the future resolves, we can check the value again
                Ok(self.current_value())
                
            }
        }
        else {
            Ok(self.current_value())
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
        assert_eq!(observer.current_value(), 42);
        value.set(100);
        assert_eq!(observer.current_value(), 100);
    }

    #[async_test] async fn test_observer_next() {
        let mut value = super::Value::new(42);
        let mut observer = value.observe();
        assert_eq!(observer.current_value(), 42);

        //push first
        value.set(100);
        let next_value = observer.next().await.unwrap();
        assert_eq!(next_value, 100);

        //read first
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            value.set(200);
        });
        //wait for next
        let next_value = observer.next().await.unwrap();
        assert_eq!(next_value, 200);
    }
}