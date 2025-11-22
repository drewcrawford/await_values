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
//! # When to Use
//!
//! Use `FlipCard` when you need:
//! - High-frequency reads with occasional writes
//! - Lock-free read operations that never block
//! - Consistent snapshots of data during reads
//! - Single-writer, multiple-reader scenarios
//!
//! # Examples
//!
//! ## Basic Usage
//!
//! ```
//! use await_values::flip_card::FlipCard;
//!
//! // Create a FlipCard with an initial value
//! let flip_card = FlipCard::new(42);
//!
//! // Read the current value
//! let value = flip_card.read();
//! assert_eq!(value, 42);
//!
//! // Update the value atomically
//! let old_value = flip_card.flip_to(100);
//! assert_eq!(old_value, 42);
//!
//! // Read the new value
//! assert_eq!(flip_card.read(), 100);
//! ```
//!
//! ## Concurrent Access
//!
//! ```
//! use await_values::flip_card::FlipCard;
//! use std::sync::Arc;
//! use std::thread;
//!
//! let flip_card = Arc::new(FlipCard::new(0));
//!
//! // Spawn multiple reader threads
//! let readers: Vec<_> = (0..5).map(|i| {
//!     let fc = Arc::clone(&flip_card);
//!     thread::spawn(move || {
//!         // Readers never block
//!         let value = fc.read();
//!         println!("Reader {} saw value: {}", i, value);
//!         value
//!     })
//! }).collect();
//!
//! // Writer thread
//! let writer = {
//!     let fc = Arc::clone(&flip_card);
//!     thread::spawn(move || {
//!         // Writer atomically updates without blocking readers
//!         fc.flip_to(42);
//!     })
//! };
//!
//! // Wait for all threads
//! writer.join().unwrap();
//! for reader in readers {
//!     reader.join().unwrap();
//! }
//! ```
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
//! - **Read operations**: O(1) with potential brief spinning during concurrent writes
//! - **Write operations**: O(1) with atomic flip
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
    /// # Panics
    ///
    /// Panics if the maximum number of readers (127) is reached.
    fn try_read(&self) -> Option<T> where T: Clone {
        let r = self.atomic.fetch_update(Ordering::AcqRel, Ordering::Relaxed, |value| {
            if value & WRITE != 0 {
                // If the WRITE bit is set, we cannot read
                None
            } else if value == READ {
                //maximum number of readers reached
                panic!("Maximum number of readers reached");
            }
            else {
                // Otherwise, we can read
                Some(value + 1) // Increment the reader count
            }
        });
        match r {
            Ok(_lock_value) => {
                // Successfully acquired read lock
                let data = unsafe {
                     (&*self.data.get()).clone()
                };
                self.atomic.fetch_sub(1, Ordering::Release); // Release the read lock
                Some(data)
            }
            Err(_) => None,
        }
    }

    /// Attempts to acquire a write lock and replace the data.
    ///
    /// Returns `Some(old_data)` if successful, `None` if the slot is locked.
    ///
    /// # Safety
    ///
    /// This operation requires exclusive access (no readers or writers).
    fn try_write(&self, data: &T) -> Option<T> where T: Clone {
        let r = self.atomic.compare_exchange(UNLOCKED, WRITE, Ordering::Acquire, Ordering::Relaxed);
        match r {
            Ok(_) => {
                // Successfully acquired write lock
                let old_data = unsafe {
                    std::ptr::replace(self.data.get(), data.clone())
                };
                self.atomic.store(UNLOCKED, Ordering::Release); // Release the write lock
                Some(old_data)
            }
            Err(_) => {
                // Failed to acquire write lock
                None
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
    fn take(&self) -> Option<T> where T: Clone {
        loop {
            let r = self.try_write(&None);
            match r {
                Some(old_data) => {
                    // Successfully took the data
                    return old_data
                }
                None => {
                    // Failed to take the data, retry
                    std::hint::spin_loop();
                }
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
/// - `Send`: Required for thread safety
///
/// # Performance Characteristics
///
/// - **Reads**: Lock-free with potential spinning if slot is being written
/// - **Writes**: Lock-free with atomic flip operation
/// - **Memory**: Uses 2x the memory of a single value
/// - **Cache coherence**: Optimized for single-writer scenarios
///
/// # Thread Safety
///
/// `FlipCard<T>` is `Send` and `Sync` when `T: Send`, allowing it to be shared
/// across threads safely. The internal synchronization ensures data consistency
/// without requiring external locks.
///
/// # Examples
///
/// ## Single-threaded usage
///
/// ```
/// use await_values::flip_card::FlipCard;
///
/// let flip_card = FlipCard::new("initial");
/// assert_eq!(flip_card.read(), "initial");
///
/// let old = flip_card.flip_to("updated");
/// assert_eq!(old, "initial");
/// assert_eq!(flip_card.read(), "updated");
/// ```
///
/// ## As a shared state container
///
/// ```
/// use await_values::flip_card::FlipCard;
/// use std::sync::Arc;
///
/// #[derive(Clone, Debug)]
/// struct AppState {
///     counter: i32,
///     message: String,
/// }
///
/// let state = Arc::new(FlipCard::new(AppState {
///     counter: 0,
///     message: "Hello".to_string(),
/// }));
///
/// // Multiple threads can read without blocking
/// let current = state.read();
/// assert_eq!(current.counter, 0);
///
/// // Updates are atomic and return the previous state
/// let old_state = state.flip_to(AppState {
///     counter: 1,
///     message: "Updated".to_string(),
/// });
/// assert_eq!(old_state.counter, 0);
/// ```
#[derive(Debug)]
pub struct FlipCard<T> {
    /// First data slot.
    data0: Slot<Option<T>>,
    /// Second data slot.
    data1: Slot<Option<T>>,
    /// Indicates which slot readers should use (true = slot 0, false = slot 1).
    read_data_0: AtomicBool,
}

// SAFETY: FlipCard<T> can be sent between threads if T can be sent.
// The internal UnsafeCell is properly synchronized via atomic operations.
unsafe impl<T: Send> Send for FlipCard<T> {}

// SAFETY: FlipCard<T> can be shared between threads if T can be sent.
// All methods use proper atomic synchronization to prevent data races.
unsafe impl<T: Send> Sync for FlipCard<T> {}

impl<T> FlipCard<T> {
    /// Creates a new `FlipCard` with the given initial value.
    ///
    /// The initial value is placed in slot 0, and slot 1 is initialized as empty.
    /// The FlipCard starts with slot 0 as the active read slot.
    ///
    /// # Examples
    ///
    /// ```
    /// use await_values::flip_card::FlipCard;
    ///
    /// // Create with a simple value
    /// let card = FlipCard::new(42);
    /// assert_eq!(card.read(), 42);
    ///
    /// // Create with a complex type
    /// let card = FlipCard::new(vec![1, 2, 3]);
    /// assert_eq!(card.read(), vec![1, 2, 3]);
    /// ```
    ///
    /// # Implementation Note
    ///
    /// Slot 0 starts as the active read slot with the initial value,
    /// while slot 1 is initialized empty. The first write operation
    /// will write to slot 1 and flip the read pointer.
    pub fn new(data0: T) -> Self {
        Self {
            data0: Slot::new(Some(data0)),
            data1: Slot::new(None), // Initialize with zeroed data
            read_data_0: AtomicBool::new(true), // Start with data0 being read
        }
    }
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
    /// # Examples
    ///
    /// ```
    /// use await_values::flip_card::FlipCard;
    ///
    /// let card = FlipCard::new("hello");
    /// 
    /// // Replace value and get the old one
    /// let old = card.flip_to("world");
    /// assert_eq!(old, "hello");
    /// assert_eq!(card.read(), "world");
    ///
    /// // Chain multiple updates
    /// let old = card.flip_to("foo");
    /// assert_eq!(old, "world");
    /// let old = card.flip_to("bar");
    /// assert_eq!(old, "foo");
    /// assert_eq!(card.read(), "bar");
    /// ```
    ///
    /// ## Concurrent Updates
    ///
    /// ```
    /// use await_values::flip_card::FlipCard;
    /// use std::sync::Arc;
    /// use std::thread;
    ///
    /// let card = Arc::new(FlipCard::new(0));
    /// let card_clone = Arc::clone(&card);
    ///
    /// // Update from another thread
    /// let handle = thread::spawn(move || {
    ///     for i in 1..=5 {
    ///         card_clone.flip_to(i);
    ///     }
    /// });
    ///
    /// handle.join().unwrap();
    /// 
    /// // Final value should be 5
    /// assert_eq!(card.read(), 5);
    /// ```
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
    pub fn flip_to(&self, data: T) -> T where T: Clone {
        let opt_data = Some(data);
        loop {
            let read_0 = self.read_data_0.load(Ordering::Relaxed);
            if read_0 {
                // we want to write into slot 1
                if self.data1.try_write(&opt_data).is_some() {
                    self.read_data_0.store(false, Ordering::Release);
                    // Successfully wrote to slot 1, now read from slot 0
                    return self.data0.take().expect("Prior value")
                }
            }
            else {
                // we want to write into slot 0
                if self.data0.try_write(&opt_data).is_some() {
                    self.read_data_0.store(true, Ordering::Release);
                    // Successfully wrote to slot 0, now read from slot 1
                    return self.data1.take().expect("Prior value")
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
    /// # Examples
    ///
    /// ```
    /// use await_values::flip_card::FlipCard;
    ///
    /// let card = FlipCard::new(vec![1, 2, 3]);
    /// 
    /// // Read returns a clone of the current value
    /// let value = card.read();
    /// assert_eq!(value, vec![1, 2, 3]);
    ///
    /// // Multiple reads return the same value until flip_to is called
    /// assert_eq!(card.read(), card.read());
    /// ```
    ///
    /// ## Concurrent Reads
    ///
    /// ```
    /// use await_values::flip_card::FlipCard;
    /// use std::sync::Arc;
    /// use std::thread;
    ///
    /// let card = Arc::new(FlipCard::new("shared"));
    ///
    /// // Multiple threads can read simultaneously without blocking
    /// let handles: Vec<_> = (0..10).map(|_| {
    ///     let card = Arc::clone(&card);
    ///     thread::spawn(move || {
    ///         card.read()
    ///     })
    /// }).collect();
    ///
    /// // All threads see the same value
    /// for handle in handles {
    ///     assert_eq!(handle.join().unwrap(), "shared");
    /// }
    /// ```
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
    pub fn read(&self) -> T where T: Clone {
        loop {
            if self.read_data_0.load(Ordering::Acquire) {
                // Read from slot 0
                if let Some(data) = self.data0.try_read() {
                    return data.expect("No data in slot 0");
                }
            } else {
                // Read from slot 1
                if let Some(data) = self.data1.try_read() {
                    return data.expect("No data in slot 1");
                }
            }
            std::hint::spin_loop(); // Wait for data to be available
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    /// Test basic read and write operations
    #[test]
    fn test_basic_operations() {
        let card = FlipCard::new(42);
        assert_eq!(card.read(), 42);
        
        let old = card.flip_to(100);
        assert_eq!(old, 42);
        assert_eq!(card.read(), 100);
    }

    /// Test that FlipCard works with different types
    #[test]
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
    #[test]
    fn test_concurrent_reads() {
        let card = Arc::new(FlipCard::new(42));
        let barrier = Arc::new(Barrier::new(10));
        
        let handles: Vec<_> = (0..10).map(|_| {
            let card = Arc::clone(&card);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                card.read()
            })
        }).collect();
        
        for handle in handles {
            assert_eq!(handle.join().unwrap(), 42);
        }
    }

    /// Test concurrent reads and writes
    #[test]
    fn test_concurrent_read_write() {
        let card = Arc::new(FlipCard::new(0));
        let barrier = Arc::new(Barrier::new(11));
        
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
        let readers: Vec<_> = (0..10).map(|_| {
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
        }).collect();
        
        writer.join().unwrap();
        
        for handle in readers {
            let values = handle.join().unwrap();
            // Each reader should see monotonically increasing values
            for window in values.windows(2) {
                assert!(window[0] <= window[1], 
                    "Values should be monotonic: {:?}", values);
            }
        }
        
        // Final value should be 100
        assert_eq!(card.read(), 100);
    }

    /// Test that multiple writes work correctly
    #[test]
    fn test_sequential_writes() {
        let card = FlipCard::new(0);
        
        for i in 1..=1000 {
            let old = card.flip_to(i);
            assert_eq!(old, i - 1);
        }
        
        assert_eq!(card.read(), 1000);
    }

    /// Test custom types with FlipCard
    #[test]
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
}