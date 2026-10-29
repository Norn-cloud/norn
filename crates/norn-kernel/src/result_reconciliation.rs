//! Result reconciliation (T2-RES-1, #2777; ruling 2026-09-27 compute
//! boundary §4).
//!
//! A Function or §26 producer result is an asynchronous **proposal**, not a
//! state transition. At dispatch the committed command pins its generation,
//! manifest and execution profile, input identity, and a relevant-state
//! dependency token ([`DispatchedCommand`], built from the T1-FN-1
//! invocation). When the result returns, [`reconcile`] decides, purely,
//! whether it may drive a commit. Its checks run in the ruling's order, and
//! the first failure refuses with nothing applied:
//!
//! 1. **Scope and world**: the receipt's command belongs to this tenant and
//!    the `main` world ([`RefusalReason::OutOfScope`]).
//! 2. **Command ownership**: the command is the dispatched one, and a result
//!    of an older generation is fenced ([`RefusalReason::Fenced`], or
//!    [`RefusalReason::Cancelled`] after a cancellation); a newer generation
//!    was never dispatched ([`RefusalReason::NotOwned`]).
//! 3. **Schema and dispatch binding**: the receipt names this dispatch, its
//!    manifest, execution profile, and dependency token, and the delivery
//!    that carried it ([`RefusalReason::BindingMismatch`]); then the full
//!    receipt contract (attempt range, runtime, output manifest, bounds,
//!    outcome schema) holds, by the injected [`ReceiptValidator`]
//!    ([`RefusalReason::InvalidReceipt`]).
//! 4. **Authenticated provenance**: the host authenticated the sender as the
//!    executor the receipt names ([`RefusalReason::Unauthenticated`]).
//! 5. **Artifact and input identity**: the receipt consumed exactly the
//!    pinned inputs ([`RefusalReason::InputsDiffer`]).
//! 6. **Terminal generation**: one accepted terminal result per command. A
//!    redelivery of the accepted result is a [`Classification::Duplicate`];
//!    any other result after acceptance is superseded
//!    ([`RefusalReason::AlreadyAccepted`]).
//! 7. **Deadline policy** ([`RefusalReason::DeadlinePassed`]).
//! 8. **Current authorization**: the command's authority is re-checked now,
//!    not at dispatch ([`RefusalReason::Unauthorized`]).
//! 9. **Declared freshness policy**:
//!    - `relevant_state`: every pinned fact version, every predicate's exact
//!      ordered membership (so a phantom insert or delete under the
//!      predicate invalidates it), and every collection epoch still hold.
//!      A failure is [`Classification::Stale`] and is rescheduled at the
//!      next generation with a fresh token, at most
//!      [`ReconcilePolicy::max_reschedules`] times;
//!    - `destination_cas`: the destination still has the expected owner
//!      version (and step 5 already pinned the immutable inputs). A moved
//!      destination is [`Classification::Superseded`]: the image-A result
//!      for a document that now holds image B is never applied;
//!    - `append_only`: the observation stays historically useful.
//!
//!    Commits that touch nothing the token pins never invalidate a result.
//!
//! An [`Acceptance`] carries the compare-and-swap expectations that make the
//! fact and destination checks hold **in the commit that accepts the
//! outcome** ([`Acceptance::commit`]): a concurrent change to a pinned object
//! between reconciliation and commit refuses the whole commit. Predicate and
//! collection pins have no per-object CAS, so they hold at commit time only
//! if nothing else commits in between: when
//! [`Acceptance::requires_cell_lane`] is set, the host must hold the cell's
//! single-writer lane **from reconciliation through the accepting commit**
//! (every v1 host already runs one writer per cell).
//!
//! A [`Refused`] result is still consumed and recorded ([`ResultRecord`],
//! [`Refused::commit`]); it never advances the transition. Duplicate and
//! late deliveries are absorbed by generation fencing, so repeated stale
//! results coalesce instead of looping.
//!
//! Fan-in ([`FanIn`]) aggregates child results by explicit child id and
//! generation, never arrival order, under an all/any/quorum policy: exactly
//! one child acceptance is [`FanInStep::Complete`], the one whose commit also
//! emits the next command.
//!
//! The command's state ([`CommandState`]) and the fan-in state are plain
//! data the host persists with the command (its outbox row or a fact), and
//! the next state must be written **in the same commit** that accepts or
//! records the result (and, for a completed fan-in, emits the next command),
//! or a crash could count a child twice. This module performs no IO.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use norn_ir::canonical::CanonicalJsonError;
use norn_ir::function::{
    Authority, Destination, Digest, FreshnessMode, FunctionInvocation, FunctionReceipt, MemberPin,
    PredicatePin,
};
use serde::{Deserialize, Serialize};

