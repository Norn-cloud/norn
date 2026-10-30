//! Versioned local payload encoding. Logical events and export/5 envelopes keep
//! their original Norn1 preimages; this module only factors repeated storage.

use std::collections::BTreeMap;

use norn_ir::{RuntimeValue as Value, canonical::HashProfile};
use serde::{Deserialize, Serialize};

use crate::CellError;
use crate::codec::canonical_json;
use crate::engine::{SqlEngine, params};
use crate::read::to_i64;

const PREFIX: &str = "norn.cell.payload-storage/1:";
const MIN_BYTES: usize = 1024;

pub(crate) fn init(engine: &impl SqlEngine) -> Result<(), CellError> {
    engine.execute_batch(
        r#"CREATE TABLE IF NOT EXISTS "_norn_payload" (
        "hash" TEXT NOT NULL PRIMARY KEY, "scope" TEXT NOT NULL,
        "world" TEXT NOT NULL, "body" TEXT NOT NULL
    )"#,
    )?;
    engine.execute_batch(
        r#"CREATE TABLE IF NOT EXISTS "_norn_command_origin" (
        "command_id" TEXT NOT NULL, "generation" INTEGER NOT NULL, "commit_seq" INTEGER NOT NULL,
        PRIMARY KEY ("command_id","generation")
    )"#,
    )?;
    engine.execute_batch(
        r#"CREATE TABLE IF NOT EXISTS "_norn_export_payload" (
        "commit_seq" INTEGER NOT NULL, "ordinal" INTEGER NOT NULL, "hash" TEXT NOT NULL,
        PRIMARY KEY ("commit_seq","ordinal")
    )"#,
    )?;
    engine.execute_batch(
        r#"CREATE TABLE IF NOT EXISTS "_norn_ledger_payload" (
        "seq" INTEGER NOT NULL PRIMARY KEY, "scope" TEXT NOT NULL,
        "world" TEXT NOT NULL, "hash" TEXT NOT NULL
    )"#,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    scope: String,
    world: String,
    hash: String,
}

fn corrupt() -> CellError {
    CellError::LedgerCorrupt {
        seq: 0,
        reason: "scoped payload reference does not verify".to_owned(),
    }
}

fn digest(scope: &str, world: &str, body: &str) -> Result<String, CellError> {
    // The format and both length-delimited identities are part of the address.
    // Body bytes already use the existing Norn1 storage normalization.
    Ok(
        HashProfile::Norn1.hash_canonical_bytes(&norn_ir::canonical::canonical_json_of(&(
            PREFIX, scope, world, body,
        ))?),
    )
}

fn normalized(text: &str) -> Result<String, CellError> {
    Ok(serde_json::from_str(&canonical_json(text)?)?)
}

fn check_scope(engine: &impl SqlEngine, scope: &str) -> Result<(), CellError> {
    let bound: Option<String> = engine.query_optional(
        r#"SELECT "value" FROM "_norn_meta" WHERE "key"='binding:scope'"#,
        &[],
        |row| row.get(0),
    )?;
    if let Some(bound) = bound
        && normalized(&bound)? != normalized(scope)?
    {
        return Err(corrupt());
    }
    Ok(())
}

