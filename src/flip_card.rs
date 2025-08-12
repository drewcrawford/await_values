/*!
FlipCard is a lock-free double-buffer.

*/



use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU8, AtomicUsize, Ordering};


#[derive(Debug)]
struct Slot<T> {
    data: UnsafeCell<T>,
    /*

Atomic layout is as follows
* w - locked for writing
* r - locked for reading

in u8 we can do
wrrrrrrr
i.e 128 readers
     */
    atomic: AtomicU8,
}

const WRITE: u8 = 0b10000000; // 128
const READ: u8 = 0b01111111; // 127
const UNLOCKED: u8 = 0b00000000; // 0

impl<T> Slot<T> {
    fn new(data: T) -> Self {
        Slot {
            data: UnsafeCell::new(data),
            atomic: AtomicU8::new(UNLOCKED), // unlocked
        }
    }

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
            Ok(lock_value) => {
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
#[derive(Debug)]
pub struct FlipCard<T> {
    data0: Slot<Option<T>>,
    data1: Slot<Option<T>>,
    read_data_0: AtomicBool, // Indicates if data0 is being read
}

unsafe impl<T: Send> Send for FlipCard<T> {}
unsafe impl<T: Send> Sync for FlipCard<T> {}

impl<T> FlipCard<T> {
    pub fn new(data0: T) -> Self {
        Self {
            data0: Slot::new(Some(data0)),
            data1: Slot::new(None), // Initialize with zeroed data
            read_data_0: AtomicBool::new(true), // Start with data0 being read
        }
    }
    pub fn flip_to(&self, data: T) -> T where T: Clone {
        let opt_data = Some(data);
        loop {
            let read_0 = self.read_data_0.load(Ordering::Relaxed);
            if read_0 {
                // we want to write into slot 1
                if self.data1.try_write(&opt_data).is_some() {
                    self.read_data_0.store(false, Ordering::Relaxed);
                    // Successfully wrote to slot 1, now read from slot 0
                    return self.data0.take().expect("Prior value")
                }
            }
            else {
                // we want to write into slot 0
                if self.data0.try_write(&opt_data).is_some() {
                    self.read_data_0.store(true, Ordering::Relaxed);
                    // Successfully wrote to slot 0, now read from slot 1
                    return self.data1.take().expect("Prior value")
                }
            }
            std::hint::spin_loop();

        }
    }
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