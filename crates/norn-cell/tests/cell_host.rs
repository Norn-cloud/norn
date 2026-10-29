#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! T4-CELL-2 (#2676): the native `CellHost` test driver over rusqlite.
//!
//! - The host-agnostic `CellHost` laws (`norn_testkit::cell_host`, T4-CELL-1)
//!   hold for [`SqliteCell`] over an in-memory store.
//! - Deterministic fixtures: commit and enqueue are atomic (a refused commit
//!   writes nothing); a timer is delivered when due, consumed once, and a
//!   stale incarnation is refused; a performed result is recorded once and
//!   survives a reopen without re-executing.
//! - Reopening the file recovers the ledger hash chain identically, and the
//!   same scenario run twice produces byte-identical stores.

mod common;

use std::cell::Cell as FlagCell;
use std::fmt::Debug;
use std::path::{Path, PathBuf};

use norn_cell::engine::{
    DefaultEngine, MAX_PARAMETERS, MAX_SAFE_INTEGER, MAX_STATEMENT_BYTES, NativeEngine, SqlEngine,
    SqlValue, validate_bindings, validate_value,
};
use norn_cell::{CellError, CellStore, JournalMode, Model, PerformClaim, SqliteCell, StoreOptions};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, CommitError, CommitReceipt, Consumed, DueTimer,
    Effect, FactKey, FactMutation, FactState, FactVersion, FactWrite, FreshnessPolicy, Generation,
    IdempotencyKey, LedgerSeq, LinkKey, LinkMutation, LinkReadExpectation, LinkState, LinkWrite,
    LogicalTime, Observed, OwnedRowsCommitProof, Performed, Timer, TimerId,
};
use norn_testkit::Settings;
use norn_testkit::cell_host::{
    CELL_HOST_LABELS, LawCommand, LawEvent, LawInput, LedgerEntry, SimulatedCell, check_cell_host,
    check_link_override, law_responder, override_object_types,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

// ------------------------------------------------------------------ laws

/// `LawInput` with serde, so the SQLite driver can store it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum WireInput {
    Timer(u8),
    Result(u8),
    Observed(u8),
}

impl From<&LawInput> for WireInput {
    fn from(input: &LawInput) -> Self {
        match input {
            LawInput::Timer(value) => Self::Timer(*value),
            LawInput::Result(value) => Self::Result(*value),
            LawInput::Observed(value) => Self::Observed(*value),
        }
    }
}

impl From<WireInput> for LawInput {
    fn from(input: WireInput) -> Self {
        match input {
            WireInput::Timer(value) => Self::Timer(value),
            WireInput::Result(value) => Self::Result(value),
            WireInput::Observed(value) => Self::Observed(value),
        }
    }
}

fn law_timer(timer: Timer<WireInput>) -> Timer<LawInput> {
    Timer {
        id: timer.id,
        due: timer.due,
        payload: timer.payload.into(),
    }
}

fn law_due(due: DueTimer<WireInput>) -> DueTimer<LawInput> {
    DueTimer {
        timer: law_timer(due.timer),
        incarnation: due.incarnation,
    }
}

/// The law types (`norn_testkit::cell_host::law_object_types`) as a store
/// model: `law.Thing` with its `v` property and its `rel` and `owner` links,
/// and `law.Child`.
fn law_model() -> Model {
    common::model_of(&[
        serde_json::from_value(json!({
            "name": "law.Thing",
            "properties": [{ "id": "v", "type": { "kind": "integer" } }],
            "links": [
                { "id": "rel", "target": "law.Thing", "cardinality": { "to": "many", "from": "many" } },
                { "id": "owner", "target": "law.Thing", "cardinality": { "to": "one", "from": "many" } }
            ]
        }))
        .expect("decodes"),
        serde_json::from_value(json!({ "name": "law.Child", "extends": "law.Thing" }))
            .expect("decodes"),
    ])
}

type LawCell<S> = SqliteCell<LawEvent, LawCommand, WireInput, S>;

fn law_cell_responder(command: &Command<LawCommand>) -> WireInput {
    WireInput::from(&law_responder(command))
}

trait LawEngine: SqlEngine + Debug + Sized + 'static {
    fn open_file(path: &Path, options: StoreOptions) -> Result<Self, CellError>;
    fn open_memory() -> Result<Self, CellError>;
}

impl LawEngine for NativeEngine {
    fn open_file(path: &Path, options: StoreOptions) -> Result<Self, CellError> {
        NativeEngine::open(path, options)
    }

    fn open_memory() -> Result<Self, CellError> {
        NativeEngine::open_in_memory()
    }
}

/// SQL subset available to a DO SQLite host. It delegates execution to the
/// native engine while rejecting native-only controls and reserved names.
#[derive(Debug)]
struct RestrictedEngine {
    native: NativeEngine,
    in_transaction: FlagCell<bool>,
}

const MAX_RESULT_ROW_BYTES: usize = 2_097_152;

impl RestrictedEngine {
    fn new(native: NativeEngine) -> Self {
        Self {
            native,
            in_transaction: FlagCell::new(false),
        }
    }
}

impl LawEngine for RestrictedEngine {
    fn open_file(path: &Path, options: StoreOptions) -> Result<Self, CellError> {
        NativeEngine::open(path, options).map(Self::new)
    }

    fn open_memory() -> Result<Self, CellError> {
        NativeEngine::open_in_memory().map(Self::new)
    }
}

impl SqlEngine for RestrictedEngine {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        validate_do_sql(sql, params)?;
        self.native.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        validate_do_sql(sql, params)?;
        let rows = self.native.query(sql, params)?;
        for row in &rows {
            let bytes = row.iter().try_fold(0_usize, |total, value| {
                validate_value(value)?;
                let size = match value {
                    SqlValue::Null => 0,
                    SqlValue::Integer(_) | SqlValue::Real(_) => 8,
                    SqlValue::Text(text) => text.len(),
                    SqlValue::Blob(blob) => blob.len(),
                };
                total
                    .checked_add(size)
                    .ok_or(CellError::OutOfRange("SQL result row bytes"))
            })?;
            if bytes > MAX_RESULT_ROW_BYTES {
                return Err(CellError::OutOfRange("SQL result row bytes"));
            }
        }
        Ok(rows)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        validate_do_sql(sql, &[])?;
        self.native.execute_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        struct Reset<'a>(&'a FlagCell<bool>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        if self.in_transaction.replace(true) {
            return Err(CellError::Engine("nested transaction refused".to_owned()).into());
        }
        let reset = Reset(&self.in_transaction);
        let result = self.native.transaction(f);
        drop(reset);
        result
    }
}

/// Tokens and quoted identifiers are inspected; comments and string literals
/// are skipped so they cannot accidentally hide a forbidden operation.
fn validate_do_sql(sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
    const FORBIDDEN: &[&str] = &[
        "begin",
        "commit",
        "rollback",
        "savepoint",
        "release",
        "pragma",
        "attach",
        "detach",
        "vacuum",
    ];
    validate_bindings(sql, params)?;
    let tokens = sql_tokens(sql);
    for (index, token) in tokens.iter().enumerate() {
        let Some(word) = token.strip_prefix("word:") else {
            continue;
        };
        if FORBIDDEN.contains(&word) || word.starts_with("_cf_") {
            return Err(CellError::Engine(format!("restricted SQL token `{word}`")));
        }
        if matches!(word, "changes" | "last_insert_rowid")
            && tokens
                .get(index.saturating_add(1))
                .is_some_and(|next| next == "(")
        {
            return Err(CellError::Engine(format!(
                "restricted SQL function `{word}`"
            )));
        }
    }
    Ok(())
}

