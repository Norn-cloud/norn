//! Core 08 (E3, #1946, `FORK-MERGE-1` & `FORK-LIFE-1`): action-command replay
//! promotion from a direct decision fork `D_i` into `main`.
//!
//! Facts merge by replay, never by copying fork rows, pages, or receipts into
//! `main`:
//! - [`execute_action_in_fork`] attaches a canonical `fork.command_scope`
//!   bookkeeping event inside the action's own SQLite commit transaction and
//!   receipt hash so authorization scope is authenticated atomically with the
//!   action call.
//! - Every commit in `D_i` after its immutable [`ForkPin::base`] is
//!   authenticated against `D_i`'s own local export journal (`norn.ActionCall`
//!   plus its receipt-bound `fork.command_scope` event).
//! - Nested exploratory sub-forks (`parent != "main"`) are refused with
//!   [`ForkMergeRefusalReason::NestedForkForbidden`] so scenario writes in
//!   `S_i` never enter `main`.
//! - When `main` has an active installed definition set, promotion verifies
//!   that the supplied [`AdmittedDefinitions`] matches `main`'s authoritative
//!   active actions, guards, model, and DDL so stale definitions cannot execute
//!   retired or revised actions.
//! - The entire command sequence is staged first on an ephemeral fork of
//!   `main`'s verified head. Each staged command validates `World::Main`
//!   admission and any `approved_intent` read-only via [`norn_kernel::prepare_action_intent`]
//!   before advancing the staging fork under its own [`norn_kernel::World::Fork`] identity
//!   (`L5`).
//! - Before touching `main`, promotion durably reserves `D_i` at its
//!   authenticated head (`fork:promoting_head`) so no sibling handle can append
//!   a command during replay and have it stranded when `D_i` closes.
//! - Each `main` commit enforces the expected `main` head and active
//!   definitions version **inside** SQLite's commit transaction, mints fresh
//!   `main` receipts under [`norn_kernel::World::Main`], and finally closes `D_i` durably as
//!   [`crate::ForkState::Promoted`].

mod replay;

use std::collections::BTreeMap;
use std::fmt;

use norn_ir::{ArtifactEnvelope, ObjectTypeName, RuntimeValue, TraceContext};
use norn_kernel::cell_host::{FactKey, JsonObject};
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionCall, ActionCallRecord, ActionDerivedValue,
    ActionOutcome, ActionRefusal, CellHost as _, DecisionRefusal, DecisionTurn, FactEvent,
    commit_action,
};
use serde::{Deserialize, Serialize};

use crate::admitted::AdmittedDefinitions;
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::export::ExportRecord;
use crate::fork::{ForkEngine, ForkPin};
use crate::host::SqliteCell;
use crate::read::LedgerHead;
use crate::store::{CellStore, EffectRow, FactWriteRow, LinkChange, MAIN_WORLD, QuantityBatch};
use replay::{authenticate_fork_commands, replay_into_main, stage_replay};

const CMD_SCOPE_DOMAIN: &str = "norn.fork.command_scope/1";
const FORK_SCOPE_EVENT_TYPE: &str = "fork.command_scope";

