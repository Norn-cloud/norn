//! RECEIPT-STORE-1 and the durable host's side of RECEIPT-CONSUME-1 (Core 07).
//!
//! A Function's recorded result becomes action evidence only through
//! [`SqliteCell::retain_function_receipt`]: the cell reconciles the receipt
//! it recorded for the command's pending delivery against the actual
//! dispatched invocation and the command's persisted state, under this
//! cell's scope, the `main` world and its clock, and retains only an accepted
//! receipt. Intake commits no business effect and grants no authority.
//!
//! The cell's `CellHost::resolve_accepted_receipt` (`resolve`) finds
//! retained evidence only under the current scope, `main` world and declared
//! manifest, re-derives its dispatch pins, then re-runs reconciliation at the
//! cell's current clock and state, rechecking the command's authority through
//! the host's installed [`crate::ResultAuthority`] (T2-RES-1 step 8; a cell
//! without one serves no receipt effect). A re-issued, cancelled or expired
//! command, revoked authority or a moved relevant-state pin refuses
//! `action.receipt_not_accepted`, and a consumed delivery is
//! `action.receipt_replayed`. The accepted binding carries every pin as a
//! compare-and-swap read, plus the command state's version, and the delivery
//! the accepting action commit consumes. That commit is the only consumption
//! record: the store's pending-delivery check refuses a second one in the
//! same transaction as the business effect.
//!
//! Predicate and collection pins and written destinations have no qualified
//! fence on this path; intake refuses them. Generic event export never
//! restores retained evidence, and a fork never resolves it.

use norn_ir::RuntimeValue as Value;
use norn_ir::canonical::CanonicalJsonError;
use norn_ir::function::{
    Digest, FreshnessMode, FunctionInvocation, FunctionReceipt, MemberPin, PredicatePin,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::receipt_binding::{
    AcceptedReceiptStore, ReceiptBinding, ReceiptBindingError, ReceiptReference, ReceiptResolution,
    bind_receipt,
};
use norn_kernel::result_reconciliation::{
    Acceptance, Classification, CommandState, Completion, DispatchedCommand, InvalidReceipt,
    ReceiptValidator, ReconcileContext, ReconcileError, ReconcilePolicy, Reconciliation,
    RefusalReason, ResultAuthorizer, ResultAuthzRequest, ResultChecks, StateView, reconcile,
};
use norn_kernel::{
    CellCommit, CommandId, CommitError, CommitReceipt, Consumed, FactExpectation, FactKey,
    FactVersion, Generation, LogicalTime,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, params};
use crate::error::CellError;
use crate::host::SqliteCell;
use crate::read::{PendingCommand, StoreReader};
use crate::store::{CellStore, CommitBatch, Consumption, MAIN_WORLD};
use crate::system::OUTBOX;

/// The object type whose `state` property holds a Function command's
/// reconciliation state ([`CommandState`]), written with its enqueue.
pub const FUNCTION_COMMAND_TYPE: &str = "norn.FunctionCommand";

/// The `payload.type` of an enqueued Function invocation command.
pub const FUNCTION_PAYLOAD_TYPE: &str = "norn.function/v1";

const TABLE: &str = "_norn_function_receipt";

/// The retained-evidence table: one row per accepted receipt digest.
pub(crate) const DDL: [&str; 1] = [r#"CREATE TABLE IF NOT EXISTS "_norn_function_receipt" (
  "reference" TEXT NOT NULL PRIMARY KEY,
  "scope" TEXT NOT NULL,
  "world" TEXT NOT NULL,
  "command_id" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  "retained_at" INTEGER NOT NULL,
  "body" TEXT NOT NULL
)"#];

/// Why [`SqliteCell::retain_function_receipt`] retained nothing.
#[derive(Debug, thiserror::Error)]
pub enum ReceiptIntakeError {
    /// A fork, or a dispatch whose freshness pins predicates, collections or
    /// a written destination: no qualified fence exists on this path.
    #[error("receipt.intake_unsupported")]
    Unsupported,
    /// The supplied dispatch pins do not re-derive from its invocation, or
    /// the pending delivery is not this receipt of this dispatch.
    #[error("receipt.intake_dispatch_mismatch")]
    DispatchMismatch,
    /// No recorded result of this command and generation is pending.
    #[error("receipt.intake_not_pending")]
    NotPending,
    /// The command has no persisted reconciliation state.
    #[error("receipt.intake_no_command_state")]
    NoCommandState,
    /// Reconciliation refused the result (scope, world, ownership, binding,
    /// contract, authentication, inputs, deadline, authority or freshness).
    #[error("receipt.intake_refused: {classification:?} {reason:?}")]
    Refused {
        /// How the result was classified.
        classification: Classification,
        /// Why it was refused.
        reason: RefusalReason,
    },
    /// The host's own failure.
    #[error("receipt store: {0}")]
    Store(Box<CellError>),
}

impl From<CellError> for ReceiptIntakeError {
    fn from(error: CellError) -> Self {
        Self::Store(Box::new(error))
    }
}

/// A host's Function command-authority recheck (T2-RES-1 step 8).
pub type ResultAuthority = Box<dyn ResultAuthorizer + Send>;

/// How one commit treats a consumed `norn.function/v1` delivery, chosen per
/// call by its entry point, never by inspecting the commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiptCommitMode {
    /// Every ordinary commit: consuming a Function delivery applies its
    /// result and always runs the authority recheck.
    Normal,
    /// [`SqliteCell::commit_result_refusal`]: consumes a pending Function
    /// delivery and its final store batch applies nothing.
    ResultRefusal,
}

