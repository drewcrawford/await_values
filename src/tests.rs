// SPDX-License-Identifier: MIT OR Apache-2.0

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

#[cfg(not(target_arch = "wasm32"))]
struct PanicWake;

#[cfg(not(target_arch = "wasm32"))]
impl Wake for PanicWake {
    fn wake(self: Arc<Self>) {
        panic!("waker failed");
    }

    fn wake_by_ref(self: &Arc<Self>) {
        panic!("waker failed");
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

/// One executor's broken waker must not make healthy observers disappear
/// from the registration stack before they can be notified.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn panicking_waker_does_not_strand_other_observers() {
    let value = super::Value::new(0);
    let mut healthy = value.observe();
    let mut panicking = value.observe();

    let healthy_wakes = Arc::new(WakeCount::default());
    let healthy_waker = Waker::from(Arc::clone(&healthy_wakes));
    let mut healthy_cx = Context::from_waker(&healthy_waker);
    assert!(matches!(
        Stream::poll_next(std::pin::Pin::new(&mut healthy), &mut healthy_cx),
        Poll::Ready(Some(0))
    ));
    assert!(matches!(
        Stream::poll_next(std::pin::Pin::new(&mut healthy), &mut healthy_cx),
        Poll::Pending
    ));

    // This observer was created last, so its registration is the first one
    // drained from the LIFO stack.
    let panic_waker = Waker::from(Arc::new(PanicWake));
    let mut panic_cx = Context::from_waker(&panic_waker);
    assert!(matches!(
        Stream::poll_next(std::pin::Pin::new(&mut panicking), &mut panic_cx),
        Poll::Ready(Some(0))
    ));
    assert!(matches!(
        Stream::poll_next(std::pin::Pin::new(&mut panicking), &mut panic_cx),
        Poll::Pending
    ));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| value.set(1)));
    assert!(result.is_err(), "the waker panic should still propagate");
    assert_eq!(
        healthy_wakes.0.load(Ordering::Relaxed),
        1,
        "later registrations must still be restored and woken"
    );
}

/// Observer cleanup also removes registrations temporarily while finding
/// its own entry, so it needs the same panic isolation as normal notify.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn panicking_waker_during_drop_does_not_strand_other_observers() {
    let value = super::Value::new(0);
    let target = value.observe();
    let mut healthy = value.observe();
    let mut panicking = value.observe();

    let healthy_wakes = Arc::new(WakeCount::default());
    let healthy_waker = Waker::from(Arc::clone(&healthy_wakes));
    let mut healthy_cx = Context::from_waker(&healthy_waker);
    assert!(matches!(
        Stream::poll_next(std::pin::Pin::new(&mut healthy), &mut healthy_cx),
        Poll::Ready(Some(0))
    ));
    assert!(matches!(
        Stream::poll_next(std::pin::Pin::new(&mut healthy), &mut healthy_cx),
        Poll::Pending
    ));

    let panic_waker = Waker::from(Arc::new(PanicWake));
    let mut panic_cx = Context::from_waker(&panic_waker);
    assert!(matches!(
        Stream::poll_next(std::pin::Pin::new(&mut panicking), &mut panic_cx),
        Poll::Ready(Some(0))
    ));
    assert!(matches!(
        Stream::poll_next(std::pin::Pin::new(&mut panicking), &mut panic_cx),
        Poll::Pending
    ));

    // Dropping the oldest observer pops both newer registrations while it
    // searches the stack for its own entry.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(target)));
    assert!(result.is_err(), "the waker panic should still propagate");
    assert_eq!(
        healthy_wakes.0.load(Ordering::Relaxed),
        1,
        "cleanup must restore and wake every displaced registration"
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
fn test_observer_from_borrowed_value() {
    let value = super::Value::new(42);
    let mut observer = super::Observer::from(&value);
    assert_eq!(observer.current_value(), Some(42));
    value.set(100);
    assert_eq!(observer.current_value(), Some(100));
}

#[wasm_lite_test]
async fn test_observer_next() {
    let value = Arc::new(super::Value::new(42));
    let mut observer = value.observe();
    assert_eq!(observer.current_value().unwrap(), 42);

    //push first
    value.set(100);
    let next_value = observer.next().await.unwrap();
    assert_eq!(next_value, 100);

    //read first
    let writer = Arc::clone(&value);
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(100));
        writer.set(200);
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
fn test_value_identity_equality() {
    let value1 = super::Value::new(42);
    let value2 = super::Value::new(42);

    assert_eq!(value1, value1);
    assert_ne!(value1, value2);

    value2.set(100);
    assert_ne!(value1, value2, "contents do not define slot identity");
}

#[wasm_lite_test]
// The whole regression is that this interior-mutable key now has stable,
// identity-based Eq/Hash; Clippy cannot infer those implementations.
#[allow(clippy::mutable_key_type)]
fn test_value_hash_is_stable_across_set() {
    use std::collections::HashSet;

    let value = Arc::new(super::Value::new(42));
    let mut set = HashSet::new();
    set.insert(Arc::clone(&value));

    value.set(100);
    assert!(
        set.contains(&value),
        "mutating a key must not make its hash-table entry unreachable"
    );
}

#[wasm_lite_test]
fn test_observer_display() {
    let value = super::Value::new(42);
    let observer = value.observe();
    let display_str = format!("{}", observer);
    assert!(display_str.starts_with("Observer(id:"));
}