/// Typed reason why `FORK-MERGE-1` refused to promote a decision fork into
/// `main` (`core.world.merge_refused`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForkMergeRefusalReason {
    /// Target is not `main`, or source is not a forked store.
    NotDecisionFork {
        /// Human-readable detail.
        detail: String,
    },
    /// Nested exploratory sub-forks (`parent != "main"`) are never promoted.
    NestedForkForbidden {
        /// The nested fork's world id.
        world: String,
        /// The fork's non-main parent world id.
        parent: String,
    },
    /// The fork and `main` do not share cell identity, scope, or base lineage.
    CellIdentityMismatch {
        /// Human-readable detail.
        detail: String,
    },
    /// The decision fork admitted no commands after its base commit.
    NoCommandsToPromote {
        /// The decision fork's world id.
        world: String,
    },
    /// Fork commits after `base` are missing, non-action commits, or disagree
    /// with the supplied command sequence or receipt-bound scope event.
    UnauthenticatedForkHistory {
        /// The fork commit sequence that failed authentication (`0` for count mismatch).
        commit_seq: u64,
        /// Human-readable detail.
        detail: String,
    },
    /// An action admitted in the fork is not in `main`'s current definitions.
    ActionNotAdmitted {
        /// The missing action id.
        action_id: String,
    },
    /// Principal authorization was revoked or denied under `main`'s current authority.
    AuthorizationDenied {
        /// The action id being replayed.
        action_id: String,
        /// The command idempotency key.
        idempotency_key: String,
        /// The calling actor whose authority was denied.
        actor: String,
        /// Stable kernel refusal code.
        code: String,
        /// Safe denial detail.
        detail: String,
    },
    /// Replaying the command against `main` was refused by definitions,
    /// preconditions, laws, approval intent, or commit admission.
    ReplayRejected {
        /// The action id being replayed.
        action_id: String,
        /// The command idempotency key.
        idempotency_key: String,
        /// Stable kernel refusal code.
        code: String,
        /// Safe refusal detail.
        detail: String,
    },
    /// Re-evaluation on `main` did not produce the same material outcome as in
    /// the decision fork (due to moved read versions, link/predicate changes,
    /// or revised rules).
    MaterialOutcomeChanged {
        /// The action id being replayed.
        action_id: String,
        /// The command idempotency key.
        idempotency_key: String,
        /// Read keys whose versions changed between fork admission and `main`.
        moved_reads: Vec<String>,
        /// Why the material outcome diverged.
        detail: String,
    },
    /// A concurrent write or runtime fault interrupted `main` replay after an
    /// earlier prefix of commands had already committed on `main`.
    PartialMainReplay {
        /// Commands already committed on `main` before the interruption.
        committed: Vec<PromotedCommand>,
        /// The command that failed to commit on `main`.
        failed_action_id: String,
        /// The failing command's idempotency key.
        failed_idempotency_key: String,
        /// Why the command failed on `main`.
        detail: String,
    },
    /// All commands committed on `main`, but closing the decision fork as
    /// `promoted` failed.
    ForkCloseFailed {
        /// Every command that committed on `main`.
        committed: Vec<PromotedCommand>,
        /// Why closing the fork failed.
        detail: String,
    },
}

impl fmt::Display for ForkMergeRefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDecisionFork { detail } | Self::CellIdentityMismatch { detail } => {
                f.write_str(detail)
            }
            Self::NestedForkForbidden { world, parent } => write!(
                f,
                "nested fork `{world}` (parent `{parent}`) cannot be promoted into main"
            ),
            Self::NoCommandsToPromote { world } => {
                write!(
                    f,
                    "decision fork `{world}` has no admitted commands to promote"
                )
            }
            Self::UnauthenticatedForkHistory { commit_seq, detail } => {
                write!(
                    f,
                    "unauthenticated fork history at commit {commit_seq}: {detail}"
                )
            }
            Self::ActionNotAdmitted { action_id } => write!(
                f,
                "action `{action_id}` is not admitted in main's current definitions"
            ),
            Self::AuthorizationDenied {
                action_id,
                idempotency_key,
                actor,
                code,
                detail,
            } => write!(
                f,
                "authorization denied for `{action_id}` ({idempotency_key}) by `{actor}` ({code}: {detail})"
            ),
            Self::ReplayRejected {
                action_id,
                idempotency_key,
                code,
                detail,
            } => write!(
                f,
                "replay rejected for `{action_id}` ({idempotency_key}) ({code}: {detail})"
            ),
            Self::MaterialOutcomeChanged {
                action_id,
                idempotency_key,
                moved_reads,
                detail,
            } => write!(
                f,
                "material outcome changed for `{action_id}` ({idempotency_key}) after moved reads [{}]: {detail}",
                moved_reads.join(", ")
            ),
            Self::PartialMainReplay {
                committed,
                failed_action_id,
                failed_idempotency_key,
                detail,
            } => write!(
                f,
                "main replay interrupted after {} committed command(s) at `{failed_action_id}` ({failed_idempotency_key}): {detail}",
                committed.len()
            ),
            Self::ForkCloseFailed { committed, detail } => write!(
                f,
                "all {} command(s) committed on main, but closing decision fork failed: {detail}",
                committed.len()
            ),
        }
    }
}