/// A cell's receipt authority state.
#[derive(Default)]
pub(crate) struct ReceiptHost {
    /// The command-authority recheck; without one no receipt resolves and
    /// no commit accepts a Function result.
    authorizer: Option<ResultAuthority>,
}

/// What a retained row holds: the receipt and the dispatch it reconciled
/// against. Intake authenticated the receipt's own `provenance.executor`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Retained {
    receipt: FunctionReceipt,
    dispatched: DispatchedCommand,
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// RECEIPT-CONSUME-1: install (or, with `None`, remove) the command
    /// authority recheck that resolving a retained Function receipt, and the
    /// commit accepting it, run at the cell's effective clock (T2-RES-1 step
    /// 8). Without one the cell serves no receipt effect
    /// (`action.receipt_effect_unsupported`) and accepts no Function result.
    pub fn set_result_authorizer(&mut self, authorizer: Option<ResultAuthority>) {
        self.receipts.authorizer = authorizer;
    }

    /// RECEIPT-STORE-1: reconcile an authenticated completion of a pending
    /// Function delivery and retain its receipt as action evidence.
    ///
    /// `completion` must carry exactly the receipt this cell recorded as the
    /// pending result of `dispatched`'s command and generation, whose enqueued
    /// payload is the `norn.function/v1` envelope of `dispatched`'s
    /// invocation. The dispatch, input and dependency digests are re-derived
    /// from that invocation; the caller-supplied execution profile must match
    /// the receipt's. The command's persisted [`CommandState`] and `checks`
    /// (the receipt contract and the command authority's recheck) decide
    /// acceptance under this cell's scope, `main` and the cell's clock. Only
    /// an accepted receipt is retained. A retry runs every check again and
    /// retains nothing new; a retry with another dispatch refuses. Nothing is
    /// committed to the ledger and the delivery stays pending.
    ///
    /// # Errors
    /// A [`ReceiptIntakeError`]; nothing was retained.
    pub fn retain_function_receipt<A, R>(
        &mut self,
        dispatched: &DispatchedCommand,
        completion: &Completion<'_>,
        checks: &ResultChecks<'_, A, R>,
    ) -> Result<Digest, ReceiptIntakeError>
    where
        A: ResultAuthorizer + ?Sized,
        R: ReceiptValidator + ?Sized,
    {
        retain(
            self.store(),
            &self.scope,
            self.now(),
            dispatched,
            completion,
            checks,
        )
    }
}