use crate::cell_host::{CellCommit, Consumed, FactExpectation, FactKey, FactVersion, FactWrite};
use crate::effect::{
    CommandId, DependencyToken, Effect, FreshnessPolicy, Generation, InputIdentity, OwnerVersion,
};
use crate::observations::LogicalTime;

/// The only world that performs (ruling 2026-09-25 §2).
pub const MAIN_WORLD: &str = "main";

/// The pins a cell commits with a Function command at dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchedCommand {
    /// The admitted invocation command.
    pub invocation: FunctionInvocation,
    /// Its digest: the dispatch a receipt must name.
    pub dispatch: Digest,
    /// The admitted manifest's execution-profile digest.
    pub execution_profile: Digest,
    /// The immutable-input identity.
    pub input_identity: Digest,
    /// The dependency token digest.
    pub dependency_token: Digest,
}

impl DispatchedCommand {
    /// Pin an admitted invocation (`norn-checker`'s
    /// `check_function_invocation` passed) under the admitted manifest's
    /// execution profile.
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn pin(
        invocation: FunctionInvocation,
        execution_profile: Digest,
    ) -> Result<Self, CanonicalJsonError> {
        Ok(Self {
            dispatch: invocation.content_digest()?,
            input_identity: invocation.input_identity()?,
            dependency_token: invocation.dependency.digest()?,
            execution_profile,
            invocation,
        })
    }

    /// The command id.
    #[must_use]
    pub fn command_id(&self) -> CommandId {
        CommandId(self.invocation.command_id.clone())
    }

    /// The generation it was dispatched under.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        Generation(self.invocation.run.generation)
    }

    /// The kernel freshness policy the command is enqueued with
    /// ([`crate::Command::freshness`]). `None` for `destination_cas` without a
    /// destination, which admission refuses.
    #[must_use]
    pub fn freshness(&self) -> Option<FreshnessPolicy> {
        match self.invocation.freshness {
            FreshnessMode::RelevantState => Some(FreshnessPolicy::RelevantState {
                token: DependencyToken(self.dependency_token.0.clone()),
            }),
            FreshnessMode::DestinationCas => {
                self.invocation.destination.as_ref().map(|destination| {
                    FreshnessPolicy::DestinationCas {
                        inputs: InputIdentity(self.input_identity.0.clone()),
                        expected: OwnerVersion(destination.expected_owner_version),
                    }
                })
            }
            FreshnessMode::AppendOnly => Some(FreshnessPolicy::AppendOnly),
        }
    }
}

/// The command's accepted terminal result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptedResult {
    /// The generation it was accepted under.
    pub generation: Generation,
    /// Its result identity (attempt and metrics excluded).
    pub result_identity: Digest,
    /// Its receipt digest.
    pub receipt: Digest,
}

/// The reconciliation state of one command, persisted with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandState {
    /// The current request generation. A re-issue or cancellation bumps it,
    /// fencing every older attempt.
    pub generation: Generation,
    /// Whether the command was cancelled (at the generation before
    /// `generation`).
    pub cancelled: bool,
    /// The accepted terminal result, once there is one.
    pub accepted: Option<AcceptedResult>,
    /// How many times a stale result rescheduled the command.
    pub reschedules: u32,
}

impl CommandState {
    /// The state of a freshly dispatched command.
    #[must_use]
    pub const fn dispatched(command: &DispatchedCommand) -> Self {
        Self {
            generation: command.generation(),
            cancelled: false,
            accepted: None,
            reschedules: 0,
        }
    }

    /// The state after cancelling the command: the generation is bumped so
    /// every outstanding attempt is fenced. `None` on generation overflow.
    #[must_use]
    pub fn cancel(&self) -> Option<Self> {
        Some(Self {
            generation: self.generation.checked_next()?,
            cancelled: true,
            ..self.clone()
        })
    }
}

