use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use atomic_waker::AtomicWaker;
use crate::ObserverError;

pub struct ActiveObservation {
    shared: Arc<Shared>,
}

impl Drop for ActiveObservation {
    fn drop(&mut self) {
        // When the ActiveObservation is dropped, we set the notified flag to true
        // to indicate that the observation is no longer active.
        self.shared.ready.store(true, std::sync::atomic::Ordering::Relaxed);
        self.shared.waker.wake();
    }
}

struct Shared {
    waker: AtomicWaker,
    ready: AtomicBool,
}

pub(crate) struct ActiveObservationFuture {
    shared: Arc<Shared>,
}

impl Future for ActiveObservationFuture {
    type Output = Result<(),ObserverError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        // Polling logic to check for changes and notify observers
        self.shared.waker.register(cx.waker());
        if self.shared.ready.load(std::sync::atomic::Ordering::Relaxed) {
            return std::task::Poll::Ready(Ok(()));
        }
        std::task::Poll::Pending
    }
}

pub fn observation() -> (ActiveObservation, ActiveObservationFuture) {
    let shared = Arc::new(Shared {
        waker: AtomicWaker::new(),
        ready: AtomicBool::new(false),
    });

    (
        ActiveObservation { shared: shared.clone() },
        ActiveObservationFuture { shared }
        
    )
}