impl<E, C, I, S> SqliteCell<E, C, I, S>
where
    S: SqlEngine,
    E: Serialize + 'static,
    C: Serialize + DeserializeOwned,
    I: Serialize + DeserializeOwned,
{
    /// T2-RES-1: commit the record of a refused Function result: it consumes
    /// the pending `norn.function/v1` delivery `consumed` and applies nothing
    /// (its final store batch has no write, link, fact, event, effect or
    /// quantity posting). Every ordinary commit consuming such a delivery,
    /// even one applying nothing, runs the installed command-authority
    /// recheck (RECEIPT-CONSUME-1).
    ///
    /// # Errors
    /// [`CommitError::ReceiptNotAccepted`] when `consumed` names no pending
    /// Function delivery or the batch applies something; otherwise as
    /// [`norn_kernel::CellHost::commit`].
    pub fn commit_result_refusal(
        &mut self,
        consumed: Consumed,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        if pending_invocation(&self.store().read(), Some(&consumed))
            .map_err(CommitError::Host)?
            .is_none()
        {
            return Err(CommitError::ReceiptNotAccepted { consumed });
        }
        let commit = CellCommit {
            consumes: Some(consumed),
            ..CellCommit::empty()
        };
        let args: crate::host::AdmissionArgs<'_> = (
            &[],
            &[],
            &[],
            &[],
            None,
            ReceiptCommitMode::ResultRefusal,
            None,
        );
        self.commit_admitted(commit, None, None, None, args)
    }
}

fn retain<S, A, R>(
    store: &CellStore<S>,
    scope: &str,
    now: LogicalTime,
    dispatched: &DispatchedCommand,
    completion: &Completion<'_>,
    checks: &ResultChecks<'_, A, R>,
) -> Result<Digest, ReceiptIntakeError>
where
    S: SqlEngine,
    A: ResultAuthorizer + ?Sized,
    R: ReceiptValidator + ?Sized,
{
    admissible(store, dispatched)?;
    let invocation = &dispatched.invocation;
    let reference = completion
        .receipt
        .content_digest()
        .map_err(CellError::from)?;
    let generation = i64::try_from(dispatched.generation().0)
        .map_err(|_overflow| CellError::OutOfRange("generation"))?;
    let outcome = store.write_tx(|| {
        let reader = store.read();
        // A sibling handle may have committed past this handle's clock.
        let now = now.max(LogicalTime::from_millis(reader.clock_floor()?));
        let retained_at = i64::try_from(now.as_millis())
            .map_err(|_overflow| CellError::OutOfRange("retained_at"))?;
        let pending = reader.pending_result(&invocation.command_id, dispatched.generation().0)?;
        let Some(pending) = pending else {
            return Ok(Err(ReceiptIntakeError::NotPending));
        };
        if !delivers(&pending, completion.receipt, invocation) {
            return Ok(Err(ReceiptIntakeError::DispatchMismatch));
        }
        let Some((state, _version)) = command_state(&reader, &invocation.command_id)? else {
            return Ok(Err(ReceiptIntakeError::NoCommandState));
        };
        let context = ReconcileContext {
            tenant: scope,
            world: MAIN_WORLD,
            now,
            policy: ReconcilePolicy::DEFAULT,
        };
        match reconcile(
            &context,
            dispatched,
            &state,
            completion,
            checks,
            &View(&reader),
        ) {
            Ok(Reconciliation::Accepted(_)) => {}
            Ok(Reconciliation::Refused(refused)) => {
                return Ok(Err(ReceiptIntakeError::Refused {
                    classification: refused.classification,
                    reason: refused.reason,
                }));
            }
            Err(ReconcileError::View(error)) => return Err(error),
            Err(ReconcileError::Canonical(error)) => return Err(CellError::from(error)),
        }
        if let Some(existing) = load(&reader, scope, &reference)? {
            // Every check passed again for this exact receipt; a retry under
            // another dispatch is not the retained evidence.
            return Ok(if existing.dispatched == *dispatched {
                Ok(())
            } else {
                Err(ReceiptIntakeError::DispatchMismatch)
            });
        }
        let body = serde_json::to_string(&Retained {
            receipt: completion.receipt.clone(),
            dispatched: dispatched.clone(),
        })?;
        store.conn.execute(
            &format!(
                "INSERT INTO \"{TABLE}\" (\"reference\", \"scope\", \"world\", \"command_id\", \"generation\", \"retained_at\", \"body\") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
            ),
            params![
                reference.0,
                scope,
                MAIN_WORLD,
                invocation.command_id,
                generation,
                retained_at,
                body
            ],
        )?;
        Ok(Ok(()))
    })?;
    outcome.map(|()| reference)
}