/// Return SQL words, quoted identifiers and punctuation, ignoring comments
/// and string literals.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "the test-only SQL lexer bounds-checks every byte offset"
)]
fn sql_tokens(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'\'' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\'' {
                        index += 1;
                        if bytes.get(index) == Some(&b'\'') {
                            index += 1;
                        } else {
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
            }
            quote @ (b'"' | b'`' | b'[') => {
                let close = if quote == b'[' { b']' } else { quote };
                index += 1;
                let start = index;
                while index < bytes.len() {
                    if bytes[index] == close {
                        if bytes.get(index + 1) == Some(&close) {
                            index += 2;
                            continue;
                        }
                        break;
                    }
                    index += 1;
                }
                let end = index.min(bytes.len());
                let identifier = String::from_utf8_lossy(&bytes[start..end]).to_ascii_lowercase();
                tokens.push(format!("word:{identifier}"));
                if index < bytes.len() {
                    index += 1;
                }
            }
            byte if byte.is_ascii_alphanumeric() || byte == b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                let word = String::from_utf8_lossy(&bytes[start..index]).to_ascii_lowercase();
                tokens.push(format!("word:{word}"));
            }
            byte => {
                tokens.push(char::from(byte).to_string());
                index += 1;
            }
        }
    }
    tokens
}

fn law_cell<S: LawEngine>(engine: S) -> Result<LawCell<S>, CellError> {
    let store = CellStore::from_engine(engine, law_model())?;
    SqliteCell::new(store, "law", law_cell_responder)
}

/// The SQL driver over the law vocabulary. With `REOPEN`, the store is a file
/// closed and reopened after every operation that writes, verifying the ledger.
struct LawSqlite<const REOPEN: bool, S: LawEngine = DefaultEngine> {
    cell: LawCell<S>,
    file: Option<(tempfile::TempDir, PathBuf)>,
}

impl<const REOPEN: bool, S: LawEngine> LawSqlite<REOPEN, S> {
    fn after_write(&mut self) {
        if let Some((_, path)) = &self.file {
            let now = self.cell.now();
            self.cell = law_cell(S::open_file(path, StoreOptions::default()).expect("reopens"))
                .expect("rebuilds store");
            self.cell.advance_to(now);
        }
    }
}

impl<const REOPEN: bool, S: LawEngine> CellHost for LawSqlite<REOPEN, S> {
    type Event = LawEvent;
    type Command = LawCommand;
    type Input = LawInput;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.cell.load(key)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.cell.load_link(key)
    }

    fn commit(
        &mut self,
        commit: CellCommit<LawEvent, LawCommand, LawInput>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let effects = commit
            .effects
            .into_iter()
            .map(|effect| match effect {
                Effect::Perform(command) => Effect::Perform(command),
                Effect::Schedule(timer) => Effect::Schedule(Timer {
                    id: timer.id,
                    due: timer.due,
                    payload: WireInput::from(&timer.payload),
                }),
                Effect::CancelTimer(id) => Effect::CancelTimer(id),
            })
            .collect();
        let receipt = self.cell.commit(CellCommit {
            consumes: commit.consumes,
            reads: commit.reads,
            writes: commit.writes,
            events: commit.events,
            effects,
            links: commit.links,
            link_reads: commit.link_reads,
        });
        self.after_write();
        receipt
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<LawEvent, LawCommand, LawInput>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let effects = commit
            .effects
            .into_iter()
            .map(|effect| match effect {
                Effect::Perform(command) => Effect::Perform(command),
                Effect::Schedule(timer) => Effect::Schedule(Timer {
                    id: timer.id,
                    due: timer.due,
                    payload: WireInput::from(&timer.payload),
                }),
                Effect::CancelTimer(id) => Effect::CancelTimer(id),
            })
            .collect();
        let receipt = self.cell.commit_owned_rows(
            CellCommit {
                consumes: commit.consumes,
                reads: commit.reads,
                writes: commit.writes,
                events: commit.events,
                effects,
                links: commit.links,
                link_reads: commit.link_reads,
            },
            proof,
        );
        self.after_write();
        receipt
    }

    fn perform(&mut self) -> Result<Option<Performed<LawInput>>, CellError> {
        let performed = self.cell.perform()?.map(|performed| Performed {
            command: performed.command,
            generation: performed.generation,
            input: performed.input.into(),
        });
        self.after_write();
        Ok(performed)
    }

    fn observe(&mut self) -> Result<Option<Observed<LawInput>>, CellError> {
        Ok(self.cell.observe()?.map(|observed| Observed {
            id: observed.id,
            provenance: observed.provenance,
            input: observed.input.into(),
        }))
    }

    fn clock(&mut self) -> Result<Option<DueTimer<LawInput>>, CellError> {
        Ok(self.cell.clock()?.map(law_due))
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.cell.publish(receipt)
    }
}

impl<const REOPEN: bool, S: LawEngine> SimulatedCell for LawSqlite<REOPEN, S> {
    const COMMITS_PERFORM_CLAIMS: bool = true;

    fn sim_new() -> Self {
        if REOPEN {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("cell.sqlite");
            let cell = law_cell(S::open_file(&path, StoreOptions::default()).expect("opens"))
                .expect("opens store");
            Self {
                cell,
                file: Some((dir, path)),
            }
        } else {
            Self {
                cell: law_cell(S::open_memory().expect("opens engine")).expect("opens store"),
                file: None,
            }
        }
    }

    fn sim_advance_to(&mut self, now: LogicalTime) {
        self.cell.advance_to(now);
    }

    fn sim_inject(&mut self, observed: Observed<LawInput>) {
        self.cell
            .inject(&Observed {
                id: observed.id,
                provenance: observed.provenance,
                input: WireInput::from(&observed.input),
            })
            .expect("ingests");
        self.after_write();
    }

    fn sim_executions(&self) -> u64 {
        self.cell.executions().expect("counts")
    }

    fn sim_outbox(&self) -> Vec<Command<LawCommand>> {
        self.cell.pending_commands().expect("outbox")
    }

    fn sim_timers(&self) -> Vec<DueTimer<LawInput>> {
        self.cell
            .pending_timers()
            .expect("timers")
            .into_iter()
            .map(law_due)
            .collect()
    }

    fn sim_ledger(&self) -> Vec<LedgerEntry<LawEvent>> {
        self.cell
            .commits()
            .expect("ledger")
            .into_iter()
            .map(|commit| LedgerEntry {
                seq: commit.seq,
                consumed: commit.consumed,
                written: commit.written,
                linked: commit.linked,
                events: commit.events,
            })
            .collect()
    }
}

#[test]
fn cell_host_laws_hold_for_the_sqlite_driver() {
    let coverage = check_cell_host::<LawSqlite<false>>(&Settings::new(file!()).cases(256));
    coverage.assert_covered(CELL_HOST_LABELS);
}

#[test]
fn cell_host_laws_hold_across_reopen() {
    let coverage = check_cell_host::<LawSqlite<true>>(&Settings::new(file!()).cases(256));
    coverage.assert_covered(CELL_HOST_LABELS);
}

#[test]
fn cell_host_laws_hold_for_the_restricted_engine() {
    let coverage =
        check_cell_host::<LawSqlite<false, RestrictedEngine>>(&Settings::new(file!()).cases(256));
    coverage.assert_covered(CELL_HOST_LABELS);
}

#[test]
fn cell_host_laws_hold_across_reopen_for_the_restricted_engine() {
    let coverage =
        check_cell_host::<LawSqlite<true, RestrictedEngine>>(&Settings::new(file!()).cases(256));
    coverage.assert_covered(CELL_HOST_LABELS);
}

