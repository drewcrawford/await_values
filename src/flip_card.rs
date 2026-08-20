// SPDX-License-Identifier: MIT OR Apache-2.0
//! Lock-free double-buffering data structure for concurrent read/write operations.
//!
//! This module provides the [`FlipCard`] type, a specialized concurrent data structure
//! that uses double-buffering to allow lock-free reads while maintaining consistency
//! during writes. It's designed for scenarios where you need high-performance concurrent
//! access with one writer and multiple readers.
//!
//! # Overview
//!
//! The `FlipCard` maintains two internal slots and atomically "flips" between them.
//! While readers access one slot, the writer can update the other slot without blocking.
//! This design ensures that readers always see consistent data without needing to wait
//! for write operations to complete.
//!
//! # The Flip Card Pattern
//!
//! The flip card pattern is a double-buffering technique where:
//! - Two copies of data are maintained (like two sides of a card)
//! - Readers always access the "front" side
//! - Writers update the "back" side
//! - An atomic "flip" operation swaps which side is front/back
//! - This allows lock-free concurrent access without blocking
//!
//! # Internal Architecture
//!
//! The implementation uses:
//! - Two `Slot<Option<T>>` buffers that alternate roles
//! - Lock-free atomic operations for synchronization
//! - A compact atomic representation supporting up to 127 concurrent readers per slot
//!
//! # Usage in await_values
//!
//! Within the `await_values` library, `FlipCard` is used internally by the `Shared<T>`
//! type to manage the current value while allowing concurrent observers to read it
//! without blocking writers. This enables the library's reactive patterns where value
//! changes can be observed asynchronously without blocking updates.
//!
//! # Safety and Correctness
//!
//! The `FlipCard` implementation ensures:
//! - No data races through atomic operations
//! - Readers always see consistent data
//! - Writers never block readers
//! - Maximum of 127 concurrent readers per slot
//!
//! # Performance Characteristics
//!
//! - **Read operations**: O(1) and lock-free, with potential brief spinning during concurrent writes
//! - **Write operations**: O(1) with atomic flip; concurrent writers serialize on an internal mutex
//! - **Memory overhead**: 2x the size of stored value plus atomic bookkeeping
//! - **Contention handling**: Lock-free with exponential backoff via spin loops

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Internal slot for storing data with reader/writer synchronization.
///
/// Each slot uses an atomic u8 to track access:
/// - Bit 7 (MSB): Write lock flag (0x80)
/// - Bits 0-6: Reader count (max 127 concurrent readers)
///
/// This compact representation allows efficient lock-free synchronization
/// while supporting a reasonable number of concurrent readers.
#[derive(Debug)]
struct Slot<T> {
    /// The actual data stored in this slot.
    data: UnsafeCell<T>,
    /// Atomic state tracking readers and writers.
    ///
    /// Layout: `wrrrrrrr` where:
    /// - `w` = write lock bit (bit 7)
    /// - `r` = reader count (bits 0-6)
    atomic: AtomicU8,
}

/// Bit mask for write lock (bit 7 set).
const WRITE: u8 = 0b10000000; // 128
/// Maximum value for reader count (all reader bits set).
const READ: u8 = 0b01111111; // 127
/// Initial state with no readers or writers.
const UNLOCKED: u8 = 0b00000000; // 0

/// Drops a slot's read lock, including while unwinding.
///
/// `try_read` calls `T::clone` while holding the lock. If that unwinds, the
/// reader count must still come back down: a leaked reader keeps the slot out
/// of `UNLOCKED` forever, and every later `try_write`/`take` spins on it
/// without end.
///
/// # Targets without unwinding
///
/// This works by running `Drop` during an unwind, so it only protects targets
/// that unwind. `wasm32-unknown-unknown` is `panic-strategy = "abort"`: a
/// panicking `T::clone` traps instead, no destructor runs, and the lock leaks
/// exactly as it did before these guards existed.
///
/// That is worse there than the native equivalent rather than merely equal. A
/// wasm panic is local to one instance — a worker traps only itself while the
/// main thread, sibling workers, and *shared memory* carry on — so the wedged
/// `FlipCard` outlives the thread that wedged it, and every surviving thread
/// spins on it forever. Only the panicking worker dies.
///
/// A `panic = "unwind"` mode is on wasm_lite's roadmap; these guards start
/// working on wasm32 the moment one exists, with no change here.
struct ReadGuard<'a>(&'a AtomicU8);

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// Drops a slot's write lock, including while unwinding.
///
/// Mirrors [`ReadGuard`] for the write side: a leaked `WRITE` bit would make
/// every later reader spin forever.
struct WriteGuard<'a>(&'a AtomicU8);

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        self.0.store(UNLOCKED, Ordering::Release);
    }
}

