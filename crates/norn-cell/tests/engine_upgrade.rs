//! ENGINE-UPGRADE-1 (#1494): a cell written by an older engine opens on this
//! one through the ordered migrations with its ledger and receipts intact,
//! and a cell this engine cannot serve refuses before anything touches it.
//!
//! `fixtures/engine-storage-v<N>.json` is a cell written by the last storage
//! version `N` engine (`tests/engine_fixture_record.rs` records it).
#![cfg(feature = "native")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

#[path = "support/engine_fixture.rs"]
mod engine_fixture;

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
use norn_cell::engine_release::{
    self, Admission, EngineStamp, EngineVersion, STAMP_FORMAT, STAMP_KEY, STORAGE_VERSION,
    StoredVersion,
};
use norn_cell::export::Exporter;
use norn_cell::test_door::TestCommit as _;
use norn_cell::{
    CellError, CellStore, CommitOutcome, InstalledDefinitionInput, JournalMode, LedgerHead, Model,
    RestoreError, StoreOptions,
};
use serde_json::Value;

const STORAGE_V1: &str = include_str!("fixtures/engine-storage-v1.json");
const STORAGE_V2: &str = include_str!("fixtures/engine-storage-v2.json");
const STORAGE_V3: &str = include_str!("fixtures/engine-storage-v3.json");
const STORAGE_V4: &str = include_str!("fixtures/engine-storage-v4.json");
const STORAGE_V5: &str = include_str!("fixtures/engine-storage-v5.json");
const STORAGE_V6: &str = include_str!("fixtures/engine-storage-v6.json");
const STORAGE_V7: &str = include_str!("fixtures/engine-storage-v7.json");
const STORAGE_V8: &str = include_str!("fixtures/engine-storage-v8.json");

/// What storage versions 8 through 11 change in an older cell's image.
fn apply_v10_upgrade(image: &mut Value) {
    apply_v8_sparse_markings_upgrade(image);
    add_export_context_table(image);
    add_outbox_queue(image);
    add_outbox_ledger(image);
}

/// Storage version 11 (D1) adds `_norn_outbox_record`, backfilled with one
/// validated epoch per stored `_norn_outbox` row.
fn add_outbox_ledger(image: &mut Value) {
    image["schema"]
        .as_array_mut()
        .expect("schema")
        .push(serde_json::json!({
            "type": "table",
            "name": "_norn_outbox_record",
            "table": "_norn_outbox_record",
            "sql": "CREATE TABLE \"_norn_outbox_record\" (\n  \"command_id\" TEXT NOT NULL,\n  \"epoch\" INTEGER NOT NULL,\n  \"seq\" INTEGER NOT NULL,\n  \"generation\" INTEGER NOT NULL,\n  \"record\" TEXT NOT NULL,\n  PRIMARY KEY (\"command_id\", \"epoch\", \"seq\")\n)",
        }));
    let cell_id = image["tables"]["_norn_meta"]["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row[1] == "binding:cell_id")
                .and_then(|row| row[2].as_str())
        })
        .map(|text| norn_cell::cell_identity::CellId::try_from(text.to_owned()).expect("cell id"));
    let outbox = &image["tables"]["_norn_outbox"];
    let columns: Vec<&str> = outbox["columns"]
        .as_array()
        .expect("outbox columns")
        .iter()
        .map(|c| c.as_str().expect("column name"))
        .collect();
    let claims = image["tables"]["_norn_perform_claim"]["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut record_rows = Vec::new();
    let mut rowid = 1_i64;
    for row in outbox["rows"].as_array().expect("outbox rows") {
        backfill_image_outbox_row(
            cell_id.as_ref(),
            &columns,
            row,
            &claims,
            &mut rowid,
            &mut record_rows,
        );
    }
    image["tables"].as_object_mut().expect("tables").insert(
        "_norn_outbox_record".to_owned(),
        serde_json::json!({
            "columns": ["rowid", "command_id", "epoch", "seq", "generation", "record"],
            "rows": record_rows,
        }),
    );
}

fn backfill_image_outbox_row(
    cell_id: Option<&norn_cell::cell_identity::CellId>,
    columns: &[&str],
    row: &Value,
    claims: &[Value],
    rowid: &mut i64,
    record_rows: &mut Vec<Value>,
) {
    use norn_kernel::world::outbox::Queue;
    use norn_kernel::world::outbox_ledger::{
        DispatchProof, OutboxRecord, OutboxRecordKind, OutcomeEvidence,
    };

    let col = |name: &str| {
        columns
            .iter()
            .position(|column| *column == name)
            .unwrap_or_else(|| panic!("outbox missing column {name}"))
    };
    let command_id = row[col("command_id")].as_str().expect("command_id");
    let generation = row[col("generation")].as_u64().expect("generation");
    let commit_seq = row[col("commit_seq")].as_u64().expect("commit_seq");
    let command_rv: norn_ir::RuntimeValue =
        serde_json::from_str(row[col("command")].as_str().expect("command")).expect("command rv");
    let queue = Queue::parse(row[col("queue")].as_str().expect("queue")).expect("queue");
    let op = norn_cell::outbox_ledger::operation_key_for_cell(cell_id, command_id).expect("op key");
    let mut records = vec![OutboxRecord {
        operation: op,
        generation,
        kind: OutboxRecordKind::Requested {
            queue,
            payload_digest: norn_cell::outbox_ledger::payload_digest(&command_rv)
                .expect("payload digest"),
            dedup_contract: norn_cell::outbox_ledger::dedup_contract(&command_rv)
                .expect("dedup contract"),
            commit_seq,
        },
    }];
    let dispatch = claims
        .iter()
        .find(|claim| claim[1] == command_id && claim[2] == generation)
        .map(|claim| {
            let claim_commit_seq = claim[3].as_u64().expect("claim commit_seq");
            records.push(OutboxRecord {
                operation: op,
                generation,
                kind: OutboxRecordKind::Dispatched { claim_commit_seq },
            });
            DispatchProof {
                operation: op,
                generation,
                claim_commit_seq,
            }
        });
    if let Some(result_text) = row[col("result")].as_str() {
        let outcome: norn_ir::RuntimeValue = serde_json::from_str(result_text).expect("result rv");
        let (evidence, outcome_seq) = if let Some(proof) = dispatch {
            (
                OutcomeEvidence::DispatchProof { proof },
                proof.claim_commit_seq,
            )
        } else {
            (OutcomeEvidence::Resolved, commit_seq)
        };
        records.push(OutboxRecord {
            operation: op,
            generation,
            kind: OutboxRecordKind::Outcome {
                evidence,
                ok: norn_cell::outbox_ledger::outcome_is_ok(&outcome),
                outcome_digest: norn_cell::outbox_ledger::outcome_digest(&outcome)
                    .expect("outcome digest"),
                commit_seq: outcome_seq,
            },
        });
    }
    if let Some(consumed_commit_seq) = row[col("consumed_commit_seq")].as_u64() {
        records.push(OutboxRecord {
            operation: op,
            generation,
            kind: OutboxRecordKind::Consumed {
                commit_seq: consumed_commit_seq,
            },
        });
    }
    for (idx, record) in records.iter().enumerate() {
        let seq = i64::try_from(idx + 1).expect("seq");
        let json = String::from_utf8(
            norn_ir::canonical::canonical_json_of(record).expect("canonical record"),
        )
        .expect("utf8");
        record_rows.push(serde_json::json!([
            *rowid, command_id, 0_i64, seq, generation, json
        ]));
        *rowid += 1;
    }
}

/// Storage version 10 (D03, #1699) adds `_norn_outbox.queue`, backfilled
/// from each stored command's classification (a `norn.decision@1` or
/// `norn.action@1` perform parks), and the partial index `_norn_outbox/queue`.
fn add_outbox_queue(image: &mut Value) {
    let schema = image["schema"].as_array_mut().expect("schema");
    for entry in schema.iter_mut() {
        if entry["name"] == "_norn_outbox"
            && let Some(sql) = entry["sql"].as_str()
        {
            entry["sql"] = Value::String(sql.replacen(
                "\"consumed_commit_seq\" INTEGER,",
                "\"consumed_commit_seq\" INTEGER, \"queue\" TEXT NOT NULL DEFAULT 'fifo' CHECK (\"queue\" IN ('fifo', 'parked')),",
                1,
            ));
        }
    }
    schema.push(serde_json::json!({
        "type": "index",
        "name": "_norn_outbox/queue",
        "table": "_norn_outbox",
        "sql": "CREATE INDEX \"_norn_outbox/queue\" ON \"_norn_outbox\" (\"queue\", \"position\") WHERE \"consumed_commit_seq\" IS NULL",
    }));
    let outbox = &mut image["tables"]["_norn_outbox"];
    let command_at = outbox["columns"]
        .as_array()
        .and_then(|columns| columns.iter().position(|column| column == "command"))
        .expect("outbox command column");
    outbox["columns"]
        .as_array_mut()
        .expect("outbox columns")
        .push(Value::String("queue".to_owned()));
    for row in outbox["rows"].as_array_mut().expect("outbox rows") {
        let command: Value =
            serde_json::from_str(row[command_at].as_str().expect("command text")).expect("command");
        let payload = &command["payload"];
        let parked = payload["kind"] == "perform"
            && (payload["function"] == "norn.decision" || payload["function"] == "norn.action")
            && payload["functionVersion"] == "1";
        let queue = if parked { "parked" } else { "fifo" };
        row.as_array_mut()
            .expect("outbox row")
            .push(Value::String(queue.to_owned()));
    }
}

/// Storage version 9 (#1533 Part 2, EXPORT-RESIDUE-1) adds the empty
/// content-addressed `_norn_export_context` table; no journal row is rewritten.
fn add_export_context_table(image: &mut Value) {
    image["schema"]
        .as_array_mut()
        .expect("schema")
        .push(serde_json::json!({
            "type": "table",
            "name": "_norn_export_context",
            "table": "_norn_export_context",
            "sql": "CREATE TABLE \"_norn_export_context\" (\n  \"hash\" TEXT NOT NULL PRIMARY KEY,\n  \"store_format\" TEXT NOT NULL,\n  \"ddl_hash\" TEXT NOT NULL,\n  \"model_hash\" TEXT NOT NULL,\n  \"ddl_sql\" TEXT NOT NULL,\n  \"bindings\" TEXT NOT NULL\n)",
        }));
    image["tables"].as_object_mut().expect("tables").insert(
        "_norn_export_context".to_owned(),
        serde_json::json!({
            "columns": ["rowid", "hash", "store_format", "ddl_hash", "model_hash", "ddl_sql", "bindings"],
            "rows": [],
        }),
    );
}

/// Storage version 8 (#1533 Part 1) adds `_norn_marking_snapshot.nonempty_slots`
/// (backfilled from non-empty rows) and drops the strictly redundant
/// `_norn_link_version/source` and `_norn_link_version/target` prefix indexes
/// (exact prefixes of `source_order` and `target_order`). Existing stored
/// `'[]'` marking rows are retained verbatim and remain readable as empty.
fn apply_v8_sparse_markings_upgrade(image: &mut Value) {
    if let Some(schema) = image["schema"].as_array_mut() {
        schema.retain(|entry| {
            entry["name"] != "_norn_link_version/source"
                && entry["name"] != "_norn_link_version/target"
        });
        for entry in schema.iter_mut() {
            if entry["name"] == "_norn_marking_snapshot"
                && let Some(sql) = entry["sql"].as_str()
            {
                entry["sql"] = Value::String(sql.replacen(
                    "\"is_current\" INTEGER NOT NULL,",
                    "\"is_current\" INTEGER NOT NULL, \"nonempty_slots\" INTEGER NOT NULL DEFAULT 0,",
                    1,
                ));
            }
        }
    }
    if let Some(snapshot) = image["tables"]["_norn_marking_snapshot"].as_object_mut() {
        if let Some(columns) = snapshot.get_mut("columns").and_then(Value::as_array_mut) {
            columns.push(Value::String("nonempty_slots".to_owned()));
        }
        if let Some(rows) = snapshot.get_mut("rows").and_then(Value::as_array_mut) {
            for row in rows.iter_mut() {
                if let Some(cells) = row.as_array_mut() {
                    cells.push(Value::from(0_i64));
                }
            }
        }
    }
}