#[test]
fn restricted_sql_rejects_forbidden_tokens_and_reserved_identifiers() {
    for sql in [
        "BEGIN",
        "cOmMiT TRANSACTION",
        "/* safe */ rollback",
        "SAVEPOINT nested",
        "release nested",
        "PRAGMA foreign_keys",
        "ATTACH DATABASE 'x' AS x",
        "detach database x",
        "vacuum",
        "SELECT changes()",
        "SELECT last_insert_rowid ()",
        "SELECT 1; -- next statement\n PRAGMA cache_size",
        "CREATE TABLE \"_CF_private\" (id INTEGER)",
    ] {
        assert!(validate_do_sql(sql, &[]).is_err(), "accepted `{sql}`");
        let batch = format!("SELECT 1; {sql}");
        assert!(
            validate_do_sql(&batch, &[]).is_err(),
            "accepted forbidden SQL after a valid statement: `{sql}`"
        );
    }
    assert!(validate_do_sql("SELECT 'PRAGMA changes() _cf_hidden'", &[]).is_ok());
    assert!(validate_do_sql("SELECT 1 /* ATTACH _cf_hidden */", &[]).is_ok());
}

#[test]
fn restricted_engine_enforces_sql_limits_and_nested_transaction_rules() {
    let engine = RestrictedEngine::open_memory().expect("opens");
    assert!(validate_do_sql(&" ".repeat(MAX_STATEMENT_BYTES.saturating_add(1)), &[]).is_err());
    let params = vec![SqlValue::Null; MAX_PARAMETERS.saturating_add(1)];
    assert!(validate_do_sql("SELECT 1", &params).is_err());
    assert!(
        validate_do_sql(
            "SELECT ?1",
            &[SqlValue::Integer(MAX_SAFE_INTEGER.saturating_add(1))],
        )
        .is_err()
    );
    let oversized = SqlValue::from("x".repeat(MAX_RESULT_ROW_BYTES.saturating_add(1)));
    assert!(engine.query("SELECT ?1", &[oversized]).is_err());

    let nested = engine.transaction(|| {
        engine.transaction(|| Ok::<_, CellError>(()))?;
        Ok::<_, CellError>(())
    });
    assert!(nested.is_err());
}

// ------------------------------------------------------------------ fixtures

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum OrderEvent {
    Placed { order: String },
    Expired { order: String },
    Charged { order: String, ok: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Charge {
    order: String,
    amount: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum OrderInput {
    Expire { order: String },
    Charged { order: String, ok: bool },
}

type OrderCell = SqliteCell<OrderEvent, Charge, OrderInput>;
type OrderCommit = CellCommit<OrderEvent, Charge, OrderInput>;

fn order_model() -> Model {
    common::model_of(&[serde_json::from_value(json!({
        "name": "shop.Order",
        "properties": [
            { "id": "status", "type": { "kind": "enum", "values": [
                { "value": "placed" }, { "value": "paid" }, { "value": "expired" }
            ] } },
            { "id": "total", "type": { "kind": "decimal", "scale": 2 } }
        ]
    }))
    .expect("decodes")])
}

fn charge_responder(command: &Command<Charge>) -> OrderInput {
    OrderInput::Charged {
        order: command.payload.order.clone(),
        ok: true,
    }
}

fn open(path: &Path) -> OrderCell {
    SqliteCell::open(
        path,
        order_model(),
        StoreOptions {
            journal_mode: JournalMode::Wal,
            cache_pages: None,
        },
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens")
}

fn order_key() -> FactKey {
    FactKey::new("shop.Order", "O-1")
}

fn order(status: &str) -> JsonObject {
    serde_json::from_value(json!({ "status": status, "total": 19.99 })).expect("values")
}

/// Turn 1: place the order, charge it, and arm its expiry timer — in one
/// commit.
fn place() -> OrderCommit {
    CellCommit {
        writes: vec![FactWrite {
            key: order_key(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: order("placed"),
            },
        }],
        events: vec![OrderEvent::Placed {
            order: "O-1".to_owned(),
        }],
        effects: vec![
            Effect::Perform(Command {
                id: CommandId("charge/O-1".to_owned()),
                generation: Generation(0),
                idempotency_key: IdempotencyKey("charge/O-1/0".to_owned()),
                freshness: FreshnessPolicy::AppendOnly,
                deadline: Some(LogicalTime::from_millis(5_000)),
                payload: Charge {
                    order: "O-1".to_owned(),
                    amount: "19.99".to_owned(),
                },
            }),
            Effect::Schedule(Timer {
                id: TimerId("expire/O-1".to_owned()),
                due: LogicalTime::from_millis(60_000),
                payload: OrderInput::Expire {
                    order: "O-1".to_owned(),
                },
            }),
        ],
        ..CellCommit::empty()
    }
}

#[test]
fn sqlite_cell_persists_the_effective_default_from_a_whole_object_put() {
    let object_type: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.EffectiveDefault",
        "properties": [{
            "id": "amount",
            "type": { "kind": "integer" },
            "constraints": { "nullable": true },
            "default": "-1"
        }]
    }))
    .expect("definition decodes");
    let model = common::model_of(&[object_type]);
    let mut cell: OrderCell =
        SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens");
    let key = FactKey::new("test.EffectiveDefault", "negative");
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("persists a put with its effective default");
    assert_eq!(
        cell.load(&key).expect("loads").values,
        Some(serde_json::from_value(json!({ "amount": -1 })).expect("runtime value"))
    );
}

#[test]
fn sqlite_cell_evaluates_referenced_numeric_defaults_in_storage_encoding() {
    let property_type: norn_ir::PropertyTypeDef = serde_json::from_value(json!({
        "name": "test.Amount",
        "type": { "kind": "integer" }
    }))
    .expect("shared property type decodes");
    for default in [-1, 1] {
        let object_type: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
            "name": "test.ReferencedDefault",
            "properties": [{
                "id": "amount",
                "type": { "kind": "ref", "name": "test.Amount" },
                "default": default.to_string()
            }],
            "invariants": [{
                "id": "not_negative_one",
                "expression": "self.amount != -1",
                "message": "amount cannot be negative one"
            }]
        }))
        .expect("object definition decodes");
        let model = Model::from_definitions(&[object_type], std::slice::from_ref(&property_type))
            .expect("referenced property projects");
        let mut cell: OrderCell =
            SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens");
        let key = FactKey::new("test.ReferencedDefault", "defaulted");
        let result = cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        });
        if default == -1 {
            let error = result.expect_err("numeric equality must reject the negative default");
            assert_eq!(error.code(), "commit.invariant_violation");
            assert!(
                cell.load(&key)
                    .expect("loads refused object")
                    .values
                    .is_none()
            );
        } else {
            result.expect("positive referenced default passes the invariant");
            assert_eq!(
                cell.load(&key).expect("loads accepted default").values,
                Some(serde_json::from_value(json!({ "amount": default })).expect("runtime value"))
            );
        }
    }
}

#[test]
fn sqlite_cell_checks_and_persists_the_same_effective_default_values() {
    let object_type: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.EffectiveDefaultInvariant",
        "properties": [{
            "id": "amount",
            "type": { "kind": "integer" },
            "constraints": { "nullable": true },
            "default": "-1"
        }, {
            "id": "label", "type": { "kind": "text" }, "default": "initial"
        }],
        "invariants": [{
            "id": "nonnegative_amount",
            "expression": "(self.amount ?? 0) >= 0",
            "message": "amount must be nonnegative"
        }]
    }))
    .expect("definition decodes");
    let model = common::model_of(&[object_type]);
    let mut cell: OrderCell =
        SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens");
    let key = FactKey::new("test.EffectiveDefaultInvariant", "negative");

    let omitted = cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect_err("the materialized -1 default violates the invariant");
    assert_eq!(omitted.code(), "commit.invariant_violation");
    assert!(
        cell.load(&key)
            .expect("loads absent object")
            .values
            .is_none()
    );

    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({ "amount": null }))
                    .expect("explicit null runtime value"),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("explicit null satisfies the coalesce invariant");
    assert_eq!(
        cell.load(&key).expect("loads explicit null").values,
        Some(
            serde_json::from_value(json!({ "amount": null, "label": "initial" }))
                .expect("stored null")
        )
    );
    let state = cell
        .load(&key)
        .expect("loads before another property update");
    let mut values = state.values.expect("object is live");
    values.insert(
        "label".to_owned(),
        norn_ir::RuntimeValue::String("updated".to_owned()),
    );
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: state.version,
            mutation: FactMutation::Put { values },
        }],
        ..CellCommit::empty()
    })
    .expect("updating another property preserves the explicit null");
    assert_eq!(
        cell.load(&key).expect("loads updated object").values,
        Some(
            serde_json::from_value(json!({ "amount": null, "label": "updated" }))
                .expect("stored null after update")
        )
    );
}