/// Reconciliation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcilePolicy {
    /// How many times stale results may reschedule one command.
    pub max_reschedules: u32,
    /// Whether a result that arrives after the command's deadline may still
    /// be accepted.
    pub accept_late: bool,
}

impl ReconcilePolicy {
    /// The default: three reschedules, late results refused.
    pub const DEFAULT: Self = Self {
        max_reschedules: 3,
        accept_late: false,
    };
}

impl Default for ReconcilePolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The cell a result is reconciled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileContext<'a> {
    /// The cell's tenant.
    pub tenant: &'a str,
    /// The world reconciling (only `main` accepts results).
    pub world: &'a str,
    /// The host's current logical time.
    pub now: LogicalTime,
    /// The policy.
    pub policy: ReconcilePolicy,
}

/// One delivered result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion<'a> {
    /// The receipt.
    pub receipt: &'a FunctionReceipt,
    /// The command the host's `perform` delivery names.
    pub command: &'a CommandId,
    /// The generation the delivery names.
    pub generation: Generation,
    /// The executor identity the host authenticated for the sender, if any.
    pub authenticated_executor: Option<&'a str>,
}

impl Completion<'_> {
    /// The delivery a commit consumes.
    #[must_use]
    pub fn consumed(&self) -> Consumed {
        Consumed::Result {
            command: self.command.clone(),
            generation: self.generation,
        }
    }
}

/// The current state a freshness check reads, supplied by the host.
pub trait StateView {
    /// The host's read failure.
    type Error;

    /// The current version of one object ([`FactVersion::ABSENT`] if never
    /// written).
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn fact_version(&self, key: &FactKey) -> Result<FactVersion, Self::Error>;

    /// The current members of a declared predicate, in its declared order.
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn predicate_members(&self, predicate: &PredicatePin) -> Result<Vec<MemberPin>, Self::Error>;

    /// The current epoch of one collection (object type).
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn collection_epoch(&self, object_type: &str) -> Result<u64, Self::Error>;
}

/// What the authorizer is asked at completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultAuthzRequest<'a> {
    /// The tenant.
    pub tenant: &'a str,
    /// The authority the command runs under.
    pub authority: &'a Authority,
    /// The command id.
    pub command: &'a str,
    /// The Function manifest digest.
    pub manifest: &'a Digest,
    /// The destination the result would be written to.
    pub destination: Option<&'a Destination>,
}

/// Why the authorizer denies accepting a result now.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultDenial {
    /// The authorizer's stable reason code.
    pub reason: String,
}

/// The authorization recheck at completion. The kernel never evaluates
/// policy itself; the shell adapts its policy decision point to this trait.
pub trait ResultAuthorizer {
    /// Decides whether the command's authority may still apply a result.
    ///
    /// # Errors
    ///
    /// The [`ResultDenial`].
    fn authorize(&self, request: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial>;
}

/// Why the receipt contract does not hold.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvalidReceipt {
    /// The validator's stable reason (a `norn-checker` code).
    pub reason: String,
}

/// The receipt contract check at completion: attempt range, runtime,
/// output-manifest digest, bounds, summary size, cache origin, and the
/// output and error schemas. The kernel does not re-implement admission; the
/// shell adapts `norn-checker`'s `check_function_receipt` (and its outcome
/// schema validation) to this trait.
pub trait ReceiptValidator {
    /// Validates `receipt` against the dispatched command and its admitted
    /// manifest.
    ///
    /// # Errors
    ///
    /// The [`InvalidReceipt`].
    fn validate(
        &self,
        dispatched: &DispatchedCommand,
        receipt: &FunctionReceipt,
    ) -> Result<(), InvalidReceipt>;
}

/// How a refused result is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// The relevant state changed since dispatch; the command may be
    /// rescheduled with a fresh token.
    Stale,
    /// A newer generation, a cancellation, an accepted result, a passed
    /// deadline, or a moved destination replaced this request.
    Superseded,
    /// A redelivery of the already accepted result.
    Duplicate,
    /// Not a valid result of this dispatch, or not authorized now.
    Rejected,
}