/// Storage version 7 (#1424 U1) adds the transactional automation trigger
/// outbox, the lossless outbox continuation table, the indexed trigger
/// projection, and the server-bound retained authority table, all initially
/// empty on older cells (purely additive DDL).
fn add_automation_tables(image: &mut Value) {
    add_automation_schema_objects(image);
    add_empty_automation_rows(image);
}

fn add_automation_schema_objects(image: &mut Value) {
    let objects = [
        (
            "table",
            "_norn_automation_outbox",
            "_norn_automation_outbox",
            "CREATE TABLE \"_norn_automation_outbox\" (\n  \"world\" TEXT NOT NULL,\n  \"automation_id\" TEXT NOT NULL,\n  \"revision\" INTEGER NOT NULL,\n  \"trigger_commit_seq\" INTEGER NOT NULL,\n  \"object_type\" TEXT NOT NULL,\n  \"object_id\" TEXT NOT NULL,\n  \"object_version\" INTEGER NOT NULL,\n  \"trigger_kind\" TEXT NOT NULL,\n  \"action_id\" TEXT,\n  \"before_values\" TEXT,\n  \"after_values\" TEXT,\n  \"recorded_at_ms\" INTEGER NOT NULL,\n  \"consumed_commit_seq\" INTEGER,\n  \"voided_commit_seq\" INTEGER,\n  PRIMARY KEY (\"world\", \"automation_id\", \"revision\", \"trigger_commit_seq\", \"object_type\", \"object_id\", \"object_version\")\n)",
        ),
        (
            "index",
            "_norn_automation_outbox/pending",
            "_norn_automation_outbox",
            "CREATE INDEX \"_norn_automation_outbox/pending\" ON \"_norn_automation_outbox\" (\"world\", \"trigger_commit_seq\", \"automation_id\", \"revision\", \"object_type\", \"object_id\", \"object_version\") WHERE \"consumed_commit_seq\" IS NULL AND \"voided_commit_seq\" IS NULL",
        ),
        (
            "index",
            "_norn_automation_outbox/commit",
            "_norn_automation_outbox",
            "CREATE INDEX \"_norn_automation_outbox/commit\" ON \"_norn_automation_outbox\" (\"world\", \"trigger_commit_seq\", \"automation_id\", \"revision\", \"object_type\", \"object_id\", \"object_version\")",
        ),
        (
            "index",
            "_norn_automation_outbox/automation",
            "_norn_automation_outbox",
            "CREATE INDEX \"_norn_automation_outbox/automation\" ON \"_norn_automation_outbox\" (\"automation_id\", \"revision\") WHERE \"consumed_commit_seq\" IS NULL AND \"voided_commit_seq\" IS NULL",
        ),
        (
            "table",
            "_norn_automation_outbox_continuation",
            "_norn_automation_outbox_continuation",
            "CREATE TABLE \"_norn_automation_outbox_continuation\" (\n  \"trigger_commit_seq\" INTEGER NOT NULL,\n  \"world\" TEXT NOT NULL,\n  \"cursor\" TEXT NOT NULL,\n  \"storage_version\" INTEGER NOT NULL,\n  \"engine_version\" TEXT NOT NULL,\n  \"definitions_version\" INTEGER NOT NULL,\n  \"matcher_version\" INTEGER NOT NULL,\n  \"recorded_at_ms\" INTEGER NOT NULL,\n  \"completed_commit_seq\" INTEGER,\n  PRIMARY KEY (\"trigger_commit_seq\")\n)",
        ),
        (
            "index",
            "_norn_automation_outbox_continuation/pending",
            "_norn_automation_outbox_continuation",
            "CREATE INDEX \"_norn_automation_outbox_continuation/pending\" ON \"_norn_automation_outbox_continuation\" (\"world\", \"trigger_commit_seq\") WHERE \"completed_commit_seq\" IS NULL",
        ),
        (
            "table",
            "_norn_retained_automation_trigger",
            "_norn_retained_automation_trigger",
            "CREATE TABLE \"_norn_retained_automation_trigger\" (\n  \"automation_id\" TEXT NOT NULL,\n  \"revision\" INTEGER NOT NULL,\n  \"object_type\" TEXT NOT NULL,\n  \"root_type\" TEXT NOT NULL,\n  \"trigger_kind\" TEXT NOT NULL,\n  \"action_id\" TEXT,\n  \"lifecycle_property\" TEXT,\n  \"lifecycle_state\" TEXT,\n  PRIMARY KEY (\"automation_id\")\n)",
        ),
        (
            "index",
            "_norn_retained_automation_trigger/type",
            "_norn_retained_automation_trigger",
            "CREATE INDEX \"_norn_retained_automation_trigger/type\" ON \"_norn_retained_automation_trigger\" (\"object_type\", \"automation_id\") WHERE \"trigger_kind\" != 'event:action'",
        ),
        (
            "index",
            "_norn_retained_automation_trigger/root",
            "_norn_retained_automation_trigger",
            "CREATE INDEX \"_norn_retained_automation_trigger/root\" ON \"_norn_retained_automation_trigger\" (\"root_type\", \"trigger_kind\")",
        ),
        (
            "index",
            "_norn_retained_automation_trigger/action",
            "_norn_retained_automation_trigger",
            "CREATE INDEX \"_norn_retained_automation_trigger/action\" ON \"_norn_retained_automation_trigger\" (\"action_id\", \"automation_id\") WHERE \"trigger_kind\" = 'event:action'",
        ),
        (
            "table",
            "_norn_retained_authority",
            "_norn_retained_authority",
            "CREATE TABLE \"_norn_retained_authority\" (\n  \"id\" TEXT NOT NULL,\n  \"revision\" INTEGER NOT NULL,\n  \"authority\" TEXT NOT NULL,\n  PRIMARY KEY (\"id\", \"revision\")\n)",
        ),
    ];
    for (kind, name, table, sql) in objects {
        image["schema"]
            .as_array_mut()
            .expect("schema")
            .push(serde_json::json!({ "type": kind, "name": name, "table": table, "sql": sql }));
    }
}

fn add_empty_automation_rows(image: &mut Value) {
    let tables = image["tables"].as_object_mut().expect("tables");
    tables.insert(
        "_norn_automation_outbox".to_owned(),
        serde_json::json!({
            "columns": [
                "rowid",
                "world",
                "automation_id",
                "revision",
                "trigger_commit_seq",
                "object_type",
                "object_id",
                "object_version",
                "trigger_kind",
                "action_id",
                "before_values",
                "after_values",
                "recorded_at_ms",
                "consumed_commit_seq",
                "voided_commit_seq"
            ],
            "rows": [],
        }),
    );
    tables.insert(
        "_norn_automation_outbox_continuation".to_owned(),
        serde_json::json!({
            "columns": [
                "rowid",
                "trigger_commit_seq",
                "world",
                "cursor",
                "storage_version",
                "engine_version",
                "definitions_version",
                "matcher_version",
                "recorded_at_ms",
                "completed_commit_seq"
            ],
            "rows": [],
        }),
    );
    tables.insert(
        "_norn_retained_automation_trigger".to_owned(),
        serde_json::json!({
            "columns": [
                "rowid",
                "automation_id",
                "revision",
                "object_type",
                "root_type",
                "trigger_kind",
                "action_id",
                "lifecycle_property",
                "lifecycle_state"
            ],
            "rows": [],
        }),
    );
    tables.insert(
        "_norn_retained_authority".to_owned(),
        serde_json::json!({
            "columns": ["rowid", "id", "revision", "authority"],
            "rows": [],
        }),
    );
}

/// RECEIPT-STORE-1 adds the retained Function receipt table, empty on every
/// older cell. Older engines ignore it, so it needs no storage version.
fn add_function_receipt_table(image: &mut Value) {
    image["schema"]
        .as_array_mut()
        .expect("schema")
        .push(serde_json::json!({
            "type": "table",
            "name": "_norn_function_receipt",
            "table": "_norn_function_receipt",
            "sql": "CREATE TABLE \"_norn_function_receipt\" (\n  \"reference\" TEXT NOT NULL PRIMARY KEY,\n  \"scope\" TEXT NOT NULL,\n  \"world\" TEXT NOT NULL,\n  \"command_id\" TEXT NOT NULL,\n  \"generation\" INTEGER NOT NULL,\n  \"retained_at\" INTEGER NOT NULL,\n  \"body\" TEXT NOT NULL\n)",
        }));
    image["tables"].as_object_mut().expect("tables").insert(
        "_norn_function_receipt".to_owned(),
        serde_json::json!({
            "columns": [
                "rowid",
                "reference",
                "scope",
                "world",
                "command_id",
                "generation",
                "retained_at",
                "body"
            ],
            "rows": [],
        }),
    );
    add_source_cursor_tables(image);
}

/// SOURCE-CAPTURE-1 adds the durable source lease and cursor tables, empty on
/// every older cell. Older engines ignore them, so they need no storage version.
fn add_source_cursor_tables(image: &mut Value) {
    let schema = image["schema"].as_array_mut().expect("schema");
    add_source_cursor_schema(schema);
    add_source_cursor_indexes(schema);
    let tables = image["tables"].as_object_mut().expect("tables");
    for (name, columns) in [
        (
            "_norn_source_lease",
            &[
                "rowid",
                "connection_id",
                "lease_id",
                "generation",
                "acquired_at_ms",
                "expires_at_ms",
            ][..],
        ),
        (
            "_norn_source_cursor",
            &[
                "rowid",
                "connection_id",
                "dataset",
                "cursor_json",
                "generation",
                "page_digest",
                "pages_json",
                "records_json",
                "updated_at_ms",
            ][..],
        ),
        (
            "_norn_source_pending",
            &[
                "rowid",
                "connection_id",
                "dataset",
                "page_digest",
                "pages_json",
                "records_json",
                "captured_at_ms",
                "processed",
            ][..],
        ),
        (
            "_norn_source_binding",
            &[
                "rowid",
                "connection_id",
                "mapping_id",
                "generation",
                "updated_at_ms",
            ][..],
        ),
        (
            "_norn_source_evidence",
            &[
                "rowid",
                "observation_id",
                "connection_id",
                "captured_commit_seq",
            ][..],
        ),
    ] {
        tables.insert(
            name.to_owned(),
            serde_json::json!({ "columns": columns, "rows": [] }),
        );
    }
}