fn store(
    engine: &impl SqlEngine,
    scope: &str,
    world: &str,
    body: &str,
) -> Result<Reference, CellError> {
    let scope = normalized(scope)?;
    let world = normalized(world)?;
    check_scope(engine, &scope)?;
    let hash = digest(&scope, &world, body)?;
    let prior: Option<(String, String, String)> = engine.query_optional(
        r#"SELECT "scope","world","body" FROM "_norn_payload" WHERE "hash"=?1"#,
        params![hash],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    match prior {
        Some((found_scope, found_world, found))
            if found_scope == scope && found_world == world && found == body => {}
        Some(_) => return Err(corrupt()),
        None => {
            engine.execute(
            r#"INSERT INTO "_norn_payload" ("hash","scope","world","body") VALUES (?1,?2,?3,?4)"#,
            params![hash, scope, world, body],
        )?;
        }
    }
    Ok(Reference { scope, world, hash })
}

fn load(engine: &impl SqlEngine, reference: &Reference) -> Result<Value, CellError> {
    check_scope(engine, &reference.scope)?;
    let stored: Option<(String, String, String)> = engine.query_optional(
        r#"SELECT "scope","world","body" FROM "_norn_payload" WHERE "hash"=?1"#,
        params![reference.hash],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let (scope, world, body) = stored.ok_or_else(corrupt)?;
    if scope != reference.scope
        || world != reference.world
        || digest(&scope, &world, &body)? != reference.hash
    {
        return Err(corrupt());
    }
    Ok(serde_json::from_str(&body)?)
}

pub(crate) fn encode_result(
    engine: &impl SqlEngine,
    scope: &str,
    world: &str,
    value: &Value,
) -> Result<String, CellError> {
    let body = canonical_json(value)?;
    if body.len() < MIN_BYTES {
        return Ok(body);
    }
    Ok(format!(
        "{PREFIX}{}",
        canonical_json(&store(engine, scope, world, &body)?)?
    ))
}

pub(crate) fn remember_command(
    engine: &impl SqlEngine,
    command: &str,
    generation: u64,
    commit: u64,
) -> Result<(), CellError> {
    let prior: Option<i64> = engine.query_optional(r#"SELECT "commit_seq" FROM "_norn_command_origin" WHERE "command_id"=?1 AND "generation"=?2"#,
        params![command, to_i64(generation, "generation")?], |row| row.get(0))?;
    if let Some(prior) = prior {
        if prior != to_i64(commit, "origin commit")? {
            return Err(corrupt());
        }
    } else {
        engine.execute(r#"INSERT INTO "_norn_command_origin" ("command_id","generation","commit_seq") VALUES (?1,?2,?3)"#,
            params![command, to_i64(generation, "generation")?, to_i64(commit, "origin commit")?])?;
    }
    Ok(())
}

pub(crate) fn command_world(
    engine: &impl SqlEngine,
    command: &str,
    generation: u64,
) -> Result<Option<String>, CellError> {
    engine.query_optional(r#"SELECT r."world" FROM "_norn_command_origin" AS o JOIN "_norn_receipt" AS r ON r."commit_seq"=o."commit_seq" WHERE o."command_id"=?1 AND o."generation"=?2"#,
        params![command, to_i64(generation, "generation")?], |row| row.get(0))
}

pub(crate) fn receipt_world(engine: &impl SqlEngine, commit: u64) -> Result<String, CellError> {
    engine.query_row(
        r#"SELECT "world" FROM "_norn_receipt" WHERE "commit_seq"=?1"#,
        params![to_i64(commit, "receipt commit")?],
        |row| row.get(0),
    )
}

pub(crate) fn decode_result(
    engine: &impl SqlEngine,
    encoded: &str,
    world: Option<&str>,
) -> Result<Value, CellError> {
    match encoded.strip_prefix(PREFIX) {
        Some(reference) => {
            let reference: Reference = serde_json::from_str(reference)?;
            if world.map(normalized).transpose()?.as_deref() != Some(reference.world.as_str()) {
                return Err(corrupt());
            }
            load(engine, &reference)
        }
        None => Ok(serde_json::from_str(encoded)?),
    }
}

fn placeholder(reference: &Reference) -> Value {
    norn_ir::runtime_object! { "$nornPayload" => reference.hash.clone() }
}

pub(crate) fn encode_event(
    engine: &impl SqlEngine,
    seq: u64,
    scope: &str,
    world: &str,
    event: &Value,
) -> Result<String, CellError> {
    if event.get("type").and_then(Value::as_str) != Some("command.result") {
        return canonical_json(event);
    }
    let Some(result) = event.get("result") else {
        return canonical_json(event);
    };
    let body = canonical_json(result)?;
    if body.len() < MIN_BYTES {
        return canonical_json(event);
    }
    let reference = store(engine, scope, world, &body)?;
    let mut encoded = event.clone();
    encoded
        .as_object_mut()
        .ok_or_else(corrupt)?
        .insert("result".to_owned(), placeholder(&reference));
    engine.execute(
        r#"INSERT INTO "_norn_ledger_payload" ("seq","scope","world","hash") VALUES (?1,?2,?3,?4)"#,
        params![
            to_i64(seq, "payload event seq")?,
            reference.scope,
            reference.world,
            reference.hash
        ],
    )?;
    canonical_json(&encoded)
}

pub(crate) fn decode_event(
    engine: &impl SqlEngine,
    seq: u64,
    world: &str,
    encoded: &str,
) -> Result<Value, CellError> {
    let reference: Option<Reference> = engine.query_optional(
        r#"SELECT "scope","world","hash" FROM "_norn_ledger_payload" WHERE "seq"=?1"#,
        params![to_i64(seq, "payload event seq")?],
        |row| {
            Ok(Reference {
                scope: row.get(0)?,
                world: row.get(1)?,
                hash: row.get(2)?,
            })
        },
    )?;
    let mut event: Value = serde_json::from_str(encoded)?;
    if let Some(reference) = reference {
        if reference.world != normalized(world)?
            || event.get("type").and_then(Value::as_str) != Some("command.result")
            || event.get("result") != Some(&placeholder(&reference))
        {
            return Err(corrupt());
        }
        event
            .as_object_mut()
            .ok_or_else(corrupt)?
            .insert("result".to_owned(), load(engine, &reference)?);
    }
    Ok(event)
}

// Include the actual referenced byte length before budgeted readers materialize
// an event. The physical placeholder length is conservatively charged as well.
pub(crate) const EVENT_BYTES: &str = r#"(COALESCE(octet_length("event"),0) + COALESCE((
    SELECT octet_length(b."body") FROM "_norn_ledger_payload" AS p
    JOIN "_norn_payload" AS b ON b."hash"=p."hash"
    WHERE p."seq"="_norn_ledger"."seq"
),0))"#;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
enum PathPart {
    Key(String),
    Index(usize),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AtPath {
    path: Vec<PathPart>,
    reference: Reference,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Packed {
    scope: String,
    world: String,
    commit_seq: u64,
    inline: String,
    references: Vec<AtPath>,
}

pub(crate) const EXPORT_BYTES: &str = r#"(octet_length(record) + COALESCE((
    SELECT SUM(octet_length(b."body")) FROM "_norn_export_payload" AS p
    JOIN "_norn_payload" AS b ON b."hash"=p."hash"
    WHERE p."commit_seq"="_norn_export_journal"."commit_seq"
),0))"#;