#[test]
fn sqlite_cell_canonicalizes_raw_numeric_strings_before_invariants_and_storage() {
    let object_type: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.RawNumericStrings",
        "properties": [
            {"id": "item_id", "type": {"kind": "text"}},
            {"id": "amount", "type": {"kind": "integer"}},
            {"id": "rate", "type": {"kind": "decimal", "scale": 2}}
        ],
        "invariants": [
            {
                "id": "amount_not_negative_one",
                "expression": "self.amount != -1",
                "message": "amount cannot be negative one"
            },
            {
                "id": "rate_not_negative_one_point_two_five",
                "expression": "self.rate != -1.25",
                "message": "rate cannot be negative one point two five"
            }
        ]
    }))
    .expect("definition decodes");
    let model = common::model_of(&[object_type]);
    let mut cell: OrderCell =
        SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens");

    for (key, values, invariant_id) in [
        (
            "negative-integer-string",
            json!({"item_id": "negative-integer-string", "amount": "-1", "rate": "1.25"}),
            "amount_not_negative_one",
        ),
        (
            "negative-decimal-string",
            json!({"item_id": "negative-decimal-string", "amount": "4", "rate": "-1.25"}),
            "rate_not_negative_one_point_two_five",
        ),
    ] {
        let key = FactKey::new("test.RawNumericStrings", key);
        let before = cell.store().read().dump().expect("dumps before refusal");
        let refused = cell
            .commit(CellCommit {
                writes: vec![FactWrite {
                    key: key.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: serde_json::from_value(values).expect("runtime input values"),
                    },
                }],
                ..CellCommit::empty()
            })
            .expect_err("invariant sees the number represented by the input string");
        assert!(matches!(
            refused,
            CommitError::InvariantViolation {
                ref id,
                ref outcome,
                ..
            } if id.as_str() == invariant_id
                && matches!(outcome.as_ref(), norn_ir::DataQualityOutcome::NotSatisfied)
        ));
        assert!(
            cell.load(&key)
                .expect("loads absent object")
                .values
                .is_none()
        );
        assert_eq!(
            cell.store().read().dump().expect("dumps after refusal"),
            before
        );
    }

    let key = FactKey::new("test.RawNumericStrings", "positive-numeric-strings");
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({
                    "item_id": "positive-numeric-strings",
                    "amount": "7",
                    "rate": "12.50"
                }))
                .expect("raw numeric string values"),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("valid numeric strings are accepted");
    assert_eq!(
        cell.load(&key).expect("loads canonical values").values,
        Some(
            serde_json::from_value(json!({
                "item_id": "positive-numeric-strings",
                "amount": 7,
                "rate": 12.50
            }))
            .expect("canonical numeric values")
        )
    );
}

#[test]
fn commit_and_enqueue_are_one_transaction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open(&dir.path().join("cell.sqlite"));
    let receipt = cell.commit(place()).expect("commits");
    assert_eq!(receipt.seq, LedgerSeq(1));
    assert_eq!(receipt.written[0].version, FactVersion(1));
    assert_eq!(cell.pending_commands().expect("outbox").len(), 1);
    assert_eq!(cell.pending_timers().expect("timers").len(), 1);
    let commits = cell.commits().expect("ledger");
    assert_eq!(commits.len(), 1);
    assert_eq!(
        commits[0].events,
        [OrderEvent::Placed {
            order: "O-1".to_owned()
        }]
    );

    // The same turn again is stale: CAS refuses it and nothing is written —
    // no fact, no event, no second command, no timer change.
    let before = cell.store().read().dump().expect("dumps");
    let refused = cell.commit(place()).expect_err("stale");
    assert_eq!(refused.code(), "commit.conflict");
    assert_eq!(cell.store().read().dump().expect("dumps"), before);
}

#[test]
fn a_timer_fires_when_due_once_and_a_stale_incarnation_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open(&dir.path().join("cell.sqlite"));
    cell.commit(place()).expect("commits");
    assert_eq!(cell.clock().expect("clock"), None, "not due yet");
    cell.advance_to(LogicalTime::from_millis(60_000));
    let due = cell.clock().expect("clock").expect("due");
    assert_eq!(due.incarnation, LedgerSeq(1));
    assert_eq!(
        cell.clock().expect("clock").as_ref(),
        Some(&due),
        "redelivered until consumed"
    );

    // Re-arm the timer (a new incarnation): the old delivery is stale.
    let rearm = CellCommit {
        effects: vec![Effect::Schedule(due.timer.clone())],
        ..CellCommit::empty()
    };
    cell.commit(rearm).expect("commits");
    let stale = CellCommit::empty().consuming(due.consumed());
    let refused = cell.commit(stale).expect_err("stale incarnation");
    assert!(matches!(refused, CommitError::NotPending { .. }));

    let due = cell.clock().expect("clock").expect("due");
    assert_eq!(due.incarnation, LedgerSeq(2));
    let expire = CellCommit {
        writes: vec![FactWrite {
            key: order_key(),
            expected: FactVersion(1),
            mutation: FactMutation::Put {
                values: order("expired"),
            },
        }],
        events: vec![OrderEvent::Expired {
            order: "O-1".to_owned(),
        }],
        ..CellCommit::empty()
    }
    .consuming(due.consumed());
    cell.commit(expire).expect("commits");
    assert_eq!(cell.clock().expect("clock"), None, "consumed once");
    assert_eq!(
        cell.load(&order_key()).expect("loads").values,
        Some(order("expired"))
    );
}

#[test]
fn a_due_timer_peek_survives_reopen_and_consumes_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut cell = open(&path);
    cell.commit(place()).expect("commits");
    cell.advance_to(LogicalTime::from_millis(60_000));
    let peeked = cell.clock().expect("clock").expect("due timer");
    assert_eq!(peeked.incarnation, LedgerSeq(1));
    drop(cell);

    let mut cell = open(&path);
    cell.advance_to(LogicalTime::from_millis(60_000));
    let due = cell
        .clock()
        .expect("reopened clock")
        .expect("redelivered timer");
    assert_eq!(due, peeked, "the peeked timer remains the same incarnation");
    cell.commit(
        CellCommit {
            writes: vec![FactWrite {
                key: order_key(),
                expected: FactVersion(1),
                mutation: FactMutation::Put {
                    values: order("expired"),
                },
            }],
            events: vec![OrderEvent::Expired {
                order: "O-1".to_owned(),
            }],
            ..CellCommit::empty()
        }
        .consuming(due.consumed()),
    )
    .expect("consume due timer");
    assert_eq!(cell.clock().expect("clock after consume"), None);
    drop(cell);

    let mut cell = open(&path);
    cell.advance_to(LogicalTime::from_millis(60_000));
    assert_eq!(cell.clock().expect("reopened clock after consume"), None);
    assert_eq!(
        cell.load(&order_key()).expect("loads").values,
        Some(order("expired"))
    );
    let expired_events = cell
        .commits()
        .expect("ledger")
        .into_iter()
        .flat_map(|commit| commit.events)
        .filter(|event| matches!(event, OrderEvent::Expired { order } if order.as_str() == "O-1"))
        .count();
    assert_eq!(expired_events, 1, "the timer effect commits once");
}

