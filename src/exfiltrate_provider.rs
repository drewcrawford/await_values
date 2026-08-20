// SPDX-License-Identifier: MIT OR Apache-2.0

//! Exposes the value registry through exfiltrate's `snapshot` command.

use exfiltrate::provider::{Provider, ProviderResult, Row, SnapshotRequest};
use wasm_lite_std::time::Instant;

use crate::registry;

/// Registers the `values` subsystem. Idempotent.
///
/// ```no_run
/// exfiltrate::begin();
/// await_values::exfiltrate_provider::install();
/// ```
pub fn install() {
    exfiltrate::provider::add_provider(Values);
}

struct Values;

impl Provider for Values {
    fn subsystem(&self) -> &'static str {
        "values"
    }

    fn description(&self) -> &'static str {
        "observable values: how many observers, how many changes, and how far behind they are"
    }

    fn snapshot(&self, request: &SnapshotRequest<'_>) -> ProviderResult {
        let Some(entries) = registry::entries() else {
            return ProviderResult::Busy;
        };
        let stats = registry::stats();

        let wanted: Option<u64> = match request.selector() {
            Some(selector) => match selector.parse::<u64>() {
                Ok(id) => Some(id),
                Err(_) => {
                    return ProviderResult::Unavailable(format!(
                        "--id must be a value id; got {selector:?}"
                    ));
                }
            },
            None => None,
        };

        let now = Instant::now();
        let mut rows = Vec::new();
        for entry in entries {
            if request.should_stop() {
                return ProviderResult::Partial(rows, "deadline".to_string());
            }
            if let Some(id) = wanted
                && entry.id != id
            {
                continue;
            }
            let mut row = Row::new()
                .support("id", entry.id)
                // How many times it changed -- never what it holds. See the
                // registry docs: reporting the value is a question nobody has
                // answered, and answering "how stale" does not require it.
                .support("generation", entry.generation)
                .support("observed_generation", entry.observed_generation)
                .support("stale_by", entry.stale_by())
                // Zero observers on a live value is a publisher nobody
                // subscribed to, which produces no error -- just silence.
                .support("live_observers", entry.live_observers)
                .support("alive", entry.alive)
                .support(
                    "age_ms",
                    now.duration_since(entry.created).as_millis() as u64,
                )
                .support("created_at_file", entry.created_at_file)
                .support("created_at_line", u64::from(entry.created_at_line));
            row = match entry.last_change {
                Some(at) => {
                    row.support("since_change_ms", now.duration_since(at).as_millis() as u64)
                }
                // Distinguishable from "unchanged for a long time": it has
                // never been set at all since construction.
                None => row.support("never_changed", true),
            };
            rows.push(row);
        }

        if stats.dropped > 0 {
            return ProviderResult::Partial(
                rows,
                format!(
                    "the registry has dropped {} value(s) to stay within its capacity of {}; \
                     raise AWAIT_VALUES_REGISTRY_CAPACITY to keep more",
                    stats.dropped, stats.capacity
                ),
            );
        }
        ProviderResult::Rows(rows)
    }
}