/// Intake runs only in `main`, for a dispatch whose pins re-derive and whose
/// freshness this path can fence.
fn admissible<S: SqlEngine>(
    store: &CellStore<S>,
    dispatched: &DispatchedCommand,
) -> Result<(), ReceiptIntakeError> {
    let invocation = &dispatched.invocation;
    if store.fork_pin().is_some()
        || !invocation.dependency.predicates.is_empty()
        || !invocation.dependency.collections.is_empty()
        || invocation.freshness == FreshnessMode::DestinationCas
    {
        return Err(ReceiptIntakeError::Unsupported);
    }
    if repins(dispatched).map_err(CellError::from)? {
        Ok(())
    } else {
        Err(ReceiptIntakeError::DispatchMismatch)
    }
}

/// Whether `dispatched`'s dispatch, input and dependency digests re-derive
/// from its invocation under its execution profile.
fn repins(dispatched: &DispatchedCommand) -> Result<bool, CanonicalJsonError> {
    let repinned = DispatchedCommand::pin(
        dispatched.invocation.clone(),
        dispatched.execution_profile.clone(),
    )?;
    Ok(repinned == *dispatched)
}

/// The retained row of `reference` in this scope and `main`, if any.
fn load<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    scope: &str,
    reference: &Digest,
) -> Result<Option<Retained>, CellError> {
    reader.check_live()?;
    let body = reader.conn.query_optional(
        &format!(
            "SELECT \"body\" FROM \"{TABLE}\" WHERE \"reference\" = ?1 AND \"scope\" = ?2 AND \"world\" = ?3"
        ),
        params![reference.0, scope, MAIN_WORLD],
        |row| row.get::<String>(0),
    )?;
    let Some(body) = body else {
        return Ok(None);
    };
    let retained: Retained = serde_json::from_str(&body)?;
    // The key is the receipt's own content digest; a row that disagrees was
    // not written by intake and is never evidence.
    if retained.receipt.content_digest()? != *reference {
        return Err(CellError::Encoding(
            "retained Function receipt does not match its digest".to_owned(),
        ));
    }
    Ok(Some(retained))
}

/// Whether `pending` is the recorded delivery of `receipt` for the
/// `norn.function/v1` enqueue of exactly `invocation`.
fn delivers(
    pending: &PendingCommand,
    receipt: &FunctionReceipt,
    invocation: &FunctionInvocation,
) -> bool {
    let payload = pending.command.command.get("payload");
    let typed = payload
        .and_then(|payload| payload.get("type"))
        .and_then(Value::as_str)
        == Some(FUNCTION_PAYLOAD_TYPE);
    let enqueued = payload
        .and_then(|payload| payload.get("invocation"))
        .and_then(|value| norn_ir::runtime::decode::<FunctionInvocation>(value).ok())
        .is_some_and(|enqueued| enqueued == *invocation);
    let recorded = pending
        .result
        .as_ref()
        .and_then(|value| norn_ir::runtime::decode::<FunctionReceipt>(value).ok())
        .is_some_and(|recorded| recorded == *receipt);
    typed && enqueued && recorded
}

/// The command's persisted reconciliation state and the version of the same
/// stored row.
fn command_state<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    command_id: &str,
) -> Result<Option<(CommandState, FactVersion)>, CellError> {
    let Some(record) = reader.object(MAIN_WORLD, FUNCTION_COMMAND_TYPE, command_id)? else {
        return Ok(None);
    };
    Ok(record
        .values
        .get("state")
        .and_then(|state| norn_ir::runtime::decode::<CommandState>(state).ok())
        .map(|state| (state, FactVersion(record.version))))
}