#[test]
fn a_performed_result_is_recorded_once_and_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut cell = open(&path);
    cell.commit(place()).expect("commits");
    let performed = cell.perform().expect("performs").expect("a command");
    assert_eq!(
        performed.input,
        OrderInput::Charged {
            order: "O-1".to_owned(),
            ok: true
        }
    );
    assert_eq!(cell.executions().expect("runs"), 1);
    drop(cell);

    // A crash before the result is consumed: the reopened cell redelivers the
    // recorded result and does not charge again.
    let mut cell = open(&path);
    assert_eq!(cell.perform().expect("performs"), Some(performed.clone()));
    assert_eq!(cell.executions().expect("runs"), 1);

    let settle = CellCommit {
        writes: vec![FactWrite {
            key: order_key(),
            expected: FactVersion(1),
            mutation: FactMutation::Put {
                values: order("paid"),
            },
        }],
        events: vec![OrderEvent::Charged {
            order: "O-1".to_owned(),
            ok: true,
        }],
        ..CellCommit::empty()
    }
    .consuming(Consumed::Result {
        command: performed.command.clone(),
        generation: performed.generation,
    });
    cell.commit(settle).expect("commits");
    assert_eq!(cell.perform().expect("performs"), None, "consumed");

    // Re-issuing the same generation never re-executes.
    cell.commit(CellCommit {
        effects: vec![place().effects[0].clone()],
        ..CellCommit::empty()
    })
    .expect("commits");
    assert_eq!(cell.perform().expect("performs"), None);
    assert_eq!(cell.executions().expect("runs"), 1);
}

#[test]
fn a_claim_without_a_result_survives_reopen_as_outcome_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut cell = open(&path);
    cell.commit(place()).expect("commits");
    let command = match cell.claim_perform().expect("claims") {
        Some(PerformClaim::Fresh(command)) => command,
        other => panic!("expected fresh claim, got {other:?}"),
    };
    assert!(
        cell.store()
            .read()
            .perform_claimed(&command.id.0, command.generation.0)
            .expect("claim reads")
    );
    drop(cell);

    let mut cell = open(&path);
    assert!(matches!(
        cell.claim_perform().expect("claim reads"),
        Some(PerformClaim::OutcomeUnknown(found)) if found == command
    ));
    assert!(matches!(
        cell.perform(),
        Err(CellError::PerformOutcomeUnknown {
            command_id,
            generation: 0
        }) if command_id == command.id.0
    ));
    let input = OrderInput::Charged {
        order: "O-1".to_owned(),
        ok: false,
    };
    let performed = cell
        .complete_perform(&command, input.clone())
        .expect("completion records")
        .expect("current generation");
    assert_eq!(performed.input, input);
    assert!(matches!(
        cell.claim_perform().expect("redelivers"),
        Some(PerformClaim::Recorded {
            command: found,
            performed: found_result,
        }) if found == command && found_result == performed
    ));
}

#[test]
fn a_late_completion_never_writes_a_replacement_generation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open(&dir.path().join("cell.sqlite"));
    cell.commit(place()).expect("commits");
    let old = match cell.claim_perform().expect("claims") {
        Some(PerformClaim::Fresh(command)) => command,
        other => panic!("expected fresh claim, got {other:?}"),
    };
    let mut replacement = old.clone();
    replacement.generation = Generation(1);
    replacement.idempotency_key = IdempotencyKey("charge/O-1/1".to_owned());
    cell.commit(CellCommit {
        effects: vec![Effect::Perform(replacement.clone())],
        ..CellCommit::empty()
    })
    .expect("replaces generation");

    assert_eq!(
        cell.complete_perform(
            &old,
            OrderInput::Charged {
                order: "O-1".to_owned(),
                ok: true,
            },
        )
        .expect("stale completion is ignored"),
        None
    );
    assert!(matches!(
        cell.claim_perform().expect("claims replacement"),
        Some(PerformClaim::Fresh(found)) if found == replacement
    ));
}

/// The commit/timer/perform scenario, start to finish.
fn scenario(path: &Path) {
    let mut cell = open(path);
    cell.commit(place()).expect("commits");
    let performed = cell.perform().expect("performs").expect("a command");
    cell.commit(
        CellCommit {
            writes: vec![FactWrite {
                key: order_key(),
                expected: FactVersion(1),
                mutation: FactMutation::Put {
                    values: order("paid"),
                },
            }],
            events: vec![OrderEvent::Charged {
                order: "O-1".to_owned(),
                ok: true,
            }],
            effects: vec![Effect::CancelTimer(TimerId("expire/O-1".to_owned()))],
            ..CellCommit::empty()
        }
        .consuming(Consumed::Result {
            command: performed.command,
            generation: performed.generation,
        }),
    )
    .expect("commits");
}

#[test]
fn reopen_recovers_the_ledger_hash_chain_identically_and_runs_are_deterministic() {
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");
    scenario(&first.path().join("cell.sqlite"));
    scenario(&second.path().join("cell.sqlite"));

    let a = open(&first.path().join("cell.sqlite"));
    let b = open(&second.path().join("cell.sqlite"));
    let head = a.store().read().verify_ledger().expect("verifies");
    assert_eq!((head.seq, head.commits), (2, 3));
    assert_eq!(b.store().read().verify_ledger().expect("verifies"), head);
    assert_eq!(
        a.store().read().dump().expect("dumps"),
        b.store().read().dump().expect("dumps"),
        "the same turns produce the same store"
    );
    assert_eq!(a.commits().expect("ledger"), b.commits().expect("ledger"));
    assert_eq!(
        a.load(&order_key()).expect("loads"),
        FactState {
            version: FactVersion(2),
            values: Some(order("paid")),
        }
    );
    assert!(a.pending_commands().expect("outbox").is_empty());
    assert!(a.pending_timers().expect("timers").is_empty());
}

