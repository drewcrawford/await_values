// SPDX-License-Identifier: MIT OR Apache-2.0

//! The value registry, end to end through exfiltrate's `snapshot`.
//!
//! Its own binary, one sequential test: the registry is process-global, so
//! "the newest entry" is otherwise whichever parallel case got there last.

use await_values::Value;
use exfiltrate_internal::command::{Command, Response};
use exfiltrate_internal::snapshot::{SnapshotRow, SnapshotSet, SnapshotValue};

fn ask(args: &[&str]) -> SnapshotSet {
    await_values::exfiltrate_provider::install();
    let response = exfiltrate::provider::Snapshot
        .execute(args.iter().map(|arg| arg.to_string()).collect())
        .expect("snapshot should not fail");
    match response {
        Response::Bytes(bytes) => rmp_serde::from_slice(&bytes).expect("decode"),
        other => panic!("expected a structured response, got {other:?}"),
    }
}

fn newest() -> SnapshotRow {
    let set = ask(&["--subsystem", "values"]);
    set.snapshots[0]
        .rows
        .last()
        .cloned()
        .expect("at least one value")
}

fn row_for(id: u64) -> SnapshotRow {
    let set = ask(&["--subsystem", "values", "--id", &id.to_string()]);
    assert_eq!(set.snapshots[0].returned, 1, "{:?}", set.snapshots[0]);
    set.snapshots[0].rows[0].clone()
}

fn u64_of(row: &SnapshotRow, name: &str) -> u64 {
    match row.get(name).unwrap().value {
        Some(SnapshotValue::U64(value)) => value,
        ref other => panic!("{name} was {other:?}"),
    }
}

fn bool_of(row: &SnapshotRow, name: &str) -> bool {
    match row.get(name).unwrap().value {
        Some(SnapshotValue::Bool(value)) => value,
        ref other => panic!("{name} was {other:?}"),
    }
}

#[wasm_lite::wasm_lite_test]
fn the_registry_reports_observers_and_staleness_but_never_values() {
    let value = Value::new(1_u32);
    let row = newest();
    let id = u64_of(&row, "id");

    // A brand new value has changed nothing and has nobody listening. Zero
    // observers on a live value is a publisher nobody subscribed to, which
    // produces no error -- just silence.
    assert_eq!(u64_of(&row, "generation"), 0);
    assert_eq!(u64_of(&row, "live_observers"), 0);
    assert!(bool_of(&row, "alive"));
    assert!(
        bool_of(&row, "never_changed"),
        "distinguishable from unchanged-for-a-long-time"
    );

    // The value itself is never reported, at any point. That is what let this
    // ship without settling whether current values may be exposed.
    for field in ["value", "current", "contents"] {
        assert!(
            row.get(field).is_none(),
            "the registry must not carry the value itself: {field}"
        );
    }

    let mut observer = value.observe();
    assert_eq!(u64_of(&row_for(id), "live_observers"), 1);

    // Setting advances the generation and makes the observer stale.
    value.set(2);
    value.set(3);
    let row = row_for(id);
    assert_eq!(u64_of(&row, "generation"), 2);
    assert_eq!(u64_of(&row, "stale_by"), 2, "nobody has caught up yet");
    assert!(row.get("never_changed").is_none());
    assert!(row.get("since_change_ms").is_some());

    // Observing catches up.
    assert_eq!(observer.current_value(), Some(3));
    let row = row_for(id);
    assert_eq!(u64_of(&row, "observed_generation"), 2);
    assert_eq!(u64_of(&row, "stale_by"), 0);

    // Dropping the observer is visible, and so is dropping the value.
    drop(observer);
    assert_eq!(u64_of(&row_for(id), "live_observers"), 0);
    drop(value);
    assert!(!bool_of(&row_for(id), "alive"));
}
