// SPDX-License-Identifier: MIT OR Apache-2.0

//! A bounded record of live observable values, exposed through exfiltrate.
//!
//! Two questions this answers that nothing else can:
//!
//! * **Is anyone listening?** A `Value` with zero observers is a publisher
//!   nobody subscribed to — a common wiring mistake that produces no error,
//!   just silence.
//! * **Is a listener falling behind?** An observer that has not caught up to
//!   the current generation is stale, and one that stays stale is a subscriber
//!   that stopped polling.
//!
//! # Values themselves are never reported
//!
//! Deliberately, and it is the reason this could be built without settling the
//! open question of *whether* current values may be exposed. Rows carry the
//! **generation** — how many times a value has changed — and when it last
//! changed, never what it holds. That is what makes staleness answerable, and
//! it cannot leak application data.
//!
//! It is also what the types allow: `T` is only `Clone`, not `Debug`, so
//! rendering a value would mean widening a bound on every user of the crate to
//! serve a debug feature.
//!
//! # Bounded, and honest about it
//!
//! The most recent `N`, from `AWAIT_VALUES_REGISTRY_CAPACITY`, defaulting to
//! [`DEFAULT_CAPACITY`]. Overflow forgets *dropped* values before live ones,
//! and every drop is counted and reported.
//!
//! Behind the `exfiltrate` feature; with it off none of this is compiled and
//! `set` does not touch a lock.

use std::collections::VecDeque;
use std::panic::Location;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use wasm_lite_std::time::Instant;

pub const DEFAULT_CAPACITY: usize = 1024;

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u64,
    pub created_at_file: &'static str,
    pub created_at_line: u32,
    pub created: Instant,
    /// How many times the value has been set. Never *what* it was set to.
    pub generation: u64,
    pub last_change: Option<Instant>,
    /// Observers created from this value that have not been dropped.
    pub live_observers: u64,
    /// The highest generation any observer has caught up to. The gap to
    /// `generation` is how far the furthest-behind reader could be.
    pub observed_generation: u64,
    pub alive: bool,
}

impl Entry {
    /// Generations produced but not yet observed by anyone.
    pub fn stale_by(&self) -> u64 {
        self.generation.saturating_sub(self.observed_generation)
    }
}

struct Registry {
    capacity: usize,
    entries: VecDeque<Entry>,
}