fn open_hierarchy(path: &Path) -> OrderCell {
    let base = serde_json::from_value(json!({
        "name": "acme.Base",
        "properties": [{ "id": "code", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
    }))
    .expect("decodes");
    let child = serde_json::from_value(json!({
        "name": "acme.Child",
        "extends": "acme.Base",
        "properties": [{ "id": "tag", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
    }))
    .expect("decodes");
    SqliteCell::open(
        path,
        common::model_of(&[base, child]),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens")
}

/// `acme.Base/K` and `acme.Child/K` are one object row and one version
/// counter here, so one commit may not name both; each alone commits with a
/// receipt that matches what `load` then reports — also across reopen.
#[test]
fn hierarchy_aliases_in_one_commit_are_refused() {
    for reopen in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cell.sqlite");
        let mut cell = open_hierarchy(&path);
        let key = |object_type: &str| FactKey::new(object_type, "K");
        let delete = |cell: &OrderCell, object_type: &str| FactWrite {
            key: key(object_type),
            expected: cell.load(&key(object_type)).expect("loads").version,
            mutation: FactMutation::Delete,
        };
        let before = cell.store().read().dump().expect("dumps");
        let refused = cell
            .commit(CellCommit {
                writes: vec![delete(&cell, "acme.Base"), delete(&cell, "acme.Child")],
                ..CellCommit::empty()
            })
            .expect_err("aliases");
        assert_eq!(refused.code(), "commit.duplicate_key");
        assert_eq!(cell.store().read().dump().expect("dumps"), before);

        for object_type in ["acme.Base", "acme.Child", "acme.Base"] {
            let write = delete(&cell, object_type);
            let receipt = cell
                .commit(CellCommit {
                    writes: vec![write],
                    ..CellCommit::empty()
                })
                .expect("commits");
            if reopen {
                cell = open_hierarchy(&path);
            }
            assert_eq!(
                receipt.written[0].version,
                cell.load(&key(object_type)).expect("loads").version,
                "reopen: {reopen}"
            );
        }
    }
}

fn open_linked_hierarchy(path: &Path) -> OrderCell {
    let base = serde_json::from_value(json!({
        "name": "acme.Base",
        "properties": [{ "id": "code", "type": { "kind": "text" }, "constraints": { "nullable": true } }],
        "links": [{ "id": "rel", "target": "acme.Base", "cardinality": { "to": "many", "from": "many" } }]
    }))
    .expect("decodes");
    let child = serde_json::from_value(json!({
        "name": "acme.Child",
        "extends": "acme.Base",
        "properties": [{ "id": "tag", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
    }))
    .expect("decodes");
    let other = serde_json::from_value(json!({
        "name": "acme.Other",
        "properties": [{ "id": "note", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
    }))
    .expect("decodes");
    SqliteCell::open(
        path,
        common::model_of(&[base, child, other]),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens")
}

fn open_narrowed_link_hierarchy(path: &Path) -> OrderCell {
    let source_base = serde_json::from_value(json!({
        "name": "acme.SourceBase",
        "properties": []
    }))
    .expect("decodes");
    let source_brand = serde_json::from_value(json!({
        "name": "acme.SourceBrand",
        "extends": "acme.SourceBase",
        "properties": [],
        "links": [{
            "id": "featured",
            "target": "acme.TargetBrand",
            "cardinality": { "to": "many", "from": "many" }
        }]
    }))
    .expect("decodes");
    let target_base = serde_json::from_value(json!({
        "name": "acme.TargetBase",
        "properties": []
    }))
    .expect("decodes");
    let target_brand = serde_json::from_value(json!({
        "name": "acme.TargetBrand",
        "extends": "acme.TargetBase",
        "properties": []
    }))
    .expect("decodes");
    SqliteCell::open(
        path,
        common::model_of(&[source_base, source_brand, target_base, target_brand]),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens")
}

fn temporal_link_cell() -> OrderCell {
    let base: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "acme.TemporalBase",
        "links": [{
            "id": "rel",
            "target": "acme.TemporalBase",
            "cardinality": { "to": "many", "from": "many" },
            "properties": [
                {
                    "id": "service_date",
                    "type": { "kind": "date" },
                    "default": "2024-02-29"
                },
                {
                    "id": "occurred_at",
                    "type": { "kind": "timestamp", "timezone": "utc" },
                    "default": "2024-02-29T12:30:00+02:30"
                },
                {
                    "id": "shared_at",
                    "type": { "kind": "ref", "name": "acme.Instant" },
                    "default": "2024-02-29T12:30:00+02:30"
                }
            ]
        }]
    }))
    .expect("base definition decodes");
    let child: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "acme.TemporalChild",
        "extends": "acme.TemporalBase",
        "links": [{
            "id": "rel",
            "target": "acme.TemporalChild",
            "cardinality": { "to": "one", "from": "one" }
        }]
    }))
    .expect("child definition decodes");
    let instant: norn_ir::PropertyTypeDef = serde_json::from_value(json!({
        "name": "acme.Instant",
        "type": { "kind": "timestamp", "timezone": "offset" }
    }))
    .expect("shared property type decodes");
    let model =
        Model::from_definitions(&[base, child], &[instant]).expect("temporal link model projects");
    SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens")
}

#[test]
fn raw_link_writes_admit_and_normalize_temporal_properties() {
    let mut cell = temporal_link_cell();
    let put = |object_type: &str, key: &str| FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![
            put("acme.TemporalChild", "S1"),
            put("acme.TemporalChild", "T1"),
            put("acme.TemporalChild", "S2"),
            put("acme.TemporalChild", "T2"),
            put("acme.TemporalChild", "S3"),
            put("acme.TemporalBase", "B"),
        ],
        ..CellCommit::empty()
    })
    .expect("creates endpoints");

    let add = |source: &str, target: &str, values: JsonObject| LinkWrite {
        key: LinkKey::new(
            FactKey::new("acme.TemporalBase", source),
            "rel",
            FactKey::new("acme.TemporalBase", target),
        ),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add { values },
    };
    let before = cell.store().read().dump().expect("dumps");
    for (property, value) in [
        ("service_date", "2025-02-29"),
        ("occurred_at", "2025-02-29T10:00:00+00:00"),
    ] {
        let submitted = if property == "service_date" {
            json!({ "service_date": value })
        } else {
            json!({ "occurred_at": value })
        };
        let values = serde_json::from_value(submitted).expect("link property values decode");
        let refused = cell
            .commit(CellCommit {
                links: vec![add("S1", "T1", values)],
                ..CellCommit::empty()
            })
            .expect_err("invalid temporal link property is refused");
        assert!(
            matches!(&refused, CommitError::EffectiveValues { .. }),
            "{refused:?}"
        );
        assert_eq!(cell.store().read().dump().expect("dumps"), before);
    }

    // The source and target are concrete TemporalChild objects, while this
    // raw host call names both through TemporalBase. The child's narrowed
    // link contract is still used by admission.
    let narrowed = cell
        .commit(CellCommit {
            links: vec![add("S3", "B", JsonObject::new())],
            ..CellCommit::empty()
        })
        .expect_err("the concrete child link only targets TemporalChild");
    assert_eq!(narrowed.code(), "commit.link_target", "{narrowed:?}");
    assert_eq!(cell.store().read().dump().expect("dumps"), before);

    let explicit_values = serde_json::from_value(json!({
        "service_date": "2024-02-29",
        "occurred_at": "2024-02-29T12:30:00+02:30"
    }))
    .expect("explicit temporal properties decode");
    cell.commit(CellCommit {
        links: vec![add("S1", "T1", explicit_values)],
        ..CellCommit::empty()
    })
    .expect("valid temporal link properties are admitted");
    cell.commit(CellCommit {
        links: vec![add("S2", "T2", JsonObject::new())],
        ..CellCommit::empty()
    })
    .expect("link defaults are admitted");

    for (source, target) in [("S1", "T1"), ("S2", "T2")] {
        let key = LinkKey::new(
            FactKey::new("acme.TemporalBase", source),
            "rel",
            FactKey::new("acme.TemporalBase", target),
        );
        let stored = serde_json::to_value(
            cell.load_link(&key)
                .expect("loads link")
                .values
                .expect("link exists"),
        )
        .expect("serializes link values");
        assert_eq!(stored["service_date"], json!("2024-02-29"));
        assert_eq!(stored["occurred_at"], json!("2024-02-29T10:00:00Z"));
        assert_eq!(stored["shared_at"], json!("2024-02-29T10:00:00Z"));
    }
}

/// #3013: a link write names a target type its declaration admits, and an
/// added link's endpoint deleted in the same commit through another type of
/// its hierarchy is caught: the link would dangle.
#[test]
fn link_targets_are_typed_and_hierarchy_deletes_cannot_dangle_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_linked_hierarchy(&dir.path().join("cell.sqlite"));
    let put = |object_type: &str, key: &str| FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![
            put("acme.Base", "S"),
            put("acme.Child", "T"),
            put("acme.Other", "T"),
        ],
        ..CellCommit::empty()
    })
    .expect("creates");
    let add = |target_type: &str| LinkWrite {
        key: LinkKey::new(
            FactKey::new("acme.Base", "S"),
            "rel",
            FactKey::new(target_type, "T"),
        ),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    let before = cell.store().read().dump().expect("dumps");
    // `rel` targets acme.Base: an unrelated type sharing the id is refused.
    let refused = cell
        .commit(CellCommit {
            links: vec![add("acme.Other")],
            ..CellCommit::empty()
        })
        .expect_err("unrelated target type");
    assert_eq!(refused.code(), "commit.link_target", "{refused:?}");
    // Deleting acme.Base/T while adding a link to acme.Child/T: one row.
    let delete = FactWrite {
        key: FactKey::new("acme.Base", "T"),
        expected: cell
            .load(&FactKey::new("acme.Base", "T"))
            .expect("loads")
            .version,
        mutation: FactMutation::Delete,
    };
    let dangling = cell
        .commit(CellCommit {
            writes: vec![delete],
            links: vec![add("acme.Child")],
            ..CellCommit::empty()
        })
        .expect_err("the link would dangle");
    assert_eq!(dangling.code(), "commit.dangling_link");
    assert_eq!(cell.store().read().dump().expect("dumps"), before);
}

