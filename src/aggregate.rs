use crate::{Observer};
use crate::active_observation::{ActiveObservation, ActiveObservationFuture};

trait ErasedObserver {
    fn aggregate_poll(&mut self, observation: ActiveObservation) -> Result<ActiveObservation,()>;
    fn observe_if_distinct(&mut self) -> bool;
}
impl <T> ErasedObserver for Observer<T> where T: PartialEq + Clone {
    fn aggregate_poll(&mut self, observation: ActiveObservation) -> Result<ActiveObservation,()> {
        match self.aggregate_poll_impl(observation) {
            Ok(f) => Ok(f.0), //extract the nongeneric part
            Err(_) => Err(()),
        }
    }

    fn observe_if_distinct(&mut self) -> bool {
        self.observe_if_distinct()
    }
}


pub struct AggregateObserver {
    observers: Vec<Box<dyn ErasedObserver>>,
}

impl AggregateObserver {
    pub fn new() -> Self {
        AggregateObserver { observers: Vec::new() }
    }

    pub fn add_observer<T>(&mut self, observer: Observer<T>) where T: 'static + PartialEq + Clone {
        // Store the observer as a boxed trait object to erase the type
        self.observers.push(Box::new(observer));
    }

    /**
    Polls all observers and returns the first index that has a value ready.
*/
    pub async fn next(&mut self) -> usize {
        println!("next call");
        loop {
            println!("loop iteration");
            let (active_observation, active_future) = crate::active_observation::observation();
            for (o,observer) in &mut self.observers.iter_mut().enumerate() {
                let r = observer.aggregate_poll(active_observation.clone());
                match r {
                    Ok(future) => {
                        //future is being returned to us
                        drop(future);
                        return o; // Return the index of the first observer that is ready
                    },
                    Err(_) => continue, // If the observer is not ready, continue to the next one
                }
            }

            let something_woke = active_future.await;
            //look for the first observer that is ready
            for (o,observer) in &mut self.observers.iter_mut().enumerate() {
                if observer.observe_if_distinct() {
                    return o; // Return the index of the first observer that is ready
                }
            }
            //in "repeat" situations we may not have any observers that are ready
            //so try again!
        }


    }
}
#[cfg(test)] mod tests {
    use test_executors::async_test;
    use crate::Value;
    use super::AggregateObserver;

    #[async_test]
    async fn test_aggregate_observer() {
        let value = Value::new(2);
        let value2 = Value::new(0.3);

        let mut o = AggregateObserver::new();
        o.add_observer(value.observe());
        o.add_observer(value2.observe());

        let o1 = o.next().await;
        let o2 = o.next().await;

        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let mut value = value;
            value.set(3);
            //don't hangup
            std::mem::forget(value);
        });
        let o3 = o.next().await;
        assert_eq!(o1, 0);
    }

    #[async_test] async fn test_repeat_values() {
        let v = Value::new(0);
        let mut o = AggregateObserver::new();
        o.add_observer(v.observe());
        let o1 = o.next().await;
        assert_eq!(o1, 0);

        std::thread::spawn(move || {
            let mut v = v;
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
        assert!(begin.elapsed().as_millis() > 49, "Should have waited for the next value");
        assert_eq!(o2, 0);
    }
}