/// The `norn.function/v1` invocation whose recorded result `consumes`
/// names, if it is still pending.
fn pending_invocation<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    consumes: Option<&Consumed>,
) -> Result<Option<Value>, CellError> {
    let Some(Consumed::Result {
        command,
        generation,
    }) = consumes
    else {
        return Ok(None);
    };
    let Some(pending) = reader.pending_result(&command.0, generation.0)? else {
        return Ok(None);
    };
    let Some(payload) = pending.command.command.get("payload") else {
        return Ok(None);
    };
    if payload.get("type").and_then(Value::as_str) != Some(FUNCTION_PAYLOAD_TYPE) {
        return Ok(None);
    }
    Ok(Some(
        payload.get("invocation").cloned().unwrap_or(Value::Null),
    ))
}

/// Whether a host batch's time follows the durable clock floor, read inside
/// its transaction: it consumes a result delivery that is not pending at its
/// exact generation (the delivery check refuses it), or is the pending
/// result of a `norn.function/v1` command. A losing or stale consumer thus
/// meets the delivery check, never a clock regression of its own handle; a
/// non-string `payload.type` is simply not a Function.
///
/// # Errors
/// The host's read failure.
pub(crate) fn follows_floor<E: SqlEngine>(
    conn: &E,
    batch: &CommitBatch,
) -> Result<bool, CellError> {
    let Some(Consumption::Result {
        command_id,
        generation,
    }) = &batch.consumes
    else {
        return Ok(false);
    };
    // No stored delivery has a generation outside the exact SQL integer
    // range, so such a delivery is not pending and the delivery check refuses.
    let Some(generation) = i64::try_from(*generation)
        .ok()
        .filter(|generation| *generation <= crate::engine::MAX_SAFE_INTEGER)
    else {
        return Ok(true);
    };
    let function = conn.query_optional(
        &format!(
            "SELECT json_type(\"command\", '$.payload.type') = 'text' AND json_extract(\"command\", '$.payload.type') = ?3 FROM \"{OUTBOX}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2 AND \"consumed_commit_seq\" IS NULL AND \"result\" IS NOT NULL LIMIT 1"
        ),
        params![command_id, generation, FUNCTION_PAYLOAD_TYPE],
        |row| row.get::<Option<i64>>(0),
    )?;
    Ok(function.is_none_or(|function| function == Some(1)))
}

/// RECEIPT-CONSUME-1, inside the commit transaction: every commit consuming
/// a pending `norn.function/v1` delivery applies its result, through an
/// action or [`Acceptance::commit`], and applies only while the dispatch's
/// scope, deadline and the installed command-authority recheck hold at the
/// commit's effective clock, `now` raised to the durable clock floor
/// (T2-RES-1 step 8). Only a refusal record, which applies nothing (no
/// write, link, event or effect; [`SqliteCell::commit_result_refusal`]),
/// consumes without it. A delivery no longer pending is the kernel's refusal.
///
/// # Errors
/// [`CommitError::ReceiptNotAccepted`], or the host's read failure.
pub(crate) fn check_consumption<S: SqlEngine, E, C, I>(
    reader: &StoreReader<'_, S>,
    commit: &CellCommit<E, C, I>,
    batch: &CommitBatch,
    (receipts, scope, now, mode): (&ReceiptHost, &str, LogicalTime, ReceiptCommitMode),
) -> Result<(), CommitError<CellError>> {
    let Some(consumed) = &commit.consumes else {
        return Ok(());
    };
    let refused = || CommitError::ReceiptNotAccepted {
        consumed: consumed.clone(),
    };
    let invocation = pending_invocation(reader, Some(consumed)).map_err(CommitError::Host)?;
    if mode == ReceiptCommitMode::ResultRefusal {
        let bare = batch.writes.is_empty()
            && batch.facts.is_empty()
            && batch.links.is_empty()
            && batch.events.is_empty()
            && batch.effects.is_empty()
            && batch.quantity.is_none();
        return if invocation.is_some() && bare {
            Ok(())
        } else {
            Err(refused())
        };
    }
    let Some(invocation) = invocation else {
        return Ok(());
    };
    let invocation = norn_ir::runtime::decode::<FunctionInvocation>(&invocation)
        .map_err(|_invalid| refused())?;
    let authorizer = receipts.authorizer.as_deref().ok_or_else(refused)?;
    // A sibling handle may have committed past this handle's clock.
    let floor = reader.clock_floor().map_err(CommitError::Host)?;
    let now = now.max(LogicalTime::from_millis(floor));
    let expired =
        !ReconcilePolicy::DEFAULT.accept_late && now.as_millis() > invocation.deadline_millis;
    if invocation.tenant != scope || invocation.world != MAIN_WORLD || expired {
        return Err(refused());
    }
    authorizer
        .authorize(&ResultAuthzRequest {
            tenant: &invocation.tenant,
            authority: &invocation.authority,
            command: &invocation.command_id,
            manifest: &invocation.manifest,
            destination: invocation.destination.as_ref(),
            now,
        })
        .map_err(|_denied| refused())
}

