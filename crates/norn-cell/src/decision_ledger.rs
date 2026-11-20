//! DECISION-LEDGER-1: the cell's ledger of resolved decisions.
//!
//! A decision is a parked `norn.decision@1` perform of a flow run (PARKED-1):
//! while the parked command waits, the decision is open. The commit that
//! consumes the command's recorded result carries one [`DECISION_RESOLVED`]
//! ledger event naming the decision, the command and generation it resolves,
//! how it was resolved, the choice and the deciding principal; the store then
//! records it in `_norn_decision` inside the same SQLite transaction. A
//! decision resolves at most once: a second resolution of the same decision,
//! or one whose commit does not consume exactly that command's result,
//! refuses and nothing commits.
//!
//! The ledger is derived only from committed batches, so export replay
//! rebuilds it exactly, on every engine that runs the store's SQL. It answers
//! "who decided, when, and what" after the parked command is consumed; it is
//! an index for reads, never authority, and every read goes through the
//! caller's gates.
//!
//! APPROVAL-RECOVERY-1: the commit that approves a parked action call also
//! records the call in `_norn_approved_call`, and a refusal the call gets is
//! recorded by its own commit carrying one [`APPROVAL_REFUSED`] event. Both
//! are ledger-derived, so a restored cell holds the same approved calls and
//! the same refusals. The commit that runs an approved call carries one
//! [`APPROVAL_CONSUMED`] event naming the approval it consumed, so each
//! commit is attributed to exactly one approval (S6c). A commit that names
//! no approval (one made before the event existed, or an ungated one) is
//! read the same way on every path: [`StoreReader::approval_consumed_at`]
//! answers it from these ledger-derived rows alone, so an opened file and
//! its restored export attribute every commit alike.
//!
//! S5a: the commit a flow step's own dispatch makes for its call carries
//! one [`STEP_CALL_OWNED`] event naming that step attempt (its run, its
//! parked command and generation), recorded in `_norn_step_call` at that
//! commit. Only such a commit's record is ever that step's outcome; a record
//! under a step's call key that no such event owns (a direct call's, or one
//! committed before the event existed) never is.

/// The `norn.decision@1` request a parked decision carries, and its bounds.
/// Hosts reach built-ins through the cell (`norn_cell::builtin_registered`).
pub use norn_builtins::decision::{
    DecisionOption, DecisionRequest, MAX_EXPIRES_IN_MS, MAX_TEXT_CHARS, OptionEffect, Risk,
    parse_request,
};
use norn_ir::RuntimeValue as Value;
use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::{StoreReader, to_i64};
use crate::store::{CommitBatch, Consumption};

/// The `type` of the ledger event that resolves a decision.
pub const DECISION_RESOLVED: &str = "norn.decision.resolved";

pub(crate) const DECISION: &str = "_norn_decision";

/// APPROVAL-RECOVERY-1: the command id prefix of a parked action call
/// (APPROVAL-1). Its approval makes the call due work.
pub const APPROVAL_COMMAND_PREFIX: &str = "norn.approval/";

/// The option that approves a parked action call.
pub const APPROVE_CHOICE: &str = "approve";

pub(crate) const APPROVED_CALL: &str = "_norn_approved_call";

pub(crate) const STEP_CALL: &str = "_norn_step_call";