fn add_source_cursor_schema(schema: &mut Vec<Value>) {
    for (name, sql) in [
        (
            "_norn_source_lease",
            "CREATE TABLE \"_norn_source_lease\" (\n  \"connection_id\" TEXT NOT NULL PRIMARY KEY,\n  \"lease_id\" TEXT NOT NULL,\n  \"generation\" INTEGER NOT NULL,\n  \"acquired_at_ms\" INTEGER NOT NULL,\n  \"expires_at_ms\" INTEGER NOT NULL\n)",
        ),
        (
            "_norn_source_cursor",
            "CREATE TABLE \"_norn_source_cursor\" (\n  \"connection_id\" TEXT NOT NULL,\n  \"dataset\" TEXT NOT NULL,\n  \"cursor_json\" TEXT NOT NULL,\n  \"generation\" INTEGER NOT NULL,\n  \"page_digest\" TEXT NOT NULL,\n  \"pages_json\" TEXT NOT NULL,\n  \"records_json\" TEXT NOT NULL,\n  \"updated_at_ms\" INTEGER NOT NULL,\n  PRIMARY KEY (\"connection_id\", \"dataset\")\n)",
        ),
        (
            "_norn_source_pending",
            "CREATE TABLE \"_norn_source_pending\" (\n  \"connection_id\" TEXT NOT NULL,\n  \"dataset\" TEXT NOT NULL,\n  \"page_digest\" TEXT NOT NULL,\n  \"pages_json\" TEXT NOT NULL,\n  \"records_json\" TEXT NOT NULL,\n  \"captured_at_ms\" INTEGER NOT NULL,\n  \"processed\" INTEGER NOT NULL DEFAULT 0,\n  PRIMARY KEY (\"connection_id\", \"dataset\", \"page_digest\")\n)",
        ),
        (
            "_norn_source_binding",
            "CREATE TABLE \"_norn_source_binding\" (\n  \"connection_id\" TEXT NOT NULL PRIMARY KEY,\n  \"mapping_id\" TEXT NOT NULL,\n  \"generation\" INTEGER NOT NULL,\n  \"updated_at_ms\" INTEGER NOT NULL\n)",
        ),
        (
            "_norn_source_evidence",
            "CREATE TABLE \"_norn_source_evidence\" (\n  \"observation_id\" TEXT NOT NULL,\n  \"connection_id\" TEXT NOT NULL,\n  \"captured_commit_seq\" INTEGER NOT NULL,\n  PRIMARY KEY (\"observation_id\", \"connection_id\")\n)",
        ),
    ] {
        schema.push(serde_json::json!({
            "type": "table",
            "name": name,
            "table": name,
            "sql": sql,
        }));
    }
}

fn add_source_cursor_indexes(schema: &mut Vec<Value>) {
    for (table, name, sql) in [
        (
            "_norn_source_evidence",
            "_norn_source_evidence/captured",
            "CREATE INDEX \"_norn_source_evidence/captured\" ON \"_norn_source_evidence\" (\"captured_commit_seq\", \"observation_id\", \"connection_id\")",
        ),
        (
            "_norn_inbox",
            "_norn_inbox/conflict_accepted",
            "CREATE INDEX \"_norn_inbox/conflict_accepted\" ON \"_norn_inbox\" (json_extract(\"observed\", '$.input.acceptedBundleId'), \"observation_id\") WHERE substr(\"observation_id\", 1, 17) = 'mapping-conflict:'",
        ),
        (
            "_norn_inbox",
            "_norn_inbox/bundle_mapping_consumed",
            "CREATE INDEX \"_norn_inbox/bundle_mapping_consumed\" ON \"_norn_inbox\" (json_extract(\"observed\", '$.input.input.mapping'), \"consumed_commit_seq\") WHERE \"observation_id\" LIKE 'mapping-bundle:%'",
        ),
        (
            "_norn_fact_values",
            "_norn_fact_values/root_first_version",
            "CREATE INDEX \"_norn_fact_values/root_first_version\" ON \"_norn_fact_values\" (\"world\", \"root_type\", \"commit_seq\", \"id\") WHERE \"version\" = 1",
        ),
        (
            "_norn_fact_values",
            "_norn_fact_values/connection_commit",
            "CREATE INDEX \"_norn_fact_values/connection_commit\" ON \"_norn_fact_values\" (\"world\", \"id\", \"commit_seq\", \"version\") WHERE \"root_type\" = 'source.Connection'",
        ),
        (
            "_norn_fact_values",
            "_norn_fact_values/connection_mapping",
            "CREATE INDEX \"_norn_fact_values/connection_mapping\" ON \"_norn_fact_values\" (\"world\", \"id\", COALESCE(json_extract(\"snapshot\", '$.values.mapping_id'), ''), \"version\") WHERE \"root_type\" = 'source.Connection'",
        ),
        (
            "_norn_fact_values",
            "_norn_fact_values/connection_nonfailed",
            "CREATE INDEX \"_norn_fact_values/connection_nonfailed\" ON \"_norn_fact_values\" (\"world\", \"id\", \"version\") WHERE \"root_type\" = 'source.Connection' AND COALESCE(json_extract(\"snapshot\", '$.values.status'), '') != 'failed'",
        ),
        (
            "_norn_fact_values",
            "_norn_fact_values/connection_watermark",
            "CREATE INDEX \"_norn_fact_values/connection_watermark\" ON \"_norn_fact_values\" (\"world\", \"id\", COALESCE(json_extract(\"snapshot\", '$.values.watermark'), ''), \"version\") WHERE \"root_type\" = 'source.Connection' AND COALESCE(json_extract(\"snapshot\", '$.values.status'), '') != 'failed'",
        ),
    ] {
        schema.push(serde_json::json!({
            "type": "index",
            "name": name,
            "table": table,
            "sql": sql,
        }));
    }
}

/// The marking history storage version (storage version 5, #1547) adds the
/// partial index of the version marking labels that are not empty to every
/// older cell; no row changes.
fn add_marking_history_index(image: &mut Value) {
    image["schema"]
        .as_array_mut()
        .expect("schema")
        .push(serde_json::json!({
            "type": "index",
            "name": "_norn_marking_version_nonempty",
            "table": "_norn_marking_version",
            "sql": "CREATE INDEX \"_norn_marking_version_nonempty\" ON \"_norn_marking_version\" (\"world\", \"object_type\", \"property\") WHERE \"markings\" != '[]'"
        }));
}

/// Storage version 3 (#1483) adds the approval recovery tables, derived from
/// the ledger, to every older cell: `_norn_approved_call` gains
/// `approved_calls` (the backfill from the cell's approving decisions), and
/// `_norn_step_call` arrives empty, as no older commit owned a step call.
fn add_approval_recovery_tables(image: &mut Value, approved_calls: &Value) {
    let objects = [
        (
            "table",
            "_norn_approved_call",
            "CREATE TABLE \"_norn_approved_call\" (\n  \"world\" TEXT NOT NULL,\n  \"decision_id\" TEXT NOT NULL,\n  \"command_id\" TEXT NOT NULL,\n  \"run_id\" TEXT NOT NULL,\n  \"resolved_at_ms\" INTEGER NOT NULL,\n  \"refused_status\" INTEGER,\n  \"refused_code\" TEXT,\n  \"refused_category\" TEXT,\n  \"consumed_seq\" INTEGER,\n  PRIMARY KEY (\"world\", \"decision_id\")\n)",
        ),
        (
            "index",
            "_norn_approved_call/run",
            "CREATE INDEX \"_norn_approved_call/run\" ON \"_norn_approved_call\" (\"world\", \"run_id\", \"decision_id\")",
        ),
        (
            "index",
            "_norn_approved_call/consumed",
            "CREATE INDEX \"_norn_approved_call/consumed\" ON \"_norn_approved_call\" (\"world\", \"consumed_seq\")",
        ),
        (
            "table",
            "_norn_step_call",
            "CREATE TABLE \"_norn_step_call\" (\n  \"world\" TEXT NOT NULL,\n  \"commit_seq\" INTEGER NOT NULL,\n  \"run_id\" TEXT NOT NULL,\n  \"command_id\" TEXT NOT NULL,\n  \"generation\" INTEGER NOT NULL,\n  PRIMARY KEY (\"world\", \"commit_seq\")\n)",
        ),
    ];
    for (kind, name, sql) in objects {
        let table = if kind == "table" {
            name
        } else {
            "_norn_approved_call"
        };
        image["schema"]
            .as_array_mut()
            .expect("schema")
            .push(serde_json::json!({ "type": kind, "name": name, "table": table, "sql": sql }));
    }
    let tables = image["tables"].as_object_mut().expect("tables");
    tables.insert(
        "_norn_approved_call".to_owned(),
        serde_json::json!({
            "columns": ["rowid", "world", "decision_id", "command_id", "run_id", "resolved_at_ms", "refused_status", "refused_code", "refused_category", "consumed_seq"],
            "rows": approved_calls,
        }),
    );
    tables.insert(
        "_norn_step_call".to_owned(),
        serde_json::json!({
            "columns": ["rowid", "world", "commit_seq", "run_id", "command_id", "generation"],
            "rows": [],
        }),
    );
}

/// The cell's transparency root, as the open checkpoint records it.
fn receipts_root(store: &CellStore<NativeEngine>) -> String {
    use std::fmt::Write as _;
    store
        .transparency_checkpoint()
        .expect("root")
        .as_bytes()
        .iter()
        .fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("hex");
            hex
        })
}

/// Norn1 hash of `value`, as the engine content-addresses its own data.
fn norn1<T: serde::Serialize>(value: &T) -> String {
    use norn_ir::canonical::{HashProfile, canonical_json_of};
    HashProfile::Norn1.hash_canonical_bytes(&canonical_json_of(value).expect("canonical"))
}

/// The open checkpoint's quantity digest, folded here independently over the
/// `(commit, receipt digest)` of each receipt that carries quantity (S1,
/// #1551).
fn quantity_receipts(store: &CellStore<NativeEngine>) -> String {
    store
        .read()
        .receipts()
        .expect("receipts")
        .iter()
        .filter(|receipt| receipt.body.quantity.is_some())
        .fold(
            norn1(&"norn.open-checkpoint.quantity/1"),
            |digest, receipt| {
                norn1(&(
                    digest.as_str(),
                    receipt.body.commit_seq,
                    receipt.hash.as_str(),
                ))
            },
        )
}

/// The last export record at or before the head, as the open checkpoint
/// pins it (S2, #1551 round 2): its commit and digest, or none.
fn export_pin(store: &CellStore<NativeEngine>, commits: u64) -> (Value, Value) {
    (1..=commits)
        .rev()
        .find_map(|commit| {
            store
                .export_record(commit)
                .expect("export record")
                .map(|record| (commit.into(), record.hash().expect("record digest").into()))
        })
        .unwrap_or((Value::Null, Value::Null))
}

/// The open checkpoint's definitions digest, folded here independently over
/// every retained definition version (#1551 round 3).
fn definitions_digest(store: &CellStore<NativeEngine>) -> String {
    store
        .read()
        .installed_definition_versions()
        .expect("definition versions")
        .iter()
        .fold(
            norn1(&"norn.open-checkpoint.definitions/1"),
            |digest, version| norn1(&(digest.as_str(), norn1(version).as_str())),
        )
}