fn create_link_read_objects(cell: &mut OrderCell, keys: &[FactKey]) {
    cell.commit(CellCommit {
        writes: keys
            .iter()
            .map(|key| FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            })
            .collect(),
        ..CellCommit::empty()
    })
    .expect("objects created");
}

/// A complete link-slice read protects both an initially empty adjacency set
/// from phantoms and a previously live target from remove/re-add ABA.
#[test]
fn bounded_link_reads_refuse_phantoms_and_version_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_linked_hierarchy(&dir.path().join("cell.sqlite"));
    let source = FactKey::new("acme.Base", "S");
    let target = FactKey::new("acme.Child", "A");
    let other_target = FactKey::new("acme.Base", "Z");
    create_link_read_objects(
        &mut cell,
        &[source.clone(), target.clone(), other_target.clone()],
    );

    let empty = cell
        .read_link_targets(&source, "rel", 8, 1024)
        .expect("reads bounded adjacency")
        .expect("host supports bounded adjacency");
    assert!(empty.is_empty());
    let link = LinkKey::new(source.clone(), "rel", target);
    let add = |expected| LinkWrite {
        key: link.clone(),
        expected: FactVersion(expected),
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        links: vec![
            add(0),
            LinkWrite {
                key: LinkKey::new(source.clone(), "rel", other_target),
                expected: FactVersion::ABSENT,
                mutation: LinkMutation::Add {
                    values: JsonObject::new(),
                },
            },
        ],
        ..CellCommit::empty()
    })
    .expect("link added after empty snapshot");
    let phantom = cell
        .commit(CellCommit {
            link_reads: vec![LinkReadExpectation {
                source: source.clone(),
                link: "rel".to_owned(),
                targets: empty,
            }],
            ..CellCommit::empty()
        })
        .expect_err("the formerly empty adjacency changed");
    assert_eq!(phantom.code(), "commit.link_read_conflict");

    let observed = cell
        .read_link_targets(&source, "rel", 8, 1024)
        .expect("reads bounded adjacency")
        .expect("host supports bounded adjacency");
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].key, link);
    assert_eq!(observed[0].key.target.key, "A");
    assert_eq!(observed[1].key.target.key, "Z");
    cell.commit(CellCommit {
        link_reads: vec![LinkReadExpectation {
            source: source.clone(),
            link: "rel".to_owned(),
            targets: observed.clone(),
        }],
        ..CellCommit::empty()
    })
    .expect("unchanged adjacency commits");

    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: link.clone(),
            expected: observed[0].version,
            mutation: LinkMutation::Remove,
        }],
        ..CellCommit::empty()
    })
    .expect("link removed");
    cell.commit(CellCommit {
        links: vec![add(observed[0].version.0 + 1)],
        ..CellCommit::empty()
    })
    .expect("same target re-added with a new version");
    let aba = cell
        .commit(CellCommit {
            link_reads: vec![LinkReadExpectation {
                source,
                link: "rel".to_owned(),
                targets: observed,
            }],
            ..CellCommit::empty()
        })
        .expect_err("the link version changed even though membership returned");
    assert_eq!(aba.code(), "commit.link_read_conflict");
}

/// Link CAS preserves concrete source and target subtype semantics even
/// though commit-time identities are canonicalized to hierarchy roots.
#[test]
fn bounded_link_reads_keep_subtype_owned_declarations_and_targets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_narrowed_link_hierarchy(&dir.path().join("cell.sqlite"));
    let source = FactKey::new("acme.SourceBrand", "S");
    let target = FactKey::new("acme.TargetBrand", "T");
    create_link_read_objects(&mut cell, &[source.clone(), target.clone()]);
    let link = LinkKey::new(source.clone(), "featured", target);
    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: link,
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("narrowed target is admitted");

    let observed = cell
        .read_link_targets(&source, "featured", 8, 1024)
        .expect("reads bounded adjacency")
        .expect("host supports bounded adjacency");
    assert_eq!(observed.len(), 1);
    assert_eq!(
        observed[0].key.source.object_type.as_str(),
        "acme.SourceBrand"
    );
    assert_eq!(
        observed[0].key.target.object_type.as_str(),
        "acme.TargetBrand"
    );
    cell.commit(CellCommit {
        link_reads: vec![LinkReadExpectation {
            source,
            link: "featured".to_owned(),
            targets: observed,
        }],
        ..CellCommit::empty()
    })
    .expect("the concrete subtype target set remains current");
}

/// #3013: `acme.Base/S -rel-> T` and `acme.Child/S -rel-> T` are one link
/// row and one version here, so one commit may not write both; deleting an
/// endpoint finds its live link whichever type of the hierarchy names it,
/// and the delete commits once the same commit removes the link by the key
/// it was added with.
#[test]
fn link_aliases_are_refused_and_deletes_find_typed_links() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_linked_hierarchy(&dir.path().join("cell.sqlite"));
    let put = |object_type: &str, key: &str| FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    let rel = |source_type: &str| {
        LinkKey::new(
            FactKey::new(source_type, "S"),
            "rel",
            FactKey::new("acme.Base", "T"),
        )
    };
    let add = |source_type: &str, expected: u64| LinkWrite {
        key: rel(source_type),
        expected: FactVersion(expected),
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![put("acme.Child", "S"), put("acme.Base", "T")],
        ..CellCommit::empty()
    })
    .expect("creates");
    let before = cell.store().read().dump().expect("dumps");
    let refused = cell
        .commit(CellCommit {
            links: vec![add("acme.Base", 0), add("acme.Child", 0)],
            ..CellCommit::empty()
        })
        .expect_err("aliases");
    assert_eq!(refused.code(), "commit.duplicate_link");
    assert_eq!(cell.store().read().dump().expect("dumps"), before);

    let receipt = cell
        .commit(CellCommit {
            links: vec![add("acme.Child", 0)],
            ..CellCommit::empty()
        })
        .expect("links");
    assert_eq!(receipt.linked[0].version, FactVersion(1));
    // Either alias reads the one row.
    assert_eq!(
        cell.load_link(&rel("acme.Base")).expect("loads").version,
        FactVersion(1)
    );

    // Deleting the source, named through its base type, finds the link.
    let delete = FactWrite {
        key: FactKey::new("acme.Base", "S"),
        expected: cell
            .load(&FactKey::new("acme.Base", "S"))
            .expect("loads")
            .version,
        mutation: FactMutation::Delete,
    };
    let dangling = cell
        .commit(CellCommit {
            writes: vec![delete.clone()],
            ..CellCommit::empty()
        })
        .expect_err("a live link would dangle");
    assert_eq!(dangling.code(), "commit.dangling_link");
    cell.commit(CellCommit {
        writes: vec![delete],
        links: vec![LinkWrite {
            key: rel("acme.Child"),
            expected: FactVersion(1),
            mutation: LinkMutation::Remove,
        }],
        ..CellCommit::empty()
    })
    .expect("delete with the link removed");
    assert!(!cell.load_link(&rel("acme.Child")).expect("loads").exists());
}