fn pack(
    engine: &impl SqlEngine,
    scope: &str,
    world: &str,
    value: &Value,
    field: &str,
    path: &mut Vec<PathPart>,
    references: &mut Vec<AtPath>,
) -> Result<Value, CellError> {
    let body = canonical_json(value)?;
    if body.len() >= MIN_BYTES || field == "ddlSql" {
        let hash = digest(scope, world, &body)?;
        let exists = engine
            .query_optional(
                r#"SELECT 1 FROM "_norn_payload" WHERE "hash"=?1 AND "scope"=?2 AND "world"=?3"#,
                params![hash, scope, world],
                |row| row.get::<i64>(0),
            )?
            .is_some();
        if exists || matches!(field, "ddlSql" | "result" | "outcome") {
            references.push(AtPath {
                path: path.clone(),
                reference: store(engine, scope, world, &body)?,
            });
            return Ok(Value::Null);
        }
    }
    match value {
        Value::Object(object) => {
            let mut packed = BTreeMap::new();
            for (key, value) in object {
                path.push(PathPart::Key(key.clone()));
                packed.insert(
                    key.clone(),
                    pack(engine, scope, world, value, key, path, references)?,
                );
                path.pop();
            }
            Ok(Value::Object(packed))
        }
        Value::Array(values) => {
            let mut packed = Vec::new();
            for (index, value) in values.iter().enumerate() {
                path.push(PathPart::Index(index));
                packed.push(pack(engine, scope, world, value, "", path, references)?);
                path.pop();
            }
            Ok(Value::Array(packed))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(value.clone()),
    }
}

fn target<'a>(value: &'a mut Value, path: &[PathPart]) -> Result<&'a mut Value, CellError> {
    let mut value = value;
    for part in path {
        value = match (part, value) {
            (PathPart::Key(key), Value::Object(object)) => {
                object.get_mut(key).ok_or_else(corrupt)?
            }
            (PathPart::Index(index), Value::Array(array)) => {
                array.get_mut(*index).ok_or_else(corrupt)?
            }
            _ => return Err(corrupt()),
        };
    }
    Ok(value)
}