/// Storage version 6 (OPEN-CHECKPOINT-1, #1533) adds `_norn_open_checkpoint`,
/// and the upgrading open records in it the head it verified, with the root
/// of its receipts, the digest of its quantity receipts, the last export
/// record it covers and the digest of its definition versions: the one row
/// it writes besides the stamp. The row's MAC (#1551 round 4) is checked
/// here independently: HMAC-SHA256 under the test key over the row and the
/// cell's identity.
fn add_open_checkpoint(
    expected: &mut Value,
    after: &Value,
    head: &LedgerHead,
    root: &str,
    quantity: &str,
    pins: &((Value, Value), String),
) {
    let entry = after["schema"]
        .as_array()
        .expect("schema")
        .iter()
        .find(|entry| entry["name"] == "_norn_open_checkpoint")
        .expect("the upgrade adds the open checkpoint table")
        .clone();
    assert_eq!(entry["type"], "table");
    let ((export_seq, export_hash), definitions) = pins;
    let cell_id = after["tables"]["_norn_meta"]["rows"]
        .as_array()
        .expect("meta rows")
        .iter()
        .find(|row| row[1] == "binding:cell_id")
        .and_then(|row| row[2].as_str())
        .expect("the fixture is bound to a cell identity")
        .to_owned();
    let export = if export_seq.is_null() {
        Value::Null
    } else {
        serde_json::json!([export_seq, export_hash])
    };
    let message = norn_ir::canonical::canonical_json_of(&serde_json::json!([
        "norn.open-checkpoint.mac/1",
        cell_id,
        head.commits,
        head.seq,
        head.hash,
        root,
        null,
        quantity,
        export,
        definitions
    ]))
    .expect("mac message");
    let mac = {
        use hmac::{KeyInit as _, Mac as _};
        let mut mac = hmac::Hmac::<transparency_sha2::Sha256>::new_from_slice(&test_key_bytes())
            .expect("key");
        mac.update(&message);
        mac.finalize()
            .into_bytes()
            .iter()
            .fold(String::new(), |mut hex, byte| {
                use std::fmt::Write as _;
                let _infallible = write!(hex, "{byte:02x}");
                hex
            })
    };
    expected["schema"]
        .as_array_mut()
        .expect("schema")
        .push(entry);
    for (index_name, table_name) in [
        (
            "_norn_quantity_history/commit_seq",
            "_norn_quantity_history",
        ),
        (
            "_norn_quantity_posting/commit_seq",
            "_norn_quantity_posting",
        ),
    ] {
        let index = after["schema"]
            .as_array()
            .expect("schema")
            .iter()
            .find(|entry| entry["name"] == index_name)
            .unwrap_or_else(|| panic!("the v6 upgrade adds {index_name}"))
            .clone();
        assert_eq!(index["type"], "index");
        assert_eq!(index["table"], table_name);
        expected["schema"]
            .as_array_mut()
            .expect("schema")
            .push(index);
    }
    expected["tables"].as_object_mut().expect("tables").insert(
        "_norn_open_checkpoint".to_owned(),
        serde_json::json!({
            "columns": ["rowid", "id", "commits", "seq", "hash", "receipts_root", "boundary_seq", "boundary_commit", "quantity_receipts", "export_seq", "export_hash", "definitions", "mac"],
            "rows": [[1, 1, head.commits, head.seq, head.hash, root, null, null, quantity, export_seq, export_hash, definitions, mac]],
        }),
    );
}

/// The catalog is compared by name: a recreated index takes a new place.
fn by_name(image: &mut Value) {
    if let Some(schema) = image["schema"].as_array_mut() {
        schema.sort_by_key(|entry| entry["name"].to_string());
    }
}

/// The v1 fixture, loaded into a new file.
fn v1_cell(dir: &Path) -> PathBuf {
    let image: Value = serde_json::from_str(STORAGE_V1).expect("fixture");
    assert_eq!(image["storage"], 1, "the fixture records storage version 1");
    let path = dir.join("cell.sqlite");
    engine_fixture::load(&image, &path);
    path
}

fn raw(path: &Path) -> NativeEngine {
    NativeEngine::open(path, StoreOptions::default()).expect("engine")
}

/// This test process's checkpoint key: random, from SQLite's
/// `randomblob`, the same for every open in the process.
fn test_key_bytes() -> [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| {
        let engine = NativeEngine::open_in_memory().expect("engine");
        let rows = engine
            .query("SELECT randomblob(32)", &[])
            .expect("random bytes");
        let [SqlValue::Blob(bytes)] = &rows[0][..] else {
            panic!("random bytes {rows:?}");
        };
        bytes.clone().try_into().expect("32 bytes")
    })
}

/// Opens with the test key, as a keyed host opens (#1551 round 4).
fn keyed() -> StoreOptions {
    StoreOptions {
        checkpoint_key: Some(norn_cell::CheckpointKey::from_bytes(test_key_bytes())),
        ..StoreOptions::default()
    }
}

fn open(path: &Path) -> Result<CellStore<NativeEngine>, CellError> {
    CellStore::open(path, engine_fixture::model(), keyed())
}

/// Write `stamp` as another engine would, below this engine's admission.
fn set_stamp(path: &Path, stamp: &str) {
    let conn = rusqlite::Connection::open(path).expect("sqlite");
    conn.execute(
        "INSERT INTO \"_norn_meta\" (\"key\", \"value\") VALUES (?2, ?1) \
         ON CONFLICT (\"key\") DO UPDATE SET \"value\" = excluded.\"value\"",
        rusqlite::params![stamp, STAMP_KEY],
    )
    .expect("stamp");
}

/// Store `stamp` as a BLOB: the column is not STRICT, so SQLite keeps it.
fn set_blob_stamp(path: &Path, stamp: &[u8]) {
    let conn = rusqlite::Connection::open(path).expect("sqlite");
    conn.execute(
        "UPDATE \"_norn_meta\" SET \"value\" = ?1 WHERE \"key\" = ?2",
        rusqlite::params![stamp, STAMP_KEY],
    )
    .expect("stamp");
}

/// Remove the stamp, as a cell written before it existed has none.
fn unstamp(path: &Path) {
    let conn = rusqlite::Connection::open(path).expect("sqlite");
    conn.execute(
        "DELETE FROM \"_norn_meta\" WHERE \"key\" = ?1",
        rusqlite::params![STAMP_KEY],
    )
    .expect("unstamp");
}

fn newer_stamp() -> String {
    format!(
        r#"{{"format":"{STAMP_FORMAT}","storage":{},"writer":"drop-99"}}"#,
        STORAGE_VERSION + 1
    )
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("cell file")
}

fn wal() -> StoreOptions {
    StoreOptions {
        journal_mode: JournalMode::Wal,
        ..keyed()
    }
}

fn journal_mode(path: &Path) -> String {
    let rows = NativeEngine::open_read_only(path)
        .expect("read-only engine")
        .query("PRAGMA journal_mode", &[])
        .expect("journal mode");
    let SqlValue::Text(mode) = &rows[0][0] else {
        panic!("journal mode {:?}", rows[0][0]);
    };
    mode.to_ascii_lowercase()
}

/// The rows of `table` in `image`.
fn rows<'a>(image: &'a Value, table: &str) -> &'a Value {
    &image["tables"][table]["rows"]
}

/// Every way a host reads a cell before or while opening it refuses `check`
/// and leaves the file's contents as they were.
fn every_open_refuses(path: &Path, check: impl Fn(&CellError) -> bool) {
    let before = engine_fixture::image_of(path);
    let file = bytes(path);
    let read_only = || NativeEngine::open_read_only(path).expect("read-only engine");
    let refusals = [
        NativeEngine::open(path, StoreOptions::default()).err(),
        NativeEngine::open(path, wal()).err(),
        open(path).err(),
        CellStore::<NativeEngine>::read_binding(&read_only(), "scope").err(),
        CellStore::<NativeEngine>::read_active_definitions(&read_only()).err(),
        CellStore::from_engine(read_only(), engine_fixture::model()).err(),
        CellStore::diagnose_history(path).err(),
    ];
    for refusal in refusals {
        let refusal = refusal.expect("the open refuses");
        assert!(check(&refusal), "unexpected refusal: {refusal}");
    }
    assert_eq!(
        bytes(path),
        file,
        "a refused open leaves the file byte-identical"
    );
    assert_eq!(
        engine_fixture::image_of(path),
        before,
        "a refused open changes nothing"
    );
}

#[test]
fn a_storage_v1_cell_opens_on_this_engine_with_identical_receipts() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    let before = engine_fixture::image_of(&path);
    assert_eq!(
        before,
        serde_json::from_str::<Value>(STORAGE_V1).expect("fixture"),
        "the image loads losslessly"
    );
    assert_eq!(
        engine_release::stored_version(&raw(&path)).expect("version"),
        StoredVersion::Unstamped
    );
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Upgrade { from: 1 }
    );

    // The open runs the migrations and verifies the whole ledger and every
    // receipt under the new engine.
    let mut store = open(&path).expect("a v1 cell opens on this engine");
    let head = store.read().verify_ledger().expect("the ledger verifies");
    let root = receipts_root(&store);
    let quantity = quantity_receipts(&store);
    let pins = (export_pin(&store, head.commits), definitions_digest(&store));
    assert_eq!(head.commits, 3);
    let receipts = store.read().receipts().expect("receipts");
    assert_eq!(receipts.len(), 3);
    // Every recorded commit replays to its recorded receipt.
    for (batch, receipt) in engine_fixture::batches().iter().zip(&receipts) {
        match store.commit(batch).expect("replay") {
            CommitOutcome::Replayed(replayed) => assert_eq!(&replayed, receipt),
            CommitOutcome::Committed(_) => panic!("a recorded commit was committed again"),
        }
    }
    assert_eq!(store.verified_head().expect("head"), head);
    drop(store);

    let after = engine_fixture::image_of(&path);
    for table in ["_norn_ledger", "_norn_receipt"] {
        assert!(
            rows(&before, table)
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
        );
        assert_eq!(
            rows(&after, table),
            rows(&before, table),
            "{table} is unchanged"
        );
    }
    // Storage version 2 adds the stamp and changes nothing else; storage
    // version 3 adds the approval recovery tables; storage version 4
    // rewrites no row; the marking history version (#1547) adds its index.
    // A later data migration lists what it changes here.
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before;
    let meta = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows");
    let next_rowid = meta
        .iter()
        .filter_map(|row| row[0].as_i64())
        .max()
        .expect("meta rowid")
        + 1;
    meta.push(serde_json::json!([next_rowid, STAMP_KEY, stamp]));
    expected["storage"] = STORAGE_VERSION.into();
    // #1469 replaces the retained definitions' one-revision-per-version index
    // with one revision per id per version, on every open (`retained.rs`).
    let schema = expected["schema"].as_array_mut().expect("schema");
    schema.retain(|entry| entry["name"] != "_norn_retained_definition/version");
    schema.push(serde_json::json!({
        "type": "index",
        "name": "_norn_retained_definition/version_id",
        "table": "_norn_retained_definition",
        "sql": "CREATE UNIQUE INDEX \"_norn_retained_definition/version_id\" ON \"_norn_retained_definition\" (\"definitions_version\", \"id\")"
    }));
    // The v1 fixture holds no decision, so nothing is backfilled.
    add_approval_recovery_tables(&mut expected, &serde_json::json!([]));
    add_marking_history_index(&mut expected);
    add_open_checkpoint(&mut expected, &after, &head, &root, &quantity, &pins);
    add_automation_tables(&mut expected);
    add_function_receipt_table(&mut expected);
    apply_v10_upgrade(&mut expected);
    let mut catalog = after;
    by_name(&mut catalog);
    by_name(&mut expected);
    assert_eq!(
        catalog, expected,
        "the upgrade writes the stamp and the listed index change alone"
    );
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );

    // A reopen at the current version migrates nothing, and the upgraded
    // cell keeps committing on the chain it inherited.
    let store = open(&path).expect("reopen");
    assert_eq!(store.read().verify_ledger().expect("ledger"), head);
    drop(store);
    let mut reopened = engine_fixture::image_of(&path);
    by_name(&mut reopened);
    assert_eq!(reopened, catalog, "a current open writes nothing");
    let mut store = open(&path).expect("reopen");
    // A new idempotency key waits for the history scrub (#1551 round 4),
    // which the host's due-work clock runs.
    assert_eq!(store.scrub_to_end(), norn_cell::ScrubStatus::Idle);
    let mut next = engine_fixture::batches()[1].clone();
    next.idempotency_key = Some("after-upgrade".to_owned());
    next.logical_time = 4_000;
    let CommitOutcome::Committed(receipt) = store.commit(&next).expect("commit") else {
        panic!("a new commit");
    };
    assert_eq!(receipt.body.commit_seq, 4);
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, 4);
}