/// Why a result was refused.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RefusalReason {
    /// The command belongs to another tenant, or this is not `main`.
    OutOfScope {
        /// What did not match.
        detail: String,
    },
    /// The command was never dispatched at this id and generation.
    NotOwned,
    /// The receipt is not bound to this dispatch.
    BindingMismatch {
        /// The receipt member that does not match.
        member: String,
    },
    /// The receipt violates its contract.
    InvalidReceipt {
        /// The validator's reason.
        reason: String,
    },
    /// The host did not authenticate the sender as the named executor.
    Unauthenticated,
    /// The receipt consumed other inputs than the dispatch pinned.
    InputsDiffer,
    /// An older generation: a re-issue fenced it.
    Fenced {
        /// The current generation.
        current: Generation,
    },
    /// The command was cancelled.
    Cancelled,
    /// The command already has an accepted terminal result.
    AlreadyAccepted,
    /// The result arrived after the deadline.
    DeadlinePassed {
        /// The deadline, in logical milliseconds.
        deadline: u64,
    },
    /// The authority may no longer apply the result.
    Unauthorized {
        /// The authorizer's reason.
        reason: String,
    },
    /// Pinned relevant state changed.
    RelevantStateChanged {
        /// The pins that no longer hold (`type/key`, `predicate:type/name`,
        /// `collection:type`).
        changed: Vec<String>,
    },
    /// The destination moved past the expected owner version.
    DestinationMoved {
        /// The expected owner version.
        expected: u64,
        /// The current version.
        current: u64,
    },
}

/// The ledger record of a reconciled result, accepted or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResultRecord {
    /// The command.
    pub command: CommandId,
    /// The receipt's generation.
    pub generation: Generation,
    /// The receipt's attempt.
    pub attempt: u32,
    /// The receipt digest.
    pub receipt: Digest,
    /// The result identity.
    pub result_identity: Digest,
    /// `None` when accepted, else the classification.
    pub classification: Option<Classification>,
    /// `None` when accepted, else the reason.
    pub reason: Option<RefusalReason>,
}

/// An accepted result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acceptance {
    /// The delivery the accepting commit consumes.
    pub consumed: Consumed,
    /// Compare-and-swap expectations on the pinned facts and predicate
    /// members, at their pinned versions.
    pub reads: Vec<FactExpectation>,
    /// The destination owner-version expectation (`destination_cas`).
    pub destination: Option<FactExpectation>,
    /// Whether the token pins predicates or collections, so the accepting
    /// commit must be serialized on the cell lane.
    pub requires_cell_lane: bool,
    /// The record to ledger.
    pub record: ResultRecord,
    /// The command's next state (the accepted result recorded).
    pub state: CommandState,
}

/// A write in the accepting commit expects another version than the pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceConflict {
    /// The object.
    pub key: FactKey,
    /// The pinned version.
    pub pinned: FactVersion,
    /// The version the write expects.
    pub expected: FactVersion,
}

impl fmt::Display for AcceptanceConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the write to {} expects {}, but the result was reconciled at {}",
            self.key, self.expected, self.pinned
        )
    }
}

impl std::error::Error for AcceptanceConflict {}

impl Acceptance {
    /// The commit that accepts the result: it consumes the delivery and
    /// carries every compare-and-swap expectation, plus the caller's writes,
    /// events, and effects (for example the next command of a fan-in). A
    /// write to a pinned object carries the expectation itself and must
    /// expect the pinned version.
    ///
    /// # Errors
    ///
    /// An [`AcceptanceConflict`] when a write expects another version.
    pub fn commit<E, C, T>(
        &self,
        writes: Vec<FactWrite>,
        events: Vec<E>,
        effects: Vec<Effect<C, T>>,
    ) -> Result<CellCommit<E, C, T>, AcceptanceConflict> {
        let mut pinned: BTreeMap<&FactKey, FactVersion> = BTreeMap::new();
        for expectation in self.reads.iter().chain(self.destination.as_ref()) {
            pinned.insert(&expectation.key, expectation.expected);
        }
        for write in &writes {
            if let Some(version) = pinned.remove(&write.key)
                && version != write.expected
            {
                return Err(AcceptanceConflict {
                    key: write.key.clone(),
                    pinned: version,
                    expected: write.expected,
                });
            }
        }
        Ok(CellCommit {
            consumes: Some(self.consumed.clone()),
            reads: pinned
                .into_iter()
                .map(|(key, expected)| FactExpectation {
                    key: key.clone(),
                    expected,
                })
                .collect(),
            writes,
            events,
            effects,
            links: Vec::new(),
            link_reads: Vec::new(),
        })
    }
}