impl<T> Slot<T> {
    /// Creates a new slot with the given initial data.
    fn new(data: T) -> Self {
        Slot {
            data: UnsafeCell::new(data),
            atomic: AtomicU8::new(UNLOCKED), // unlocked
        }
    }

    /// Attempts to acquire a read lock and clone the data.
    ///
    /// Returns `Some(data)` if successful, `None` if a write lock is held.
    ///
    /// A saturated reader count is treated like write contention: the caller
    /// receives `None` and retries after another reader releases its slot.
    fn try_read(&self) -> Option<T>
    where
        T: Clone,
    {
        let r = self
            .atomic
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |value| {
                if value & WRITE != 0 {
                    // If the WRITE bit is set, we cannot read
                    None
                } else if value == READ {
                    // The reader count is saturated. Back off until a reader
                    // leaves rather than panicking in safe code.
                    None
                } else {
                    // Otherwise, we can read
                    Some(value + 1) // Increment the reader count
                }
            });
        match r {
            Ok(_lock_value) => {
                // Successfully acquired read lock. The guard releases it even if
                // `T::clone` panics, so an unwinding clone can't wedge the slot.
                let _guard = ReadGuard(&self.atomic);
                Some(unsafe { (&*self.data.get()).clone() })
            }
            Err(_) => None,
        }
    }

    /// Attempts to acquire a write lock and replace the data.
    ///
    /// Returns the old data if successful, or gives ownership of `data` back
    /// if the slot is locked so the caller can retry without cloning it.
    ///
    /// # Safety
    ///
    /// This operation requires exclusive access (no readers or writers).
    fn try_write(&self, data: T) -> Result<T, T> {
        let r = self
            .atomic
            .compare_exchange(UNLOCKED, WRITE, Ordering::Acquire, Ordering::Relaxed);
        match r {
            Ok(_) => {
                // Successfully acquired the write lock. Keep the guard around
                // the unsafe replacement so future changes cannot accidentally
                // leak the lock during an unwind.
                let _guard = WriteGuard(&self.atomic);
                Ok(unsafe { std::ptr::replace(self.data.get(), data) })
            }
            Err(_) => {
                // Failed to acquire the write lock; return the input so a
                // retry does not need to clone it.
                Err(data)
            }
        }
    }
}
impl<T> Slot<Option<T>> {
    /// Takes the value from the slot, replacing it with `None`.
    ///
    /// This method spins until it can acquire the write lock.
    ///
    /// # Returns
    ///
    /// The previous value in the slot.
    fn take(&self) -> Option<T> {
        loop {
            match self.try_write(None) {
                Ok(old_data) => {
                    // Successfully took the data
                    return old_data;
                }
                Err(None) => {
                    // Failed to take the data, retry
                    std::hint::spin_loop();
                }
                Err(Some(_)) => unreachable!("try_write returns its input on contention"),
            }
        }
    }
}
/// A lock-free double-buffer for concurrent read/write operations.
///
/// `FlipCard<T>` maintains two internal slots and atomically switches between them
/// during write operations. This allows readers to access consistent data from one
/// slot while writers update the other slot.
///
/// # Type Requirements
///
/// The type `T` must implement:
/// - `Clone`: Required for read operations to return owned values
/// - `Send`: Required to move the card (and its value) between threads
/// - `Sync`: Required to *share* the card, because concurrent readers clone
///   out of the same slot at the same time
///
/// # Performance Characteristics
///
/// - **Reads**: Lock-free with potential spinning if slot is being written
/// - **Writes**: Atomic flip; concurrent writers serialize on an internal mutex
/// - **Memory**: Uses 2x the memory of a single value
/// - **Cache coherence**: Optimized for single-writer scenarios
///
/// # Thread Safety
///
/// `FlipCard<T>` is `Send` when `T: Send`, and `Sync` when `T: Send + Sync`.
/// The internal synchronization ensures data consistency without requiring
/// external locks. `Sync` needs `T: Sync` on top of `T: Send` because `read`
/// clones under a shared read lock: several threads can be inside `T::clone`
/// on the same value at once.
#[derive(Debug)]
pub struct FlipCard<T> {
    /// First data slot.
    data0: Slot<Option<T>>,
    /// Second data slot.
    data1: Slot<Option<T>>,
    /// Indicates which slot readers should use (true = slot 0, false = slot 1).
    read_data_0: AtomicBool,
    /// Serializes writers.
    ///
    /// Two concurrent `flip_to` calls could otherwise both write into the same
    /// back slot and both `take` the front slot, so the second `take` finds
    /// `None` and violates the "front slot always holds a value" invariant.
    /// Readers never touch this lock.
    write_lock: wasm_lite_std::Mutex<()>,
}