/// A cell the last storage version 2 engine wrote (stamped, with #1469's
/// index already in place) opens on this engine with its ledger and receipts
/// intact; the upgrade runs steps 3 and 4, restamps it and adds the approval
/// recovery tables,
/// backfilling `_norn_approved_call` from the direct call the v2 engine
/// parked and approved: row for row what this engine records replaying the
/// same history into a new cell.
#[test]
fn a_storage_v2_cell_opens_on_this_engine_with_identical_receipts() {
    let image: Value = serde_json::from_str(STORAGE_V2).expect("fixture");
    assert_eq!(image["storage"], 2, "the fixture records storage version 2");
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    engine_fixture::load(&image, &path);
    let before = engine_fixture::image_of(&path);
    assert_eq!(before, image, "the image loads losslessly");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Upgrade { from: 2 }
    );

    assert!(
        rows(&before, "_norn_decision")
            .as_array()
            .is_some_and(|decisions| decisions.iter().any(|row| row[2]
                == engine_fixture::APPROVAL_DECISION
                && row[3] == engine_fixture::APPROVAL_COMMAND
                && row[7] == "decided"
                && row[8] == "approve")),
        "the v2 engine recorded the approving decision"
    );

    let mut store = open(&path).expect("a v2 cell opens on this engine");
    let root = receipts_root(&store);
    let quantity = quantity_receipts(&store);
    let head = store.read().verify_ledger().expect("the ledger verifies");
    let pins = (export_pin(&store, head.commits), definitions_digest(&store));
    assert_eq!(head.commits, engine_fixture::COMMITS);
    let receipts = store.read().receipts().expect("receipts");
    assert_eq!(
        u64::try_from(receipts.len()).expect("count"),
        engine_fixture::COMMITS
    );
    for (batch, receipt) in engine_fixture::batches().iter().zip(&receipts) {
        match store.commit(batch).expect("replay") {
            CommitOutcome::Replayed(replayed) => assert_eq!(&replayed, receipt),
            CommitOutcome::Committed(_) => panic!("a recorded commit was committed again"),
        }
    }
    drop(store);

    let after = engine_fixture::image_of(&path);
    for table in ["_norn_ledger", "_norn_receipt"] {
        assert_eq!(
            rows(&after, table),
            rows(&before, table),
            "{table} is unchanged"
        );
    }
    // This engine, replaying the same history into a new cell, records the
    // approval as it commits; the upgrade must backfill exactly those rows.
    let replayed = dir.path().join("replayed.sqlite");
    engine_fixture::write_cell(&replayed);
    let replayed = engine_fixture::image_of(&replayed);
    let approved = rows(&replayed, "_norn_approved_call");
    assert_eq!(
        approved.as_array().map(Vec::len),
        Some(1),
        "a fresh cell records the one approved call: {approved}"
    );
    assert_eq!(approved[0][2], engine_fixture::APPROVAL_DECISION);
    assert_eq!(
        rows(&after, "_norn_approved_call"),
        approved,
        "the upgrade backfills the approved call row for row"
    );
    assert_eq!(
        rows(&after, "_norn_step_call"),
        rows(&replayed, "_norn_step_call")
    );
    // The step restamps the existing `engine` row in place and adds the
    // approval recovery tables; nothing else changes.
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before;
    let stamp_row = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows")
        .iter_mut()
        .find(|row| row[1] == STAMP_KEY)
        .expect("the v2 cell is stamped");
    stamp_row[2] = stamp.into();
    expected["storage"] = STORAGE_VERSION.into();
    add_approval_recovery_tables(&mut expected, approved);
    add_marking_history_index(&mut expected);
    add_open_checkpoint(&mut expected, &after, &head, &root, &quantity, &pins);
    add_automation_tables(&mut expected);
    add_function_receipt_table(&mut expected);
    apply_v10_upgrade(&mut expected);
    let mut catalog = after;
    by_name(&mut catalog);
    by_name(&mut expected);
    assert_eq!(
        catalog, expected,
        "the upgrade writes the stamp and the approval recovery tables alone"
    );
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );
}

/// A cell the last storage version 3 engine wrote (#1483's approval recovery
/// tables in place, holding the approved direct call it recorded as it
/// committed) opens on this engine with its ledger and receipts intact. Step
/// 4 (MAP-DERIVATION-1, #1488) rewrites nothing and step 5 (#1547) adds the
/// marking history index: the upgrade restamps the cell, adds that index, and
/// changes no other row, table or index.
#[test]
fn a_storage_v3_cell_opens_on_this_engine_with_identical_receipts() {
    let image: Value = serde_json::from_str(STORAGE_V3).expect("fixture");
    assert_eq!(image["storage"], 3, "the fixture records storage version 3");
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    engine_fixture::load(&image, &path);
    let before = engine_fixture::image_of(&path);
    assert_eq!(before, image, "the image loads losslessly");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Upgrade { from: 3 }
    );
    let approved = rows(&before, "_norn_approved_call");
    assert_eq!(
        approved.as_array().map(Vec::len),
        Some(1),
        "the v3 engine recorded the one approved call: {approved}"
    );
    assert_eq!(approved[0][2], engine_fixture::APPROVAL_DECISION);
    assert_eq!(approved[0][3], engine_fixture::APPROVAL_COMMAND);

    let mut store = open(&path).expect("a v3 cell opens on this engine");
    let head = store.read().verify_ledger().expect("the ledger verifies");
    let root = receipts_root(&store);
    let quantity = quantity_receipts(&store);
    let pins = (export_pin(&store, head.commits), definitions_digest(&store));
    assert_eq!(head.commits, engine_fixture::COMMITS);
    let receipts = store.read().receipts().expect("receipts");
    assert_eq!(
        u64::try_from(receipts.len()).expect("count"),
        engine_fixture::COMMITS
    );
    for (batch, receipt) in engine_fixture::batches().iter().zip(&receipts) {
        match store.commit(batch).expect("replay") {
            CommitOutcome::Replayed(replayed) => assert_eq!(&replayed, receipt),
            CommitOutcome::Committed(_) => panic!("a recorded commit was committed again"),
        }
    }
    assert_eq!(store.verified_head().expect("head"), head);
    drop(store);

    let after = engine_fixture::image_of(&path);
    for table in [
        "_norn_ledger",
        "_norn_receipt",
        "_norn_approved_call",
        "_norn_step_call",
    ] {
        assert_eq!(
            rows(&after, table),
            rows(&before, table),
            "{table} is unchanged"
        );
    }
    // This engine, replaying the same history into a new cell, records the
    // same approval recovery rows the v3 engine did.
    let replayed = dir.path().join("replayed.sqlite");
    engine_fixture::write_cell(&replayed);
    let replayed = engine_fixture::image_of(&replayed);
    for table in ["_norn_approved_call", "_norn_step_call"] {
        assert_eq!(
            rows(&after, table),
            rows(&replayed, table),
            "{table} matches a cell this engine writes"
        );
    }
    // The step restamps the existing `engine` row in place; it adds no
    // table or index and rewrites no row.
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before;
    let stamp_row = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows")
        .iter_mut()
        .find(|row| row[1] == STAMP_KEY)
        .expect("the v3 cell is stamped");
    stamp_row[2] = stamp.into();
    expected["storage"] = STORAGE_VERSION.into();
    add_marking_history_index(&mut expected);
    add_open_checkpoint(&mut expected, &after, &head, &root, &quantity, &pins);
    add_automation_tables(&mut expected);
    add_function_receipt_table(&mut expected);
    apply_v10_upgrade(&mut expected);
    let mut catalog = after;
    by_name(&mut catalog);
    by_name(&mut expected);
    assert_eq!(
        catalog, expected,
        "the upgrade writes the stamp, the marking history index and the open checkpoint alone"
    );
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );

    // A reopen at the current version writes nothing.
    let store = open(&path).expect("reopen");
    assert_eq!(store.read().verify_ledger().expect("ledger"), head);
    drop(store);
    let mut reopened = engine_fixture::image_of(&path);
    by_name(&mut reopened);
    assert_eq!(reopened, catalog, "a current open writes nothing");
}

/// A cell the last storage version 4 engine wrote opens on this engine with
/// its ledger and receipts intact. Step 5 (#1547) creates the marking history
/// index and step 6 (OPEN-CHECKPOINT-1, #1533) adds `_norn_open_checkpoint`:
/// the upgrading open verifies the whole history and records its head and
/// receipt root there. The next open verifies from that checkpoint and writes
/// nothing.
#[test]
fn a_storage_v4_cell_opens_on_this_engine_with_identical_receipts() {
    let image: Value = serde_json::from_str(STORAGE_V4).expect("fixture");
    assert_eq!(image["storage"], 4, "the fixture records storage version 4");
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    engine_fixture::load(&image, &path);
    let before = engine_fixture::image_of(&path);
    assert_eq!(before, image, "the image loads losslessly");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Upgrade { from: 4 }
    );

    let mut store = open(&path).expect("a v4 cell opens on this engine");
    let head = store.read().verify_ledger().expect("the ledger verifies");
    let root = receipts_root(&store);
    let quantity = quantity_receipts(&store);
    let pins = (export_pin(&store, head.commits), definitions_digest(&store));
    assert_eq!(head.commits, engine_fixture::COMMITS);
    let receipts = store.read().receipts().expect("receipts");
    for (batch, receipt) in engine_fixture::batches().iter().zip(&receipts) {
        match store.commit(batch).expect("replay") {
            CommitOutcome::Replayed(replayed) => assert_eq!(&replayed, receipt),
            CommitOutcome::Committed(_) => panic!("a recorded commit was committed again"),
        }
    }
    assert_eq!(store.verified_head().expect("head"), head);
    drop(store);

    let after = engine_fixture::image_of(&path);
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before;
    let stamp_row = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows")
        .iter_mut()
        .find(|row| row[1] == STAMP_KEY)
        .expect("the v4 cell is stamped");
    stamp_row[2] = stamp.into();
    expected["storage"] = STORAGE_VERSION.into();
    add_marking_history_index(&mut expected);
    add_open_checkpoint(&mut expected, &after, &head, &root, &quantity, &pins);
    add_automation_tables(&mut expected);
    add_function_receipt_table(&mut expected);
    apply_v10_upgrade(&mut expected);
    let mut catalog = after;
    by_name(&mut catalog);
    by_name(&mut expected);
    assert_eq!(
        catalog, expected,
        "the upgrade writes the stamp, the marking history index and the open checkpoint alone"
    );
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );

    // A reopen verifies from the checkpoint and writes nothing.
    let store = open(&path).expect("reopen");
    assert_eq!(store.read().verify_ledger().expect("ledger"), head);
    drop(store);
    let mut reopened = engine_fixture::image_of(&path);
    by_name(&mut reopened);
    assert_eq!(reopened, catalog, "a current open writes nothing");
}