/// A refused result: recorded and consumed, never applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The delivery the recording commit consumes.
    pub consumed: Consumed,
    /// The classification.
    pub classification: Classification,
    /// The reason.
    pub reason: RefusalReason,
    /// The record to ledger.
    pub record: ResultRecord,
    /// The command's next state.
    pub state: CommandState,
    /// When `Some`, re-issue the command at this generation with a fresh
    /// dependency token in the same commit (a bounded reschedule).
    pub reschedule: Option<Generation>,
}

impl Refused {
    /// The commit that records the refused result: it consumes the delivery
    /// and carries the caller's events (the record) and effects (a
    /// reschedule), and no state write.
    #[must_use]
    pub fn commit<E, C, T>(
        &self,
        events: Vec<E>,
        effects: Vec<Effect<C, T>>,
    ) -> CellCommit<E, C, T> {
        CellCommit {
            consumes: Some(self.consumed.clone()),
            events,
            effects,
            ..CellCommit::empty()
        }
    }
}

/// The reconciliation of one delivered result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The result may drive the transition, under the acceptance's CAS.
    Accepted(Acceptance),
    /// The result is recorded and does not advance the transition.
    Refused(Refused),
}

/// The injected checks reconciliation delegates: the authorization recheck
/// and the receipt contract.
#[derive(Debug)]
pub struct ResultChecks<'a, A: ?Sized, R: ?Sized> {
    /// The authorization recheck.
    pub authorizer: &'a A,
    /// The receipt contract.
    pub validator: &'a R,
}

/// Why reconciliation could not decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileError<E> {
    /// The host's state view failed.
    View(E),
    /// The receipt cannot be canonicalized, so it has no identity to record.
    Canonical(CanonicalJsonError),
}