/// One governed action call captured for execution in a decision fork and
/// subsequent `FORK-MERGE-1` replay promotion into `main`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkActionCall {
    /// Admitted action id (for example `supply.po.create`).
    pub action_id: String,
    /// Structured scope required by the action (`orgId`, `environmentId`, ...).
    pub scope: BTreeMap<String, String>,
    /// Calling principal identity.
    pub actor: String,
    /// Raw caller input passed to [`commit_action`].
    pub input: JsonObject,
    /// Caller idempotency key.
    pub idempotency_key: String,
    /// Optional concrete subtype narrower than the action's `objectKind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_type: Option<ObjectTypeName>,
    /// Optional provenance trace context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,
    /// Optional prepared-intent digest if an approval binds this call on `main`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_intent: Option<String>,
}

/// One command successfully replayed from a decision fork into `main`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromotedCommand {
    /// Admitted action id.
    pub action_id: String,
    /// Caller idempotency key.
    pub idempotency_key: String,
    /// Commit sequence where the command originally committed in the fork.
    pub fork_commit_seq: u64,
    /// Fresh commit sequence minted on `main`.
    pub main_commit_seq: u64,
    /// Fresh receipt hash minted on `main`.
    pub main_receipt_hash: String,
    /// Target object written or transitioned by the action.
    pub target: FactKey,
    /// Target object version on `main` after replay.
    pub target_version: u64,
    /// Any read keys whose version had moved on `main` while still yielding
    /// the exact same material outcome.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moved_reads: Vec<String>,
}

/// Receipt of a completed `FORK-MERGE-1` promotion from decision fork `D_i`
/// into `main`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkPromotionReport {
    /// World id of the promoted decision fork (now durably `promoted`).
    pub source_world: String,
    /// Base head of `main` at which the decision fork was created.
    pub fork_base: LedgerHead,
    /// Verified head of `main` immediately before replay began.
    pub main_head_before: LedgerHead,
    /// Verified head of `main` immediately after all commands replayed.
    pub main_head_after: LedgerHead,
    /// Ordered list of commands replayed into `main`.
    pub promoted: Vec<PromotedCommand>,
}

#[derive(Debug, Clone, PartialEq)]
struct MaterialOutcome {
    target: FactKey,
    effective_input: Option<JsonObject>,
    derived_values: Vec<ActionDerivedValue>,
    from: String,
    to: String,
    row_operations: Vec<RuntimeValue>,
    writes: Vec<FactWriteRow>,
    links: Vec<LinkChange>,
    events: Vec<RuntimeValue>,
    effects: Vec<EffectRow>,
    quantity: Option<QuantityBatch<norn_ir::quantity_ledger::QuantityRequest>>,
    quantity_result: Option<QuantityBatch<norn_kernel::quantity::QuantityResult>>,
}

impl MaterialOutcome {
    fn from_export(export: &ExportRecord, record: &ActionCallRecord) -> Self {
        let writes = export
            .batch
            .writes
            .iter()
            .filter(|write| write.target().0 != ACTION_CALL_TYPE)
            .cloned()
            .collect();
        let events = export
            .batch
            .events
            .iter()
            .filter(|event| !is_fork_scope_event(event))
            .map(normalize_event_for_replay)
            .collect();
        Self {
            target: record.object.clone(),
            effective_input: record.effective_input.clone(),
            derived_values: record.derived_values.clone(),
            from: record.from.clone(),
            to: record.to.clone(),
            row_operations: record.row_operations.clone(),
            writes,
            links: export.batch.links.clone(),
            events,
            effects: export.batch.effects.clone(),
            quantity: export.batch.quantity.clone(),
            quantity_result: export.receipt.body.quantity_result.clone(),
        }
    }
}