/// A cell the last storage version 5 engine wrote (#1547's marking history
/// index already in place) opens on this engine with its ledger and receipts
/// intact. Step 6 (OPEN-CHECKPOINT-1, #1533) adds `_norn_open_checkpoint`:
/// the upgrading open verifies the whole history and records its head and
/// receipt root there, the one row it writes besides the stamp. The next open
/// verifies from that checkpoint and writes nothing. Step 7 adds the empty
/// automation tables.
#[test]
fn a_storage_v5_cell_opens_on_this_engine_with_identical_receipts() {
    let image: Value = serde_json::from_str(STORAGE_V5).expect("fixture");
    assert_eq!(image["storage"], 5, "the fixture records storage version 5");
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    engine_fixture::load(&image, &path);
    let before = engine_fixture::image_of(&path);
    assert_eq!(before, image, "the image loads losslessly");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Upgrade { from: 5 }
    );

    let mut store = open(&path).expect("a v5 cell opens on this engine");
    let head = store.read().verify_ledger().expect("the ledger verifies");
    let root = receipts_root(&store);
    let quantity = quantity_receipts(&store);
    let pins = (export_pin(&store, head.commits), definitions_digest(&store));
    assert_eq!(head.commits, engine_fixture::COMMITS);
    let receipts = store.read().receipts().expect("receipts");
    for (batch, receipt) in engine_fixture::batches().iter().zip(&receipts) {
        match store.commit(batch).expect("replay") {
            CommitOutcome::Replayed(replayed) => assert_eq!(&replayed, receipt),
            CommitOutcome::Committed(_) => panic!("a recorded commit was committed again"),
        }
    }
    assert_eq!(store.verified_head().expect("head"), head);
    drop(store);

    let after = engine_fixture::image_of(&path);
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before;
    let stamp_row = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows")
        .iter_mut()
        .find(|row| row[1] == STAMP_KEY)
        .expect("the v5 cell is stamped");
    stamp_row[2] = stamp.into();
    expected["storage"] = STORAGE_VERSION.into();
    add_open_checkpoint(&mut expected, &after, &head, &root, &quantity, &pins);
    add_automation_tables(&mut expected);
    add_function_receipt_table(&mut expected);
    apply_v10_upgrade(&mut expected);
    let mut catalog = after;
    by_name(&mut catalog);
    by_name(&mut expected);
    assert_eq!(
        catalog, expected,
        "the v5 upgrade writes the stamp, checkpoint, and additive schema"
    );
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );

    // A reopen verifies from the checkpoint and writes nothing.
    let mut store = open(&path).expect("reopen");
    assert_eq!(store.read().verify_ledger().expect("ledger"), head);
    let mut reopened = engine_fixture::image_of(&path);
    by_name(&mut reopened);
    assert_eq!(reopened, catalog, "a current open writes nothing");
    assert_post_upgrade_commit(&mut store, "after-v5-upgrade");
}

/// V6's keyed checkpoint remains valid after the v7 migration. Every
/// retained commit, including parking and approving a call, replays to its
/// original receipt via the export journal.
#[tokio::test]
async fn a_storage_v6_cell_opens_on_this_engine_with_identical_receipts() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut exporter = Exporter::local(&dir.path().join("export")).expect("exporter");
    let receipts = {
        let mut store = upgrade_v6_with_checkpoint(&path);
        let receipts = store.read().receipts().expect("receipts");
        export_all(&store, &mut exporter).await;
        assert_post_upgrade_commit(&mut store, "after-v6-upgrade");
        receipts
    };
    let manifest = exporter.verify().await.expect("verified export");
    assert_eq!(manifest.head_commit_seq, engine_fixture::COMMITS);
    let restored_path = dir.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &restored_path,
        engine_fixture::model(),
        engine_fixture::SCOPE,
        &manifest.bindings,
        StoreOptions::default(),
    )
    .await
    .expect("replay all v6 export records");
    let restored = CellStore::open(
        &restored_path,
        engine_fixture::model(),
        StoreOptions::default(),
    )
    .expect("open restored cell");
    assert_eq!(
        restored.read().receipts().expect("restored receipts"),
        receipts
    );
}

fn upgrade_v6_with_checkpoint(path: &Path) -> CellStore<NativeEngine> {
    let image: Value = serde_json::from_str(STORAGE_V6).expect("fixture");
    assert_eq!(image["storage"], 6);
    engine_fixture::load(&image, path);
    assert_eq!(engine_fixture::image_of(path), image);
    assert_eq!(
        engine_release::admission(&raw(path)).expect("admission"),
        Admission::Upgrade { from: 6 }
    );

    let open_keyed = || {
        CellStore::open(
            path,
            engine_fixture::model(),
            engine_fixture::checkpoint_fixture_options(),
        )
        .expect("open v6 fixture with its checkpoint key")
    };
    let store = open_keyed();
    assert_fixture_checkpoint_in_use(&store);
    assert_eq!(store.scrub_to_end(), norn_cell::ScrubStatus::Idle);
    let head = store.verified_head().expect("verified head");
    assert_eq!(head.commits, engine_fixture::COMMITS);
    let receipts = store.read().receipts().expect("receipts");
    assert_eq!(u64::try_from(receipts.len()).expect("count"), head.commits);
    drop(store);

    let mut expected = image;
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let stamp_row = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows")
        .iter_mut()
        .find(|row| row[1] == STAMP_KEY)
        .expect("the v6 cell is stamped");
    stamp_row[2] = stamp.into();
    expected["storage"] = STORAGE_VERSION.into();
    add_automation_tables(&mut expected);
    add_function_receipt_table(&mut expected);
    apply_v10_upgrade(&mut expected);
    by_name(&mut expected);
    let mut actual = engine_fixture::image_of(path);
    by_name(&mut actual);
    assert_eq!(
        actual, expected,
        "v7 only adds tables and restamps the cell"
    );
    assert_eq!(
        engine_release::admission(&raw(path)).expect("admission"),
        Admission::Current
    );

    let store = open_keyed();
    assert_fixture_checkpoint_in_use(&store);
    let mut reopened = engine_fixture::image_of(path);
    by_name(&mut reopened);
    assert_eq!(reopened, actual, "a current open writes nothing");
    assert_eq!(store.scrub_to_end(), norn_cell::ScrubStatus::Idle);
    assert_eq!(store.read().verify_ledger().expect("reopened ledger"), head);
    assert_eq!(
        store.read().receipts().expect("reopened receipts"),
        receipts
    );
    store
}

fn assert_fixture_checkpoint_in_use(store: &CellStore<NativeEngine>) {
    assert!(matches!(
        store.scrub_status(),
        norn_cell::ScrubStatus::Verifying {
            target: engine_fixture::COMMITS,
            history_ready: false,
            ..
        }
    ));
}

fn assert_post_upgrade_commit(store: &mut CellStore<NativeEngine>, key: &str) {
    assert_eq!(store.scrub_to_end(), norn_cell::ScrubStatus::Idle);
    let mut next = engine_fixture::batches()[1].clone();
    next.idempotency_key = Some(key.to_owned());
    next.logical_time = 6_000;
    let CommitOutcome::Committed(receipt) = store.commit(&next).expect("post-upgrade commit")
    else {
        panic!("expected a new commit after storage upgrade");
    };
    assert_eq!(receipt.body.commit_seq, engine_fixture::COMMITS + 1);
    assert_eq!(
        store.read().verify_ledger().expect("ledger").commits,
        engine_fixture::COMMITS + 1
    );
}

#[test]
fn a_failed_open_rolls_the_upgrade_back() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    let before = engine_fixture::image_of(&path);
    let other = norn_cell::Model::from_definitions(&[], &[]).expect("model");
    assert!(matches!(
        CellStore::open(&path, other, StoreOptions::default()),
        Err(CellError::SchemaMismatch { .. })
    ));
    assert_eq!(engine_fixture::image_of(&path), before);
    assert_eq!(
        engine_release::stored_version(&raw(&path)).expect("version"),
        StoredVersion::Unstamped
    );
}

#[test]
fn a_new_cell_is_stamped_with_this_engine() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    drop(open(&path).expect("new cell"));
    let StoredVersion::Stamped(stamp) =
        engine_release::stored_version(&raw(&path)).expect("version")
    else {
        panic!("a new cell is stamped");
    };
    let stamp: EngineStamp = serde_json::from_str(&stamp).expect("stamp");
    assert_eq!(stamp, EngineStamp::current());
    assert_eq!(stamp.storage, STORAGE_VERSION);
}

#[test]
fn the_v1_window_does_not_admit_a_cell_this_engine_upgraded() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    drop(open(&path).expect("upgrade"));
    // Simulated: this engine's admission with the storage version 1 window,
    // against the upgraded cell. The shipped version 1 engines read no stamp,
    // so they cannot refuse it themselves (`docs/platform/engine-upgrades.md`).
    let previous = EngineVersion {
        storage: 1,
        upgrades_from: 1,
    };
    let stored = engine_release::stored_version(&raw(&path)).expect("version");
    assert!(matches!(
        engine_release::admit(&stored, previous),
        Err(CellError::EngineDowngrade {
            cell: STORAGE_VERSION,
            engine: 1,
            ..
        })
    ));
}

#[test]
fn every_open_refuses_a_cell_a_newer_engine_wrote() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    drop(open(&path).expect("upgrade"));
    // This engine, against a cell the next storage version wrote: every open
    // path refuses with the registered code and names the writer.
    let next = STORAGE_VERSION + 1;
    set_stamp(&path, &newer_stamp());
    every_open_refuses(&path, |refusal| {
        matches!(
            refusal,
            CellError::EngineDowngrade { cell, writer, engine }
                if *cell == next && writer == "drop-99" && *engine == STORAGE_VERSION
        ) && refusal
            .to_string()
            .starts_with(norn_checker::ENGINE_DOWNGRADE)
    });
}

#[test]
fn an_unknown_future_engine_stamp_refuses() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    drop(open(&path).expect("upgrade"));
    for stamp in [
        // A stamp encoding from a future release.
        r#"{"format":"norn.cell.engine/2","storage":7,"writer":"drop-120","layout":"columnar"}"#,
        // A version past any this engine can count.
        r#"{"format":"norn.cell.engine/1","storage":99999999999,"writer":"drop-120"}"#,
        "not a stamp",
    ] {
        set_stamp(&path, stamp);
        every_open_refuses(&path, |refusal| {
            matches!(refusal, CellError::EngineUnknownVersion { .. })
                && refusal
                    .to_string()
                    .starts_with(norn_checker::ENGINE_UNKNOWN_VERSION)
        });
    }
    // A stamp that is not text is as unreadable as malformed JSON.
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    set_blob_stamp(&path, stamp.as_bytes());
    every_open_refuses(&path, |refusal| {
        matches!(refusal, CellError::EngineUnknownVersion { .. })
            && refusal
                .to_string()
                .starts_with(norn_checker::ENGINE_UNKNOWN_VERSION)
    });
}

/// How a refused open is recognised.
type Refused = fn(&CellError) -> bool;

fn ledger_refused(error: &CellError) -> bool {
    matches!(error, CellError::LedgerCorrupt { .. })
}

fn pin_refused(error: &CellError) -> bool {
    matches!(error, CellError::Encoding(reason) if reason.contains("past the store's head"))
}