impl<E: fmt::Display> fmt::Display for ReconcileError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::View(error) => write!(f, "the state view failed: {error}"),
            Self::Canonical(error) => write!(f, "the receipt has no canonical identity: {error}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for ReconcileError<E> {}

/// Reconcile one delivered result against the dispatch, the command's state,
/// current authorization, and current state. See the module docs for the
/// order of checks.
///
/// # Errors
///
/// [`ReconcileError::View`] when the host's view fails, or
/// [`ReconcileError::Canonical`] when the receipt cannot be canonicalized;
/// nothing is decided either way and the delivery stays pending.
pub fn reconcile<V, A, R>(
    context: &ReconcileContext<'_>,
    dispatched: &DispatchedCommand,
    state: &CommandState,
    completion: &Completion<'_>,
    checks: &ResultChecks<'_, A, R>,
    view: &V,
) -> Result<Reconciliation, ReconcileError<V::Error>>
where
    V: StateView + ?Sized,
    A: ResultAuthorizer + ?Sized,
    R: ReceiptValidator + ?Sized,
{
    let record = record_of(completion.receipt).map_err(ReconcileError::Canonical)?;
    let refuse = |classification, reason| {
        Ok(Reconciliation::Refused(refusal(
            completion,
            state,
            &record,
            classification,
            reason,
        )))
    };
    if let Some((classification, reason)) =
        check_dispatch(context, dispatched, state, completion, checks.validator)
    {
        return refuse(classification, reason);
    }
    let invocation = &dispatched.invocation;
    if !context.policy.accept_late && context.now.as_millis() > invocation.deadline_millis {
        return refuse(
            Classification::Superseded,
            RefusalReason::DeadlinePassed {
                deadline: invocation.deadline_millis,
            },
        );
    }
    let request = ResultAuthzRequest {
        tenant: &invocation.tenant,
        authority: &invocation.authority,
        command: &invocation.command_id,
        manifest: &invocation.manifest,
        destination: invocation.destination.as_ref(),
    };
    if let Err(denial) = checks.authorizer.authorize(&request) {
        return refuse(
            Classification::Rejected,
            RefusalReason::Unauthorized {
                reason: denial.reason,
            },
        );
    }
    let fresh = match freshness(dispatched, view).map_err(ReconcileError::View)? {
        Freshness::Fresh(fresh) => fresh,
        Freshness::Changed(changed) => {
            return Ok(Reconciliation::Refused(stale(
                context, completion, state, &record, changed,
            )));
        }
        Freshness::DestinationMoved { expected, current } => {
            return refuse(
                Classification::Superseded,
                RefusalReason::DestinationMoved { expected, current },
            );
        }
        Freshness::NoDestination => {
            return refuse(
                Classification::Rejected,
                RefusalReason::BindingMismatch {
                    member: "destination".to_owned(),
                },
            );
        }
    };
    let accepted = AcceptedResult {
        generation: completion.generation,
        result_identity: record.result_identity.clone(),
        receipt: record.receipt.clone(),
    };
    Ok(Reconciliation::Accepted(Acceptance {
        consumed: completion.consumed(),
        reads: fresh.reads,
        destination: fresh.destination,
        requires_cell_lane: fresh.requires_cell_lane,
        record,
        state: CommandState {
            accepted: Some(accepted),
            ..state.clone()
        },
    }))
}

fn record_of(receipt: &FunctionReceipt) -> Result<ResultRecord, CanonicalJsonError> {
    Ok(ResultRecord {
        command: CommandId(receipt.command_id.clone()),
        generation: Generation(receipt.run.generation),
        attempt: receipt.attempt,
        receipt: receipt.content_digest()?,
        result_identity: receipt.result_identity()?,
        classification: None,
        reason: None,
    })
}

fn refusal(
    completion: &Completion<'_>,
    state: &CommandState,
    record: &ResultRecord,
    classification: Classification,
    reason: RefusalReason,
) -> Refused {
    Refused {
        consumed: completion.consumed(),
        classification,
        record: ResultRecord {
            classification: Some(classification),
            reason: Some(reason.clone()),
            ..record.clone()
        },
        reason,
        state: state.clone(),
        reschedule: None,
    }
}

fn mismatch(member: &str) -> (Classification, RefusalReason) {
    (
        Classification::Rejected,
        RefusalReason::BindingMismatch {
            member: member.to_owned(),
        },
    )
}

/// Steps 1–6: scope, ownership and fencing, binding and receipt contract,
/// provenance, inputs, terminal generation. `None` when every check passes.
fn check_dispatch<R: ReceiptValidator + ?Sized>(
    context: &ReconcileContext<'_>,
    dispatched: &DispatchedCommand,
    state: &CommandState,
    completion: &Completion<'_>,
    validator: &R,
) -> Option<(Classification, RefusalReason)> {
    let receipt = completion.receipt;
    let invocation = &dispatched.invocation;
    if context.world != MAIN_WORLD || invocation.world != MAIN_WORLD {
        return Some((
            Classification::Rejected,
            RefusalReason::OutOfScope {
                detail: format!("world {}", context.world),
            },
        ));
    }
    if invocation.tenant != context.tenant {
        return Some((
            Classification::Rejected,
            RefusalReason::OutOfScope {
                detail: format!("tenant {}", invocation.tenant),
            },
        ));
    }
    if receipt.command_id != invocation.command_id || receipt.run.id != invocation.run.id {
        return Some((Classification::Rejected, RefusalReason::NotOwned));
    }
    let generation = Generation(receipt.run.generation);
    if generation < state.generation {
        let reason = if state.cancelled {
            RefusalReason::Cancelled
        } else {
            RefusalReason::Fenced {
                current: state.generation,
            }
        };
        return Some((Classification::Superseded, reason));
    }
    if generation > state.generation || generation != dispatched.generation() {
        return Some((Classification::Rejected, RefusalReason::NotOwned));
    }
    if completion.command.0 != receipt.command_id || completion.generation != generation {
        return Some(mismatch("delivery"));
    }
    for (member, pinned, found) in [
        ("dispatch", &dispatched.dispatch, &receipt.dispatch),
        ("manifest", &invocation.manifest, &receipt.manifest),
        (
            "executionProfile",
            &dispatched.execution_profile,
            &receipt.execution_profile,
        ),
        (
            "dependencyToken",
            &dispatched.dependency_token,
            &receipt.dependency_token,
        ),
    ] {
        if pinned != found {
            return Some(mismatch(member));
        }
    }
    if let Err(invalid) = validator.validate(dispatched, receipt) {
        return Some((
            Classification::Rejected,
            RefusalReason::InvalidReceipt {
                reason: invalid.reason,
            },
        ));
    }
    if completion.authenticated_executor != Some(receipt.provenance.executor.as_str()) {
        return Some((Classification::Rejected, RefusalReason::Unauthenticated));
    }
    if receipt.input_identity != dispatched.input_identity {
        return Some((Classification::Rejected, RefusalReason::InputsDiffer));
    }
    if let Some(accepted) = &state.accepted {
        let identity = receipt.result_identity().ok();
        let classification = if accepted.generation == generation
            && identity.as_ref() == Some(&accepted.result_identity)
        {
            Classification::Duplicate
        } else {
            Classification::Superseded
        };
        return Some((classification, RefusalReason::AlreadyAccepted));
    }
    None
}

struct Fresh {
    reads: Vec<FactExpectation>,
    destination: Option<FactExpectation>,
    requires_cell_lane: bool,
}

enum Freshness {
    Fresh(Fresh),
    Changed(Vec<String>),
    DestinationMoved { expected: u64, current: u64 },
    NoDestination,
}

fn freshness<V: StateView + ?Sized>(
    dispatched: &DispatchedCommand,
    view: &V,
) -> Result<Freshness, V::Error> {
    let invocation = &dispatched.invocation;
    match invocation.freshness {
        FreshnessMode::AppendOnly => Ok(Freshness::Fresh(Fresh {
            reads: Vec::new(),
            destination: None,
            requires_cell_lane: false,
        })),
        FreshnessMode::DestinationCas => {
            let Some(destination) = &invocation.destination else {
                return Ok(Freshness::NoDestination);
            };
            let key = FactKey::new(destination.object_type.clone(), destination.key.clone());
            let current = view.fact_version(&key)?;
            if current.0 == destination.expected_owner_version {
                Ok(Freshness::Fresh(Fresh {
                    reads: Vec::new(),
                    destination: Some(FactExpectation {
                        key,
                        expected: current,
                    }),
                    requires_cell_lane: false,
                }))
            } else {
                Ok(Freshness::DestinationMoved {
                    expected: destination.expected_owner_version,
                    current: current.0,
                })
            }
        }
        FreshnessMode::RelevantState => relevant_state(dispatched, view),
    }
}

fn relevant_state<V: StateView + ?Sized>(
    dispatched: &DispatchedCommand,
    view: &V,
) -> Result<Freshness, V::Error> {
    let token = &dispatched.invocation.dependency;
    let mut changed = Vec::new();
    let mut reads: BTreeMap<FactKey, FactVersion> = BTreeMap::new();
    for pin in &token.facts {
        let key = FactKey::new(pin.object_type.clone(), pin.key.clone());
        if view.fact_version(&key)?.0 != pin.version {
            changed.push(key.to_string());
        }
        reads.insert(key, FactVersion(pin.version));
    }
    for predicate in &token.predicates {
        if view.predicate_members(predicate)? != predicate.members {
            changed.push(format!(
                "predicate:{}/{}",
                predicate.object_type, predicate.predicate
            ));
        }
        for member in &predicate.members {
            reads.insert(
                FactKey::new(predicate.object_type.clone(), member.key.clone()),
                FactVersion(member.version),
            );
        }
    }
    for collection in &token.collections {
        if view.collection_epoch(&collection.object_type)? != collection.epoch {
            changed.push(format!("collection:{}", collection.object_type));
        }
    }
    if !changed.is_empty() {
        return Ok(Freshness::Changed(changed));
    }
    Ok(Freshness::Fresh(Fresh {
        reads: reads
            .into_iter()
            .map(|(key, expected)| FactExpectation { key, expected })
            .collect(),
        destination: None,
        requires_cell_lane: !token.predicates.is_empty() || !token.collections.is_empty(),
    }))
}

/// A stale result: recorded, and rescheduled at the next generation while
/// the reschedule budget lasts.
fn stale(
    context: &ReconcileContext<'_>,
    completion: &Completion<'_>,
    state: &CommandState,
    record: &ResultRecord,
    changed: Vec<String>,
) -> Refused {
    let mut refused = refusal(
        completion,
        state,
        record,
        Classification::Stale,
        RefusalReason::RelevantStateChanged { changed },
    );
    if state.reschedules < context.policy.max_reschedules
        && let Some(next) = state.generation.checked_next()
    {
        refused.state = CommandState {
            generation: next,
            reschedules: state.reschedules.saturating_add(1),
            ..state.clone()
        };
        refused.reschedule = Some(next);
    }
    refused
}

// ---------------------------------------------------------------------------
// Fan-in
// ---------------------------------------------------------------------------

/// How many children a fan-in needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum FanInPolicy {
    /// Every child.
    All,
    /// Any one child.
    Any,
    /// At least `required` children.
    Quorum {
        /// The quorum.
        required: u32,
    },
}