pub(crate) fn encode_export(
    engine: &impl SqlEngine,
    record: &crate::export::ExportRecord,
) -> Result<String, CellError> {
    let value: Value = serde_json::from_str(&canonical_json(record)?)?;
    let scope = normalized(&record.batch.scope)?;
    let world = normalized(&record.batch.world)?;
    let mut references = Vec::new();
    let inline = pack(
        engine,
        &scope,
        &world,
        &value,
        "",
        &mut Vec::new(),
        &mut references,
    )?;
    let packed = Packed {
        scope,
        world,
        commit_seq: record.receipt.body.commit_seq,
        inline: canonical_json(&inline)?,
        references,
    };
    for (ordinal, at) in packed.references.iter().enumerate() {
        engine.execute(r#"INSERT INTO "_norn_export_payload" ("commit_seq","ordinal","hash") VALUES (?1,?2,?3)"#,
            params![to_i64(packed.commit_seq, "payload export seq")?, i64::try_from(ordinal).map_err(|_overflow| corrupt())?, at.reference.hash])?;
    }
    Ok(format!("{PREFIX}{}", canonical_json(&packed)?))
}

pub(crate) fn decode_export(
    engine: &impl SqlEngine,
    encoded: &str,
    expected_commit: u64,
) -> Result<crate::export::ExportRecord, CellError> {
    let Some(encoded) = encoded.strip_prefix(PREFIX) else {
        return Ok(serde_json::from_str(encoded)?);
    };
    let packed: Packed = serde_json::from_str(encoded)?;
    if packed.commit_seq != expected_commit {
        return Err(corrupt());
    }
    check_scope(engine, &packed.scope)?;
    let rows = engine.query(
        r#"SELECT "hash" FROM "_norn_export_payload" WHERE "commit_seq"=?1 ORDER BY "ordinal""#,
        params![to_i64(packed.commit_seq, "payload export seq")?],
    )?;
    let indexed: Vec<String> = rows
        .into_iter()
        .map(|row| crate::engine::SqlRow(row).get(0))
        .collect::<Result<_, _>>()?;
    if indexed
        != packed
            .references
            .iter()
            .map(|at| at.reference.hash.clone())
            .collect::<Vec<_>>()
    {
        return Err(corrupt());
    }
    // The inline JSON is a string, so the local envelope adds no nesting to
    // legacy JSON payloads. Validate all paths before loading any large body.
    let mut value: Value = serde_json::from_str(&packed.inline)?;
    let mut seen = std::collections::BTreeSet::new();
    for at in &packed.references {
        if at.reference.scope != packed.scope
            || at.reference.world != packed.world
            || !seen.insert(at.path.clone())
            || *target(&mut value, &at.path)? != Value::Null
        {
            return Err(corrupt());
        }
    }
    for at in &packed.references {
        *target(&mut value, &at.path)? = load(engine, &at.reference)?;
    }
    let record: crate::export::ExportRecord =
        norn_ir::runtime::decode(&value).map_err(CellError::Encoding)?;
    if record.batch.scope != packed.scope
        || record.batch.world != packed.world
        || record.receipt.body.commit_seq != packed.commit_seq
    {
        return Err(corrupt());
    }
    Ok(record)
}