/// The decision ledger's DDL, applied with the other system tables.
pub(crate) const DDL: [&str; 6] = [
    r#"CREATE TABLE IF NOT EXISTS "_norn_decision" (
  "world" TEXT NOT NULL,
  "decision_id" TEXT NOT NULL,
  "command_id" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  "run_id" TEXT NOT NULL,
  "step" TEXT NOT NULL,
  "resolution" TEXT NOT NULL,
  "choice" TEXT,
  "note" TEXT,
  "decided_by" TEXT,
  "idempotency_key" TEXT,
  "request_hash" TEXT,
  "resolved_at_ms" INTEGER NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  PRIMARY KEY ("world", "decision_id")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_decision/run" ON "_norn_decision" ("world", "run_id", "decision_id")"#,
    // APPROVAL-RECOVERY-1: every approved action call, its refusal once one
    // is recorded ([`APPROVAL_REFUSED`]), and the commit that consumed it
    // ([`APPROVAL_CONSUMED`]). Every column derives from the ledger alone, so
    // an opened file and its restored export hold the same rows (S6c).
    r#"CREATE TABLE IF NOT EXISTS "_norn_approved_call" (
  "world" TEXT NOT NULL,
  "decision_id" TEXT NOT NULL,
  "command_id" TEXT NOT NULL,
  "run_id" TEXT NOT NULL,
  "resolved_at_ms" INTEGER NOT NULL,
  "refused_status" INTEGER,
  "refused_code" TEXT,
  "refused_category" TEXT,
  "consumed_seq" INTEGER,
  PRIMARY KEY ("world", "decision_id")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_approved_call/run" ON "_norn_approved_call" ("world", "run_id", "decision_id")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_approved_call/consumed" ON "_norn_approved_call" ("world", "consumed_seq")"#,
    // S5a: the step attempt whose own dispatch made each commit that ran a
    // flow step's call ([`STEP_CALL_OWNED`]), from the ledger alone.
    r#"CREATE TABLE IF NOT EXISTS "_norn_step_call" (
  "world" TEXT NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  "run_id" TEXT NOT NULL,
  "command_id" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  PRIMARY KEY ("world", "commit_seq")
)"#,
];

/// APPROVAL-RECOVERY-1, in-place upgrade: a file whose approvals were
/// recorded before `_norn_approved_call` existed gains their rows from its
/// own decision rows, exactly the row replaying its ledger records for each
/// (the approving decision, its parked command, its run and the commit's
/// logical time; such a ledger carries no refusal or consumption event, so
/// none is recorded either way). Nothing is marked apart: whether a commit
/// consumed an approval is read from the rows alone
/// ([`StoreReader::approval_consumed_at`]), the same on both paths (S6c).
/// Idempotent: an approval already recorded is left as it is, so its refusal
/// stays.
pub(crate) const BACKFILL_APPROVED_CALLS: &str = r#"INSERT OR IGNORE INTO "_norn_approved_call" ("world","decision_id","command_id","run_id","resolved_at_ms") SELECT "world","decision_id","command_id","run_id","resolved_at_ms" FROM "_norn_decision" WHERE "resolution"='decided' AND "choice"='approve' AND substr("command_id",1,14)='norn.approval/'"#;

/// How a decision was resolved (DECISION-TERMINAL-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// A decider chose an option.
    Decided,
    /// Its deadline passed first.
    Expired,
    /// It was withdrawn before anyone decided.
    Withdrawn,
}

impl Resolution {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Decided => "decided",
            Self::Expired => "expired",
            Self::Withdrawn => "withdrawn",
        }
    }

    fn parse(text: &str) -> Result<Self, CellError> {
        match text {
            "decided" => Ok(Self::Decided),
            "expired" => Ok(Self::Expired),
            "withdrawn" => Ok(Self::Withdrawn),
            _unknown => Err(refused("decision.resolution_invalid")),
        }
    }
}

/// What a [`DECISION_RESOLVED`] event records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionResolved {
    /// The decision id.
    pub decision_id: String,
    /// The parked command it resolves.
    pub command_id: String,
    /// The command's generation.
    pub generation: u64,
    /// The run that waited on it.
    pub run_id: String,
    /// The flow step that asked.
    pub step: String,
    /// How it was resolved.
    pub resolution: Resolution,
    /// The chosen option id, when decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    /// The decider's note, when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The deciding principal's subject id, when decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    /// The decider's idempotency key, when decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The digest of the decider's request (choice and note), when decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_hash: Option<String>,
}

impl DecisionResolved {
    /// The ledger event a resolving commit carries.
    ///
    /// # Errors
    /// A value outside the runtime value domain.
    pub fn event(&self) -> Result<Value, CellError> {
        Ok(Value::Object(
            [
                (
                    "type".to_owned(),
                    Value::String(DECISION_RESOLVED.to_owned()),
                ),
                (
                    "decision".to_owned(),
                    norn_ir::runtime::encode(self).map_err(CellError::Encoding)?,
                ),
            ]
            .into(),
        ))
    }
}