fn is_fork_scope_event(event: &RuntimeValue) -> bool {
    matches!(
        event,
        RuntimeValue::Object(map)
            if map.get("type").and_then(RuntimeValue::as_str) == Some(FORK_SCOPE_EVENT_TYPE)
    )
}

fn scope_binding_event(
    idempotency_key: &str,
    scope: &BTreeMap<String, String>,
) -> Result<RuntimeValue, CellError> {
    let digest = crate::codec::hash_json(&(CMD_SCOPE_DOMAIN, idempotency_key, scope))?;
    Ok(RuntimeValue::Object(BTreeMap::from([
        (
            "type".to_owned(),
            RuntimeValue::String(FORK_SCOPE_EVENT_TYPE.to_owned()),
        ),
        (
            "idempotencyKey".to_owned(),
            RuntimeValue::String(idempotency_key.to_owned()),
        ),
        ("scopeDigest".to_owned(), RuntimeValue::String(digest)),
    ])))
}

fn normalize_event_for_replay(event: &RuntimeValue) -> RuntimeValue {
    // `FactEvent` is internally tagged with `#[serde(tag = "op", rename_all = "snake_case")]`.
    if let RuntimeValue::Object(map) = event
        && map.get("op").and_then(RuntimeValue::as_str) == Some("naming_series_allocated")
    {
        let mut normalized = map.clone();
        normalized.remove("epochMillis");
        return RuntimeValue::Object(normalized);
    }
    event.clone()
}

struct AuthenticatedForkCommand<'a> {
    call: &'a ForkActionCall,
    fork_commit_seq: u64,
    read_versions: BTreeMap<FactKey, u64>,
    material_outcome: MaterialOutcome,
}

fn merge_refused(reason: ForkMergeRefusalReason) -> CellError {
    CellError::ForkMergeRefused {
        reason: Box::new(reason),
    }
}

/// Execute one governed action call inside an open forked cell via the real
/// kernel [`commit_action`] path (`WORLD-FORK`, `FORK-LIFE-1`), atomically
/// attaching a receipt-bound `fork.command_scope` event in the same commit
/// transaction for `FORK-MERGE-1` authentication.
///
/// # Errors
/// `core.world.simulation_forbidden` on `main`; `core.world.fork_closed` on a
/// closed or promoting fork; `core.world.merge_refused` if `definitions`
/// disagrees with the fork's active installed definition set; or any
/// [`ActionRefusal`] from [`commit_action`].
pub fn execute_action_in_fork<S: SqlEngine>(
    fork: &mut SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    definitions: &AdmittedDefinitions,
    command: &ForkActionCall,
    authorizer: &dyn ActionAuthorizer,
) -> Result<ActionOutcome, ActionRefusal<CellError>> {
    if fork.store().fork_pin().is_none() {
        return Err(ActionRefusal::Decision(DecisionRefusal::Host(
            CellError::WorldSimulationForbidden,
        )));
    }
    if let Err(error) = fork.store().refuse_fork_write() {
        return Err(ActionRefusal::Decision(DecisionRefusal::Host(error)));
    }
    if let Err(error) = verify_active_definitions(fork.store(), definitions) {
        return Err(ActionRefusal::Decision(DecisionRefusal::Host(error)));
    }
    let Some(admitted) = definitions.action(&command.action_id) else {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: command.action_id.clone(),
        });
    };
    let scope_event = scope_binding_event(&command.idempotency_key, &command.scope)
        .map_err(|error| ActionRefusal::Decision(DecisionRefusal::Host(error)))?;
    let catalog = definitions.catalog();
    let call = ActionCall {
        action: &admitted.root,
        action_artifact_id: &admitted.artifact_id,
        action_version: &admitted.version,
        scope: command.scope.clone(),
        actor: command.actor.clone(),
        approved_intent: command.approved_intent.clone(),
        input: command.input.clone(),
        idempotency_key: command.idempotency_key.clone(),
        object_type: command.object_type.clone(),
        turn: DecisionTurn {
            id: command.idempotency_key.clone(),
            now: fork.now(),
            world: fork.store().world(),
            consumes: None,
        },
        trace: command.trace.clone(),
    };
    fork.attributed(vec![scope_event], |cell| {
        commit_action(cell, &catalog, &call, authorizer)
    })
}