// SAFETY: FlipCard<T> can be sent between threads if T can be sent.
// The internal UnsafeCell is properly synchronized via atomic operations.
unsafe impl<T: Send> Send for FlipCard<T> {}

// SAFETY: sharing a FlipCard<T> hands out `&T` to several threads at once -
// `read` clones out of the front slot under a *shared* read lock, so up to 127
// threads can be inside `T::clone` on the same value simultaneously. That is
// exactly what `T: Sync` licenses, so `Sync` is required here in addition to
// `Send` (which covers moving `T` between threads via `flip_to`/`read`).
// Dropping `Sync` would let safe code race inside `T::clone` for any
// `Send + !Sync` type, e.g. `RefCell<i32>`.
unsafe impl<T: Send + Sync> Sync for FlipCard<T> {}

impl<T> FlipCard<T> {
    /// Creates a new `FlipCard` with the given initial value.
    ///
    /// The initial value is placed in slot 0, and slot 1 is initialized as empty.
    /// The FlipCard starts with slot 0 as the active read slot.
    ///
    /// # Implementation Note
    ///
    /// Slot 0 starts as the active read slot with the initial value,
    /// while slot 1 is initialized empty. The first write operation
    /// will write to slot 1 and flip the read pointer.
    pub fn new(data0: T) -> Self {
        Self {
            data0: Slot::new(Some(data0)),
            data1: Slot::new(None),             // Initialize with zeroed data
            read_data_0: AtomicBool::new(true), // Start with data0 being read
            write_lock: wasm_lite_std::Mutex::new(()),
        }
    }
}