/// Why a fan-in cannot be declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanInError {
    /// No children.
    NoChildren,
    /// A quorum of zero, or larger than the children.
    Quorum {
        /// The quorum.
        required: u32,
        /// The number of children.
        children: usize,
    },
}

impl fmt::Display for FanInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoChildren => f.write_str("a fan-in needs at least one child"),
            Self::Quorum { required, children } => write!(
                f,
                "a quorum of {required} is not between 1 and the {children} children"
            ),
        }
    }
}

impl std::error::Error for FanInError {}

/// A fan-in over explicit child commands and generations. Built only by
/// [`FanIn::new`], which validates the policy against the children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanIn {
    children: BTreeMap<CommandId, Generation>,
    policy: FanInPolicy,
    required: usize,
}

/// The persisted progress of a fan-in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FanInState {
    /// The children whose results were accepted.
    pub accepted: BTreeSet<CommandId>,
    /// Whether the fan-in completed (its next command was emitted).
    pub complete: bool,
}

/// Why a child result does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanInIgnored {
    /// Not a child of this fan-in.
    UnknownChild,
    /// A generation other than the child's pinned one.
    OtherGeneration,
    /// The child was already counted.
    Duplicate,
    /// The fan-in already completed.
    AlreadyComplete,
}

/// The effect of one accepted child result on a fan-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FanInStep {
    /// Counted; more children are required.
    Counted {
        /// The next state.
        state: FanInState,
    },
    /// The last required child: accept it and emit the next command in the
    /// same commit. Happens exactly once per fan-in.
    Complete {
        /// The next state.
        state: FanInState,
    },
    /// Not counted.
    Ignored {
        /// Why.
        reason: FanInIgnored,
    },
}

