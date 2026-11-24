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
//! manifest, then re-runs reconciliation at the cell's current clock and
//! state: a re-issued, cancelled or expired command, or a moved relevant-state
//! pin, refuses `action.receipt_not_accepted`, and a consumed delivery is
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
    RefusalReason, ResultAuthorizer, ResultAuthzRequest, ResultChecks, ResultDenial, StateView,
    reconcile,
};
use norn_kernel::{CommandId, FactExpectation, FactKey, FactVersion, Generation, LogicalTime};
use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, params};
use crate::error::CellError;
use crate::host::SqliteCell;
use crate::read::{PendingCommand, StoreReader};
use crate::store::{CellStore, MAIN_WORLD};

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

/// The refusal code of a predicate or collection freshness read, which this
/// path never qualifies (intake refuses such pins first).
const UNQUALIFIED_PIN: &str = "action.receipt_effect_unsupported";

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

/// What a retained row holds: the receipt, the dispatch it reconciled
/// against, and the executor identity intake authenticated.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Retained {
    receipt: FunctionReceipt,
    dispatched: DispatchedCommand,
    executor: String,
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// RECEIPT-STORE-1: reconcile an authenticated completion of a pending
    /// Function delivery and retain its receipt as action evidence.
    ///
    /// `completion` must carry exactly the receipt this cell recorded as the
    /// pending result of `dispatched`'s command and generation, whose enqueued
    /// payload is the `norn.function/v1` envelope of `dispatched`'s
    /// invocation; the dispatch digests are re-derived, never trusted. The
    /// command's persisted [`CommandState`] and `checks` (the receipt
    /// contract and the command authority's recheck) decide acceptance under
    /// this cell's scope, `main` and the cell's clock. Only an accepted
    /// receipt is retained, and retaining it again is a no-op. Nothing is
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
            self.bound_scope(),
            self.now(),
            dispatched,
            completion,
            checks,
        )
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
    let invocation = &dispatched.invocation;
    if store.fork_pin().is_some()
        || !invocation.dependency.predicates.is_empty()
        || !invocation.dependency.collections.is_empty()
        || invocation.freshness == FreshnessMode::DestinationCas
    {
        return Err(ReceiptIntakeError::Unsupported);
    }
    let repinned = DispatchedCommand::pin(invocation.clone(), dispatched.execution_profile.clone())
        .map_err(CellError::from)?;
    if repinned != *dispatched {
        return Err(ReceiptIntakeError::DispatchMismatch);
    }
    let reference = completion
        .receipt
        .content_digest()
        .map_err(CellError::from)?;
    let generation = i64::try_from(dispatched.generation().0)
        .map_err(|_overflow| CellError::OutOfRange("generation"))?;
    let retained_at =
        i64::try_from(now.as_millis()).map_err(|_overflow| CellError::OutOfRange("retained_at"))?;
    let outcome = store.write_tx(|| {
        if load(store, scope, &reference)?.is_some() {
            // The digest names this exact receipt, already reconciled.
            return Ok(Ok(()));
        }
        let reader = store.read();
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
        let body = serde_json::to_string(&Retained {
            receipt: completion.receipt.clone(),
            dispatched: dispatched.clone(),
            executor: completion.receipt.provenance.executor.clone(),
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

/// The retained row of `reference` in this scope and `main`, if any.
fn load<S: SqlEngine>(
    store: &CellStore<S>,
    scope: &str,
    reference: &Digest,
) -> Result<Option<Retained>, CellError> {
    let body = store.conn.query_optional(
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

/// The command's persisted reconciliation state and its object version.
fn command_state<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    command_id: &str,
) -> Result<Option<(CommandState, u64)>, CellError> {
    let Some(record) = reader.object(MAIN_WORLD, FUNCTION_COMMAND_TYPE, command_id)? else {
        return Ok(None);
    };
    let Some(state) = record.values.get("state") else {
        return Ok(None);
    };
    Ok(norn_ir::runtime::decode::<CommandState>(state)
        .ok()
        .map(|state| (state, record.version)))
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
        Err(CellError::ComputeRefused(UNQUALIFIED_PIN))
    }

    fn collection_epoch(&self, _object_type: &str) -> Result<u64, CellError> {
        Err(CellError::ComputeRefused(UNQUALIFIED_PIN))
    }
}

/// The receipt contract and the executor's authentication are immutable
/// facts of a retained receipt, established by the intake that retained it.
/// Resolution re-runs reconciliation for what can change since: scope,
/// world, generation fencing, cancellation, deadline and freshness. Authority
/// to publish is the accepting action's own authorization and laws.
struct RetainedChecks;

impl ResultAuthorizer for RetainedChecks {
    fn authorize(&self, _request: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
        Ok(())
    }
}

impl ReceiptValidator for RetainedChecks {
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

/// RECEIPT-CONSUME-1: resolve `reference` to a checked binding under this
/// cell's scope, `main`, `manifest` and current clock and state.
///
/// # Errors
/// A typed binding refusal, or [`ReceiptBindingError::Store`] for a host
/// failure.
pub(crate) fn resolve<S: SqlEngine>(
    store: &CellStore<S>,
    scope: &str,
    now: LogicalTime,
    reference: &ReceiptReference,
    manifest: &Digest,
) -> Result<ReceiptBinding<JsonObject>, ReceiptBindingError<CellError>> {
    let resolved = evidence(store, scope, now, reference.digest(), manifest)?;
    bind_receipt(&resolved, reference)
}

fn evidence<S: SqlEngine>(
    store: &CellStore<S>,
    scope: &str,
    now: LogicalTime,
    reference: &Digest,
    manifest: &Digest,
) -> Result<Resolved, ReceiptBindingError<CellError>> {
    if store.fork_pin().is_some() {
        return Ok(Resolved::Unknown);
    }
    let Some(retained) = load(store, scope, reference).map_err(ReceiptBindingError::Store)? else {
        return Ok(Resolved::Unknown);
    };
    if retained.receipt.manifest != *manifest {
        return Ok(Resolved::Unknown);
    }
    let reader = store.read();
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
    let Some((state, version)) =
        command_state(&reader, &receipt.command_id).map_err(ReceiptBindingError::Store)?
    else {
        return Err(ReceiptBindingError::Mismatch);
    };
    let context = ReconcileContext {
        tenant: scope,
        world: MAIN_WORLD,
        now,
        policy: ReconcilePolicy::DEFAULT,
    };
    let command = CommandId(receipt.command_id.clone());
    let completion = Completion {
        receipt,
        command: &command,
        generation: Generation(receipt.run.generation),
        authenticated_executor: Some(retained.executor.as_str()),
    };
    let checks = ResultChecks {
        authorizer: &RetainedChecks,
        validator: &RetainedChecks,
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
        expected: FactVersion(version),
    });
    Ok(Resolved::Accepted(Box::new(Evidence {
        acceptance,
        receipt: retained.receipt,
    })))
}