impl<T: Default> Default for FlipCard<T> {
    /// Creates a `FlipCard` with the default value of `T`.
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Clone> Clone for FlipCard<T> {
    /// Creates a clone of the `FlipCard` with the current value.
    ///
    /// The clone will have the same value as the original at the time of cloning,
    /// but the two FlipCards will be independent and can be updated separately.
    fn clone(&self) -> Self {
        Self::new(self.read())
    }
}

impl<T> From<T> for FlipCard<T> {
    /// Creates a `FlipCard` from a value.
    ///
    /// This is equivalent to calling [`FlipCard::new`].
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> FlipCard<T> {
    /// Atomically replaces the current value with a new one.
    ///
    /// This method writes the new value to the inactive slot, then atomically
    /// flips the read pointer to make it active. The old value is then extracted
    /// from what is now the inactive slot.
    ///
    /// # Returns
    ///
    /// The previous value that was stored in the FlipCard.
    ///
    /// # Performance
    ///
    /// This method may spin briefly if the target slot is temporarily locked
    /// by a concurrent operation. The spinning is optimized with `spin_loop`
    /// hints for CPU efficiency.
    ///
    /// # Algorithm
    ///
    /// 1. Determine the inactive slot based on `read_data_0`
    /// 2. Attempt to write the new value to the inactive slot
    /// 3. Atomically flip the read pointer to the newly written slot
    /// 4. Extract and return the old value from the now-inactive slot
    pub fn flip_to(&self, data: T) -> T {
        let mut opt_data = Some(data);
        let _guard = self.write_lock.lock_sync();
        loop {
            let read_0 = self.read_data_0.load(Ordering::Relaxed);
            if read_0 {
                // we want to write into slot 1
                match self.data1.try_write(opt_data) {
                    Ok(prior) => {
                        assert!(prior.is_none(), "back slot must be empty");
                        self.read_data_0.store(false, Ordering::Release);
                        // Successfully wrote to slot 1, now read from slot 0
                        return self.data0.take().expect("Prior value");
                    }
                    Err(data) => opt_data = data,
                }
            } else {
                // we want to write into slot 0
                match self.data0.try_write(opt_data) {
                    Ok(prior) => {
                        assert!(prior.is_none(), "back slot must be empty");
                        self.read_data_0.store(true, Ordering::Release);
                        // Successfully wrote to slot 0, now read from slot 1
                        return self.data1.take().expect("Prior value");
                    }
                    Err(data) => opt_data = data,
                }
            }
            std::hint::spin_loop();
        }
    }
    /// Reads the current value.
    ///
    /// This method reads from the currently active slot. It may briefly spin
    /// if the slot is being written to, but readers never block writers and
    /// vice versa due to the double-buffering design.
    ///
    /// # Panics
    ///
    /// Panics if the active slot contains `None`, which should never happen
    /// in normal operation as the FlipCard maintains invariants.
    ///
    /// # Performance
    ///
    /// - Best case: O(1) when no concurrent write is happening
    /// - Worst case: Brief spinning during concurrent write operations
    /// - No memory allocation beyond the `Clone` of the returned value
    ///
    /// # Implementation Details
    ///
    /// The method first checks `read_data_0` to determine which slot is active,
    /// then attempts to read from that slot. If the read fails (due to a
    /// concurrent write), it spins and retries. The spin loop uses CPU hints
    /// for efficiency.
    pub fn read(&self) -> T
    where
        T: Clone,
    {
        loop {
            if self.read_data_0.load(Ordering::Acquire) {
                // Read from slot 0
                if let Some(Some(val)) = self.data0.try_read() {
                    return val;
                }
                // If we got None (either from try_read() or from inner Option), it means the slot was cleared concurrently.
                // This implies a writer flipped to slot 1 and cleared slot 0.
                // We should retry the loop to pick up the new active slot.
            } else {
                // Read from slot 1
                if let Some(Some(val)) = self.data1.try_read() {
                    return val;
                }
                // Same as above, slot was cleared concurrently. Retry.
            }
            std::hint::spin_loop(); // Wait for data to be available
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wasm_lite::wasm_lite_test;

    // `std::thread::spawn` is unsupported on wasm32; `wasm_lite_std` spawns Web
    // Workers instead. Same shape, so the threaded tests below read identically
    // on both targets.
    #[cfg(not(target_arch = "wasm32"))]
    use std::thread;
    #[cfg(target_arch = "wasm32")]
    use wasm_lite_std as thread;

    /// How many threads the contention tests spawn.
    ///
    /// Each wasm32 thread is a Web Worker inside a per-test page load, so the
    /// browser runs are deliberately smaller: enough concurrency to interleave
    /// readers and writers, but not so much that the suite outruns the runner's
    /// page deadline.
    #[cfg(not(target_arch = "wasm32"))]
    const THREADS: usize = 10;
    #[cfg(target_arch = "wasm32")]
    const THREADS: usize = 4;

    /// Iterations per thread in the write-stress tests. Scaled for the same reason.
    #[cfg(not(target_arch = "wasm32"))]
    const STRESS: u64 = 100_000;
    #[cfg(target_arch = "wasm32")]
    const STRESS: u64 = 2_000;

    /// Blocks every participant until all of them have arrived.
    ///
    /// Stands in for `std::sync::Barrier`, which `wasm_lite_std` does not
    /// provide. Spinning is fine here: it only ever runs on a spawned
    /// thread/worker, never the browser main thread, and only for as long as
    /// the slowest participant takes to start.
    struct StartGate {
        arrived: std::sync::atomic::AtomicUsize,
        parties: usize,
    }

    impl StartGate {
        fn new(parties: usize) -> Self {
            Self {
                arrived: std::sync::atomic::AtomicUsize::new(0),
                parties,
            }
        }

        fn wait(&self) {
            self.arrived.fetch_add(1, Ordering::AcqRel);
            while self.arrived.load(Ordering::Acquire) < self.parties {
                std::hint::spin_loop();
            }
        }
    }

    /// Test basic read and write operations
    #[wasm_lite_test]
    fn test_basic_operations() {
        let card = FlipCard::new(42);
        assert_eq!(card.read(), 42);

        let old = card.flip_to(100);
        assert_eq!(old, 42);
        assert_eq!(card.read(), 100);
    }

    /// Reaching the representable reader limit is transient contention, not a
    /// caller error, and must not turn safe reads into a process-wide panic.
    #[wasm_lite_test]
    fn saturated_reader_count_backs_off() {
        let slot = Slot::new(42);
        slot.atomic.store(READ, Ordering::Relaxed);
        assert_eq!(slot.try_read(), None);
    }

    /// Test that FlipCard works with different types
    #[wasm_lite_test]
    fn test_different_types() {
        // String
        let card = FlipCard::new(String::from("hello"));
        assert_eq!(card.read(), "hello");
        card.flip_to(String::from("world"));
        assert_eq!(card.read(), "world");

        // Vector
        let card = FlipCard::new(vec![1, 2, 3]);
        assert_eq!(card.read(), vec![1, 2, 3]);
        let old = card.flip_to(vec![4, 5, 6]);
        assert_eq!(old, vec![1, 2, 3]);
        assert_eq!(card.read(), vec![4, 5, 6]);
    }

    /// Test concurrent reads don't block each other
    #[wasm_lite_test(worker)]
    fn test_concurrent_reads() {
        let card = Arc::new(FlipCard::new(42));
        let barrier = Arc::new(StartGate::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let card = Arc::clone(&card);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    card.read()
                })
            })
            .collect();

        for handle in handles {
            assert_eq!(handle.join().unwrap(), 42);
        }
    }