/// Current fact versions in `main`. Predicate and collection pins are never
/// retained, so reading them here fails closed.
struct View<'r, 's, S: SqlEngine>(&'r StoreReader<'s, S>);

impl<S: SqlEngine> StateView for View<'_, '_, S> {
    type Error = CellError;

    fn fact_version(&self, key: &FactKey) -> Result<FactVersion, CellError> {
        self.0
            .version(MAIN_WORLD, key.object_type.as_str(), &key.key)
            .map(FactVersion)
    }

    fn predicate_members(&self, _predicate: &PredicatePin) -> Result<Vec<MemberPin>, CellError> {
        Err(CellError::ReceiptPinUnqualified)
    }

    fn collection_epoch(&self, _object_type: &str) -> Result<u64, CellError> {
        Err(CellError::ReceiptPinUnqualified)
    }
}

/// The receipt contract is an immutable fact of a retained receipt, checked
/// by the intake that retained it. Resolution re-runs reconciliation for what
/// can change since: scope, world, generation fencing, cancellation,
/// deadline, the command's authority and freshness.
struct RetainedContract;

impl ReceiptValidator for RetainedContract {
    fn validate(
        &self,
        _dispatched: &DispatchedCommand,
        _receipt: &FunctionReceipt,
    ) -> Result<(), InvalidReceipt> {
        Ok(())
    }
}

/// Owned evidence resolved for one reference.
struct Evidence {
    acceptance: Acceptance,
    receipt: FunctionReceipt,
}

/// The outcome of looking up one reference.
enum Resolved {
    Unknown,
    Replayed,
    Accepted(Box<Evidence>),
}

impl AcceptedReceiptStore for Resolved {
    type Error = CellError;

    fn resolve(&self, _reference: &Digest) -> Result<ReceiptResolution<'_>, CellError> {
        // `bind_receipt` checks the requested digest against the receipt.
        Ok(match self {
            Self::Unknown => ReceiptResolution::Unknown,
            Self::Replayed => ReceiptResolution::Replayed,
            Self::Accepted(evidence) => ReceiptResolution::Accepted {
                acceptance: &evidence.acceptance,
                receipt: &evidence.receipt,
            },
        })
    }
}

/// RECEIPT-CONSUME-1: resolve `reference` to a checked binding under the
/// cell's scope, `main`, `manifest`, effective clock and state, and the
/// command authority its installed recheck grants now; `None` without one.
///
/// # Errors
/// A typed binding refusal, or [`ReceiptBindingError::Store`] for a host
/// failure.
pub(crate) fn resolve<E, C, I, S: SqlEngine>(
    cell: &SqliteCell<E, C, I, S>,
    reference: &ReceiptReference,
    manifest: &Digest,
) -> Result<Option<ReceiptBinding<JsonObject>>, ReceiptBindingError<CellError>> {
    let Some(authorizer) = cell.receipts.authorizer.as_deref() else {
        return Ok(None);
    };
    let store = cell.store();
    if store.fork_pin().is_some() {
        return bind_receipt(&Resolved::Unknown, reference).map(Some);
    }
    let lookup = Lookup {
        scope: &cell.scope,
        now: cell.now(),
        authorizer,
    };
    // One transaction: the retained row, the pending delivery, the command
    // state and its version, the pinned facts and the clock floor are read
    // from one committed snapshot.
    let resolved = store
        .write_tx(|| Ok(evidence(store, &lookup, reference.digest(), manifest)))
        .map_err(ReceiptBindingError::Store)?;
    bind_receipt(&resolved?, reference).map(Some)
}