impl FanIn {
    /// Declare a fan-in.
    ///
    /// # Errors
    ///
    /// A [`FanInError`] for no children or an impossible quorum.
    pub fn new(
        children: BTreeMap<CommandId, Generation>,
        policy: FanInPolicy,
    ) -> Result<Self, FanInError> {
        if children.is_empty() {
            return Err(FanInError::NoChildren);
        }
        let required = match policy {
            FanInPolicy::All => children.len(),
            FanInPolicy::Any => 1,
            FanInPolicy::Quorum { required } => {
                let count = usize::try_from(required).unwrap_or(usize::MAX);
                if count == 0 || count > children.len() {
                    return Err(FanInError::Quorum {
                        required,
                        children: children.len(),
                    });
                }
                count
            }
        };
        Ok(Self {
            children,
            policy,
            required,
        })
    }

    /// The children and their pinned generations.
    #[must_use]
    pub const fn children(&self) -> &BTreeMap<CommandId, Generation> {
        &self.children
    }

    /// The policy.
    #[must_use]
    pub const fn policy(&self) -> FanInPolicy {
        self.policy
    }

    /// How many children complete the fan-in.
    #[must_use]
    pub const fn required(&self) -> usize {
        self.required
    }

    /// Count one accepted child result. Arrival order never matters: the
    /// child id and generation decide, and completion happens once.
    #[must_use]
    pub fn count(
        &self,
        state: &FanInState,
        child: &CommandId,
        generation: Generation,
    ) -> FanInStep {
        let ignored = |reason| FanInStep::Ignored { reason };
        let Some(pinned) = self.children.get(child) else {
            return ignored(FanInIgnored::UnknownChild);
        };
        if *pinned != generation {
            return ignored(FanInIgnored::OtherGeneration);
        }
        if state.complete {
            return ignored(FanInIgnored::AlreadyComplete);
        }
        if state.accepted.contains(child) {
            return ignored(FanInIgnored::Duplicate);
        }
        let mut next = state.clone();
        next.accepted.insert(child.clone());
        if next.accepted.len() >= self.required {
            next.complete = true;
            FanInStep::Complete { state: next }
        } else {
            FanInStep::Counted { state: next }
        }
    }
}