/// Core 08 (`FORK-MERGE-1`, `FORK-LIFE-1`): promote an open direct decision
/// fork `D_i` into `main` by replaying every admitted action command against
/// `main`'s current state under `main`'s current definitions, laws, and
/// authority.
///
/// The full sequence is staged first on an ephemeral fork of `main`'s verified
/// head (verifying `World::Main` admission and any `approved_intent` read-only
/// via [`norn_kernel::prepare_action_intent`], and advancing staging state under its own
/// [`norn_kernel::World::Fork`] identity). Before touching `main`, `D_i` is durably reserved
/// at its authenticated head, and every `main` commit verifies the staged head
/// and active definitions version inside SQLite's write transaction.
///
/// # Errors
/// `core.world.fork_closed` if `fork` is already closed or promoting;
/// `core.world.merge_refused` with a typed [`ForkMergeRefusalReason`] if
/// validation, authentication, staging, authority, or material-outcome
/// equivalence fails; cell availability/storage errors otherwise.
pub fn promote_decision_fork<S: ForkEngine>(
    main: &mut SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    fork: &mut SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    definitions: &AdmittedDefinitions,
    authorizer: &dyn ActionAuthorizer,
    commands: &[ForkActionCall],
) -> Result<ForkPromotionReport, CellError> {
    let (pin, main_head_before, fork_head, main_defs_version) =
        validate_promotion_stores(main, fork, definitions)?;
    let authenticated = authenticate_fork_commands(fork, &pin, fork_head.commits, commands)?;
    let start_time_ms = main
        .now()
        .as_millis()
        .max(main.store().read().clock_floor()?);
    let moved_reads = stage_replay(
        main,
        &pin,
        &main_head_before,
        definitions,
        authorizer,
        &authenticated,
        start_time_ms,
    )?;
    // Atomically reserve the source fork at its authenticated head before
    // mutating `main` so no sibling can append commands that would be stranded.
    fork.store_mut().reserve_fork_promotion(&fork_head)?;
    let (promoted, main_head_after) = match replay_into_main(
        main,
        &main_head_before,
        main_defs_version,
        definitions,
        authorizer,
        &authenticated,
        &moved_reads,
        start_time_ms,
    ) {
        Ok(result) => result,
        Err(error) => {
            if !is_partial_main_replay(&error) {
                let _ = fork.store_mut().release_fork_promotion();
            }
            return Err(error);
        }
    };
    if let Err(error) = fork.store_mut().mark_fork_promoted() {
        return Err(merge_refused(ForkMergeRefusalReason::ForkCloseFailed {
            committed: promoted,
            detail: error.to_string(),
        }));
    }
    Ok(ForkPromotionReport {
        source_world: pin.world,
        fork_base: pin.base,
        main_head_before,
        main_head_after,
        promoted,
    })
}

fn is_partial_main_replay(error: &CellError) -> bool {
    matches!(
        error,
        CellError::ForkMergeRefused { reason }
            if matches!(reason.as_ref(), ForkMergeRefusalReason::PartialMainReplay { .. })
    )
}

fn verify_active_definitions<S: SqlEngine>(
    store: &CellStore<S>,
    definitions: &AdmittedDefinitions,
) -> Result<Option<u64>, CellError> {
    if definitions.model().model_hash() != store.model().model_hash()
        || definitions.model().ddl_hash() != store.model().ddl_hash()
    {
        return Err(merge_refused(ForkMergeRefusalReason::ReplayRejected {
            action_id: String::new(),
            idempotency_key: String::new(),
            code: "core.world.definition_mismatch".to_owned(),
            detail: "supplied admitted definitions do not match the cell's active model".to_owned(),
        }));
    }
    if let Some(installed) = CellStore::read_active_definitions(&store.conn)? {
        let envelopes: Vec<ArtifactEnvelope> = serde_json::from_str(&installed.canonical_set)?;
        let active = AdmittedDefinitions::from_envelopes(&envelopes)?;
        if definitions.model().model_hash() != active.model().model_hash()
            || definitions.model().ddl_hash() != active.model().ddl_hash()
            || definitions.actions() != active.actions()
            || definitions.guards() != active.guards()
        {
            return Err(merge_refused(ForkMergeRefusalReason::ReplayRejected {
                action_id: String::new(),
                idempotency_key: String::new(),
                code: "core.world.definition_mismatch".to_owned(),
                detail: format!(
                    "supplied admitted definitions disagree with active installed definitions version {}",
                    installed.version
                ),
            }));
        }
        return Ok(Some(installed.version));
    }
    Ok(None)
}