/// One resolved decision, as the ledger records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRow {
    /// The event's fields.
    pub resolved: DecisionResolved,
    /// The resolving commit's logical time, in milliseconds.
    pub resolved_at_ms: u64,
    /// The resolving commit.
    pub commit_seq: u64,
}

fn refused(code: &'static str) -> CellError {
    CellError::DecisionRefused(code)
}

/// Whether a ledger event is a decision resolution (host bookkeeping, not a
/// domain event a typed commit reader decodes).
#[must_use]
pub fn is_decision_resolved(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some(DECISION_RESOLVED)
}

fn resolutions(batch: &CommitBatch) -> Result<Vec<DecisionResolved>, CellError> {
    let mut found = Vec::new();
    for event in batch
        .events
        .iter()
        .filter(|event| is_decision_resolved(event))
    {
        let object = event
            .as_object()
            .ok_or_else(|| refused("decision.resolution_invalid"))?;
        if object.len() != 2 {
            return Err(refused("decision.resolution_invalid"));
        }
        let resolved: DecisionResolved = object
            .get("decision")
            .and_then(|decision| norn_ir::runtime::decode(decision).ok())
            .ok_or_else(|| refused("decision.resolution_invalid"))?;
        found.push(resolved);
    }
    // One decision per commit: it consumes exactly its own command's result.
    if found.len() > 1 {
        return Err(refused("decision.resolution_invalid"));
    }
    if let Some(resolved) = found.first() {
        let consumes_it = matches!(
            &batch.consumes,
            Some(Consumption::Result { command_id, generation })
                if *command_id == resolved.command_id && *generation == resolved.generation
        );
        let decided = resolved.resolution == Resolution::Decided;
        if !consumes_it
            || resolved.decision_id.is_empty()
            || resolved.run_id.is_empty()
            || decided != resolved.choice.is_some()
            || decided != resolved.decided_by.is_some()
        {
            return Err(refused("decision.resolution_invalid"));
        }
    }
    Ok(found)
}

fn refusals(batch: &CommitBatch) -> Result<Vec<ApprovalRefused>, CellError> {
    let mut found = Vec::new();
    for event in batch
        .events
        .iter()
        .filter(|event| is_approval_refused(event))
    {
        let object = event
            .as_object()
            .ok_or_else(|| refused("approval.refusal_invalid"))?;
        if object.len() != 2 {
            return Err(refused("approval.refusal_invalid"));
        }
        let refusal: ApprovalRefused = object
            .get("refused")
            .and_then(|refused| norn_ir::runtime::decode(refused).ok())
            .ok_or_else(|| refused("approval.refusal_invalid"))?;
        if refusal.code.is_empty() || refusal.category.is_empty() {
            return Err(refused("approval.refusal_invalid"));
        }
        found.push(refusal);
    }
    if found.len() > 1 {
        return Err(refused("approval.refusal_invalid"));
    }
    Ok(found)
}

fn consumptions(batch: &CommitBatch) -> Result<Vec<ApprovalConsumed>, CellError> {
    let mut found = Vec::new();
    for event in batch
        .events
        .iter()
        .filter(|event| is_approval_consumed(event))
    {
        let object = event
            .as_object()
            .ok_or_else(|| refused("approval.consumption_invalid"))?;
        if object.len() != 2 {
            return Err(refused("approval.consumption_invalid"));
        }
        let consumed: ApprovalConsumed = object
            .get("consumed")
            .and_then(|consumed| norn_ir::runtime::decode(consumed).ok())
            .ok_or_else(|| refused("approval.consumption_invalid"))?;
        found.push(consumed);
    }
    if found.len() > 1 {
        return Err(refused("approval.consumption_invalid"));
    }
    Ok(found)
}