impl Registry {
    /// Inserts, evicting if full. Returns how many entries it forgot.
    ///
    /// A method so the policy is testable on a local registry rather than the
    /// process-global one, which every other test shares.
    fn push(&mut self, entry: Entry) -> u64 {
        let mut forgotten = 0;
        while self.entries.len() >= self.capacity {
            let victim = self
                .entries
                .iter()
                .position(|entry| !entry.alive)
                .unwrap_or(0);
            self.entries.remove(victim);
            forgotten += 1;
        }
        self.entries.push_back(entry);
        forgotten
    }
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static CREATED: AtomicU64 = AtomicU64::new(0);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn configured_capacity() -> usize {
    std::env::var("AWAIT_VALUES_REGISTRY_CAPACITY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|capacity| *capacity > 0)
        .unwrap_or(DEFAULT_CAPACITY)
}

fn with<R>(f: impl FnOnce(&mut Registry) -> R) -> Option<R> {
    // `try_lock`: `set` is on the hot path and must not block on a query.
    let mut guard = REGISTRY.try_lock().ok()?;
    let registry = guard.get_or_insert_with(|| Registry {
        capacity: configured_capacity(),
        entries: VecDeque::new(),
    });
    Some(f(registry))
}

pub(crate) fn record_created(location: &'static Location<'static>) -> u64 {
    CREATED.fetch_add(1, Ordering::Relaxed);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let recorded = with(|registry| {
        let forgotten = registry.push(Entry {
            id,
            created_at_file: location.file(),
            created_at_line: location.line(),
            created: Instant::now(),
            generation: 0,
            last_change: None,
            live_observers: 0,
            observed_generation: 0,
            alive: true,
        });
        DROPPED.fetch_add(forgotten, Ordering::Relaxed);
    });
    if recorded.is_none() {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
    id
}

fn update(id: u64, f: impl FnOnce(&mut Entry)) {
    let _ = with(|registry| {
        if let Some(entry) = registry.entries.iter_mut().find(|entry| entry.id == id) {
            f(entry);
        }
    });
}

pub(crate) fn record_set(id: u64) {
    update(id, |entry| {
        entry.generation += 1;
        entry.last_change = Some(Instant::now());
    });
}

pub(crate) fn record_observer_created(id: u64) {
    update(id, |entry| entry.live_observers += 1);
}

pub(crate) fn record_observer_dropped(id: u64) {
    update(id, |entry| {
        entry.live_observers = entry.live_observers.saturating_sub(1);
    });
}

/// Records that an observer has caught up to the value's current generation.
pub(crate) fn record_observed(id: u64) {
    update(id, |entry| entry.observed_generation = entry.generation);
}

pub(crate) fn record_value_dropped(id: u64) {
    update(id, |entry| entry.alive = false);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegistryStats {
    pub created: u64,
    pub dropped: u64,
    pub retained: usize,
    pub capacity: usize,
}

pub fn stats() -> RegistryStats {
    let (retained, capacity) = with(|registry| (registry.entries.len(), registry.capacity))
        .unwrap_or((0, configured_capacity()));
    RegistryStats {
        created: CREATED.load(Ordering::Relaxed),
        dropped: DROPPED.load(Ordering::Relaxed),
        retained,
        capacity,
    }
}

/// Retained entries, oldest first.
///
/// `None` when the registry was locked; report `busy` rather than an empty
/// list.
pub fn entries() -> Option<Vec<Entry>> {
    with(|registry| registry.entries.iter().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, alive: bool) -> Entry {
        Entry {
            id,
            created_at_file: "test",
            created_at_line: 1,
            created: Instant::now(),
            generation: 0,
            last_change: None,
            live_observers: 0,
            observed_generation: 0,
            alive,
        }
    }

    fn registry(capacity: usize) -> Registry {
        Registry {
            capacity,
            entries: VecDeque::new(),
        }
    }

    /// Overflow forgets a dropped value before a live one: a live one is the
    /// whole reason to look.
    #[test]
    fn overflow_forgets_dropped_before_live() {
        let mut registry = registry(2);
        registry.push(entry(1, true));
        registry.push(entry(2, false));
        assert_eq!(registry.push(entry(3, true)), 1);
        let ids: Vec<u64> = registry.entries.iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec![1, 3]);
    }

    #[test]
    fn overflow_of_all_live_drops_the_oldest_and_counts_it() {
        let mut registry = registry(2);
        registry.push(entry(1, true));
        registry.push(entry(2, true));
        assert_eq!(registry.push(entry(3, true)), 1);
        let ids: Vec<u64> = registry.entries.iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec![2, 3]);
    }

    /// Staleness is the gap between what has been produced and what has been
    /// caught up to, and it never goes negative if the counters race.
    #[test]
    fn staleness_is_the_generation_gap_and_saturates() {
        let mut entry = entry(1, true);
        entry.generation = 5;
        entry.observed_generation = 2;
        assert_eq!(entry.stale_by(), 3);
        entry.observed_generation = 7;
        assert_eq!(entry.stale_by(), 0, "a race must not underflow");
    }

    /// An observer count that went to zero and back is not negative in between.
    #[test]
    fn dropping_more_observers_than_exist_saturates() {
        let mut registry = registry(4);
        registry.push(entry(1, true));
        // Simulated directly: the public path cannot produce this, but a
        // saturating counter is cheaper than reasoning about whether it can.
        let entry = registry.entries.front_mut().unwrap();
        entry.live_observers = 0;
        entry.live_observers = entry.live_observers.saturating_sub(1);
        assert_eq!(entry.live_observers, 0);
    }
}