/// #3018 review: a narrowing override binds however the source is named,
/// exactly as on the reference host.
#[test]
fn link_overrides_bind_through_a_supertype_name() {
    let mut cell: OrderCell = SqliteCell::open_in_memory(
        common::model_of(&override_object_types()),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens");
    check_link_override(&mut cell);
}

#[test]
fn link_overrides_bind_through_a_supertype_name_on_restricted_engine() {
    let engine = RestrictedEngine::open_memory().expect("opens engine");
    let store = CellStore::from_engine(engine, common::model_of(&override_object_types()))
        .expect("opens store");
    let mut cell: SqliteCell<OrderEvent, Charge, OrderInput, RestrictedEngine> =
        SqliteCell::new(store, "org_demo/dev", charge_responder).expect("creates host");
    check_link_override(&mut cell);
}

/// #3018 review: sibling types that each declare `rel` get a table each,
/// yet a link's compare-and-swap version stays canonical: stale versions
/// are refused and the receipt's version is the stored one.
#[test]
fn sibling_link_tables_keep_one_canonical_version() {
    let root =
        serde_json::from_value(json!({ "name": "acme.Root", "properties": [] })).expect("decodes");
    let sibling = |name: &str| {
        serde_json::from_value(json!({
            "name": name,
            "extends": "acme.Root",
            "properties": [],
            "links": [{ "id": "rel", "target": "acme.Root", "cardinality": { "to": "many", "from": "many" } }]
        }))
        .expect("decodes")
    };
    let mut cell: OrderCell = SqliteCell::open_in_memory(
        common::model_of(&[root, sibling("acme.A"), sibling("acme.B")]),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens");
    cell.commit(CellCommit {
        writes: ["s", "t"]
            .into_iter()
            .map(|key| FactWrite {
                key: FactKey::new("acme.B", key),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            })
            .collect(),
        ..CellCommit::empty()
    })
    .expect("creates");
    let key = LinkKey::new(
        FactKey::new("acme.B", "s"),
        "rel",
        FactKey::new("acme.B", "t"),
    );
    let mut change = |expected: u64, add: bool| {
        cell.commit(CellCommit {
            links: vec![LinkWrite {
                key: key.clone(),
                expected: FactVersion(expected),
                mutation: if add {
                    LinkMutation::Add {
                        values: JsonObject::new(),
                    }
                } else {
                    LinkMutation::Remove
                },
            }],
            ..CellCommit::empty()
        })
        .map(|receipt| receipt.linked[0].version.0)
        .map_err(|error| error.code())
    };
    assert_eq!(change(0, true), Ok(1));
    assert_eq!(change(1, false), Ok(2));
    assert_eq!(change(0, true), Err("commit.link_conflict"));
    assert_eq!(change(1, true), Err("commit.link_conflict"));
    assert_eq!(change(2, true), Ok(3));
    let state = cell.load_link(&key).expect("loads");
    assert_eq!(state.version, FactVersion(3));
    assert!(state.exists());
}

/// #3018 review: sibling declarations of `rel` to unrelated roots are
/// different links even between the same keys: changing `A/s -rel-> X/t`
/// leaves the version of `B/s -rel-> Y/t` alone, as on the reference host.
#[test]
fn sibling_links_to_unrelated_targets_keep_separate_versions() {
    let def = |value: serde_json::Value| serde_json::from_value(value).expect("decodes");
    let sibling = |name: &str, target: &str| {
        def(json!({
            "name": name,
            "extends": "acme.Root",
            "properties": [],
            "links": [{ "id": "rel", "target": target, "cardinality": { "to": "many", "from": "many" } }]
        }))
    };
    let mut cell: OrderCell = SqliteCell::open_in_memory(
        common::model_of(&[
            def(json!({ "name": "acme.Root", "properties": [] })),
            def(json!({ "name": "acme.X", "properties": [] })),
            def(json!({ "name": "acme.Y", "properties": [] })),
            sibling("acme.A", "acme.X"),
            sibling("acme.B", "acme.Y"),
        ]),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens");
    let put = |object_type: &str| FactWrite {
        key: FactKey::new(object_type, if object_type == "acme.B" { "s" } else { "t" }),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![put("acme.B"), put("acme.Y")],
        ..CellCommit::empty()
    })
    .expect("creates");
    let link = |source: &str, target: &str, add: bool| LinkWrite {
        key: LinkKey::new(FactKey::new(source, "s"), "rel", FactKey::new(target, "t")),
        expected: FactVersion::ABSENT,
        mutation: if add {
            LinkMutation::Add {
                values: JsonObject::new(),
            }
        } else {
            LinkMutation::Remove
        },
    };
    let removed = cell
        .commit(CellCommit {
            links: vec![link("acme.A", "acme.X", false)],
            ..CellCommit::empty()
        })
        .expect("removing an absent link bumps its version");
    assert_eq!(removed.linked[0].version, FactVersion(1));
    let added = cell
        .commit(CellCommit {
            links: vec![link("acme.B", "acme.Y", true)],
            ..CellCommit::empty()
        })
        .expect("a different link is still at version 0");
    assert_eq!(added.linked[0].version, FactVersion(1));
    let key = LinkKey::new(
        FactKey::new("acme.B", "s"),
        "rel",
        FactKey::new("acme.Y", "t"),
    );
    assert_eq!(cell.load_link(&key).expect("loads").version, FactVersion(1));
}

/// #3013: a subtype that narrows an inherited link's target is honoured:
/// with `Child.rel` narrowed to `acme.Child`, `Child/S -rel-> Base/T` is
/// refused while `Base/S -rel-> Base/T` and `Child/S -rel-> Child/U` commit.
#[test]
fn a_narrowed_link_target_is_enforced() {
    let base = serde_json::from_value(json!({
        "name": "acme.Base",
        "properties": [{ "id": "code", "type": { "kind": "text" }, "constraints": { "nullable": true } }],
        "links": [{ "id": "rel", "target": "acme.Base", "cardinality": { "to": "many", "from": "many" } }]
    }))
    .expect("decodes");
    let child = serde_json::from_value(json!({
        "name": "acme.Child",
        "extends": "acme.Base",
        "properties": [{ "id": "tag", "type": { "kind": "text" }, "constraints": { "nullable": true } }],
        "links": [{ "id": "rel", "target": "acme.Child", "cardinality": { "to": "many", "from": "many" } }]
    }))
    .expect("decodes");
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell: OrderCell = SqliteCell::open(
        &dir.path().join("cell.sqlite"),
        common::model_of(&[base, child]),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens");
    let put = |object_type: &str, key: &str| FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![
            put("acme.Base", "B"),
            put("acme.Child", "S"),
            put("acme.Base", "T"),
            put("acme.Child", "U"),
        ],
        ..CellCommit::empty()
    })
    .expect("creates");
    let add = |source: (&str, &str), target: (&str, &str)| LinkWrite {
        key: LinkKey::new(
            FactKey::new(source.0, source.1),
            "rel",
            FactKey::new(target.0, target.1),
        ),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    let refused = cell
        .commit(CellCommit {
            links: vec![add(("acme.Child", "S"), ("acme.Base", "T"))],
            ..CellCommit::empty()
        })
        .expect_err("Child.rel targets acme.Child only");
    assert_eq!(refused.code(), "commit.link_target", "{refused:?}");
    cell.commit(CellCommit {
        links: vec![
            add(("acme.Base", "B"), ("acme.Base", "T")),
            add(("acme.Child", "S"), ("acme.Child", "U")),
        ],
        ..CellCommit::empty()
    })
    .expect("admitted targets commit");
}