fn owners(batch: &CommitBatch) -> Result<Vec<StepCallOwner>, CellError> {
    let mut found = Vec::new();
    for event in batch
        .events
        .iter()
        .filter(|event| is_step_call_owned(event))
    {
        let object = event
            .as_object()
            .ok_or_else(|| refused("step.call_owner_invalid"))?;
        if object.len() != 2 {
            return Err(refused("step.call_owner_invalid"));
        }
        let owner: StepCallOwner = object
            .get("owner")
            .and_then(|owner| norn_ir::runtime::decode(owner).ok())
            .ok_or_else(|| refused("step.call_owner_invalid"))?;
        if owner.run_id.is_empty() || owner.command_id.is_empty() {
            return Err(refused("step.call_owner_invalid"));
        }
        found.push(owner);
    }
    if found.len() > 1 {
        return Err(refused("step.call_owner_invalid"));
    }
    Ok(found)
}

/// Before `batch`'s writes apply: a decision it resolves must be open.
///
/// # Errors
/// `decision.resolution_invalid`, `decision.resolved`, or SQL engine errors.
pub(crate) fn check<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
) -> Result<(), CellError> {
    for resolved in resolutions(batch)? {
        if reader
            .decision_resolution(&batch.world, &resolved.decision_id)?
            .is_some()
        {
            return Err(refused("decision.resolved"));
        }
    }
    // APPROVAL-RECOVERY-1: a refusal is recorded once, for an approved call.
    for refusal in refusals(batch)? {
        match reader.approved_call(&batch.world, &refusal.decision_id)? {
            Some(call) if call.refused.is_none() && call.consumed_seq.is_none() => {}
            Some(_) => return Err(refused("approval.refused")),
            None => return Err(refused("approval.refusal_invalid")),
        }
    }
    // S6c: an approval is consumed once, by a commit, only while no refusal
    // is recorded for it, and never refused by the commit that consumes it
    // (S8: both would pass against the state before the commit).
    let refused_now = refusals(batch)?;
    for consumed in consumptions(batch)? {
        if refused_now
            .iter()
            .any(|refusal| refusal.decision_id == consumed.decision_id)
        {
            return Err(refused("approval.consumption_invalid"));
        }
        match reader.approved_call(&batch.world, &consumed.decision_id)? {
            Some(call) if call.refused.is_none() && call.consumed_seq.is_none() => {}
            Some(_) => return Err(refused("approval.refused")),
            None => return Err(refused("approval.consumption_invalid")),
        }
    }
    // S5a: a commit is owned by at most one step attempt.
    owners(batch)?;
    Ok(())
}

/// APPROVAL-RECOVERY-1: the `type` of the ledger event that records the
/// refusal of an approved action call. Once recorded the approval never
/// runs the call again, and export replay rebuilds the refusal with the rest
/// of the ledger.
pub const APPROVAL_REFUSED: &str = "norn.approval.refused";

/// What an [`APPROVAL_REFUSED`] event records: the approving decision and
/// the refusal its call got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalRefused {
    /// The approving decision.
    pub decision_id: String,
    /// The refusal's HTTP status.
    pub status: u16,
    /// The refusal's code.
    pub code: String,
    /// The refusal's category (`norn/refusal@2`), as it was served: a
    /// replay serves it again, never one rebuilt from the status.
    pub category: String,
}

impl ApprovalRefused {
    /// The ledger event a settling commit carries.
    ///
    /// # Errors
    /// A value outside the runtime value domain.
    pub fn event(&self) -> Result<Value, CellError> {
        Ok(Value::Object(
            [
                (
                    "type".to_owned(),
                    Value::String(APPROVAL_REFUSED.to_owned()),
                ),
                (
                    "refused".to_owned(),
                    norn_ir::runtime::encode(self).map_err(CellError::Encoding)?,
                ),
            ]
            .into(),
        ))
    }
}

/// Whether a ledger event records an approved call's refusal (host
/// bookkeeping, not a domain event a typed commit reader decodes).
#[must_use]
pub fn is_approval_refused(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some(APPROVAL_REFUSED)
}

/// S6c: the `type` of the ledger event the commit that runs an approved
/// action call carries, naming the approval it consumed.
pub const APPROVAL_CONSUMED: &str = "norn.approval.consumed";

/// What an [`APPROVAL_CONSUMED`] event records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalConsumed {
    /// The approving decision the commit ran under.
    pub decision_id: String,
}