/// U1: an open commits its upgrade only after every check that can refuse
/// the file has passed. A v1 cell whose ledger does not verify, or whose
/// fork pin is past its head, refuses and stays byte-identical, unstamped,
/// in its stored journal mode, with default and with WAL options.
#[test]
fn a_v1_cell_that_fails_verification_stays_byte_identical() {
    let image: Value = serde_json::from_str(STORAGE_V1).expect("fixture");
    let mut tampered_ledger = image.clone();
    // The second event's text no longer hashes to its chained hash.
    tampered_ledger["tables"]["_norn_ledger"]["rows"][1][4] =
        r#"{"key":"a3","type":"fixture.deleted"}"#.into();
    let mut pinned_past_head = image;
    let meta = pinned_past_head["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows");
    let mut rowid = meta
        .iter()
        .filter_map(|row| row[0].as_i64())
        .max()
        .expect("meta rowid");
    let base = serde_json::json!({"seq": 99, "hash": "0".repeat(64), "commits": 99}).to_string();
    for (key, value) in [
        ("fork:world", "what-if"),
        ("fork:parent", "main"),
        ("fork:base", base.as_str()),
        ("fork:main_horizon", "99"),
    ] {
        rowid += 1;
        meta.push(serde_json::json!([rowid, key, value]));
    }
    let cases: [(&str, Value, Refused); 2] = [
        ("ledger", tampered_ledger, ledger_refused),
        ("fork pin", pinned_past_head, pin_refused),
    ];
    for (name, image, refused) in cases {
        // A refused open keeps the stored journal mode, even one that asked for WAL.
        for (mode, options) in [("delete", StoreOptions::default()), ("wal", wal())] {
            let dir = tempfile::tempdir().expect("directory");
            let path = dir.path().join("cell.sqlite");
            engine_fixture::load(&image, &path);
            assert_eq!(
                journal_mode(&path),
                "delete",
                "{name}: the fixture is DELETE"
            );
            let before = bytes(&path);
            let error = CellStore::open(&path, engine_fixture::model(), options).expect_err(name);
            assert!(
                refused(&error),
                "{name}/{mode}: unexpected refusal: {error}"
            );
            assert_eq!(
                bytes(&path),
                before,
                "{name}/{mode}: the file is byte-identical"
            );
            assert_eq!(
                journal_mode(&path),
                "delete",
                "{name}/{mode}: the journal mode is unchanged"
            );
            assert!(
                !dir.path().join("cell.sqlite-wal").exists(),
                "{name}/{mode}: no WAL sidecar"
            );
            assert_eq!(
                engine_release::stored_version(&raw(&path)).expect("version"),
                StoredVersion::Unstamped,
                "{name}/{mode}: the stamp is not committed"
            );
        }
    }
}

/// U3: the native engine sets the file's journal mode only after admitting
/// its storage version. A newer DELETE-mode cell opened with WAL options
/// refuses, and its bytes and journal mode are unchanged.
#[test]
fn a_newer_cell_refuses_a_journal_mode_change() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    drop(open(&path).expect("upgrade"));
    set_stamp(&path, &newer_stamp());
    assert_eq!(journal_mode(&path), "delete");
    let before = bytes(&path);
    assert!(matches!(
        NativeEngine::open(&path, wal()),
        Err(CellError::EngineDowngrade { .. })
    ));
    assert!(matches!(
        CellStore::open(&path, engine_fixture::model(), wal()),
        Err(CellError::EngineDowngrade { .. })
    ));
    assert_eq!(bytes(&path), before, "the file is byte-identical");
    assert_eq!(
        journal_mode(&path),
        "delete",
        "the journal mode is unchanged"
    );
    assert!(!dir.path().join("cell.sqlite-wal").exists());
}

/// An engine over the cell file whose first transaction starts only after
/// another opener has written `stamp`: provisioning races that opener. It
/// counts the schema statements it is asked to run.
struct Racing {
    inner: NativeEngine,
    path: PathBuf,
    stamp: RefCell<Option<String>>,
    raced: RefCell<Option<Vec<u8>>>,
    ddl: std::cell::Cell<usize>,
}

impl Racing {
    fn count_ddl(&self, sql: &str) {
        let head = sql.trim_start().get(..6).unwrap_or_default();
        if ["CREATE", "ALTER ", "DROP T", "DROP I"]
            .iter()
            .any(|verb| head.eq_ignore_ascii_case(verb))
        {
            self.ddl.set(self.ddl.get() + 1);
        }
    }
}

impl SqlEngine for Racing {
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.inner.definition_writes(f)
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.count_ddl(sql);
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.count_ddl(sql);
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        if let Some(stamp) = self.stamp.borrow_mut().take() {
            set_stamp(&self.path, &stamp);
            self.raced.replace(Some(bytes(&self.path)));
        }
        self.inner.transaction(f)
    }
}

fn provision_input() -> InstalledDefinitionInput {
    InstalledDefinitionInput {
        canonical_set: "[]".to_owned(),
        registry: Vec::new(),
        receipt_sha256: "synthetic".to_owned(),
        admission_receipt: None,
        readmission: None,
        diff: serde_json::json!({}),
        installed_at: 1,
        layout: None,
    }
}

fn provision<E: SqlEngine>(engine: E) -> Result<CellStore<E>, CellError> {
    CellStore::provision_definitions(
        engine,
        Model::from_definitions(&[], &[])?,
        &provision_input(),
        engine_fixture::SCOPE,
    )
}

/// U2: provisioning an existing cell admits it and runs the migrations at
/// the start of its write transaction. A second opener that makes the cell
/// newer after provisioning's first check is refused with nothing written;
/// with no race, an existing v1 cell is upgraded and stamped.
#[test]
fn provisioning_admits_and_upgrades_inside_its_transaction() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("tenant.sqlite");
    drop(provision(raw(&path)).expect("first provisioning"));
    unstamp(&path);
    assert_eq!(
        engine_release::stored_version(&raw(&path)).expect("version"),
        StoredVersion::Unstamped
    );

    let racing = Racing {
        inner: raw(&path),
        path: path.clone(),
        stamp: RefCell::new(Some(newer_stamp())),
        raced: RefCell::default(),
        ddl: std::cell::Cell::new(0),
    };
    let raced = std::rc::Rc::new(racing);
    let refused = provision(RcEngine(std::rc::Rc::clone(&raced)));
    assert!(
        matches!(
            refused,
            Err(CellError::EngineDowngrade { cell, .. }) if cell == STORAGE_VERSION + 1
        ),
        "{:?}",
        refused.err()
    );
    let after_race = raced.raced.borrow().clone().expect("the opener raced");
    assert_eq!(bytes(&path), after_race, "provisioning wrote nothing");
    assert_eq!(
        raced.ddl.get(),
        0,
        "the raced admission refuses before any schema statement runs"
    );

    unstamp(&path);
    let store = provision(raw(&path)).expect("retried provisioning");
    drop(store);
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current,
        "provisioning upgraded and stamped the v1 cell"
    );
}

/// Shares one [`Racing`] engine so the test can read what it saw.
struct RcEngine(std::rc::Rc<Racing>);

impl SqlEngine for RcEngine {
    fn read_isolation(&self) -> ReadIsolation {
        self.0.read_isolation()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.0.definition_writes(f)
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.0.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.0.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.0.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.0.transaction(f)
    }
}

/// U5: a fork admits the copied snapshot's storage version before it reads
/// the copy's head or writes its pin.
#[test]
fn a_fork_refuses_a_copy_another_engine_made_newer() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    let store = open(&path).expect("upgrade");
    let head = store.verified_head().expect("head");
    set_stamp(&path, &newer_stamp());
    assert!(matches!(
        store.fork("what-if", &head),
        Err(CellError::EngineDowngrade { .. })
    ));
}

async fn export_all(store: &CellStore<NativeEngine>, exporter: &mut Exporter) {
    let start = exporter.verify().await.expect("verify").head_commit_seq + 1;
    for seq in start..=store.export_commit_count().expect("count") {
        exporter
            .append(&store.export_record(seq).expect("journal").expect("record"))
            .await
            .expect("append");
    }
}

/// The v1 fixture upgraded, and a verified export of its legacy records.
async fn legacy_export(dir: &Path) -> (CellStore<NativeEngine>, Exporter) {
    let path = v1_cell(dir);
    let store = open(&path).expect("upgrade");
    let mut exporter = Exporter::local(&dir.join("legacy")).expect("exporter");
    export_all(&store, &mut exporter).await;
    (store, exporter)
}

/// U4: a legacy export, whose records carry no storage version, restores as
/// storage version 1: its records keep no version and the restored file is
/// at this engine's.
#[tokio::test]
async fn a_legacy_export_restores_as_storage_version_1() {
    let dir = tempfile::tempdir().expect("directory");
    let (_store, mut exporter) = legacy_export(dir.path()).await;
    let manifest = exporter.verify().await.expect("verified");
    let target = dir.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        engine_fixture::model(),
        engine_fixture::SCOPE,
        &manifest.bindings,
        StoreOptions::default(),
    )
    .await
    .expect("a legacy export restores");
    let restored = open(&target).expect("restored cell");
    for seq in 1..=3 {
        let record = restored
            .export_record(seq)
            .expect("journal")
            .expect("record");
        assert_eq!(
            record.storage_version, None,
            "a replayed record keeps no version"
        );
    }
    drop(restored);
    assert_eq!(
        engine_release::admission(&raw(&target)).expect("admission"),
        Admission::Current
    );
}

/// U4: export records carry the storage version that committed them, inside
/// their content address; an export a newer engine wrote refuses before
/// anything is staged.
#[tokio::test]
async fn an_export_a_newer_engine_wrote_refuses_before_staging() {
    let dir = tempfile::tempdir().expect("directory");
    let (mut store, _legacy) = legacy_export(dir.path()).await;
    let mut next = engine_fixture::batches()[1].clone();
    next.idempotency_key = Some("after-upgrade".to_owned());
    next.logical_time = 4_000;
    store.commit(&next).expect("commit");
    let mut records: Vec<_> = (1..=4)
        .map(|seq| store.export_record(seq).expect("journal").expect("record"))
        .collect();
    assert_eq!(records[3].storage_version, Some(STORAGE_VERSION));

    // The same history as a newer engine would export it: every record names
    // the newer version inside its content address, and the chain verifies.
    let mut previous = norn_cell::system::GENESIS_HASH.to_owned();
    for record in &mut records {
        record.storage_version = Some(STORAGE_VERSION + 1);
        record.previous = previous;
        previous = record.hash().expect("hash");
    }
    let mut newer = Exporter::local(&dir.path().join("newer")).expect("exporter");
    for record in &records {
        newer.append(record).await.expect("append");
    }
    let manifest = newer.verify().await.expect("the newer export verifies");
    let staging = dir.path().join("target");
    std::fs::create_dir(&staging).expect("target directory");
    let refused = CellStore::restore_from(
        &mut newer,
        &staging.join("restored.sqlite"),
        engine_fixture::model(),
        engine_fixture::SCOPE,
        &manifest.bindings,
        StoreOptions::default(),
    )
    .await;
    assert!(
        matches!(
            refused,
            Err(RestoreError::Cell(CellError::EngineDowngrade { cell, .. }))
                if cell == STORAGE_VERSION + 1
        ),
        "{:?}",
        refused.err()
    );
    assert_eq!(
        std::fs::read_dir(&staging)
            .expect("target directory")
            .count(),
        0,
        "nothing is staged"
    );
}