/// The cell context a lookup resolves under.
struct Lookup<'a> {
    scope: &'a str,
    now: LogicalTime,
    authorizer: &'a dyn ResultAuthorizer,
}

fn evidence<S: SqlEngine>(
    store: &CellStore<S>,
    lookup: &Lookup<'_>,
    reference: &Digest,
    manifest: &Digest,
) -> Result<Resolved, ReceiptBindingError<CellError>> {
    let reader = store.read();
    let floor = reader.clock_floor().map_err(ReceiptBindingError::Store)?;
    // A sibling handle may have committed past this handle's clock.
    let now = lookup.now.max(LogicalTime::from_millis(floor));
    let Some(retained) =
        load(&reader, lookup.scope, reference).map_err(ReceiptBindingError::Store)?
    else {
        return Ok(Resolved::Unknown);
    };
    if retained.receipt.manifest != *manifest {
        return Ok(Resolved::Unknown);
    }
    if !repins(&retained.dispatched)? {
        return Err(ReceiptBindingError::Mismatch);
    }
    let receipt = &retained.receipt;
    let Some(pending) = reader
        .pending_result(&receipt.command_id, receipt.run.generation)
        .map_err(ReceiptBindingError::Store)?
    else {
        // An unconsumed row means a newer generation replaced this delivery;
        // otherwise a commit consumed it.
        return if reader
            .pending_command(&receipt.command_id)
            .map_err(ReceiptBindingError::Store)?
            .is_some()
        {
            Err(ReceiptBindingError::NotAccepted)
        } else {
            Ok(Resolved::Replayed)
        };
    };
    if !delivers(&pending, receipt, &retained.dispatched.invocation) {
        return Err(ReceiptBindingError::Mismatch);
    }
    let Some((state, state_version)) =
        command_state(&reader, &receipt.command_id).map_err(ReceiptBindingError::Store)?
    else {
        return Err(ReceiptBindingError::Mismatch);
    };
    let context = ReconcileContext {
        tenant: lookup.scope,
        world: MAIN_WORLD,
        now,
        policy: ReconcilePolicy::DEFAULT,
    };
    let command = CommandId(receipt.command_id.clone());
    let completion = Completion {
        receipt,
        command: &command,
        generation: Generation(receipt.run.generation),
        // Intake authenticated this executor for this immutable receipt.
        authenticated_executor: Some(receipt.provenance.executor.as_str()),
    };
    let checks = ResultChecks {
        authorizer: lookup.authorizer,
        validator: &RetainedContract,
    };
    let mut acceptance = match reconcile(
        &context,
        &retained.dispatched,
        &state,
        &completion,
        &checks,
        &View(&reader),
    ) {
        Ok(Reconciliation::Accepted(acceptance)) => acceptance,
        Ok(Reconciliation::Refused(_refused)) => return Err(ReceiptBindingError::NotAccepted),
        Err(ReconcileError::View(error)) => return Err(ReceiptBindingError::Store(error)),
        Err(ReconcileError::Canonical(error)) => {
            return Err(ReceiptBindingError::Canonical(error));
        }
    };
    // A re-issue or cancellation between resolution and the accepting
    // commit moves the command state and refuses that commit.
    acceptance.reads.push(FactExpectation {
        key: FactKey::new(FUNCTION_COMMAND_TYPE, receipt.command_id.as_str()),
        expected: state_version,
    });
    Ok(Resolved::Accepted(Box::new(Evidence {
        acceptance,
        receipt: retained.receipt,
    })))
}
