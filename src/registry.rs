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

use std::collections::{HashMap, VecDeque};
use std::panic::Location;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use wasm_lite_std::time::Instant;

/// How many entries the registry keeps when `AWAIT_VALUES_REGISTRY_CAPACITY` is unset.
pub const DEFAULT_CAPACITY: usize = 1024;

/// One recorded value.
///
/// Note what is absent: nothing here is the value itself. Generation and
/// timing answer staleness, and cannot leak application data.
#[derive(Clone, Debug)]
pub struct Entry {
    /// A locally minted id, always distinct. What `--id` selects.
    pub id: u64,
    /// Source file the value was created in, captured with `#[track_caller]`.
    pub created_at_file: &'static str,
    /// Line within that file.
    pub created_at_line: u32,
    /// When it was created.
    pub created: Instant,
    /// How many times the value has been set. Never *what* it was set to.
    pub generation: u64,
    /// When it was last set, and `None` if it never has been. The two are
    /// distinguished so "never set since construction" cannot be misread as
    /// "unchanged for a while".
    pub last_change: Option<Instant>,
    /// Observers created from this value that have not been dropped.
    pub live_observers: u64,
    /// The oldest generation held by any live observer. The gap to
    /// `generation` is how far the furthest-behind reader is.
    ///
    /// When there are no live observers, this retains the last observed
    /// generation; consult [`live_observers`](Self::live_observers) first.
    pub observed_generation: u64,
    /// Whether the value still exists. A dropped value with live observers is
    /// a publisher that went away underneath its readers.
    pub alive: bool,
}

impl Entry {
    /// Generations the furthest-behind live observer has not yet seen.
    ///
    /// When [`live_observers`](Self::live_observers) is zero, this is historical
    /// rather than the staleness of a current subscriber.
    pub fn stale_by(&self) -> u64 {
        self.generation.saturating_sub(self.observed_generation)
    }
}

struct Registry {
    capacity: usize,
    entries: VecDeque<Entry>,
    /// Last generation observed by each `(value id, observer id)` pair.
    observer_generations: HashMap<u64, HashMap<u64, u64>>,
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
            let removed = self.entries.remove(victim).expect("victim exists");
            self.observer_generations.remove(&removed.id);
            forgotten += 1;
        }
        self.entries.push_back(entry);
        forgotten
    }

    fn recompute_observed_generation(&mut self, id: u64) {
        let oldest = self
            .observer_generations
            .get(&id)
            .and_then(|observers| observers.values().min())
            .copied();
        if let Some(oldest) = oldest
            && let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id)
        {
            entry.observed_generation = oldest;
        }
    }

    fn observer_created(&mut self, id: u64, observer_id: u64, observed_generation: u64) {
        if !self.entries.iter().any(|entry| entry.id == id) {
            return;
        }
        let replaced = self
            .observer_generations
            .entry(id)
            .or_default()
            .insert(observer_id, observed_generation);
        if replaced.is_none()
            && let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id)
        {
            entry.live_observers += 1;
        }
        self.recompute_observed_generation(id);
    }

    fn observer_observed(&mut self, id: u64, observer_id: u64) -> Option<u64> {
        let generation = self.entries.iter().find(|entry| entry.id == id)?.generation;
        let replaced = self
            .observer_generations
            .entry(id)
            .or_default()
            .insert(observer_id, generation);
        // If observer creation lost the registry's non-blocking lock race, a
        // later successful observation repairs both pieces of bookkeeping.
        if replaced.is_none()
            && let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id)
        {
            entry.live_observers += 1;
        }
        self.recompute_observed_generation(id);
        Some(generation)
    }

    fn observer_dropped(&mut self, id: u64, observer_id: u64) {
        let (removed, now_empty) = self
            .observer_generations
            .get_mut(&id)
            .map(|observers| {
                let removed = observers.remove(&observer_id).is_some();
                (removed, observers.is_empty())
            })
            .unwrap_or((false, false));
        if !removed {
            return;
        }
        if now_empty {
            self.observer_generations.remove(&id);
        }
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.live_observers = entry.live_observers.saturating_sub(1);
        }
        self.recompute_observed_generation(id);
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
        observer_generations: HashMap::new(),
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

pub(crate) fn record_observer_created(id: u64, observer_id: u64, observed_generation: u64) {
    let _ = with(|registry| {
        registry.observer_created(id, observer_id, observed_generation);
    });
}

pub(crate) fn record_observer_dropped(id: u64, observer_id: u64) {
    let _ = with(|registry| {
        registry.observer_dropped(id, observer_id);
    });
}

/// Records that an observer has caught up to the value's current generation.
pub(crate) fn record_observed(id: u64, observer_id: u64) -> Option<u64> {
    with(|registry| registry.observer_observed(id, observer_id)).flatten()
}

pub(crate) fn record_value_dropped(id: u64) {
    update(id, |entry| entry.alive = false);
}

/// Counts covering the whole registry, including what it has forgotten.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegistryStats {
    /// Total ever recorded, including entries since forgotten.
    pub created: u64,
    /// How many were forgotten to stay within `capacity`. Non-zero means the
    /// snapshot is not the whole history -- which is the difference between
    /// "nothing is stuck" and "I lost it".
    pub dropped: u64,
    /// How many are held right now.
    pub retained: usize,
    /// The bound currently in force.
    pub capacity: usize,
}

/// Reads the registry counters without copying out the entries.
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
            observer_generations: HashMap::new(),
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

    /// A fast observer cannot hide another live observer that is behind.
    #[test]
    fn observed_generation_tracks_the_oldest_live_observer() {
        let mut registry = registry(4);
        let mut value = entry(1, true);
        value.generation = 5;
        registry.push(value);
        registry.observer_created(1, 10, 2);
        registry.observer_created(1, 11, 2);

        assert_eq!(registry.observer_observed(1, 10), Some(5));
        assert_eq!(registry.entries[0].observed_generation, 2);
        assert_eq!(registry.entries[0].stale_by(), 3);

        assert_eq!(registry.observer_observed(1, 11), Some(5));
        assert_eq!(registry.entries[0].observed_generation, 5);
        assert_eq!(registry.entries[0].stale_by(), 0);
    }

    /// A missed/duplicate drop must not decrement some other observer's count.
    #[test]
    fn dropping_an_unknown_observer_does_not_change_the_live_count() {
        let mut registry = registry(4);
        registry.push(entry(1, true));
        registry.observer_created(1, 10, 0);
        registry.observer_dropped(1, 999);
        assert_eq!(registry.entries[0].live_observers, 1);
    }
}