fn validate_promotion_stores<S: SqlEngine>(
    main: &SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    fork: &SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    definitions: &AdmittedDefinitions,
) -> Result<(ForkPin, LedgerHead, LedgerHead, Option<u64>), CellError> {
    if main.store().fork_pin().is_some() {
        return Err(merge_refused(ForkMergeRefusalReason::NotDecisionFork {
            detail: "promotion target must be main, not a fork".to_owned(),
        }));
    }
    let Some(pin) = fork.store().fork_pin().cloned() else {
        return Err(merge_refused(ForkMergeRefusalReason::NotDecisionFork {
            detail: "promotion source must be a decision fork, not main".to_owned(),
        }));
    };
    fork.store().refuse_fork_write()?;
    if pin.parent != MAIN_WORLD {
        return Err(merge_refused(ForkMergeRefusalReason::NestedForkForbidden {
            world: pin.world,
            parent: pin.parent,
        }));
    }
    if main.scope != fork.scope {
        return Err(merge_refused(
            ForkMergeRefusalReason::CellIdentityMismatch {
                detail: format!(
                    "scope mismatch: main is `{}` but fork is `{}`",
                    main.scope, fork.scope
                ),
            },
        ));
    }
    let main_id = main.store().cell_id()?;
    let fork_id = fork.store().cell_id()?;
    if main_id.is_none() || main_id != fork_id {
        return Err(merge_refused(
            ForkMergeRefusalReason::CellIdentityMismatch {
                detail: "decision fork cell identity does not match main".to_owned(),
            },
        ));
    }
    let main_defs_version = verify_active_definitions(main.store(), definitions)?;
    let main_head = main.store().verified_head()?;
    verify_fork_base_on_main(main, &pin, &main_head)?;
    let fork_head = fork.store().verified_head()?;
    if fork_head.commits <= pin.base.commits {
        return Err(merge_refused(ForkMergeRefusalReason::NoCommandsToPromote {
            world: pin.world,
        }));
    }
    Ok((pin, main_head, fork_head, main_defs_version))
}

fn verify_fork_base_on_main<S: SqlEngine>(
    main: &SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    pin: &ForkPin,
    main_head: &LedgerHead,
) -> Result<(), CellError> {
    if main_head.commits < pin.base.commits {
        return Err(merge_refused(
            ForkMergeRefusalReason::CellIdentityMismatch {
                detail: format!(
                    "main head commit {} precedes fork base commit {}",
                    main_head.commits, pin.base.commits
                ),
            },
        ));
    }
    if pin.base.commits == 0 {
        if pin.base.hash != crate::system::GENESIS_HASH {
            return Err(merge_refused(
                ForkMergeRefusalReason::CellIdentityMismatch {
                    detail: "fork base hash at genesis disagrees with main".to_owned(),
                },
            ));
        }
        return Ok(());
    }
    let base_receipts = main
        .store()
        .read()
        .receipts_after(pin.base.commits.saturating_sub(1), 1)?;
    let matched = base_receipts.first().is_some_and(|receipt| {
        receipt.body.commit_seq == pin.base.commits && receipt.body.ledger_head == pin.base.hash
    });
    if !matched {
        return Err(merge_refused(
            ForkMergeRefusalReason::CellIdentityMismatch {
                detail: format!(
                    "fork base commit {} hash does not match main's ledger history",
                    pin.base.commits
                ),
            },
        ));
    }
    Ok(())
}