impl ApprovalConsumed {
    /// The ledger event the consuming commit carries.
    ///
    /// # Errors
    /// A value outside the runtime value domain.
    pub fn event(&self) -> Result<Value, CellError> {
        Ok(Value::Object(
            [
                (
                    "type".to_owned(),
                    Value::String(APPROVAL_CONSUMED.to_owned()),
                ),
                (
                    "consumed".to_owned(),
                    norn_ir::runtime::encode(self).map_err(CellError::Encoding)?,
                ),
            ]
            .into(),
        ))
    }
}

/// Whether a ledger event records an approval's consumption (host
/// bookkeeping, not a domain event a typed commit reader decodes).
#[must_use]
pub fn is_approval_consumed(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some(APPROVAL_CONSUMED)
}

/// S5a: the `type` of the ledger event the commit a flow step's own
/// dispatch makes for its call carries, naming that step attempt.
pub const STEP_CALL_OWNED: &str = "norn.step.call_owned";

/// What a [`STEP_CALL_OWNED`] event records: the step attempt whose own
/// dispatch made the commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepCallOwner {
    /// The run.
    pub run_id: String,
    /// The step attempt's parked `norn.action@1` command.
    pub command_id: String,
    /// That command's generation.
    pub generation: u64,
}

impl StepCallOwner {
    /// The ledger event the step's commit carries.
    ///
    /// # Errors
    /// A value outside the runtime value domain.
    pub fn event(&self) -> Result<Value, CellError> {
        Ok(Value::Object(
            [
                ("type".to_owned(), Value::String(STEP_CALL_OWNED.to_owned())),
                (
                    "owner".to_owned(),
                    norn_ir::runtime::encode(self).map_err(CellError::Encoding)?,
                ),
            ]
            .into(),
        ))
    }
}

/// Whether a ledger event records a step call's owner (host bookkeeping, not
/// a domain event a typed commit reader decodes).
#[must_use]
pub fn is_step_call_owned(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some(STEP_CALL_OWNED)
}

/// APPROVAL-RECOVERY-1: one approved action call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedCall {
    /// The approving decision.
    pub decision_id: String,
    /// The parked command that carries the call.
    pub command_id: String,
    /// The run whose step makes the call, or for a direct call the parked
    /// command's id.
    pub run_id: String,
    /// When the approval committed (logical milliseconds).
    pub resolved_at_ms: u64,
    /// S5c: the approving commit's ledger position.
    pub commit_seq: u64,
    /// The refusal recorded for it, if one was.
    pub refused: Option<RecordedRefusal>,
    /// S6c: the commit that ran the call under this approval, once one did.
    pub consumed_seq: Option<u64>,
}

/// The refusal recorded for an approved call ([`ApprovalRefused`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRefusal {
    /// Its HTTP status.
    pub status: u16,
    /// Its code.
    pub code: String,
    /// Its category as served.
    pub category: String,
}

/// Whether `resolved` approves a parked action call.
fn approves_call(resolved: &DecisionResolved) -> bool {
    resolved.resolution == Resolution::Decided
        && resolved.choice.as_deref() == Some(APPROVE_CHOICE)
        && resolved.command_id.starts_with(APPROVAL_COMMAND_PREFIX)
}