    /// Test concurrent reads and writes
    #[wasm_lite_test(worker)]
    fn test_concurrent_read_write() {
        let card = Arc::new(FlipCard::new(0));
        let barrier = Arc::new(StartGate::new(THREADS + 1));

        // Writer thread
        let writer = {
            let card = Arc::clone(&card);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for i in 1..=100 {
                    card.flip_to(i);
                }
            })
        };

        // Reader threads
        let readers: Vec<_> = (0..THREADS)
            .map(|_| {
                let card = Arc::clone(&card);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let mut values = Vec::new();
                    for _ in 0..10 {
                        values.push(card.read());
                    }
                    values
                })
            })
            .collect();

        writer.join().unwrap();

        for handle in readers {
            let values = handle.join().unwrap();
            // Each reader should see monotonically increasing values
            for window in values.windows(2) {
                assert!(
                    window[0] <= window[1],
                    "Values should be monotonic: {:?}",
                    values
                );
            }
        }

        // Final value should be 100
        assert_eq!(card.read(), 100);
    }

    /// Test that Clone creates independent FlipCards
    #[wasm_lite_test]
    fn test_clone() {
        let card1 = FlipCard::new(42);
        let card2 = card1.clone();

        // Both should start with the same value
        assert_eq!(card1.read(), 42);
        assert_eq!(card2.read(), 42);

        // Update card1
        card1.flip_to(100);
        assert_eq!(card1.read(), 100);
        assert_eq!(card2.read(), 42); // card2 should be unchanged

        // Update card2
        card2.flip_to(200);
        assert_eq!(card1.read(), 100); // card1 should be unchanged
        assert_eq!(card2.read(), 200);
    }

    /// Test that From trait works correctly
    #[wasm_lite_test]
    fn test_from() {
        let card: FlipCard<i32> = 42.into();
        assert_eq!(card.read(), 42);

        let card = FlipCard::from("hello");
        assert_eq!(card.read(), "hello");
    }

    /// Test that multiple writes work correctly
    #[wasm_lite_test]
    fn test_sequential_writes() {
        let card = FlipCard::new(0);

        for i in 1..=1000 {
            let old = card.flip_to(i);
            assert_eq!(old, i - 1);
        }

        assert_eq!(card.read(), 1000);
    }

    /// Test custom types with FlipCard
    #[wasm_lite_test]
    fn test_custom_type() {
        #[derive(Clone, Debug, PartialEq)]
        struct Config {
            name: String,
            value: i32,
        }

        let card = FlipCard::new(Config {
            name: "initial".to_string(),
            value: 0,
        });

        let config = card.read();
        assert_eq!(config.name, "initial");
        assert_eq!(config.value, 0);

        let old = card.flip_to(Config {
            name: "updated".to_string(),
            value: 42,
        });

        assert_eq!(old.name, "initial");
        assert_eq!(old.value, 0);

        let new = card.read();
        assert_eq!(new.name, "updated");
        assert_eq!(new.value, 42);
    }

    /// Test that concurrent writers don't panic or lose the slot invariant.
    #[wasm_lite_test(worker)]
    fn test_concurrent_writers() {
        let card = Arc::new(FlipCard::new(0u64));
        let barrier = Arc::new(StartGate::new(4));
        let writers: Vec<_> = (0..4)
            .map(|t| {
                let card = Arc::clone(&card);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    for i in 0..STRESS {
                        card.flip_to(t * 1_000_000 + i);
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        card.read();
    }

    /// A value whose `Clone` panics on demand.
    ///
    /// The test below is the only one here that stays native-only. It asserts
    /// that [`ReadGuard`] releases during an unwind, and
    /// `wasm32-unknown-unknown` is `panic-strategy = "abort"` — nothing
    /// unwinds, so no destructor runs and the behaviour under test does not
    /// hold there. See [`ReadGuard`] for what that costs a wasm32 caller.
    ///
    /// This is a missing-unwind limitation, not a permanent one: if wasm_lite
    /// ships the `panic = "unwind"` mode on its roadmap, it should be revisited
    /// as a `#[wasm_lite_test(worker)]` case.
    #[cfg(not(target_arch = "wasm32"))]
    mod fussy {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Mutex, MutexGuard};

        pub static BOOM: AtomicBool = AtomicBool::new(false);

        /// Serializes any test that arms [`BOOM`].
        ///
        /// Both the flag and the panic hook `expect_panic` swaps are
        /// process-wide, and libtest runs tests in parallel. Keep the gate even
        /// with one current caller so another panic-path test cannot later be
        /// added without the required serialization.
        static GATE: Mutex<()> = Mutex::new(());

        /// Holds the gate for the caller's whole test body.
        ///
        /// A test that panics while holding it poisons the mutex, but the only
        /// thing being guarded is "one at a time" — there is no invariant left
        /// broken — so recover rather than cascading the failure into the other
        /// test.
        pub fn gate() -> MutexGuard<'static, ()> {
            GATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        #[derive(Debug, PartialEq)]
        pub struct Fussy(pub u32);

        impl Clone for Fussy {
            fn clone(&self) -> Self {
                if BOOM.load(Ordering::Relaxed) {
                    panic!("clone failed");
                }
                Fussy(self.0)
            }
        }
    }

    /// Runs `f` on another thread and reports whether it finished in time.
    ///
    /// A leaked slot lock shows up as an endless spin rather than a wrong
    /// answer, so the tests need a deadline instead of a plain assertion.
    ///
    /// The deadline is generous on purpose. It separates "finished" from
    /// "spinning forever", not "fast" from "slow", and a passing run returns as
    /// soon as the flag flips — so the only thing a short deadline buys is a
    /// spurious failure on a loaded machine.
    #[cfg(not(target_arch = "wasm32"))]
    fn finishes(f: impl FnOnce() + Send + 'static) -> bool {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&done);
        thread::spawn(move || {
            f();
            flag.store(true, std::sync::atomic::Ordering::Release);
        });
        for _ in 0..3_000 {
            if done.load(std::sync::atomic::Ordering::Acquire) {
                return true;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// Swallows the panic from `f`, without the default hook spamming stderr.
    #[cfg(not(target_arch = "wasm32"))]
    fn expect_panic(f: impl FnOnce()) {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        std::panic::set_hook(hook);
        assert!(r.is_err(), "expected the clone to panic");
    }

    /// A `T::clone` that unwinds out of `read` must not leak the read lock.
    ///
    /// Without [`ReadGuard`] the reader count never comes back down, so the
    /// slot never returns to `UNLOCKED` and every later write spins forever.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn panicking_clone_does_not_leak_read_lock() {
        use fussy::{BOOM, Fussy};
        let _gate = fussy::gate();

        let card = Arc::new(FlipCard::new(Fussy(1)));
        assert_eq!(card.read(), Fussy(1));

        BOOM.store(true, Ordering::Relaxed);
        expect_panic({
            let card = Arc::clone(&card);
            move || {
                card.read();
            }
        });
        BOOM.store(false, Ordering::Relaxed);

        let writer = Arc::clone(&card);
        assert!(
            finishes(move || {
                writer.flip_to(Fussy(2));
            }),
            "flip_to never completed: read lock leaked on unwind"
        );
        assert_eq!(card.read(), Fussy(2));
    }

    #[wasm_lite_test(worker)]
    fn reproduce_panic() {
        let card = Arc::new(FlipCard::new(0));
        let card_clone = card.clone();

        // Writer thread
        let writer = thread::spawn(move || {
            for i in 1..(STRESS * 10) {
                card_clone.flip_to(i);
                // Small sleep to allow reader to interleave
                // thread::sleep(Duration::from_nanos(1));
            }
        });

        // Reader thread
        let reader = thread::spawn(move || {
            for _ in 0..(STRESS * 10) {
                let _ = card.read();
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }
}