#[cfg(all(test, feature = "native"))]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test assertions"
    )]
    use super::*;
    use crate::{CellStore, CommitBatch, Consumption, EffectRow, MAIN_WORLD, Model, OutboxCommand};

    fn batch(key: &str) -> CommitBatch {
        CommitBatch {
            scope: "org_demo/dev".to_owned(),
            world: MAIN_WORLD.to_owned(),
            logical_time: 100,
            idempotency_key: Some(key.to_owned()),
            ..CommitBatch::default()
        }
    }

    fn result_store() -> (CellStore, Value) {
        let mut store =
            CellStore::open_in_memory(Model::from_definitions(&[], &[]).expect("model"))
                .expect("store");
        store.bind("scope", "org_demo/dev").expect("binding");
        let mut enqueue = batch("enqueue");
        enqueue.effects.push(EffectRow::Enqueue {
            command: OutboxCommand {
                command_id: "read-1".to_owned(),
                generation: 0,
                idempotency_key: "read-1".to_owned(),
                deadline: None,
                command: norn_ir::runtime_object! { "kind" => "read" },
            },
        });
        store.commit(&enqueue).expect("enqueue");
        let outcome =
            norn_ir::runtime_object! { "body" => "payload-marker".repeat(10_000), "ok" => true };
        assert!(
            store
                .record_performed("read-1", 0, &outcome)
                .expect("result")
        );
        assert_eq!(
            store
                .read()
                .head_command()
                .expect("pending")
                .expect("command")
                .result,
            Some(outcome.clone())
        );
        let mut consume = batch("consume");
        consume.consumes = Some(Consumption::Result {
            command_id: "read-1".to_owned(),
            generation: 0,
        });
        consume.events.push(
            norn_ir::runtime_object! { "type" => "command.result", "result" => outcome.clone() },
        );
        store.commit(&consume).expect("consume");
        (store, outcome)
    }

    #[test]
    fn results_events_and_journal_share_one_scoped_body_and_one_ddl() {
        let (store, outcome) = result_store();
        let body = canonical_json(&outcome).expect("body");
        let count: i64 = store
            .conn
            .query_row(
                r#"SELECT COUNT(*) FROM "_norn_payload" WHERE "body"=?1"#,
                params![body],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 1);
        let event_bytes: i64 = store
            .conn
            .query_row(
                r#"SELECT SUM(octet_length("event")) FROM "_norn_ledger""#,
                &[],
                |row| row.get(0),
            )
            .expect("event bytes");
        assert!(event_bytes < 1024);
        let journal_bytes: i64 = store
            .conn
            .query_row(
                r#"SELECT SUM(octet_length("record")) FROM "_norn_export_journal""#,
                &[],
                |row| row.get(0),
            )
            .expect("journal bytes");
        assert!(
            journal_bytes < 20_000,
            "journal must contain references: {journal_bytes}"
        );
        let ddl = canonical_json(&store.read().model().schema().to_sql()).expect("ddl");
        let ddl_count: i64 = store
            .conn
            .query_row(
                r#"SELECT COUNT(*) FROM "_norn_payload" WHERE "body"=?1"#,
                params![ddl],
                |row| row.get(0),
            )
            .expect("ddl count");
        assert_eq!(ddl_count, 1);
        assert_eq!(
            store.read().runs("read-1").expect("runs")[0].outcome,
            outcome
        );
        assert_eq!(
            store.read().ledger(0).expect("events")[0]
                .event
                .get("result"),
            Some(&outcome)
        );
        let record = store.export_record(2).expect("export").expect("record");
        assert_eq!(record.entries[0].event.get("result"), Some(&outcome));
        assert_eq!(record.format, crate::export::EXPORT_FORMAT);
        store
            .read()
            .verify_ledger()
            .expect("logical hashes unchanged");
        // Old inline journal history remains readable alongside references.
        store
            .conn
            .execute(
                r#"UPDATE "_norn_export_journal" SET "record"=?1 WHERE "commit_seq"=2"#,
                params![canonical_json(&record).expect("legacy bytes")],
            )
            .expect("legacy fixture");
        assert_eq!(store.export_record(2).expect("inline export"), Some(record));
    }

    #[test]
    fn missing_changed_and_cross_scope_payloads_fail_closed() {
        let (store, outcome) = result_store();
        let hash = digest(
            "org_demo/dev",
            MAIN_WORLD,
            &canonical_json(&outcome).expect("body"),
        )
        .expect("hash");
        store
            .conn
            .execute(
                r#"UPDATE "_norn_payload" SET "world"='other' WHERE "hash"=?1"#,
                params![hash],
            )
            .expect("tamper world");
        assert!(store.read().runs("read-1").is_err());
        store
            .conn
            .execute(
                r#"UPDATE "_norn_payload" SET "world"='main',"body"='{}' WHERE "hash"=?1"#,
                params![hash],
            )
            .expect("tamper bytes");
        assert!(store.read().ledger(0).is_err());
        store
            .conn
            .execute(
                r#"DELETE FROM "_norn_payload" WHERE "hash"=?1"#,
                params![hash],
            )
            .expect("remove fixture payload");
        assert!(store.export_record(2).is_err());
    }

    #[test]
    fn references_cannot_hide_payload_bytes_from_history_budgets() {
        let (store, _) = result_store();
        let limits = crate::reactive::ReactiveLimits {
            max_candidate_bytes_per_commit: 1024,
            ..crate::reactive::ReactiveLimits::from_profile(
                &norn_ir::ResourceLimitProfile::norn_limits_v1(),
            )
        };
        let mut budget = crate::reactive::DecodeBudget::new(&limits);
        assert!(
            store
                .read()
                .ledger_page_with_budget(0, 1, &mut budget)
                .expect("budget")
                .is_none()
        );
        // Every occurrence is charged, including four references to one body.
        let bytes = crate::export::record_decode_bytes(&store.conn, 2)
            .expect("logical bytes")
            .expect("journal record");
        assert!(bytes > 400_000);
        store
            .conn
            .execute(
                r#"DELETE FROM "_norn_export_payload" WHERE "commit_seq"=2"#,
                &[],
            )
            .expect("remove length index");
        assert!(
            store.export_record(2).is_err(),
            "missing length bindings must refuse before loading bodies"
        );
    }
    #[test]
    fn result_references_must_match_the_commands_world() {
        let (store, outcome) = result_store();
        let other = encode_result(&store.conn, "org_demo/dev", "other", &outcome)
            .expect("valid other-world reference");
        store
            .conn
            .execute(
                r#"UPDATE "_norn_run" SET "outcome"=?1 WHERE "command_id"='read-1'"#,
                params![other],
            )
            .expect("substitute fixture reference");
        assert!(store.read().runs("read-1").is_err());
    }

    #[test]
    fn journal_sequence_is_bound_before_decoding_referenced_content() {
        let (store, _) = result_store();
        let encoded: String = store
            .conn
            .query_row(
                r#"SELECT "record" FROM "_norn_export_journal" WHERE "commit_seq"=2"#,
                &[],
                |row| row.get(0),
            )
            .expect("packed record");
        let mut packed: Packed =
            serde_json::from_str(encoded.strip_prefix(PREFIX).expect("version prefix"))
                .expect("packed manifest");
        packed.inline = "not JSON".to_owned();
        let encoded = format!(
            "{PREFIX}{}",
            serde_json::to_string(&packed).expect("fixture")
        );
        assert!(
            matches!(
                decode_export(&store.conn, &encoded, 1),
                Err(CellError::LedgerCorrupt { .. })
            ),
            "foreign row must refuse before parsing inline JSON"
        );
        assert!(
            matches!(
                decode_export(&store.conn, &encoded, 2),
                Err(CellError::Json(_))
            ),
            "matching sequence reaches the intentionally malformed inline JSON"
        );
    }

    #[test]
    fn local_reference_envelopes_preserve_the_admitted_json_nesting_depth() {
        let mut store =
            CellStore::open_in_memory(Model::from_definitions(&[], &[]).expect("model"))
                .expect("store");
        let mut nested = Value::Bool(true);
        for _ in 0..70 {
            nested = Value::Array(vec![nested]);
        }
        let mut input = batch("nested");
        input
            .events
            .push(norn_ir::runtime_object! { "type" => "nested", "body" => nested });
        store.commit(&input).expect("nested event commits");
        let record = store
            .export_record(1)
            .expect("nested journal decodes")
            .expect("record");
        assert_eq!(record.batch, input);
        assert_eq!(record.entries[0].event, record.batch.events[0]);
    }

    #[test]
    fn export_rebuild_interns_results_again_without_expanding_stored_copies() {
        let (store, outcome) = result_store();
        let records = (1..=2)
            .map(|seq| store.export_record(seq).expect("export").expect("record"))
            .collect::<Vec<_>>();
        let rebuilt = CellStore::rebuild_export_history(
            crate::engine::NativeEngine::open_in_memory().expect("engine"),
            &records,
            &records[1].hash().expect("head hash"),
        )
        .expect("verified replay");
        assert_eq!(
            rebuilt.read().runs("read-1").expect("restored runs")[0].outcome,
            outcome
        );
        let body = canonical_json(&outcome).expect("body");
        let count: i64 = rebuilt
            .conn
            .query_row(
                r#"SELECT COUNT(*) FROM "_norn_payload" WHERE "body"=?1"#,
                params![body],
                |row| row.get(0),
            )
            .expect("body copies");
        assert_eq!(count, 1);
        let raw: String = rebuilt
            .conn
            .query_row(
                r#"SELECT "outcome" FROM "_norn_run" WHERE "command_id"='read-1'"#,
                &[],
                |row| row.get(0),
            )
            .expect("stored outcome");
        assert!(raw.starts_with(PREFIX));
    }

    #[test]
    fn decomposed_scope_preserves_the_existing_nfc_journal_behavior() {
        let mut store =
            CellStore::open_in_memory(Model::from_definitions(&[], &[]).expect("model"))
                .expect("store");
        let scope = "org_e\u{301}/dev";
        store.bind("scope", scope).expect("binding");
        let mut input = batch("unicode-scope");
        input.scope = scope.to_owned();
        input
            .events
            .push(norn_ir::runtime_object! { "type" => "unicode.scope" });
        store.commit(&input).expect("existing scope commits");
        let record = store
            .export_record(1)
            .expect("normalized journal decodes")
            .expect("record");
        assert_eq!(record.batch.scope, "org_é/dev");
        assert_eq!(
            record.batch.scope,
            normalized(scope).expect("canonical scope")
        );
        store.read().verify_ledger().expect("unchanged chain");
    }

    #[test]
    fn missing_ledger_bindings_refuse_without_reinterpreting_legacy_lookalikes() {
        let (store, _) = result_store();
        store
            .conn
            .execute(r#"DELETE FROM "_norn_ledger_payload" WHERE "seq"=1"#, &[])
            .expect("remove fixture index");
        assert!(store.read().ledger(0).is_err());
        let mut legacy =
            CellStore::open_in_memory(Model::from_definitions(&[], &[]).expect("model"))
                .expect("store");
        let mut input = batch("literal-marker");
        input.events.push(norn_ir::runtime_object! { "type" => "command.result", "result" => norn_ir::runtime_object! { "$nornPayload" => "literal-user-data" } });
        legacy.commit(&input).expect("literal event");
        assert_eq!(
            legacy
                .read()
                .ledger(0)
                .expect("literal event remains readable")[0]
                .event,
            input.events[0]
        );
    }
    #[test]
    fn scoped_payloads_refuse_a_foreign_batch_without_partial_writes() {
        let mut store =
            CellStore::open_in_memory(Model::from_definitions(&[], &[]).expect("model"))
                .expect("store");
        store.bind("scope", "org_demo/dev").expect("binding");
        let mut input = batch("foreign-scope");
        input.scope = "org_other/dev".to_owned();
        input.events.push(norn_ir::runtime_object! { "type" => "command.result", "result" => norn_ir::runtime_object! { "body" => "x".repeat(2_000) } });
        assert!(store.commit(&input).is_err());
        assert_eq!(
            store.read().verify_ledger().expect("unchanged ledger").seq,
            0
        );
        let rows: i64 = store
            .conn
            .query_row(r#"SELECT COUNT(*) FROM "_norn_payload""#, &[], |row| {
                row.get(0)
            })
            .expect("content count");
        assert_eq!(rows, 0);
    }
}
