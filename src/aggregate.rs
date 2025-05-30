use std::any::Any;
use crate::{Observer, ObserverError};

trait ErasedObserver {
    fn aggregate_poll(&mut self);
}
impl <T> ErasedObserver for Observer<T> {
    fn aggregate_poll(&mut self) {
        self.aggregate_poll_impl()
    }
}


pub struct AggregateObserver {
    observers: Vec<Box<dyn ErasedObserver>>,
}

impl AggregateObserver {
    pub fn new() -> Self {
        AggregateObserver { observers: Vec::new() }
    }

    pub fn add_observer<T>(&mut self, observer: Observer<T>) where T: 'static {
        // Store the observer as a boxed trait object to erase the type
        self.observers.push(Box::new(observer));
    }

    /**
    Polls all observers and returns the first index that has a value ready.
*/
    pub async fn next(&mut self) -> usize {
        for observer in &mut self.observers {
            observer.aggregate_poll();
        }
        todo!()
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
    }
}