fn apply_v8_marked_fixture_upgrade(image: &mut Value) -> Result<(), Box<dyn std::error::Error>> {
    let stamp = serde_json::to_string(&EngineStamp::current())?;
    let stamp_row = image["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .and_then(|rows| rows.iter_mut().find(|row| row[1] == STAMP_KEY))
        .ok_or_else(|| std::io::Error::other("v7 fixture missing engine stamp"))?;
    stamp_row[2] = stamp.into();
    image["storage"] = STORAGE_VERSION.into();
    apply_v10_upgrade(image);
    let rows = image["tables"]["_norn_marking_snapshot"]["rows"]
        .as_array_mut()
        .ok_or_else(|| std::io::Error::other("v7 fixture missing snapshot rows"))?;
    for row in rows.iter_mut() {
        let Some(cells) = row.as_array_mut() else {
            continue;
        };
        let is_marked = cells.get(2).and_then(Value::as_str) == Some("fixture.MarkedItem")
            && matches!(
                (
                    cells.get(3).and_then(Value::as_str),
                    cells.get(5).and_then(Value::as_i64),
                ),
                (Some("m1"), Some(0)) | (Some("m2"), Some(0 | 1))
            );
        if is_marked && let Some(last) = cells.last_mut() {
            *last = Value::from(1_i64);
        }
    }
    Ok(())
}

fn assert_v7_replays_and_markings(
    store: &mut CellStore<NativeEngine>,
    receipts: &[norn_cell::Receipt],
) -> Result<(), Box<dyn std::error::Error>> {
    for (batch, receipt) in engine_fixture::batches().iter().zip(receipts) {
        let CommitOutcome::Replayed(replayed) = store.commit(batch)? else {
            return Err("recorded v7 commit was committed again".into());
        };
        assert_eq!(&replayed, receipt);
    }
    let expected = |secret: &[&str]| {
        Some(std::collections::BTreeMap::from([
            ("label".to_owned(), Vec::new()),
            (
                "secret".to_owned(),
                secret.iter().map(|s| (*s).to_owned()).collect(),
            ),
        ]))
    };
    assert_eq!(
        store
            .read()
            .effective_markings("main", "fixture.MarkedItem", "m1", None)?,
        expected(&[])
    );
    assert_eq!(
        store
            .read()
            .effective_markings("main", "fixture.MarkedItem", "m1", Some(1))?,
        expected(&["pii.contact"])
    );
    assert_eq!(
        store
            .read()
            .effective_markings("main", "fixture.MarkedItem", "m2", None)?,
        expected(&["pii.contact"])
    );
    assert_eq!(
        store
            .read()
            .effective_markings("main", "fixture.Item", "a0", None)?,
        Some(std::collections::BTreeMap::from([
            ("label".to_owned(), Vec::new()),
            ("qty".to_owned(), Vec::new()),
        ]))
    );
    Ok(())
}

/// Storage version 7 → 8 (#1533 Part 1): opening the genuine storage-v7 fixture
/// (carrying unmarked, marked, and declassified snapshots plus an authenticated
/// checkpoint) drops the two redundant `_norn_link_version` prefix indexes,
/// backfills exact `_norn_marking_snapshot.nonempty_slots` pins (`0` for
/// unmarked/declassified current, `1` for marked current and historical),
/// preserves ledger/receipts/checkpoint verbatim, and fails closed if a marked
/// row is later deleted.
#[test]
fn a_storage_v7_cell_opens_on_this_engine_and_backfills_nonempty_slots()
-> Result<(), Box<dyn std::error::Error>> {
    const V7_COMMITS: u64 = 5;
    let image: Value = serde_json::from_str(STORAGE_V7)?;
    assert_eq!(image["storage"], 7);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("v7-marked.sqlite");
    engine_fixture::load(&image, &path);
    assert_eq!(engine_fixture::image_of(&path), image);
    assert_eq!(
        engine_release::admission(&raw(&path))?,
        Admission::Upgrade { from: 7 }
    );

    let model = engine_fixture::v7_marked_model()?;
    let mut store = CellStore::open(
        &path,
        model.clone(),
        engine_fixture::checkpoint_fixture_options(),
    )?;
    assert!(matches!(
        store.scrub_status(),
        norn_cell::ScrubStatus::Verifying {
            target: V7_COMMITS,
            history_ready: false,
            ..
        }
    ));
    assert_eq!(store.scrub_to_end(), norn_cell::ScrubStatus::Idle);
    let head = store.verified_head()?;
    assert_eq!(head.commits, V7_COMMITS);
    let receipts = store.read().receipts()?;
    assert_eq!(u64::try_from(receipts.len())?, V7_COMMITS);
    assert_v7_replays_and_markings(&mut store, &receipts)?;
    drop(store);

    let mut expected = image;
    add_function_receipt_table(&mut expected);
    apply_v8_marked_fixture_upgrade(&mut expected)?;
    by_name(&mut expected);
    let mut actual = engine_fixture::image_of(&path);
    by_name(&mut actual);
    assert_eq!(
        actual, expected,
        "v8 drops redundant link prefix indexes, backfills nonempty_slots, and restamps"
    );
    assert_eq!(engine_release::admission(&raw(&path))?, Admission::Current);

    let store = CellStore::open(
        &path,
        model.clone(),
        engine_fixture::checkpoint_fixture_options(),
    )?;
    let mut reopened = engine_fixture::image_of(&path);
    by_name(&mut reopened);
    assert_eq!(reopened, actual, "a current open writes nothing");
    assert_eq!(store.scrub_to_end(), norn_cell::ScrubStatus::Idle);
    assert_eq!(store.read().verify_ledger()?, head);
    assert_eq!(store.read().receipts()?, receipts);
    drop(store);

    // Deleting `m2`'s marked row while leaving its backfilled snapshot pin intact must fail closed.
    raw(&path).execute(
        "DELETE FROM _norn_marking_current WHERE world = 'main' AND object_type = 'fixture.MarkedItem' AND subject = (SELECT subject FROM _norn_marking_subject WHERE world = 'main' AND object_type = 'fixture.MarkedItem' AND id = 'm2') AND property = 'secret'",
        &[],
    )?;
    let tampered = CellStore::open(&path, model, StoreOptions::default())?;
    assert_eq!(
        tampered
            .read()
            .effective_markings("main", "fixture.MarkedItem", "m2", None)?,
        None
    );
    Ok(())
}

/// The `_norn_export_journal` rows of `image`: commit, hash and stored text.
fn journal_rows(image: &Value) -> Vec<(u64, String, String)> {
    rows(image, "_norn_export_journal")
        .as_array()
        .expect("journal rows")
        .iter()
        .map(|row| {
            (
                row[1].as_u64().expect("commit"),
                row[2].as_str().expect("hash").to_owned(),
                row[3].as_str().expect("record").to_owned(),
            )
        })
        .collect()
}

/// Storage version 8 → 9 (#1533 Part 2, EXPORT-RESIDUE-1): the genuine
/// storage-v8 fixture opens with only the empty `_norn_export_context` table
/// and the stamp added, its packed journal rows unchanged. Steady-state
/// commits after the upgrade store residues beside them, and the mixed
/// journal exports, verifies and restores with every record hash unchanged.
#[tokio::test]
async fn a_storage_v8_cell_upgrades_and_exports_packed_and_residue_rows_alike()
-> Result<(), Box<dyn std::error::Error>> {
    const V8_COMMITS: u64 = 5;
    let image: Value = serde_json::from_str(STORAGE_V8)?;
    assert_eq!(image["storage"], 8);
    let packed = journal_rows(&image);
    assert_eq!(u64::try_from(packed.len())?, V8_COMMITS);
    assert!(
        packed
            .iter()
            .all(|(.., record)| record.starts_with("norn.cell.payload-storage/1:"))
    );
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("v8.sqlite");
    engine_fixture::load(&image, &path);
    assert_eq!(
        engine_release::admission(&raw(&path))?,
        Admission::Upgrade { from: 8 }
    );
    let model = engine_fixture::v7_marked_model()?;
    let mut store = CellStore::open(
        &path,
        model.clone(),
        engine_fixture::checkpoint_fixture_options(),
    )?;
    assert_eq!(store.scrub_to_end(), norn_cell::ScrubStatus::Idle);
    let receipts = store.read().receipts()?;
    drop(store);

    let expected = upgraded_v8_image(image.clone())?;
    let mut actual = engine_fixture::image_of(&path);
    by_name(&mut actual);
    assert_eq!(
        actual, expected,
        "v9 adds the empty context table, v10 the outbox queue, and the open restamps"
    );

    store = CellStore::open(
        &path,
        model.clone(),
        engine_fixture::checkpoint_fixture_options(),
    )?;
    assert_eq!(store.scrub_to_end(), norn_cell::ScrubStatus::Idle);
    for (commit, hash, _) in &packed {
        let record = store.export_record(*commit)?.ok_or("packed record")?;
        assert_eq!(
            &record.hash()?,
            hash,
            "packed commit {commit} reads as written"
        );
    }
    for (index, qty) in [11_i64, 12].into_iter().enumerate() {
        let mut next = engine_fixture::batches()[1].clone();
        next.idempotency_key = Some(format!("after-v8-upgrade-{index}"));
        next.logical_time = 6_000 + u64::try_from(index)?;
        if let Some(norn_cell::FactWriteRow::Put { values, .. }) = next.writes.first_mut() {
            values.insert("qty".to_owned(), norn_ir::RuntimeValue::from(qty));
        }
        assert!(matches!(store.commit(&next)?, CommitOutcome::Committed(_)));
    }
    let head = store.export_commit_count()?;
    assert_eq!(head, V8_COMMITS + 2);
    let mut exporter = Exporter::local(&dir.path().join("export"))?;
    export_all(&store, &mut exporter).await;
    let receipts = [receipts, store.read().receipts()?.split_off(5)].concat();
    drop(store);

    let mixed = journal_rows(&engine_fixture::image_of(&path));
    assert_eq!(
        &mixed[..packed.len()],
        packed.as_slice(),
        "no packed row is rewritten"
    );
    for (.., record) in &mixed[packed.len()..] {
        assert!(
            record.starts_with("norn.cell.export-residue/1:"),
            "{record:.80}"
        );
    }
    let manifest = exporter.verify().await?;
    assert_eq!(manifest.head_commit_seq, head);
    assert_restores_hashes(
        dir.path(),
        &mut exporter,
        &manifest,
        model,
        &receipts,
        &mixed,
    )
    .await
}

/// The storage-v8 image as this engine leaves it: restamped, with the empty
/// context table, sorted by name.
fn upgraded_v8_image(mut expected: Value) -> Result<Value, Box<dyn std::error::Error>> {
    let stamp = serde_json::to_string(&EngineStamp::current())?;
    let stamp_row = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .and_then(|rows| rows.iter_mut().find(|row| row[1] == STAMP_KEY))
        .ok_or("v8 fixture missing engine stamp")?;
    stamp_row[2] = stamp.into();
    expected["storage"] = STORAGE_VERSION.into();
    add_export_context_table(&mut expected);
    add_outbox_queue(&mut expected);
    add_outbox_ledger(&mut expected);
    add_source_cursor_tables(&mut expected);
    by_name(&mut expected);
    Ok(expected)
}

/// Restore `exporter`'s verified export and prove every replayed record
/// keeps its original hash and receipt.
async fn assert_restores_hashes(
    dir: &Path,
    exporter: &mut Exporter,
    manifest: &norn_cell::export::ExportManifest,
    model: Model,
    receipts: &[norn_cell::Receipt],
    mixed: &[(u64, String, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    let restored_path = dir.join("restored.sqlite");
    CellStore::restore_from(
        exporter,
        &restored_path,
        model.clone(),
        engine_fixture::SCOPE,
        &manifest.bindings,
        StoreOptions::default(),
    )
    .await?;
    let restored = CellStore::open(&restored_path, model, StoreOptions::default())?;
    assert_eq!(restored.read().receipts()?, receipts);
    drop(restored);
    let replayed = journal_rows(&engine_fixture::image_of(&restored_path));
    assert_eq!(
        replayed
            .iter()
            .map(|(commit, hash, _)| (*commit, hash.clone()))
            .collect::<Vec<_>>(),
        mixed
            .iter()
            .map(|(commit, hash, _)| (*commit, hash.clone()))
            .collect::<Vec<_>>(),
        "a restore replays every record to its original hash"
    );
    Ok(())
}