/// Record the decision `batch` resolves, inside the commit's transaction.
///
/// # Errors
/// As [`check`]; SQL engine errors.
pub(crate) fn apply<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    commit_seq: u64,
) -> Result<(), CellError> {
    for resolved in resolutions(batch)? {
        reader.conn.execute(
            &format!(
                r#"INSERT INTO "{DECISION}" ("world","decision_id","command_id","generation","run_id","step","resolution","choice","note","decided_by","idempotency_key","request_hash","resolved_at_ms","commit_seq") VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"#
            ),
            params![
                batch.world,
                resolved.decision_id,
                resolved.command_id,
                to_i64(resolved.generation, "decision generation")?,
                resolved.run_id,
                resolved.step,
                resolved.resolution.as_str(),
                resolved.choice,
                resolved.note,
                resolved.decided_by,
                resolved.idempotency_key,
                resolved.request_hash,
                to_i64(batch.logical_time, "logical time")?,
                to_i64(commit_seq, "commit sequence")?
            ],
        )?;
        // APPROVAL-RECOVERY-1: the approval and the call's due record are
        // one transaction, so no crash leaves an approval without it.
        if approves_call(&resolved) {
            reader.conn.execute(
                &format!(
                    r#"INSERT INTO "{APPROVED_CALL}" ("world","decision_id","command_id","run_id","resolved_at_ms") VALUES (?1,?2,?3,?4,?5)"#
                ),
                params![
                    batch.world,
                    resolved.decision_id,
                    resolved.command_id,
                    resolved.run_id,
                    to_i64(batch.logical_time, "logical time")?
                ],
            )?;
        }
    }
    for consumed in consumptions(batch)? {
        reader.conn.execute(
            &format!(
                r#"UPDATE "{APPROVED_CALL}" SET "consumed_seq"=?3 WHERE "world"=?1 AND "decision_id"=?2 AND "consumed_seq" IS NULL"#
            ),
            params![
                batch.world,
                consumed.decision_id,
                to_i64(commit_seq, "commit sequence")?
            ],
        )?;
    }
    for refusal in refusals(batch)? {
        reader.conn.execute(
            &format!(
                r#"UPDATE "{APPROVED_CALL}" SET "refused_status"=?3,"refused_code"=?4,"refused_category"=?5 WHERE "world"=?1 AND "decision_id"=?2 AND "refused_code" IS NULL"#
            ),
            params![
                batch.world,
                refusal.decision_id,
                i64::from(refusal.status),
                refusal.code,
                refusal.category
            ],
        )?;
    }
    for owner in owners(batch)? {
        reader.conn.execute(
            &format!(
                r#"INSERT INTO "{STEP_CALL}" ("world","commit_seq","run_id","command_id","generation") VALUES (?1,?2,?3,?4,?5)"#
            ),
            params![
                batch.world,
                to_i64(commit_seq, "commit sequence")?,
                owner.run_id,
                owner.command_id,
                to_i64(owner.generation, "generation")?
            ],
        )?;
    }
    Ok(())
}

fn decode_approved(row: &SqlRow) -> Result<ApprovedCall, CellError> {
    let status: Option<i64> = row.get(4)?;
    let code: Option<String> = row.get(5)?;
    let category: Option<String> = row.get(6)?;
    let consumed: Option<i64> = row.get(7)?;
    Ok(ApprovedCall {
        decision_id: row.get(0)?,
        command_id: row.get(1)?,
        run_id: row.get(2)?,
        resolved_at_ms: u64::try_from(row.get::<i64>(3)?)
            .map_err(|_negative| CellError::OutOfRange("approval time"))?,
        commit_seq: u64::try_from(row.get::<i64>(8)?)
            .map_err(|_negative| CellError::OutOfRange("approval commit"))?,
        consumed_seq: consumed
            .map(|seq| {
                u64::try_from(seq).map_err(|_negative| CellError::OutOfRange("consuming commit"))
            })
            .transpose()?,
        refused: match (status, code, category) {
            (Some(status), Some(code), Some(category)) => Some(RecordedRefusal {
                status: u16::try_from(status)
                    .map_err(|_bad| CellError::OutOfRange("refusal status"))?,
                code,
                category,
            }),
            _unrefused => None,
        },
    })
}

/// The approved call's columns and its approving commit, over `a` (the
/// approved call) joined to `d` (its decision).
const APPROVED_COLUMNS: &str = r#"a."decision_id",a."command_id",a."run_id",a."resolved_at_ms",a."refused_status",a."refused_code",a."refused_category",a."consumed_seq",d."commit_seq""#;

