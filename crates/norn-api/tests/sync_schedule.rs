//! TIMER-ALARM-1: a cell with scheduled connections tells its host when the
//! next sync falls due, so an idle cell (no request arriving) still syncs: a
//! connection never synced is due at once, a running one only once its lease
//! lapses, and an unscheduled one never.
#![allow(clippy::expect_used, reason = "test assertions")]

use std::collections::BTreeMap;

use norn_api::sources::{RUN_LEASE_MS, Sources};
use serde_json::json;

const NOW: u64 = 1_800_000_000_000;

fn connection(id: &str, every: u32) -> serde_json::Value {
    json!({ "id": id, "system": "erpnext", "label": id, "connector": "c", "mapping": "m",
            "instance": "i", "baseUrl": "https://erp.example.test", "everyMinutes": every })
}

#[test]
fn an_idle_cell_knows_when_its_next_sync_is_due() {
    let config = json!([connection("erpnext.rest", 15)]).to_string();
    let mut sources = Sources::new(&config, BTreeMap::new()).expect("configured");
    // Never synced: due now, and the schedule claims it.
    assert_eq!(sources.next_due(NOW), Some(NOW));
    assert_eq!(sources.due(NOW).as_deref(), Some("erpnext.rest"));
    // Claimed and running: due again only when its lease lapses.
    assert_eq!(sources.next_due(NOW + 1), Some(NOW + RUN_LEASE_MS));
    assert_eq!(sources.due(NOW + 1), None);

    let off = json!([connection("erpnext.rest", 0)]).to_string();
    let off = Sources::new(&off, BTreeMap::new()).expect("configured");
    assert_eq!(off.next_due(NOW), None);
}