fn decode(row: &SqlRow) -> Result<DecisionRow, CellError> {
    let unsigned = |index: usize, what: &'static str| {
        u64::try_from(row.get::<i64>(index)?).map_err(|_negative| CellError::OutOfRange(what))
    };
    Ok(DecisionRow {
        resolved: DecisionResolved {
            decision_id: row.get(0)?,
            command_id: row.get(1)?,
            generation: unsigned(2, "decision generation")?,
            run_id: row.get(3)?,
            step: row.get(4)?,
            resolution: Resolution::parse(&row.get::<String>(5)?)?,
            choice: row.get(6)?,
            note: row.get(7)?,
            decided_by: row.get(8)?,
            idempotency_key: row.get(9)?,
            request_hash: row.get(10)?,
        },
        resolved_at_ms: unsigned(11, "decision resolved time")?,
        commit_seq: unsigned(12, "decision commit")?,
    })
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// APPROVAL-RECOVERY-1: the approved call of `decision_id`, if it
    /// approved one.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn approved_call(
        &self,
        world: &str,
        decision_id: &str,
    ) -> Result<Option<ApprovedCall>, CellError> {
        self.conn.query_optional(
            &format!(
                r#"SELECT {APPROVED_COLUMNS} FROM "{APPROVED_CALL}" a JOIN "{DECISION}" d ON d."world"=a."world" AND d."decision_id"=a."decision_id" WHERE a."world"=?1 AND a."decision_id"=?2"#
            ),
            params![world, decision_id],
            decode_approved,
        )
    }

    /// APPROVAL-RECOVERY-1: the approved calls of `run_id`'s steps, in the
    /// ledger order of their approving commits (S5c).
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn approved_calls_of_run(
        &self,
        world: &str,
        run_id: &str,
    ) -> Result<Vec<ApprovedCall>, CellError> {
        let rows = self.conn.query(
            &format!(
                r#"SELECT {APPROVED_COLUMNS} FROM "{APPROVED_CALL}" a JOIN "{DECISION}" d ON d."world"=a."world" AND d."decision_id"=a."decision_id" WHERE a."world"=?1 AND a."run_id"=?2 ORDER BY d."commit_seq""#
            ),
            params![world, run_id],
        )?;
        rows.into_iter()
            .map(|values| decode_approved(&SqlRow(values)))
            .collect()
    }

    /// S6c: the approval the commit `commit_seq` consumed, if it consumed
    /// one. A commit that names none (made before consumption was recorded,
    /// or ungated) answers `None`, read from the ledger-derived rows alone,
    /// so an opened file and its restored export answer alike.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn approval_consumed_at(
        &self,
        world: &str,
        commit_seq: u64,
    ) -> Result<Option<String>, CellError> {
        self.conn.query_optional(
            &format!(
                r#"SELECT "decision_id" FROM "{APPROVED_CALL}" WHERE "world"=?1 AND "consumed_seq"=?2"#
            ),
            params![world, to_i64(commit_seq, "commit sequence")?],
            |row| row.get(0),
        )
    }

    /// S5a: the step attempt whose own dispatch made the commit
    /// `commit_seq`, if a step's dispatch made it. A commit no
    /// [`STEP_CALL_OWNED`] event names (a direct call's, or one made before
    /// the event existed) answers `None`, from the ledger-derived rows alone.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn step_call_owner_at(
        &self,
        world: &str,
        commit_seq: u64,
    ) -> Result<Option<StepCallOwner>, CellError> {
        self.conn.query_optional(
            &format!(
                r#"SELECT "run_id","command_id","generation" FROM "{STEP_CALL}" WHERE "world"=?1 AND "commit_seq"=?2"#
            ),
            params![world, to_i64(commit_seq, "commit sequence")?],
            |row| {
                Ok(StepCallOwner {
                    run_id: row.get(0)?,
                    command_id: row.get(1)?,
                    generation: u64::try_from(row.get::<i64>(2)?)
                        .map_err(|_negative| CellError::OutOfRange("generation"))?,
                })
            },
        )
    }

    /// How `decision_id` was resolved in `world`, once it was.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn decision_resolution(
        &self,
        world: &str,
        decision_id: &str,
    ) -> Result<Option<DecisionRow>, CellError> {
        self.conn.query_optional(
            &format!(
                r#"SELECT "decision_id","command_id","generation","run_id","step","resolution","choice","note","decided_by","idempotency_key","request_hash","resolved_at_ms","commit_seq" FROM "{DECISION}" WHERE "world"=?1 AND "decision_id"=?2"#
            ),
            params![world, decision_id],
            decode,
        )
    }
}
