//! The action commit path (T2-KER-2c, #2895).
//!
//! An authorized call of an action whose effect program is `fire_transition`
//! or `upsert_input_object` commits through the decision-commit path
//! ([`crate::decision_commit`]) in **one** [`CellHost::commit`]: the fact
//! writes and ledger events plus the call's idempotency record.
//! [`commit_action`] is the entry point a shell (`norn dev`'s
//! `POST /v1/actions/{action}`, the cell shim) calls.
//!
//! Order of checks, each refusing with nothing committed:
//!
//! 1. **Command shape and scope, fail-closed**:
//!    [`crate::action::check_action_command`] (organization scope, identity
//!    fields) and every `scopeRequired` key present and non-blank.
//! 2. **Effect**: only `fire_transition` and `upsert_input_object` are
//!    executable here.
//! 3. **Input preparation**: ordered ZEN derivations run against caller input
//!    and prior derived values, then the effective object is checked against
//!    the closed input schema. The raw caller input remains the idempotency
//!    binding.
//! 4. **Current-state authorization**, by the injected [`ActionAuthorizer`],
//!    with the target's committed values when it exists. A missing transition
//!    target is still checked with `target_values: None` and reports
//!    [`ActionRefusal::TargetNotFound`]. A create upsert defers resource
//!    authorization until its proposed values have been prepared.
//! 5. **Preconditions**: every action precondition (a boolean ZEN Expression
//!    over effective input and the authorized, declared slice) holds. The
//!    slice's fact versions and complete bounded link sets are compared again
//!    in the commit; false, non-boolean, failing, or stale reads refuse.
//! 6. **Object kind**: the target's type is the effect's `objectKind` or a
//!    subtype of it.
//! 7. **Idempotency**: a committed call with the same action and idempotency
//!    key replays (no second transition) when its binding (object, input,
//!    actor) is equal, and refuses otherwise. Existing targets are authorized
//!    against current state before replay. An absent upsert replay is checked
//!    as an absent current target and remains not-found, rather than becoming
//!    a fresh create.
//! 8. **Effect preparation and proposed-state authorization**: a transition
//!    uses one `transition` fact change; an upsert uses one `create` or the
//!    input's `set` changes. Both go through [`crate::prepare_decision`]. The
//!    target's prepared `put` is canonicalized with the shared effective-value
//!    builder, then authorized as the proposed state.
//! 9. **Commit**: the prepared writes and call record commit atomically. A
//!    concurrent call that commits first makes this one stale; if that call
//!    had the same idempotency key and binding, this one replays after current
//!    state authorization.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EvaluationBudget, Expression, Value as ExprValue};
use norn_ir::{
    ActionCommand, ActionCommandOperation, ActionEffectProgram, ActionRoot, Decimal,
    DecisionOutput, FactChange, KeyName, NamingSeriesIR, ObjectTypeName, OwnedChildCollection,
    OwnedCollectionAction, PropertyId, ResourceLimitProfile, RuntimeValue, SliceDeclaration,
    SliceKey, UpsertWriteMode,
};
use serde::{Deserialize, Serialize};

use crate::action::{ActionCommandRejection, ActionCommandShape, check_action_command};
use crate::cell_host::{
    CellCommit, CellHost, CommitError, CommitReceipt, FactExpectation, FactKey, FactMutation,
    FactState, FactVersion, FactWrite, JsonObject, JsonValue, LinkKey, LinkMutation,
    LinkReadExpectation, LinkWrite, OwnedRowsCommitProof,
};
use crate::decision_commit::{
    DecisionRefusal, DecisionTurn, FactEvent, FactSlice, TypeCatalog, expr_value,
    fill_lifecycle_defaults, prepare_decision_with_limits, prepare_owned_rows, widen_effect,
};
use crate::envelope::{EnvelopeCounts, TurnEnvelopeMeter, check_decision_envelope};
use crate::limits::{EnvelopeLimits, SliceMeter};
use crate::naming_series::{SeriesCandidate, prepare_candidate, scope_from_values, type_series};

/// The object type of the kernel's idempotency records of action calls.
///
/// A schema-backed host must register this internal type in its model before
/// calling [`commit_action`]. The native dev host integration owns that model
/// registration (#2911); the kernel never creates host storage tables.
pub const ACTION_CALL_TYPE: &str = "norn.ActionCall";
const MAX_NAMING_SERIES_RETRIES: usize = 128;

/// Protected per-parent/collection manifest of stable child row ids and order.
pub const OWNED_ROWS_TYPE: &str = "norn.OwnedRows";

/// Internal `ActionCall` effect marker for an admitted owned-row transaction.
pub const ACTION_EFFECT_MUTATE_PARENT_WITH_ROWS: &str = "mutate_parent_with_rows";

/// Deterministic internal manifest key for one parent-owned collection.
///
/// The Norn1 canonical digest keeps the storage key bounded independently of
/// the parent key size. The manifest body retains the full identity and is
/// checked on load, so a digest collision fails closed.
///
/// # Errors
/// Canonical encoding of the tuple of identity strings failed.
pub fn owned_rows_manifest_key(
    parent: &FactKey,
    collection_id: &str,
) -> Result<FactKey, norn_ir::canonical::CanonicalJsonError> {
    let digest = norn_ir::canonical::domain_hash_hex(
        b"norn.owned-rows-manifest-key.v1\0",
        &(
            parent.object_type.as_str(),
            parent.key.as_str(),
            collection_id,
        ),
    )?;
    Ok(FactKey::new(OWNED_ROWS_TYPE, digest))
}

/// One call of an action, as the shell received it.
#[derive(Debug, Clone)]
pub struct ActionCall<'a> {
    /// The admitted action definition.
    pub action: &'a ActionRoot,
    /// The artifact id of the admitted action definition.
    pub action_artifact_id: &'a str,
    /// The admitted action artifact's version.
    pub action_version: &'a str,
    /// Explicit scope (`orgId`, `environmentId`, ...).
    pub scope: BTreeMap<String, String>,
    /// The calling actor's identity.
    pub actor: String,
    /// The caller's raw input. The kernel derives and validates its effective
    /// input while retaining this value for idempotency and delegation
    /// binding.
    pub input: JsonObject,
    /// The caller's idempotency key.
    pub idempotency_key: String,
    /// The target's own type when it is a subtype of the effect's
    /// `objectKind`; `None` means the `objectKind` itself.
    pub object_type: Option<ObjectTypeName>,
    /// The turn the call commits in (identity, logical time, world, and the
    /// consumed delivery, if any).
    pub turn: DecisionTurn,
}

/// What the authorizer is asked about one call.
#[derive(Debug, Clone, Copy)]
pub struct ActionAuthzRequest<'a> {
    /// The action.
    pub action: &'a ActionRoot,
    /// The call's scope.
    pub scope: &'a BTreeMap<String, String>,
    /// The actor.
    pub actor: &'a str,
    /// The call's input.
    pub input: &'a JsonObject,
    /// The target object.
    pub target: &'a FactKey,
    /// Trusted target values for this check. The kernel supplies committed
    /// values for current-state authorization and canonical proposed values
    /// for post-write authorization. `None` means the target is absent during
    /// a current-state check; a create proposal is always supplied as `Some`.
    pub target_values: Option<&'a JsonObject>,
}

/// Trusted metadata for permission to inspect naming-series failures.
///
/// This request has no generated target: allocation may have failed before a
/// valid key exists. Granting diagnostic access does not authorize a create.
#[derive(Debug, Clone, Copy)]
pub struct NamingSeriesInspectionRequest<'a> {
    /// The admitted action.
    pub action: &'a ActionRoot,
    /// The call's explicit scope.
    pub scope: &'a BTreeMap<String, String>,
    /// The calling actor.
    pub actor: &'a str,
    /// The concrete object type being created.
    pub object_type: &'a ObjectTypeName,
    /// The ancestor that owns the series and its counter.
    pub series_owner: &'a ObjectTypeName,
    /// The declared generated key.
    pub key_name: &'a KeyName,
}

/// One object read on behalf of an action's declared precondition slice.
#[derive(Debug, Clone, Copy)]
pub struct ActionReadAuthzRequest<'a> {
    /// The action whose precondition reads this object.
    pub action: &'a ActionRoot,
    /// The call's explicit scope.
    pub scope: &'a BTreeMap<String, String>,
    /// The calling actor.
    pub actor: &'a str,
    /// The final, derived action input.
    pub input: &'a JsonObject,
    /// The type named by the slice declaration. This can be a base type while
    /// `target` carries the concrete subtype returned by the host.
    pub declared_type: &'a ObjectTypeName,
    /// The object whose values are used by a precondition.
    pub target: &'a FactKey,
    /// The trusted committed values, or `None` when the object is absent.
    pub target_values: Option<&'a JsonObject>,
}

/// The collection-level read gate checked before loading a declared slice
/// object or enumerating a link target set.
#[derive(Debug, Clone, Copy)]
pub struct ActionReadTypeAuthzRequest<'a> {
    /// The action whose precondition reads this type.
    pub action: &'a ActionRoot,
    /// The call's explicit scope.
    pub scope: &'a BTreeMap<String, String>,
    /// The calling actor.
    pub actor: &'a str,
    /// The final, derived action input.
    pub input: &'a JsonObject,
    /// The declared type being read.
    pub object_type: &'a ObjectTypeName,
}

/// Why the authorizer denies a call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionDenial {
    /// The actor may not see the target: reported as not-found.
    Invisible,
    /// The actor may see the target but not perform the action.
    Forbidden {
        /// The authorizer's stable reason code (for example the failing
        /// conjunct of the PDP's AND rule).
        reason: String,
    },
}

/// The authorization stage of an action call. The kernel never evaluates
/// policy itself; the shell adapts its policy decision point (the
/// `norn-authz` Cedar PDP, #2578) to this trait.
pub trait ActionAuthorizer {
    /// Proves that action and slice authorization inspect no stored values,
    /// relationships, historical actors, or live entitlements. Unknown
    /// authorizers retain whole-object information lineage.
    fn supports_reference_lineage(&self) -> bool {
        false
    }

    /// The same proof scoped to this action and its slice-read authorization.
    /// Policies for other actions need not classify an independent reference.
    /// Unknown authorizers retain the conservative global proof.
    fn supports_action_reference_lineage(&self, _action: &ActionRoot) -> bool {
        self.supports_reference_lineage()
    }

    /// Decides one call against the trusted current or proposed target values
    /// in `request`. Existing targets are checked in both states before
    /// commit; replay checks use current state only.
    ///
    /// # Errors
    ///
    /// The [`ActionDenial`].
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial>;

    /// Grants disclosure of refusals from a generated-key create, including
    /// private counter state. Defaults to concealment. Adapters must separately
    /// authorize this diagnostic permission for the actor, scope, and series;
    /// the actual generated object still requires normal authorization.
    fn can_inspect_naming_series(&self, _request: &NamingSeriesInspectionRequest<'_>) -> bool {
        false
    }

    /// Authorizes reading one object in an action's precondition slice.
    /// Implementations default to deny; a shell must opt into its ordinary
    /// object-read policy explicitly.
    ///
    /// # Errors
    ///
    /// Returns the read policy denial; the default is invisible.
    fn authorize_read(&self, _request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Err(ActionDenial::Invisible)
    }

    /// Authorizes access to the collection before the kernel loads one of its
    /// objects. The default fails closed.
    ///
    /// # Errors
    ///
    /// Returns the collection policy denial; the default is invisible.
    fn authorize_read_type(
        &self,
        _request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        Err(ActionDenial::Invisible)
    }
}

/// One field produced by an ordered action input derivation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionDerivedValue {
    /// The derived field.
    pub field: String,
    /// The exact runtime value written to the effective input.
    pub value: RuntimeValue,
}

/// The successful outcome of one action precondition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionPreconditionOutcome {
    /// The expression evaluated.
    pub expression: String,
    /// Whether it held (successful records contain only `true`).
    pub satisfied: bool,
}

/// The result of applying ordered derivations and validating the final input.
#[derive(Debug, PartialEq)]
pub struct ActionInputPreparation<'a> {
    action: &'a ActionRoot,
    raw_input: &'a JsonObject,
    action_artifact_id: Option<&'a str>,
    action_version: Option<&'a str>,
    effective_input: JsonObject,
    derived_values: Vec<ActionDerivedValue>,
    remaining_fuel: u64,
    receipt_evidence_bytes: usize,
    precondition_outcomes: Vec<ActionPreconditionOutcome>,
}

impl ActionInputPreparation<'_> {
    fn matches_call(&self, call: &ActionCall<'_>) -> bool {
        self.action == call.action
            && self.raw_input == &call.input
            && self.action_artifact_id == Some(call.action_artifact_id)
            && self.action_version == Some(call.action_version)
    }

    /// The caller input after all derivations, before the action reads facts.
    #[must_use]
    pub const fn effective_input(&self) -> &JsonObject {
        &self.effective_input
    }

    /// The ordered values each derivation produced.
    #[must_use]
    pub fn derived_values(&self) -> &[ActionDerivedValue] {
        &self.derived_values
    }

    /// The ordered outcomes of all successful preconditions.
    #[must_use]
    pub fn precondition_outcomes(&self) -> &[ActionPreconditionOutcome] {
        &self.precondition_outcomes
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActionDerivedValueRef<'a> {
    field: &'a str,
    value: &'a RuntimeValue,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActionPreconditionOutcomeRef<'a> {
    expression: &'a str,
    satisfied: bool,
}

/// The idempotency record of a committed call (the values of its
/// [`ACTION_CALL_TYPE`] object).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionCallRecord {
    /// The action.
    pub action_id: String,
    /// The action artifact that was admitted when the call committed. Missing
    /// only on records written before action attribution was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_artifact_id: Option<String>,
    /// The action artifact version that was admitted when the call committed.
    /// Missing only on records written before action attribution was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_version: Option<String>,
    /// The effect program that committed this call. Missing in legacy records
    /// written before effect attribution and interpreted as `fire_transition`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_kind: Option<String>,
    /// The target object.
    pub object: FactKey,
    /// The call's input.
    pub input: JsonObject,
    /// The final input after ordered derivations. Legacy records use `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_input: Option<JsonObject>,
    /// The values produced by each derivation, in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_values: Vec<ActionDerivedValue>,
    /// Successful precondition results, in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub precondition_outcomes: Vec<ActionPreconditionOutcome>,
    /// The actor.
    pub actor: String,
    /// The turn the call committed in.
    pub turn: String,
    /// The fired transition.
    pub transition: String,
    /// The state it left.
    pub from: String,
    /// The state it entered.
    pub to: String,
    /// Normalized row operations committed by a parent-and-rows action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub row_operations: Vec<JsonValue>,
}

impl ActionCallRecord {
    fn for_transition(
        call: &ActionCall<'_>,
        preparation: &ActionInputPreparation<'_>,
        target: &FactKey,
        transition: &norn_ir::TransitionId,
    ) -> Self {
        let action = call.action;
        Self {
            action_id: action.action_id.clone(),
            action_artifact_id: Some(call.action_artifact_id.to_owned()),
            action_version: Some(call.action_version.to_owned()),
            effect_kind: Some("fire_transition".to_owned()),
            object: target.clone(),
            input: call.input.clone(),
            effective_input: (!action.derive.is_empty())
                .then(|| preparation.effective_input.clone()),
            derived_values: preparation.derived_values.clone(),
            precondition_outcomes: Vec::new(),
            actor: call.actor.clone(),
            turn: call.turn.id.clone(),
            transition: transition.as_str().to_owned(),
            from: String::new(),
            to: String::new(),
            row_operations: Vec::new(),
        }
    }

    fn binds_same_call(&self, other: &Self) -> bool {
        self.action_id == other.action_id
            && self.object == other.object
            && self.input == other.input
            && self.actor == other.actor
    }

    fn binds_same_generated_call(
        &self,
        action_id: &str,
        object_type: &ObjectTypeName,
        input: &JsonObject,
        actor: &str,
    ) -> bool {
        self.action_id == action_id
            && self.object.object_type == *object_type
            && self.input == *input
            && self.actor == actor
    }

    fn to_values(&self) -> JsonObject {
        let string = |value: &str| JsonValue::String(value.to_owned());
        let mut values = JsonObject::from([
            ("actionId".to_owned(), string(&self.action_id)),
            (
                "object".to_owned(),
                JsonValue::Object(JsonObject::from([
                    (
                        "objectType".to_owned(),
                        string(self.object.object_type.as_str()),
                    ),
                    ("key".to_owned(), string(&self.object.key)),
                ])),
            ),
            ("input".to_owned(), JsonValue::Object(self.input.clone())),
            ("actor".to_owned(), string(&self.actor)),
            ("turn".to_owned(), string(&self.turn)),
            ("transition".to_owned(), string(&self.transition)),
            ("from".to_owned(), string(&self.from)),
            ("to".to_owned(), string(&self.to)),
            (
                "rowOperations".to_owned(),
                JsonValue::Array(self.row_operations.clone()),
            ),
        ]);
        if let Some(effective_input) = &self.effective_input {
            values.insert(
                "effectiveInput".to_owned(),
                JsonValue::Object(effective_input.clone()),
            );
        }
        values.insert(
            "derivedValues".to_owned(),
            JsonValue::Array(
                self.derived_values
                    .iter()
                    .map(|derived| {
                        JsonValue::Object(JsonObject::from([
                            ("field".to_owned(), JsonValue::String(derived.field.clone())),
                            ("value".to_owned(), derived.value.clone()),
                        ]))
                    })
                    .collect(),
            ),
        );
        values.insert(
            "preconditionOutcomes".to_owned(),
            JsonValue::Array(
                self.precondition_outcomes
                    .iter()
                    .map(|outcome| {
                        JsonValue::Object(JsonObject::from([
                            (
                                "expression".to_owned(),
                                JsonValue::String(outcome.expression.clone()),
                            ),
                            ("satisfied".to_owned(), JsonValue::Bool(outcome.satisfied)),
                        ]))
                    })
                    .collect(),
            ),
        );
        if let Some(action_artifact_id) = &self.action_artifact_id {
            values.insert("actionArtifactId".to_owned(), string(action_artifact_id));
        }
        if let Some(action_version) = &self.action_version {
            values.insert("actionVersion".to_owned(), string(action_version));
        }
        if let Some(effect_kind) = &self.effect_kind {
            values.insert("effectKind".to_owned(), string(effect_kind));
        }
        values
    }

    /// Decodes a stored call record, including absent or null evidence columns
    /// on records committed before those columns were added. Malformed
    /// identities or non-null evidence values return `None`.
    #[must_use]
    pub fn from_values(values: &JsonObject) -> Option<Self> {
        let string = |name: &str| match values.get(name) {
            Some(JsonValue::String(value)) => Some(value.clone()),
            _ => None,
        };
        let Some(JsonValue::Object(object)) = values.get("object") else {
            return None;
        };
        let Some(JsonValue::Object(input)) = values.get("input") else {
            return None;
        };
        let (Some(JsonValue::String(object_type)), Some(JsonValue::String(key))) =
            (object.get("objectType"), object.get("key"))
        else {
            return None;
        };
        let metadata = (values.get("actionArtifactId"), values.get("actionVersion"));
        let (action_artifact_id, action_version) = match metadata {
            (Some(JsonValue::String(id)), Some(JsonValue::String(version))) => {
                (Some(id.clone()), Some(version.clone()))
            }
            (None | Some(JsonValue::Null), None | Some(JsonValue::Null)) => (None, None),
            _ => return None,
        };
        let effect_kind = match values.get("effectKind") {
            Some(JsonValue::String(kind)) => Some(kind.clone()),
            None | Some(JsonValue::Null) => Some("fire_transition".to_owned()),
            _ => return None,
        };
        let row_operations = match values.get("rowOperations") {
            Some(JsonValue::Array(operations)) => operations.clone(),
            None | Some(JsonValue::Null) => Vec::new(),
            _ => return None,
        };
        let effective_input = match values.get("effectiveInput") {
            Some(JsonValue::Object(input)) => Some(input.clone()),
            None | Some(JsonValue::Null) => None,
            _ => return None,
        };
        let derived_values = match values.get("derivedValues") {
            None | Some(JsonValue::Null) => Vec::new(),
            Some(value) => norn_ir::runtime::decode(value).ok()?,
        };
        let precondition_outcomes = match values.get("preconditionOutcomes") {
            None | Some(JsonValue::Null) => Vec::new(),
            Some(value) => norn_ir::runtime::decode(value).ok()?,
        };
        Some(Self {
            action_id: string("actionId")?,
            action_artifact_id,
            action_version,
            effect_kind,
            object: FactKey::new(object_type.clone(), key.clone()),
            input: input.clone(),
            effective_input,
            derived_values,
            precondition_outcomes,
            actor: string("actor")?,
            turn: string("turn")?,
            transition: string("transition")?,
            from: string("from")?,
            to: string("to")?,
            row_operations,
        })
    }
}

/// What an accepted call did.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionOutcome {
    /// The transition committed now.
    Committed {
        /// The commit's receipt (ledger position and written versions).
        receipt: CommitReceipt,
        /// The call's record, as written.
        record: ActionCallRecord,
    },
    /// An exact retry of a committed call: nothing new was committed.
    Replayed {
        /// The original call's record.
        record: ActionCallRecord,
    },
}

/// Why an action call was refused. Nothing was committed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ActionRefusal<X> {
    /// No organization scope, or a blank identity field.
    #[error("action call refused: {0}")]
    Command(#[source] ActionCommandRejection),
    /// A scope key the action requires is missing or blank.
    #[error("the action requires scope {key:?}")]
    MissingScope {
        /// The missing key.
        key: String,
    },
    /// The action's effect program is not executable on this path.
    #[error("action {action_id:?} has no executable action effect program")]
    UnsupportedEffect {
        /// The action.
        action_id: String,
    },
    /// The input does not name the target.
    #[error("the input does not satisfy the declared schema at {field:?}")]
    InvalidInput {
        /// The `objectIdField`.
        field: String,
    },
    /// An ordered derivation failed or produced a value that could not be
    /// represented by the admitted expression profile.
    #[error("input derivation for {field:?} failed ({reason})")]
    DerivationFailed {
        /// The field being derived.
        field: String,
        /// The derivation source.
        expression: String,
        /// Why the expression failed.
        reason: String,
    },
    /// A caller input or evidence payload exceeds the shared action bound.
    #[error("action {subject} is {actual_bytes} bytes; the limit is {max_bytes} bytes")]
    InputBoundExceeded {
        /// The bounded payload name.
        subject: String,
        /// Its canonical size.
        actual_bytes: usize,
        /// The maximum admitted size.
        max_bytes: usize,
    },
    /// Row expressions exhausted the shared action expression fuel.
    #[error("action expression evaluation exceeded the {limit}-unit budget")]
    ExpressionBudgetExceeded {
        /// The action's shared fuel ceiling.
        limit: u64,
    },
    /// The declared read slice exceeded the slice profile or a host could
    /// not provide a bounded, atomicity-safe adjacency snapshot.
    #[error("action read slice was refused at {entry:?}: {reason}")]
    SliceUnavailable {
        /// The slice entry being loaded.
        entry: String,
        /// The safe refusal detail.
        reason: String,
    },
    /// A visible, required single-object slice entry does not exist.
    #[error("required action slice entry {entry:?} is missing")]
    SliceObjectMissing {
        /// The declared entry.
        entry: String,
    },
    /// A precondition's fact or adjacency read changed before commit.
    #[error("action precondition read changed before commit at {key}")]
    StalePreconditionRead {
        /// The stale object or adjacency source.
        key: FactKey,
        /// The link name when the stale read was a complete adjacency set.
        link: Option<String>,
    },
    /// A precondition is false or cannot be evaluated (fail-closed).
    #[error("precondition `{precondition}` does not hold ({})", .reason.as_deref().unwrap_or("false"))]
    PreconditionFailed {
        /// The precondition source.
        precondition: String,
        /// Why evaluation failed, when it failed rather than being false.
        reason: Option<String>,
    },
    /// The target does not exist, or the actor may not see it.
    #[error("the target object was not found")]
    TargetNotFound,
    /// The actor may see the target but not perform the action.
    #[error("the action is not permitted ({reason})")]
    PermissionDenied {
        /// The authorizer's reason code.
        reason: String,
    },
    /// The target is not of the effect's object kind or a subtype.
    #[error("the action fires on {expected}, but the target is a {actual}")]
    KindMismatch {
        /// The effect's object kind.
        expected: String,
        /// The target's type.
        actual: String,
    },
    /// The idempotency key was already used for a different call.
    #[error("idempotency key {key:?} was already used for a different call of this action")]
    IdempotencyConflict {
        /// The key.
        key: String,
    },
    /// The target does not satisfy the action's declared create/draft mode.
    #[error("the action's {mode} write mode does not hold ({reason})")]
    WriteModeFailed {
        /// The declared write mode.
        mode: String,
        /// Why the target failed the mode.
        reason: String,
    },
    /// The transition was refused by the decision-commit path.
    #[error(transparent)]
    Decision(DecisionRefusal<X>),
}

impl<X> ActionRefusal<X> {
    /// Stable machine-readable code of the refusal.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Command(rejection) => rejection.code(),
            Self::MissingScope { .. } => "action.missing_scope",
            Self::UnsupportedEffect { .. } => "action.unsupported_effect",
            Self::InvalidInput { .. } => "action.invalid_input",
            Self::DerivationFailed { .. } => "action.derivation_failed",
            Self::InputBoundExceeded { .. } => "action.input_bound_exceeded",
            Self::ExpressionBudgetExceeded { .. } => "action.expression_budget_exceeded",
            Self::SliceUnavailable { .. } => "action.slice_unavailable",
            Self::SliceObjectMissing { .. } => "action.slice_object_missing",
            Self::StalePreconditionRead { .. } => "action.precondition_stale_read",
            Self::PreconditionFailed { .. } => "action.precondition_failed",
            Self::TargetNotFound => "action.target_not_found",
            Self::PermissionDenied { .. } => "action.permission_denied",
            Self::KindMismatch { .. } => "artifact_set.action.effect_kind_mismatch",
            Self::IdempotencyConflict { .. } => "action.idempotency_conflict",
            Self::WriteModeFailed { .. } => "action.write_mode_failed",
            Self::Decision(refusal) => refusal.code(),
        }
    }
}

impl<X> From<DecisionRefusal<X>> for ActionRefusal<X> {
    fn from(refusal: DecisionRefusal<X>) -> Self {
        Self::Decision(refusal)
    }
}

/// Commits one authorized `fire_transition` action call (see the module
/// docs for the order of checks).
///
/// # Errors
///
/// An [`ActionRefusal`]; nothing was committed.
pub fn commit_action<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    commit_action_with_limits(host, catalog, call, authorizer, &EnvelopeLimits::V1)
}

/// Commits one action call using an input preparation already computed for
/// this exact action definition, artifact identity/version, and raw input.
///
/// The opaque, non-cloneable token must come from
/// [`prepare_action_input_for_call`] and is consumed here. This lets an API
/// perform authorization with the effective input without evaluating
/// declared derivations a second time.
///
/// # Errors
///
/// An [`ActionRefusal`]; nothing was committed.
pub fn commit_action_with_preparation<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    preparation: ActionInputPreparation<'_>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    commit_action_inner(
        host,
        catalog,
        call,
        authorizer,
        Some(preparation),
        &EnvelopeLimits::V1,
        None,
    )
}

/// Commits one action call under explicit per-decision envelope limits.
///
/// # Errors
///
/// An [`ActionRefusal`]; nothing was committed.
pub fn commit_action_with_limits<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    limits: &EnvelopeLimits,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    commit_action_inner(host, catalog, call, authorizer, None, limits, None)
}

/// Commits one action call under the meter's per-decision and aggregate turn
/// limits.
///
/// A replay and every refusal leave the meter unchanged. A new transition is
/// charged only after its host commit succeeds.
///
/// # Errors
///
/// An [`ActionRefusal`]; nothing was committed and the meter is unchanged.
pub fn commit_action_with_meter<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    meter: &mut TurnEnvelopeMeter,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let limits = meter.decision_limits();
    commit_action_inner(host, catalog, call, authorizer, None, &limits, Some(meter))
}

fn commit_action_inner<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    preparation: Option<ActionInputPreparation<'_>>,
    limits: &EnvelopeLimits,
    meter: Option<&mut TurnEnvelopeMeter>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let action = call.action;
    if (action.quantity_effect.is_some() || !action.create_links.is_empty())
        && matches!(
            action.effect_program,
            Some(ActionEffectProgram::MutateParentWithRows { .. })
        )
    {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: action.action_id.clone(),
        });
    }
    check_scope(call)?;
    let prepared_input = match preparation {
        Some(preparation) if preparation.matches_call(call) => preparation,
        Some(_) => {
            return Err(ActionRefusal::InvalidInput {
                field: "$prepared_input".to_owned(),
            });
        }
        None => prepare_action_input_for_call::<H::Error>(
            action,
            &call.input,
            call.action_artifact_id,
            call.action_version,
        )?,
    };
    match action.effect_program.as_ref() {
        Some(ActionEffectProgram::FireTransition {
            object_id_field,
            object_kind,
            lifecycle,
            transition,
            ..
        }) => commit_transition(
            host,
            catalog,
            call,
            prepared_input,
            authorizer,
            object_id_field,
            object_kind,
            lifecycle,
            transition,
            limits,
            meter,
        ),
        Some(ActionEffectProgram::UpsertInputObject {
            object_id_field,
            object_kind,
            write_mode,
            ..
        }) => commit_upsert(
            host,
            catalog,
            call,
            prepared_input,
            authorizer,
            object_id_field,
            object_kind,
            write_mode,
            limits,
            meter,
        ),
        Some(ActionEffectProgram::MutateParentWithRows {
            object_id_field,
            object_kind,
            parent_values_field,
            row_ops_field,
            parent_version_field,
            parent_delete_field,
            collections,
            ..
        }) => commit_parent_rows(
            host,
            catalog,
            call,
            prepared_input,
            authorizer,
            object_id_field,
            object_kind,
            parent_values_field,
            row_ops_field,
            parent_version_field,
            parent_delete_field.as_deref(),
            collections,
            limits,
            meter,
        ),
        None => Err(ActionRefusal::UnsupportedEffect {
            action_id: action.action_id.clone(),
        }),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the admitted action effect, host, and turn policy are explicit commit inputs"
)]
fn commit_transition<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    mut preparation: ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    object_id_field: &str,
    object_kind: &str,
    lifecycle: &norn_ir::PropertyId,
    transition: &norn_ir::TransitionId,
    limits: &EnvelopeLimits,
    meter: Option<&mut TurnEnvelopeMeter>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let action = call.action;
    let Some(JsonValue::String(object_id)) = preparation.effective_input.get(object_id_field)
    else {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    };
    if object_id.trim().is_empty() {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    }
    let object_type = call
        .object_type
        .clone()
        .unwrap_or_else(|| ObjectTypeName(object_kind.to_owned()));

    let target = FactKey {
        object_type: object_type.clone(),
        key: object_id.clone(),
    };
    let state = authorize_target(
        host,
        call,
        &preparation.effective_input,
        authorizer,
        &target,
    )?;
    check_kind(catalog, &object_type, object_kind)?;
    let record_key = FactKey::new(
        ACTION_CALL_TYPE,
        format!("{}/{}", action.action_id, call.idempotency_key),
    );
    let record = ActionCallRecord::for_transition(call, &preparation, &target, transition);
    let recorded = host.load(&record_key).map_err(host_failure)?;
    if let Some(outcome) = replay(&recorded, &record, &call.idempotency_key)? {
        return Ok(outcome);
    }
    let slice = load_action_slice(
        &ActionSliceContext {
            host,
            catalog,
            call,
            effective_input: &preparation.effective_input,
            authorizer,
        },
        &target,
        &state,
        SliceMeter::for_profile(&ResourceLimitProfile::norn_limits_v1()),
    )?;
    preparation.precondition_outcomes = check_action_preconditions(
        action,
        &preparation.effective_input,
        &slice.values,
        &mut preparation.remaining_fuel,
        &mut preparation.receipt_evidence_bytes,
        usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
    )?;
    let mut record = record;
    record
        .precondition_outcomes
        .clone_from(&preparation.precondition_outcomes);
    check_action_record_size::<H::Error>(call.action, &record, limits)?;
    let change = FactChange::Transition {
        object_type,
        key: object_id.clone(),
        lifecycle: lifecycle.clone(),
        transition: transition.clone(),
    };
    commit_effect(
        host,
        catalog,
        call,
        authorizer,
        vec![change],
        Pending {
            effective_input: preparation.effective_input.clone(),
            remaining_fuel: preparation.remaining_fuel,
            target,
            slice: slice.facts,
            reference_only: slice.reference_only,
            link_reads: slice.link_reads,
            precondition_reads: slice.precondition_keys,
            record_key,
            recorded,
            record,
            series_candidate: None,
        },
        EnvelopePolicy { limits, meter },
    )
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the admitted action effect, host, and turn policy are explicit commit inputs"
)]
fn commit_upsert<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    mut preparation: ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    object_id_field: &str,
    object_kind: &str,
    write_mode: &UpsertWriteMode,
    limits: &EnvelopeLimits,
    meter: Option<&mut TurnEnvelopeMeter>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let action = call.action;
    let object_type = call
        .object_type
        .clone()
        .unwrap_or_else(|| ObjectTypeName(object_kind.to_owned()));
    check_kind(catalog, &object_type, object_kind)?;
    let naming_series = type_series(catalog, &object_type);
    if matches!(write_mode, UpsertWriteMode::CreateOnly)
        && let Some(series) = naming_series.as_ref()
    {
        if series.key_properties != [norn_ir::PropertyId(object_id_field.to_owned())]
            || preparation.effective_input.contains_key(object_id_field)
        {
            return Err(ActionRefusal::InvalidInput {
                field: object_id_field.to_owned(),
            });
        }
        let disclose = authorizer.can_inspect_naming_series(&NamingSeriesInspectionRequest {
            action,
            scope: &call.scope,
            actor: &call.actor,
            object_type: &object_type,
            series_owner: series.owner,
            key_name: series.key_name,
        });
        let outcome = commit_upsert_series(
            host,
            catalog,
            call,
            preparation,
            authorizer,
            object_id_field,
            &object_type,
            series.owner,
            series.key_name,
            series.key_properties,
            series.declaration,
            limits,
            meter,
        );
        // No honest target exists for some allocator failures. Conceal all
        // refusals alike unless diagnostic access was separately granted, so
        // denied calls cannot distinguish a healthy counter from a bad one.
        return if disclose {
            outcome
        } else {
            outcome.map_err(|_concealed_refusal| ActionRefusal::TargetNotFound)
        };
    }
    let Some(JsonValue::String(object_id)) = preparation.effective_input.get(object_id_field)
    else {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    };
    if object_id.trim().is_empty() {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    }

    let target = FactKey {
        object_type: object_type.clone(),
        key: object_id.clone(),
    };
    let state = authorize_upsert_target(
        host,
        call,
        &preparation.effective_input,
        authorizer,
        &target,
    )?;
    if naming_series.is_some() && matches!(write_mode, UpsertWriteMode::Upsert) && !state.exists() {
        return Err(ActionRefusal::WriteModeFailed {
            mode: "upsert".to_owned(),
            reason: "a naming-series object cannot be created with a caller-supplied key"
                .to_owned(),
        });
    }

    let record_key = FactKey::new(
        ACTION_CALL_TYPE,
        format!("{}/{}", action.action_id, call.idempotency_key),
    );
    let record = ActionCallRecord {
        action_id: action.action_id.clone(),
        action_artifact_id: Some(call.action_artifact_id.to_owned()),
        action_version: Some(call.action_version.to_owned()),
        effect_kind: Some("upsert_input_object".to_owned()),
        object: target.clone(),
        input: call.input.clone(),
        effective_input: (!action.derive.is_empty()).then(|| preparation.effective_input.clone()),
        derived_values: preparation.derived_values.clone(),
        precondition_outcomes: Vec::new(),
        actor: call.actor.clone(),
        turn: call.turn.id.clone(),
        transition: String::new(),
        from: String::new(),
        to: String::new(),
        row_operations: Vec::new(),
    };
    let recorded = host.load(&record_key).map_err(host_failure)?;
    if !state.exists() && recorded.exists() {
        // An absent target with an existing idempotency record is not a new
        // create. Recheck current state so a replay cannot bypass current
        // authorization, and preserve the not-found response for the absent
        // target regardless of the policy verdict.
        authorize_current_target(
            call,
            &preparation.effective_input,
            authorizer,
            &target,
            &state,
        )?;
    }
    if let Some(outcome) = replay(&recorded, &record, &call.idempotency_key)? {
        return Ok(outcome);
    }

    let slice = load_action_slice(
        &ActionSliceContext {
            host,
            catalog,
            call,
            effective_input: &preparation.effective_input,
            authorizer,
        },
        &target,
        &state,
        SliceMeter::for_profile(&ResourceLimitProfile::norn_limits_v1()),
    )?;
    preparation.precondition_outcomes = check_action_preconditions(
        action,
        &preparation.effective_input,
        &slice.values,
        &mut preparation.remaining_fuel,
        &mut preparation.receipt_evidence_bytes,
        usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
    )?;
    let mut record = record;
    record
        .precondition_outcomes
        .clone_from(&preparation.precondition_outcomes);
    check_action_record_size::<H::Error>(call.action, &record, limits)?;

    let mut business_input = preparation.effective_input.clone();
    for binding in &action.create_links {
        if catalog
            .effective_type(object_type)
            .is_some_and(|definition| definition.property(&binding.target_id_field).is_none())
        {
            business_input.remove(&binding.target_id_field);
        }
    }
    if let Some(quantity) = &action.quantity_effect {
        business_input.remove(&quantity.balance_versions_field);
    }
    let changes = match write_mode {
        UpsertWriteMode::Upsert | UpsertWriteMode::CreateOnly if !state.exists() => {
            vec![FactChange::Create {
                object_type: object_type.clone(),
                key: object_id.clone(),
                values: business_input.clone(),
            }]
        }
        UpsertWriteMode::Upsert => business_input
            .iter()
            .map(|(property, value)| FactChange::Set {
                object_type: object_type.clone(),
                key: object_id.clone(),
                property: norn_ir::PropertyId(property.clone()),
                value: value.clone(),
            })
            .collect(),
        UpsertWriteMode::CreateOnly => {
            return Err(ActionRefusal::WriteModeFailed {
                mode: "create_only".to_owned(),
                reason: "the target already exists".to_owned(),
            });
        }
        UpsertWriteMode::DraftOnly {
            lifecycle,
            state: draft,
        } => {
            if !state.exists() {
                return Err(ActionRefusal::TargetNotFound);
            }
            let current = state
                .values
                .as_ref()
                .and_then(|values| values.get(lifecycle.as_str()))
                .and_then(JsonValue::as_str);
            if current != Some(draft.as_str()) {
                return Err(ActionRefusal::WriteModeFailed {
                    mode: "draft_only".to_owned(),
                    reason: format!(
                        "the target's {} state is {:?}, expected {:?}",
                        lifecycle.as_str(),
                        current,
                        draft
                    ),
                });
            }
            business_input
                .iter()
                .map(|(property, value)| FactChange::Set {
                    object_type: object_type.clone(),
                    key: object_id.clone(),
                    property: norn_ir::PropertyId(property.clone()),
                    value: value.clone(),
                })
                .collect()
        }
    };
    commit_effect(
        host,
        catalog,
        call,
        authorizer,
        changes,
        Pending {
            effective_input: record
                .effective_input
                .clone()
                .unwrap_or_else(|| preparation.effective_input.clone()),
            remaining_fuel: preparation.remaining_fuel,
            target,
            slice: slice.facts,
            reference_only: slice.reference_only,
            link_reads: slice.link_reads,
            precondition_reads: slice.precondition_keys,
            record_key,
            recorded,
            record,
            series_candidate: None,
        },
        EnvelopePolicy { limits, meter },
    )
}

#[derive(Debug, Clone)]
enum ParsedOwnedRowOp {
    Add {
        collection: String,
        row_id: String,
        expected: FactVersion,
        values: JsonObject,
    },
    Update {
        collection: String,
        row_id: String,
        expected: FactVersion,
        values: JsonObject,
    },
    Remove {
        collection: String,
        row_id: String,
        expected: FactVersion,
    },
    Reorder {
        collection: String,
        rows: Vec<(String, FactVersion)>,
    },
}

impl ParsedOwnedRowOp {
    fn collection(&self) -> &str {
        match self {
            Self::Add { collection, .. }
            | Self::Update { collection, .. }
            | Self::Remove { collection, .. }
            | Self::Reorder { collection, .. } => collection,
        }
    }
}

fn input_version<X>(input: &JsonObject, field: &str) -> Result<FactVersion, ActionRefusal<X>> {
    let Some(JsonValue::String(text)) = input.get(field) else {
        return Err(ActionRefusal::InvalidInput {
            field: field.to_owned(),
        });
    };
    parse_version(text).ok_or_else(|| ActionRefusal::InvalidInput {
        field: field.to_owned(),
    })
}

fn parse_version(text: &str) -> Option<FactVersion> {
    let version = text.parse::<u64>().ok()?;
    (version.to_string() == text).then_some(FactVersion(version))
}

fn event_limit_as_usize(limits: &EnvelopeLimits) -> usize {
    usize::try_from(limits.max_events).unwrap_or(usize::MAX)
}

fn invalid_row_ops<X>() -> ActionRefusal<X> {
    ActionRefusal::InvalidInput {
        field: "rowOps".to_owned(),
    }
}

fn parse_owned_row_op<X>(
    operation: &JsonValue,
    declared: &BTreeMap<String, OwnedChildCollection>,
) -> Result<ParsedOwnedRowOp, ActionRefusal<X>> {
    let Some(object) = operation.as_object() else {
        return Err(invalid_row_ops());
    };
    let (Some(JsonValue::String(collection)), Some(JsonValue::String(kind))) =
        (object.get("collection"), object.get("op"))
    else {
        return Err(invalid_row_ops());
    };
    let Some(collection_definition) = declared.get(collection) else {
        return Err(invalid_row_ops());
    };
    match kind.as_str() {
        "add" | "update" => parse_owned_row_value_op(collection, kind, object),
        "remove" => parse_owned_row_remove(collection, object),
        "reorder" => {
            let slice_limit = usize::try_from(
                crate::limits::ResourceLimitProfile::norn_limits_v1()
                    .slice
                    .max_records,
            )
            .unwrap_or(usize::MAX);
            let collection_limit =
                usize::try_from(collection_definition.max_rows.0).unwrap_or(usize::MAX);
            parse_owned_row_reorder(collection, object, collection_limit.min(slice_limit))
        }
        _ => Err(invalid_row_ops()),
    }
}

fn parse_owned_row_value_op<X>(
    collection: &str,
    kind: &str,
    object: &JsonObject,
) -> Result<ParsedOwnedRowOp, ActionRefusal<X>> {
    let (
        Some(JsonValue::String(row_id)),
        Some(JsonValue::String(version)),
        Some(JsonValue::Object(values)),
    ) = (
        object.get("rowId"),
        object.get("expectedVersion"),
        object.get("values"),
    )
    else {
        return Err(invalid_row_ops());
    };
    if object.len() != 5 {
        return Err(invalid_row_ops());
    }
    let expected = parse_version(version).ok_or_else(invalid_row_ops)?;
    let collection = collection.to_owned();
    let row_id = row_id.clone();
    let values = values.clone();
    Ok(if kind == "add" {
        ParsedOwnedRowOp::Add {
            collection,
            row_id,
            expected,
            values,
        }
    } else {
        ParsedOwnedRowOp::Update {
            collection,
            row_id,
            expected,
            values,
        }
    })
}

fn parse_owned_row_remove<X>(
    collection: &str,
    object: &JsonObject,
) -> Result<ParsedOwnedRowOp, ActionRefusal<X>> {
    let (Some(JsonValue::String(row_id)), Some(JsonValue::String(version))) =
        (object.get("rowId"), object.get("expectedVersion"))
    else {
        return Err(invalid_row_ops());
    };
    if object.len() != 4 {
        return Err(invalid_row_ops());
    }
    Ok(ParsedOwnedRowOp::Remove {
        collection: collection.to_owned(),
        row_id: row_id.clone(),
        expected: parse_version(version).ok_or_else(invalid_row_ops)?,
    })
}

fn parse_owned_row_reorder<X>(
    collection: &str,
    object: &JsonObject,
    max_rows: usize,
) -> Result<ParsedOwnedRowOp, ActionRefusal<X>> {
    let Some(JsonValue::Array(rows)) = object.get("rowIds") else {
        return Err(invalid_row_ops());
    };
    if object.len() != 3 || rows.len() > max_rows {
        return Err(invalid_row_ops());
    }
    let mut parsed_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(row) = row.as_object() else {
            return Err(invalid_row_ops());
        };
        let (Some(JsonValue::String(row_id)), Some(JsonValue::String(version))) =
            (row.get("rowId"), row.get("expectedVersion"))
        else {
            return Err(invalid_row_ops());
        };
        if row.len() != 2 {
            return Err(invalid_row_ops());
        }
        parsed_rows.push((
            row_id.clone(),
            parse_version(version).ok_or_else(invalid_row_ops)?,
        ));
    }
    Ok(ParsedOwnedRowOp::Reorder {
        collection: collection.to_owned(),
        rows: parsed_rows,
    })
}

fn parse_owned_row_ops<X>(
    raw: &[JsonValue],
    limits: &EnvelopeLimits,
    declared: &BTreeMap<String, OwnedChildCollection>,
) -> Result<Vec<ParsedOwnedRowOp>, ActionRefusal<X>> {
    if raw.len() > event_limit_as_usize(limits) {
        return Err(ActionRefusal::Decision(DecisionRefusal::EnvelopeExceeded {
            kind: crate::decider::EnvelopeKind::Events,
            limit: limits.max_events,
            actual: raw.len(),
        }));
    }
    let mut parsed = Vec::with_capacity(raw.len());
    for operation in raw {
        parsed.push(parse_owned_row_op(operation, declared)?);
    }
    Ok(parsed)
}

fn read_manifest_ids<X>(
    manifest: &FactState,
    parent: &FactKey,
    collection_id: &str,
    collection: &OwnedChildCollection,
) -> Result<Vec<String>, ActionRefusal<X>> {
    let Some(values) = manifest.values.as_ref() else {
        return Ok(Vec::new());
    };
    let valid_identity = values.get("parentType")
        == Some(&JsonValue::String(parent.object_type.as_str().to_owned()))
        && values.get("parentKey") == Some(&JsonValue::String(parent.key.clone()))
        && values.get("collectionId") == Some(&JsonValue::String(collection_id.to_owned()));
    let Some(JsonValue::Array(rows)) = values.get("rowIds") else {
        return Err(manifest_corrupt::<X>(parent, collection_id));
    };
    if !valid_identity || rows.len() > usize::try_from(collection.max_rows.0).unwrap_or(100) {
        return Err(manifest_corrupt::<X>(parent, collection_id));
    }
    let mut ids = Vec::with_capacity(rows.len());
    let mut seen = BTreeSet::new();
    for row in rows {
        let Some(row_id) = row.as_str() else {
            return Err(manifest_corrupt::<X>(parent, collection_id));
        };
        if !seen.insert(row_id.to_owned()) {
            return Err(manifest_corrupt::<X>(parent, collection_id));
        }
        ids.push(row_id.to_owned());
    }
    Ok(ids)
}

fn manifest_corrupt<X>(parent: &FactKey, collection_id: &str) -> ActionRefusal<X> {
    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
        reason: format!(
            "owned-row manifest for {parent} collection {collection_id:?} is malformed"
        ),
    })
}

fn integer_equals(value: Option<&JsonValue>, expected: usize) -> bool {
    value
        .and_then(JsonValue::as_i64)
        .and_then(|integer| usize::try_from(integer).ok())
        == Some(expected)
}

fn decimal_value(value: usize) -> Result<JsonValue, String> {
    let text = value.to_string();
    Decimal::parse(&text)
        .map(JsonValue::Number)
        .map_err(|error| error.to_string())
}

fn owned_manifest_key<X>(
    parent: &FactKey,
    collection_id: &str,
) -> Result<FactKey, ActionRefusal<X>> {
    owned_rows_manifest_key(parent, collection_id).map_err(|error| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("owned-row manifest key could not be derived: {error}"),
        })
    })
}

fn manifest_values(parent: &FactKey, collection_id: &str, row_ids: &[String]) -> JsonObject {
    JsonObject::from([
        (
            "parentType".to_owned(),
            JsonValue::String(parent.object_type.as_str().to_owned()),
        ),
        (
            "parentKey".to_owned(),
            JsonValue::String(parent.key.clone()),
        ),
        (
            "collectionId".to_owned(),
            JsonValue::String(collection_id.to_owned()),
        ),
        (
            "rowIds".to_owned(),
            JsonValue::Array(row_ids.iter().cloned().map(JsonValue::String).collect()),
        ),
    ])
}

fn validate_row_values<X>(
    catalog: &TypeCatalog<'_>,
    values: &JsonObject,
    collection: &OwnedChildCollection,
    config: &OwnedCollectionAction,
    field: &str,
) -> Result<(), ActionRefusal<X>> {
    if values.contains_key(collection.row_id_property.as_str())
        || values.contains_key(collection.ordering_key.as_str())
        || catalog
            .lifecycle_properties(&collection.child_type)
            .iter()
            .any(|property| values.contains_key(property.as_str()))
        || config
            .derive
            .keys()
            .any(|property| values.contains_key(property.as_str()))
    {
        return Err(ActionRefusal::InvalidInput {
            field: field.to_owned(),
        });
    }
    validate_stored_patch::<X>(catalog, &collection.child_type, values, field)?;
    Ok(())
}

fn validate_stored_patch<X>(
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeName,
    values: &JsonObject,
    field: &str,
) -> Result<(), ActionRefusal<X>> {
    let Some(effective) = catalog.effective_type(object_type) else {
        return Err(ActionRefusal::InvalidInput {
            field: field.to_owned(),
        });
    };
    for property in values.keys() {
        let Some(declared) = effective
            .properties
            .iter()
            .find(|candidate| candidate.id.as_str() == property)
        else {
            return Err(ActionRefusal::InvalidInput {
                field: field.to_owned(),
            });
        };
        if declared.computed
            || type_series(catalog, object_type)
                .is_some_and(|series| series.key_properties.contains(&declared.id))
        {
            return Err(ActionRefusal::InvalidInput {
                field: field.to_owned(),
            });
        }
    }
    Ok(())
}

fn row_expression_context<X>(
    input: &JsonObject,
    proposed: &JsonObject,
) -> Result<ExprValue, ActionRefusal<X>> {
    let input = expr_value(&JsonValue::Object(input.clone())).map_err(|reason| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("row action input could not be evaluated: {reason}"),
        })
    })?;
    let proposed = expr_value(&JsonValue::Object(proposed.clone())).map_err(|reason| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("proposed row could not be evaluated: {reason}"),
        })
    })?;
    Ok(ExprValue::object([("input", input), ("self", proposed)]))
}

fn derive_row_values<X>(
    catalog: &TypeCatalog<'_>,
    collection: &OwnedChildCollection,
    config: &OwnedCollectionAction,
    input: &JsonObject,
    _current: Option<&JsonObject>,
    mut proposed: JsonObject,
    remaining_fuel: &mut u64,
) -> Result<JsonObject, ActionRefusal<X>> {
    if catalog.effective_type(&collection.child_type).is_none() {
        return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!(
                "owned child type {:?} has no effective schema",
                collection.child_type.as_str()
            ),
        }));
    }
    for (property, expression) in &config.derive {
        let context = row_expression_context::<X>(input, &proposed)?;
        let invalid = |reason| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: format!(
                    "row derivation for {:?} failed: {reason}",
                    property.as_str()
                ),
            })
        };
        let evaluated =
            evaluate_row_expression(expression.as_str(), &context, remaining_fuel, invalid)?;
        let value = expression_value_to_runtime(&evaluated).map_err(invalid)?;
        proposed.insert(property.as_str().to_owned(), value);
    }
    Ok(proposed)
}

fn check_row_preconditions<X>(
    config: &OwnedCollectionAction,
    input: &JsonObject,
    _current: Option<&JsonObject>,
    proposed: &JsonObject,
    remaining_fuel: &mut u64,
) -> Result<(), ActionRefusal<X>> {
    let context = row_expression_context::<X>(input, proposed)?;
    for precondition in &config.preconditions {
        let invalid = |reason| ActionRefusal::PreconditionFailed {
            precondition: precondition.as_str().to_owned(),
            reason: Some(reason),
        };
        match evaluate_row_expression(precondition.as_str(), &context, remaining_fuel, invalid)? {
            ExprValue::Bool(true) => {}
            ExprValue::Bool(false) => {
                return Err(ActionRefusal::PreconditionFailed {
                    precondition: precondition.as_str().to_owned(),
                    reason: None,
                });
            }
            ExprValue::Null
            | ExprValue::Number(_)
            | ExprValue::String(_)
            | ExprValue::Array(_)
            | ExprValue::Object(_) => {
                return Err(invalid(
                    "the expression did not evaluate to a boolean".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn evaluate_row_expression<X>(
    source: &str,
    context: &ExprValue,
    remaining_fuel: &mut u64,
    invalid: impl Fn(String) -> ActionRefusal<X>,
) -> Result<ExprValue, ActionRefusal<X>> {
    let expression = Expression::compile(source).map_err(|error| invalid(error.to_string()))?;
    let attempt = expression.evaluate_metered(context, EvaluationBudget::new(*remaining_fuel));
    *remaining_fuel = remaining_fuel.saturating_sub(attempt.units_used);
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
    )]
    let evaluated = attempt.result.map_err(|error| match error {
        norn_expr::ExprError::BudgetExhausted { .. } => ActionRefusal::ExpressionBudgetExceeded {
            limit: ResourceLimitProfile::norn_limits_v1().turn.fuel_units,
        },
        _ => invalid(error.to_string()),
    })?;
    Ok(evaluated.into_value())
}

fn stale_action<X>(key: FactKey, expected: FactVersion, current: FactVersion) -> ActionRefusal<X> {
    ActionRefusal::Decision(DecisionRefusal::Stale {
        key,
        expected,
        current,
    })
}

struct OwnedProposalAuthorization<'a, 'catalog, 'call> {
    catalog: &'a TypeCatalog<'catalog>,
    call: &'a ActionCall<'call>,
    effective_input: &'a JsonObject,
    authorizer: &'a dyn ActionAuthorizer,
    parent: &'a FactKey,
    authorize_parent: bool,
    proposed_rows: &'a BTreeSet<(String, String)>,
    declared: &'a BTreeMap<String, OwnedChildCollection>,
}

fn authorize_owned_proposals<X>(
    context: &OwnedProposalAuthorization<'_, '_, '_>,
    writes: &mut [FactWrite],
) -> Result<(), ActionRefusal<X>> {
    let mut targets = Vec::new();
    if context.authorize_parent {
        targets.push(context.parent.clone());
    }
    for (collection_id, row_id) in context.proposed_rows {
        let Some(collection) = context.declared.get(collection_id) else {
            return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: format!("unknown owned collection {collection_id:?} in proposed row set"),
            }));
        };
        targets.push(FactKey::new(collection.child_type.as_str(), row_id.clone()));
    }
    for target in targets {
        let Some(write) = writes.iter_mut().find(|write| write.key == target) else {
            // An exact no-op update/reorder has no proposed write. Its
            // committed snapshot was already authorized above.
            continue;
        };
        let FactMutation::Put { values } = &mut write.mutation else {
            continue;
        };
        let canonical = context
            .catalog
            .effective_object_values(&target.object_type, values)
            .map_err(|error| {
                ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("the effective values for {target}: {error}"),
                })
            })?;
        *values = canonical;
        match context.authorizer.authorize(&ActionAuthzRequest {
            action: context.call.action,
            scope: &context.call.scope,
            actor: &context.call.actor,
            input: context.effective_input,
            target: &target,
            target_values: Some(values),
        }) {
            Ok(()) => {}
            Err(ActionDenial::Invisible) => return Err(ActionRefusal::TargetNotFound),
            Err(ActionDenial::Forbidden { reason }) => {
                return Err(ActionRefusal::PermissionDenied { reason });
            }
        }
    }
    Ok(())
}

fn invalid_snapshot<X>(reason: impl Into<String>) -> ActionRefusal<X> {
    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
        reason: reason.into(),
    })
}

fn snapshot_collection<'a, X>(
    rows: &'a BTreeMap<String, Vec<String>>,
    collection_id: &str,
) -> Result<&'a Vec<String>, ActionRefusal<X>> {
    rows.get(collection_id).ok_or_else(|| {
        invalid_snapshot(format!(
            "owned-row snapshot is missing collection {collection_id:?}"
        ))
    })
}

fn snapshot_row<'a, X>(
    rows: &'a BTreeMap<(String, String), FactState>,
    collection_id: &str,
    row_id: &str,
) -> Result<&'a FactState, ActionRefusal<X>> {
    rows.get(&(collection_id.to_owned(), row_id.to_owned()))
        .ok_or_else(|| {
            invalid_snapshot(format!(
                "owned-row snapshot is missing row {row_id:?} in collection {collection_id:?}"
            ))
        })
}

fn snapshot_config<'a, X>(
    configs: &'a BTreeMap<String, &OwnedCollectionAction>,
    collection_id: &str,
) -> Result<&'a OwnedCollectionAction, ActionRefusal<X>> {
    configs.get(collection_id).copied().ok_or_else(|| {
        invalid_snapshot(format!(
            "owned-row action is missing collection {collection_id:?}"
        ))
    })
}

fn snapshot_manifest<'a, X>(
    manifests: &'a BTreeMap<String, FactState>,
    collection_id: &str,
) -> Result<&'a FactState, ActionRefusal<X>> {
    manifests.get(collection_id).ok_or_else(|| {
        invalid_snapshot(format!(
            "owned-row snapshot is missing manifest for collection {collection_id:?}"
        ))
    })
}

fn snapshot_declared<'a, X>(
    declared: &'a BTreeMap<String, OwnedChildCollection>,
    collection_id: &str,
) -> Result<&'a OwnedChildCollection, ActionRefusal<X>> {
    declared.get(collection_id).ok_or_else(|| {
        invalid_snapshot(format!(
            "owned-row declaration is missing collection {collection_id:?}"
        ))
    })
}

fn normalized_row_operations<X>(
    raw: &[JsonValue],
    delete_parent: bool,
    declared: &BTreeMap<String, OwnedChildCollection>,
    ordered_rows: &BTreeMap<String, Vec<String>>,
    row_states: &BTreeMap<(String, String), FactState>,
) -> Result<Vec<JsonValue>, ActionRefusal<X>> {
    let mut operations = raw.to_vec();
    if delete_parent {
        for collection_id in declared.keys() {
            for row_id in snapshot_collection(ordered_rows, collection_id)? {
                let version = snapshot_row(row_states, collection_id, row_id)?.version;
                operations.push(JsonValue::Object(JsonObject::from([
                    (
                        "collection".to_owned(),
                        JsonValue::String(collection_id.clone()),
                    ),
                    ("op".to_owned(), JsonValue::String("remove".to_owned())),
                    ("rowId".to_owned(), JsonValue::String(row_id.clone())),
                    (
                        "expectedVersion".to_owned(),
                        JsonValue::String(version.0.to_string()),
                    ),
                    ("cascade".to_owned(), JsonValue::Bool(true)),
                ])));
            }
        }
    }
    Ok(operations)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the admitted parent-and-row effect is one atomic authorization and commit boundary"
)]
fn commit_parent_rows<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    mut preparation: ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    object_id_field: &str,
    object_kind: &str,
    parent_values_field: &str,
    row_ops_field: &str,
    parent_version_field: &str,
    parent_delete_field: Option<&str>,
    collection_actions: &[OwnedCollectionAction],
    limits: &EnvelopeLimits,
    meter: Option<&mut TurnEnvelopeMeter>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let Some(JsonValue::String(object_id)) = preparation.effective_input.get(object_id_field)
    else {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    };
    if object_id.trim().is_empty() {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    }

    let object_type = call
        .object_type
        .clone()
        .unwrap_or_else(|| ObjectTypeName(object_kind.to_owned()));
    let target = FactKey::new(object_type.as_str(), object_id.clone());
    crate::check_fact_key_identity(&target)
        .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
    let parent_state = host.load(&target).map_err(host_failure)?;
    if parent_state.exists() {
        authorize_current_target(
            call,
            &preparation.effective_input,
            authorizer,
            &target,
            &parent_state,
        )?;
    }
    check_kind(catalog, &object_type, object_kind)?;

    let expected_parent =
        input_version::<H::Error>(&preparation.effective_input, parent_version_field)?;
    let parent_values = match preparation.effective_input.get(parent_values_field) {
        Some(JsonValue::Object(values)) => values.clone(),
        _ => {
            return Err(ActionRefusal::InvalidInput {
                field: parent_values_field.to_owned(),
            });
        }
    };
    let delete_parent = match parent_delete_field {
        None => false,
        Some(field) => match preparation.effective_input.get(field) {
            None => false,
            Some(JsonValue::Bool(delete)) => *delete,
            _ => {
                return Err(ActionRefusal::InvalidInput {
                    field: field.to_owned(),
                });
            }
        },
    };
    if delete_parent && !parent_values.is_empty() {
        return Err(ActionRefusal::InvalidInput {
            field: parent_values_field.to_owned(),
        });
    }
    let Some(JsonValue::Array(raw_row_ops)) = preparation.effective_input.get(row_ops_field) else {
        return Err(ActionRefusal::InvalidInput {
            field: row_ops_field.to_owned(),
        });
    };
    if delete_parent && !raw_row_ops.is_empty() {
        return Err(ActionRefusal::InvalidInput {
            field: row_ops_field.to_owned(),
        });
    }

    let Some(effective_type) = catalog.effective_type(&object_type) else {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: call.action.action_id.clone(),
        });
    };
    validate_stored_patch::<H::Error>(catalog, &object_type, &parent_values, parent_values_field)?;
    let mut declared = effective_type
        .owned_collections
        .iter()
        .map(|collection| (collection.id.as_str().to_owned(), collection.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut configs = BTreeMap::new();
    for config in collection_actions {
        if configs
            .insert(config.id.as_str().to_owned(), config)
            .is_some()
        {
            return Err(ActionRefusal::UnsupportedEffect {
                action_id: call.action.action_id.clone(),
            });
        }
    }
    let Some(action_type) = catalog.effective_type(&ObjectTypeName(object_kind.to_owned())) else {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: call.action.action_id.clone(),
        });
    };
    // Coverage belongs to the action's pinned type, not to collections added
    // by a concrete subtype. A base action cannot delete those collections
    // without their derivations/guards, nor accept operations against them.
    if action_type.owned_collections.is_empty()
        || action_type.owned_collections.len() != configs.len()
        || action_type
            .owned_collections
            .iter()
            .any(|collection| !configs.contains_key(collection.id.as_str()))
        || configs.keys().any(|id| !declared.contains_key(id))
        || (delete_parent && declared.keys().any(|id| !configs.contains_key(id)))
    {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: call.action.action_id.clone(),
        });
    }
    declared.retain(|id, _| configs.contains_key(id));
    if declared
        .values()
        .any(|collection| collection.max_rows.0 == 0 || collection.max_rows.0 > 100)
        || declared.iter().any(|(id, collection)| {
            let Some(child) = catalog.effective_type(&collection.child_type) else {
                return true;
            };
            configs.get(id.as_str()).is_none_or(|config| {
                config.derive.keys().any(|property| {
                    property == &collection.row_id_property
                        || property == &collection.ordering_key
                        || type_series(catalog, &collection.child_type)
                            .is_some_and(|series| series.key_properties.contains(property))
                        || child
                            .properties
                            .iter()
                            .find(|candidate| candidate.id == *property)
                            .is_none_or(|candidate| candidate.computed)
                })
            })
        })
    {
        return Err(ActionRefusal::UnsupportedEffect {
            action_id: call.action.action_id.clone(),
        });
    }
    let parsed = parse_owned_row_ops::<H::Error>(raw_row_ops, limits, &declared)?;
    if parsed
        .iter()
        .any(|op| !declared.contains_key(op.collection()))
    {
        return Err(ActionRefusal::InvalidInput {
            field: row_ops_field.to_owned(),
        });
    }

    let mut manifest_states = BTreeMap::<String, FactState>::new();
    let mut ordered_rows = BTreeMap::<String, Vec<String>>::new();
    let mut row_states = BTreeMap::<(String, String), FactState>::new();
    let mut row_facts = BTreeSet::new();
    let mut manifest_bytes = 0usize;
    let slice_limits = crate::limits::ResourceLimitProfile::norn_limits_v1().slice;
    let max_snapshot_rows = usize::try_from(slice_limits.max_records).unwrap_or(usize::MAX);
    if declared.len() > max_snapshot_rows {
        return Err(ActionRefusal::SliceUnavailable {
            entry: "$owned_rows".to_owned(),
            reason: "owned-row manifests exceed the bounded slice records".to_owned(),
        });
    }
    let mut collection_rows = 0usize;
    for (collection_id, collection) in &declared {
        let manifest_key = owned_manifest_key::<H::Error>(&target, collection_id)?;
        let manifest = host.load(&manifest_key).map_err(host_failure)?;
        let ids = read_manifest_ids::<H::Error>(&manifest, &target, collection_id, collection)?;
        if let Some(values) = &manifest.values {
            let bytes = norn_ir::canonical::canonical_json_of(values)
                .map_err(|error| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned-row manifest could not be measured: {error}"),
                    })
                })?
                .len();
            manifest_bytes = manifest_bytes.saturating_add(bytes);
        }
        for row_id in &ids {
            let row_key = FactKey::new(collection.child_type.as_str(), row_id.clone());
            crate::check_fact_key_identity(&row_key)
                .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
        }
        collection_rows = collection_rows.saturating_add(ids.len());
        manifest_states.insert(collection_id.clone(), manifest);
        ordered_rows.insert(collection_id.clone(), ids.clone());
    }
    let snapshot_cost = collection_rows
        .saturating_add(raw_row_ops.len())
        .saturating_add(declared.len());
    if snapshot_cost > max_snapshot_rows {
        return Err(ActionRefusal::SliceUnavailable {
            entry: "$owned_rows".to_owned(),
            reason: "owned-row state and operations exceed the bounded slice records".to_owned(),
        });
    }
    let input_bytes = norn_ir::canonical::canonical_json_of(&preparation.effective_input)
        .map_err(|error| ActionRefusal::InvalidInput {
            field: format!("$input ({error})"),
        })?
        .len();
    let max_snapshot_bytes =
        usize::try_from(slice_limits.max_canonical_bytes).unwrap_or(usize::MAX);
    let mut snapshot_bytes = manifest_bytes.saturating_add(input_bytes);
    if let Some(values) = &parent_state.values {
        let bytes = norn_ir::canonical::canonical_json_of(values)
            .map_err(|error| {
                ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("parent value snapshot could not be measured: {error}"),
                })
            })?
            .len();
        snapshot_bytes = snapshot_bytes.saturating_add(bytes);
    }
    if snapshot_bytes > max_snapshot_bytes {
        return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: "owned-row state, manifests, and action input snapshot exceed the bounded slice bytes"
                .to_owned(),
        }));
    }
    for (collection_id, collection) in &declared {
        let ids = snapshot_collection(&ordered_rows, collection_id)?;
        for (index, row_id) in ids.iter().enumerate() {
            let row_key = FactKey::new(collection.child_type.as_str(), row_id.clone());
            crate::check_fact_key_identity(&row_key)
                .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
            if !row_facts.insert(row_key.clone()) {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("row {row_key} appears in more than one owned collection"),
                }));
            }
            let row_state = host.load(&row_key).map_err(host_failure)?;
            authorize_current_target(
                call,
                &preparation.effective_input,
                authorizer,
                &row_key,
                &row_state,
            )?;
            let Some(values) = row_state.values.as_ref() else {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("owned row {row_key} is missing from storage"),
                }));
            };
            let bytes = norn_ir::canonical::canonical_json_of(values)
                .map_err(|error| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned row value snapshot could not be measured: {error}"),
                    })
                })?
                .len();
            snapshot_bytes = snapshot_bytes.saturating_add(bytes);
            if snapshot_bytes > max_snapshot_bytes {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: "owned-row state, manifests, and action input snapshot exceed the bounded slice bytes"
                        .to_owned(),
                }));
            }
            if values.get(collection.row_id_property.as_str())
                != Some(&JsonValue::String(row_id.clone()))
                || !integer_equals(
                    values.get(collection.ordering_key.as_str()),
                    index
                        .checked_add(1)
                        .ok_or_else(|| ActionRefusal::Decision(DecisionRefusal::Overflow))?,
                )
            {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!(
                        "owned row {row_key} has an inconsistent stable id or ordering key"
                    ),
                }));
            }
            let link_key = LinkKey::new(target.clone(), collection_id.clone(), row_key.clone());
            let link_state = host.load_link(&link_key).map_err(host_failure)?;
            let Some(link_values) = link_state.values.as_ref() else {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("owned row {row_key} is missing its declared parent link"),
                }));
            };
            let bytes = norn_ir::canonical::canonical_json_of(link_values)
                .map_err(|error| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!(
                            "owned row link value snapshot could not be measured: {error}"
                        ),
                    })
                })?
                .len();
            snapshot_bytes = snapshot_bytes.saturating_add(bytes);
            if snapshot_bytes > max_snapshot_bytes {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: "owned-row state, links, manifests, and action input snapshot exceed the bounded slice bytes"
                        .to_owned(),
                }));
            }
            row_states.insert((collection_id.clone(), row_id.clone()), row_state);
        }
    }

    let record_key = FactKey::new(
        ACTION_CALL_TYPE,
        format!("{}/{}", call.action.action_id, call.idempotency_key),
    );
    let record = ActionCallRecord {
        action_id: call.action.action_id.clone(),
        action_artifact_id: Some(call.action_artifact_id.to_owned()),
        action_version: Some(call.action_version.to_owned()),
        effect_kind: Some(ACTION_EFFECT_MUTATE_PARENT_WITH_ROWS.to_owned()),
        object: target.clone(),
        input: call.input.clone(),
        effective_input: (!call.action.derive.is_empty())
            .then(|| preparation.effective_input.clone()),
        derived_values: preparation.derived_values.clone(),
        precondition_outcomes: Vec::new(),
        actor: call.actor.clone(),
        turn: call.turn.id.clone(),
        transition: String::new(),
        from: String::new(),
        to: String::new(),
        row_operations: normalized_row_operations(
            raw_row_ops,
            delete_parent,
            &declared,
            &ordered_rows,
            &row_states,
        )?,
    };
    let recorded = host.load(&record_key).map_err(host_failure)?;
    if !parent_state.exists() && recorded.exists() {
        // A deleted parent cannot be used to reveal a formerly successful
        // mutation through its idempotency record. Create retries still
        // replay because their current parent exists and was authorized.
        authorize_current_target(
            call,
            &preparation.effective_input,
            authorizer,
            &target,
            &parent_state,
        )?;
    }
    if let Some(outcome) = replay(&recorded, &record, &call.idempotency_key)? {
        return Ok(outcome);
    }

    if !parent_state.exists() && type_series(catalog, &object_type).is_some() {
        return Err(ActionRefusal::InvalidInput {
            field: object_id_field.to_owned(),
        });
    }
    if parsed.iter().any(|op| {
        matches!(op, ParsedOwnedRowOp::Add { collection, .. }
            if declared.get(collection).is_some_and(|owned| type_series(catalog, &owned.child_type).is_some()))
    }) {
        return Err(ActionRefusal::InvalidInput {
            field: row_ops_field.to_owned(),
        });
    }

    // Reserve the same owned-state/input work already bounded above before
    // loading any declared dependency. Declared aliases intentionally incur
    // another charge: their authorized expression values are materialized
    // separately from the owned mutation snapshot.
    let mut slice_meter = SliceMeter::new(slice_limits);
    for record_index in 0..snapshot_cost {
        let bytes = if record_index == 0 {
            u64::try_from(snapshot_bytes).unwrap_or(u64::MAX)
        } else {
            0
        };
        slice_meter
            .admit_record(bytes, 0)
            .map_err(|error| ActionRefusal::SliceUnavailable {
                entry: "$owned_rows".to_owned(),
                reason: error.to_string(),
            })?;
    }
    let action_slice = load_action_slice(
        &ActionSliceContext {
            host,
            catalog,
            call,
            effective_input: &preparation.effective_input,
            authorizer,
        },
        &target,
        &parent_state,
        slice_meter,
    )?;
    preparation.precondition_outcomes = check_action_preconditions(
        call.action,
        &preparation.effective_input,
        &action_slice.values,
        &mut preparation.remaining_fuel,
        &mut preparation.receipt_evidence_bytes,
        usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
    )?;
    let mut record = record;
    record
        .precondition_outcomes
        .clone_from(&preparation.precondition_outcomes);
    check_action_record_size::<H::Error>(call.action, &record, limits)?;

    // Replays must be resolved before CAS validation: a create retry carries
    // its original absent-version token although the committed target now
    // exists. Current authorization of the parent and all descendants above
    // still happens before the replay is exposed.
    if delete_parent && !parent_state.exists() {
        return Err(ActionRefusal::TargetNotFound);
    }
    if delete_parent
        && declared
            .values()
            .any(|collection| !collection.cascade_on_parent_delete)
    {
        return Err(ActionRefusal::WriteModeFailed {
            mode: "parent_delete".to_owned(),
            reason: "every owned collection must declare cascadeOnParentDelete".to_owned(),
        });
    }

    let mut changes = Vec::<FactChange>::new();
    if delete_parent {
        changes.push(FactChange::Delete {
            object_type: object_type.clone(),
            key: object_id.clone(),
        });
    } else if !parent_state.exists() {
        changes.push(FactChange::Create {
            object_type: object_type.clone(),
            key: object_id.clone(),
            values: parent_values,
        });
    } else {
        changes.extend(
            parent_values
                .iter()
                .map(|(property, value)| FactChange::Set {
                    object_type: object_type.clone(),
                    key: object_id.clone(),
                    property: PropertyId(property.clone()),
                    value: value.clone(),
                }),
        );
        if parent_values.is_empty() && !parsed.is_empty() {
            let current_values = parent_state.values.as_ref().ok_or_else(|| {
                ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: "existing parent has no committed value snapshot".to_owned(),
                })
            })?;
            let lifecycle_properties = catalog.lifecycle_properties(&object_type);
            let touch = effective_type.properties.iter().find_map(|property| {
                if property.computed || lifecycle_properties.contains(&property.id) {
                    None
                } else {
                    current_values
                        .get(property.id.as_str())
                        .map(|value| (property.id.clone(), value.clone()))
                }
            });
            let Some((property, value)) = touch else {
                return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: "a row-only mutation requires a writable parent property to advance the parent version".to_owned(),
                }));
            };
            changes.push(FactChange::Set {
                object_type: object_type.clone(),
                key: object_id.clone(),
                property,
                value,
            });
        }
    }

    let mut proposed_rows = BTreeSet::<(String, String)>::new();
    let mut row_values = BTreeMap::<(String, String), JsonObject>::new();
    let mut guarded_rows = BTreeMap::<(String, String), JsonObject>::new();
    let mut link_writes = Vec::<LinkWrite>::new();
    let mut manifest_writes = Vec::<FactWrite>::new();
    if delete_parent {
        for (collection_id, collection) in &declared {
            for row_id in snapshot_collection(&ordered_rows, collection_id)? {
                let row_key = FactKey::new(collection.child_type.as_str(), row_id.clone());
                changes.push(FactChange::Delete {
                    object_type: collection.child_type.clone(),
                    key: row_id.clone(),
                });
                let row_state = snapshot_row(&row_states, collection_id, row_id)?;
                let values = row_state.values.as_ref().ok_or_else(|| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!(
                            "owned row {row_id:?} disappeared from the loaded snapshot"
                        ),
                    })
                })?;
                guarded_rows.insert((collection_id.clone(), row_id.clone()), values.clone());
                let link_key = LinkKey::new(target.clone(), collection_id.clone(), row_key);
                let link_state = host.load_link(&link_key).map_err(host_failure)?;
                link_writes.push(LinkWrite {
                    key: link_key,
                    expected: link_state.version,
                    mutation: LinkMutation::Remove,
                });
            }
            let manifest_key = owned_manifest_key::<H::Error>(&target, collection_id)?;
            let manifest = snapshot_manifest(&manifest_states, collection_id)?;
            if manifest.exists() {
                manifest_writes.push(FactWrite {
                    key: manifest_key,
                    expected: manifest.version,
                    mutation: FactMutation::Delete,
                });
            }
        }
    } else {
        for (collection_id, collection) in &declared {
            let collection_ops = parsed
                .iter()
                .filter(|op| op.collection() == collection_id)
                .collect::<Vec<_>>();
            let initial = snapshot_collection(&ordered_rows, collection_id)?;
            let mut additions = Vec::new();
            let mut removals = BTreeSet::new();
            let mut replacements = BTreeMap::<String, JsonObject>::new();
            let mut touched = BTreeSet::<String>::new();
            let mut reorder = None;
            for op in &collection_ops {
                match op {
                    ParsedOwnedRowOp::Add {
                        row_id,
                        expected,
                        values,
                        ..
                    } => {
                        if *expected != FactVersion::ABSENT || initial.contains(row_id) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        if !touched.insert(row_id.clone()) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let row_key = FactKey::new(collection.child_type.as_str(), row_id.clone());
                        crate::check_fact_key_identity(&row_key).map_err(|error| {
                            ActionRefusal::Decision(DecisionRefusal::from_commit(error))
                        })?;
                        if row_facts.contains(&row_key) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let current = host.load(&row_key).map_err(host_failure)?;
                        if current.exists() || current.version != FactVersion::ABSENT {
                            if current.exists() {
                                authorize_current_target(
                                    call,
                                    &preparation.effective_input,
                                    authorizer,
                                    &row_key,
                                    &current,
                                )?;
                            }
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let config = snapshot_config(&configs, collection_id)?;
                        validate_row_values::<H::Error>(
                            catalog,
                            values,
                            collection,
                            config,
                            row_ops_field,
                        )?;
                        let mut proposal = values.clone();
                        proposal.insert(
                            collection.row_id_property.as_str().to_owned(),
                            JsonValue::String(row_id.clone()),
                        );
                        additions.push(row_id.clone());
                        replacements.insert(row_id.clone(), proposal);
                        proposed_rows.insert((collection_id.clone(), row_id.clone()));
                    }
                    ParsedOwnedRowOp::Update {
                        row_id,
                        expected,
                        values,
                        ..
                    } => {
                        if !initial.contains(row_id) || !touched.insert(row_id.clone()) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let current = snapshot_row(&row_states, collection_id, row_id)?;
                        if current.version != *expected {
                            return Err(stale_action(
                                FactKey::new(collection.child_type.as_str(), row_id.clone()),
                                *expected,
                                current.version,
                            ));
                        }
                        let config = snapshot_config(&configs, collection_id)?;
                        validate_row_values::<H::Error>(
                            catalog,
                            values,
                            collection,
                            config,
                            row_ops_field,
                        )?;
                        let current_values = current.values.as_ref().ok_or_else(|| {
                            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                                reason: format!(
                                    "owned row {row_id:?} disappeared from the loaded snapshot"
                                ),
                            })
                        })?;
                        let mut proposal = current_values.clone();
                        proposal.extend(values.clone());
                        replacements.insert(row_id.clone(), proposal);
                        proposed_rows.insert((collection_id.clone(), row_id.clone()));
                    }
                    ParsedOwnedRowOp::Remove {
                        row_id, expected, ..
                    } => {
                        if !initial.contains(row_id) || !touched.insert(row_id.clone()) {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        let current = snapshot_row(&row_states, collection_id, row_id)?;
                        if current.version != *expected {
                            return Err(stale_action(
                                FactKey::new(collection.child_type.as_str(), row_id.clone()),
                                *expected,
                                current.version,
                            ));
                        }
                        let current_values = current.values.as_ref().ok_or_else(|| {
                            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                                reason: format!(
                                    "owned row {row_id:?} disappeared from the loaded snapshot"
                                ),
                            })
                        })?;
                        guarded_rows.insert(
                            (collection_id.clone(), row_id.clone()),
                            current_values.clone(),
                        );
                        removals.insert(row_id.clone());
                    }
                    ParsedOwnedRowOp::Reorder { rows, .. } => {
                        if reorder.is_some() {
                            return Err(ActionRefusal::InvalidInput {
                                field: row_ops_field.to_owned(),
                            });
                        }
                        for (row_id, expected) in rows {
                            let state = row_states.get(&(collection_id.clone(), row_id.clone()));
                            let actual = state.map_or(FactVersion::ABSENT, |state| state.version);
                            if actual != *expected {
                                return Err(stale_action(
                                    FactKey::new(collection.child_type.as_str(), row_id.clone()),
                                    *expected,
                                    actual,
                                ));
                            }
                        }
                        reorder = Some(rows.clone());
                    }
                }
            }
            let mut final_order = initial
                .iter()
                .filter(|row_id| !removals.contains(*row_id))
                .cloned()
                .collect::<Vec<_>>();
            final_order.extend(additions.iter().cloned());
            if let Some(reorder) = reorder {
                let requested = reorder
                    .iter()
                    .map(|(row_id, _)| row_id.clone())
                    .collect::<Vec<_>>();
                let expected_members = final_order.iter().cloned().collect::<BTreeSet<_>>();
                let requested_members = requested.iter().cloned().collect::<BTreeSet<_>>();
                if requested.len() != requested_members.len()
                    || requested_members != expected_members
                {
                    return Err(ActionRefusal::InvalidInput {
                        field: row_ops_field.to_owned(),
                    });
                }
                final_order = requested;
            }
            if final_order.len() > usize::try_from(collection.max_rows.0).unwrap_or(100) {
                return Err(ActionRefusal::InvalidInput {
                    field: row_ops_field.to_owned(),
                });
            }
            for row_id in &final_order {
                let row_key = (collection_id.clone(), row_id.clone());
                if let Some(values) = replacements.get(row_id) {
                    row_values.insert(row_key, values.clone());
                } else if let Some(values) = row_states
                    .get(&(collection_id.clone(), row_id.clone()))
                    .and_then(|state| state.values.clone())
                {
                    row_values.insert(row_key, values);
                }
            }
            for (index, row_id) in final_order.iter().enumerate() {
                let order = decimal_value(
                    index
                        .checked_add(1)
                        .ok_or_else(|| ActionRefusal::Decision(DecisionRefusal::Overflow))?,
                )
                .map_err(|reason| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput { reason })
                })?;
                let key = (collection_id.clone(), row_id.clone());
                let mut values = row_values.get(&key).cloned().ok_or_else(|| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned row {row_id:?} has no final value snapshot"),
                    })
                })?;
                let old_order = row_states
                    .get(&key)
                    .and_then(|state| state.values.as_ref())
                    .and_then(|values| values.get(collection.ordering_key.as_str()));
                let order_changed = old_order != Some(&order);
                values.insert(collection.ordering_key.as_str().to_owned(), order.clone());
                values = catalog
                    .materialize_object_defaults(&collection.child_type, &values)
                    .map_err(|error| {
                        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                            reason: format!(
                                "owned row defaults could not be materialized: {error}"
                            ),
                        })
                    })?;
                values = fill_lifecycle_defaults::<H::Error>(
                    catalog,
                    &FactKey::new(collection.child_type.as_str(), row_id.clone()),
                    &values,
                )?;
                let current_values = row_states.get(&key).and_then(|state| state.values.as_ref());
                let recompute_derived = additions.contains(row_id)
                    || replacements.contains_key(row_id)
                    || order_changed;
                if recompute_derived {
                    values = derive_row_values::<H::Error>(
                        catalog,
                        collection,
                        snapshot_config(&configs, collection_id)?,
                        &preparation.effective_input,
                        current_values,
                        values,
                        &mut preparation.remaining_fuel,
                    )?;
                    guarded_rows.insert(key.clone(), values.clone());
                }
                row_values.insert(key.clone(), values.clone());
                if additions.contains(row_id) {
                    changes.push(FactChange::Create {
                        object_type: collection.child_type.clone(),
                        key: row_id.clone(),
                        values: values.clone(),
                    });
                    let link_key = LinkKey::new(
                        target.clone(),
                        collection_id.clone(),
                        FactKey::new(collection.child_type.as_str(), row_id.clone()),
                    );
                    let link_state = host.load_link(&link_key).map_err(host_failure)?;
                    if link_state.exists() {
                        return Err(ActionRefusal::InvalidInput {
                            field: row_ops_field.to_owned(),
                        });
                    }
                    link_writes.push(LinkWrite {
                        key: link_key,
                        expected: link_state.version,
                        mutation: LinkMutation::Add {
                            values: JsonObject::new(),
                        },
                    });
                } else {
                    let old = row_states
                        .get(&key)
                        .and_then(|state| state.values.as_ref())
                        .ok_or_else(|| {
                            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                                reason: format!(
                                    "owned row {row_id:?} disappeared from the loaded snapshot"
                                ),
                            })
                        })?;
                    for (property, value) in &values {
                        if property == collection.row_id_property.as_str() {
                            continue;
                        }
                        if old.get(property) != Some(value)
                            && (recompute_derived
                                || (property == collection.ordering_key.as_str() && order_changed))
                        {
                            changes.push(FactChange::Set {
                                object_type: collection.child_type.clone(),
                                key: row_id.clone(),
                                property: PropertyId(property.clone()),
                                value: value.clone(),
                            });
                        }
                    }
                }
                if proposed_rows.contains(&key) || order_changed {
                    proposed_rows.insert(key);
                }
            }
            for row_id in &removals {
                changes.push(FactChange::Delete {
                    object_type: collection.child_type.clone(),
                    key: row_id.clone(),
                });
                let link_key = LinkKey::new(
                    target.clone(),
                    collection_id.clone(),
                    FactKey::new(collection.child_type.as_str(), row_id.clone()),
                );
                let link_state = host.load_link(&link_key).map_err(host_failure)?;
                if !link_state.exists() {
                    return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned row {row_id:?} is missing its declared parent link"),
                    }));
                }
                link_writes.push(LinkWrite {
                    key: link_key,
                    expected: link_state.version,
                    mutation: LinkMutation::Remove,
                });
            }
            if !collection_ops.is_empty() {
                let manifest_key = owned_manifest_key::<H::Error>(&target, collection_id)?;
                let manifest = snapshot_manifest(&manifest_states, collection_id)?;
                manifest_writes.push(FactWrite {
                    key: manifest_key,
                    expected: manifest.version,
                    mutation: FactMutation::Put {
                        values: manifest_values(&target, collection_id, &final_order),
                    },
                });
            }
        }
    }

    // Materialize link defaults before sealing the owned-row proof so the
    // native host's identical canonicalization leaves its protected writes
    // byte-for-byte unchanged during admission.
    for write in &mut link_writes {
        if let LinkMutation::Add { values } = &mut write.mutation {
            *values = catalog
                .effective_link_values(&target.object_type, &write.key.link, values)
                .map_err(|error| {
                    ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                        reason: format!("owned collection link values are invalid: {error}"),
                    })
                })?;
        }
    }

    let mut slice = action_slice.facts;
    for ((collection_id, row_id), state) in &row_states {
        let row_type = &snapshot_declared(&declared, collection_id)?.child_type;
        reconcile_action_target_snapshot::<H::Error>(
            catalog,
            &mut slice,
            &FactKey::new(row_type.as_str(), row_id.clone()),
            state,
        )?;
    }
    for op in &parsed {
        if let ParsedOwnedRowOp::Add {
            collection, row_id, ..
        } = op
        {
            reconcile_action_target_snapshot::<H::Error>(
                catalog,
                &mut slice,
                &FactKey::new(
                    snapshot_declared(&declared, collection)?
                        .child_type
                        .as_str(),
                    row_id.clone(),
                ),
                &FactState::default(),
            )?;
        }
    }
    let commands = build_action_commands::<H::Error>(
        call.action,
        &preparation.effective_input,
        &format!("{}/{}", call.action.action_id, call.idempotency_key),
        &mut preparation.remaining_fuel,
    )?;
    let output = DecisionOutput {
        facts: changes,
        commands,
        timers: Vec::new(),
    };
    let mut prepared =
        prepare_owned_rows::<H::Error>(catalog, &slice, &call.turn, &output, limits)?;
    for (collection_id, manifest) in &manifest_states {
        let key = owned_manifest_key::<H::Error>(&target, collection_id)?;
        if !manifest_writes.iter().any(|write| write.key == key) {
            prepared.reads.push(FactExpectation {
                key,
                expected: manifest.version,
            });
        }
    }
    let counts =
        check_action_envelope::<H::Error>(limits, prepared.events.len(), output.commands.len())?;
    authorize_owned_proposals(
        &OwnedProposalAuthorization {
            catalog,
            call,
            effective_input: &preparation.effective_input,
            authorizer,
            parent: &target,
            authorize_parent: !delete_parent,
            proposed_rows: &proposed_rows,
            declared: &declared,
        },
        &mut prepared.writes,
    )?;
    // Do not expose a row guard's outcome until every proposed write has
    // passed authorization. Removed rows use their authorized current state.
    for ((collection_id, row_id), values) in &guarded_rows {
        check_row_preconditions::<H::Error>(
            snapshot_config(&configs, collection_id)?,
            &preparation.effective_input,
            row_states
                .get(&(collection_id.clone(), row_id.clone()))
                .and_then(|state| state.values.as_ref()),
            values,
            &mut preparation.remaining_fuel,
        )?;
    }
    if delete_parent && !parent_state.exists() {
        return Err(ActionRefusal::TargetNotFound);
    }
    if expected_parent != parent_state.version {
        return Err(stale_action(target, expected_parent, parent_state.version));
    }
    let mut events = std::mem::take(&mut prepared.events);
    let action_call = format!("{}/{}", call.action.action_id, call.idempotency_key);
    for effect in &prepared.effects {
        if let crate::effect::Effect::Perform(command) = effect {
            events.push(FactEvent::CommandEnqueued {
                command_id: command.id.0.clone(),
                generation: command.generation.0,
                idempotency_key: command.idempotency_key.0.clone(),
                action_call: action_call.clone(),
                payload: command.payload.clone(),
            });
        }
    }
    let mut writes = prepared.writes;
    writes.extend(manifest_writes);
    writes.push(FactWrite {
        key: record_key.clone(),
        expected: recorded.version,
        mutation: FactMutation::Put {
            values: record.to_values(),
        },
    });
    let commit = CellCommit {
        consumes: prepared.consumes,
        reads: prepared.reads,
        writes,
        events: events.into_iter().map(Into::into).collect(),
        effects: prepared.effects.into_iter().map(widen_effect).collect(),
        links: link_writes,
        link_reads: action_slice.link_reads,
    };
    if let Some(meter) = meter.as_deref() {
        meter.check(counts).map_err(|exceeded| {
            ActionRefusal::Decision(DecisionRefusal::TurnEnvelopeExceeded {
                kind: exceeded.kind,
                limit: exceeded.limit,
                actual: exceeded.actual,
            })
        })?;
    }
    let proof = OwnedRowsCommitProof::new(&commit, limits.max_events);
    match host.commit_owned_rows(commit, proof) {
        Ok(receipt) => {
            if let Some(meter) = meter {
                meter.charge(counts);
            }
            Ok(ActionOutcome::Committed { receipt, record })
        }
        Err(error @ CommitError::InvariantViolation { .. }) => Err(ActionRefusal::Decision(
            ledger_action_invariant_refusal(host, call, error),
        )),
        Err(error) => {
            if matches!(
                &error,
                CommitError::Conflict { .. }
                    | CommitError::LinkConflict { .. }
                    | CommitError::LinkReadConflict { .. }
                    | CommitError::UnsupportedLinkRead { .. }
                    | CommitError::NotPending { .. }
            ) && let Some(outcome) = replay_parent_rows_after_race(
                host,
                call,
                &preparation.effective_input,
                authorizer,
                &target,
                &record_key,
                &record,
                &declared,
            )? {
                return Ok(outcome);
            }
            Err(
                precondition_read_refusal(&error, &action_slice.precondition_keys).unwrap_or_else(
                    || ActionRefusal::Decision(DecisionRefusal::from_commit(error)),
                ),
            )
        }
    }
}

/// Rechecks the winner's committed parent and descendants before exposing an
/// idempotent replay after a compare-and-swap race.
#[allow(
    clippy::too_many_arguments,
    reason = "replay reauthorizes the parent and descendants with the prepared effective input"
)]
fn replay_parent_rows_after_race<H>(
    host: &H,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    parent: &FactKey,
    record_key: &FactKey,
    record: &ActionCallRecord,
    declared: &BTreeMap<String, OwnedChildCollection>,
) -> Result<Option<ActionOutcome>, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
{
    let recorded = host.load(record_key).map_err(host_failure)?;
    if !recorded.exists() {
        return Ok(None);
    }
    let parent_state = host.load(parent).map_err(host_failure)?;
    authorize_current_target(call, effective_input, authorizer, parent, &parent_state)?;
    for (collection_id, collection) in declared {
        let key = owned_manifest_key::<H::Error>(parent, collection_id)?;
        let manifest = host.load(&key).map_err(host_failure)?;
        for row_id in read_manifest_ids::<H::Error>(&manifest, parent, collection_id, collection)? {
            let row = FactKey::new(collection.child_type.as_str(), row_id);
            let state = host.load(&row).map_err(host_failure)?;
            authorize_current_target(call, effective_input, authorizer, &row, &state)?;
        }
    }
    replay(&recorded, record, &call.idempotency_key)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "allocation, idempotency, authorization, and CAS retry form one bounded action path"
)]
fn commit_upsert_series<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    mut preparation: ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    object_id_field: &str,
    object_type: &ObjectTypeName,
    series_owner: &ObjectTypeName,
    key_name: &KeyName,
    key_properties: &[PropertyId],
    declaration: &NamingSeriesIR,
    limits: &EnvelopeLimits,
    meter: Option<&mut TurnEnvelopeMeter>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let record_key = FactKey::new(
        ACTION_CALL_TYPE,
        format!("{}/{}", call.action.action_id, call.idempotency_key),
    );
    let mut meter = meter;
    for _attempt in 0..MAX_NAMING_SERIES_RETRIES {
        // A replay is resolved before reading the counter or ledger clock. It
        // therefore retains the original generated key after time or counter
        // state advances.
        let recorded = host.load(&record_key).map_err(host_failure)?;
        if let Some(record) = recorded_generated_call(
            &recorded,
            &call.action.action_id,
            object_type,
            &call.input,
            &call.actor,
            &call.idempotency_key,
        )? {
            validate_generated_replay_input(
                host,
                catalog,
                call,
                &mut preparation,
                authorizer,
                GeneratedReplay {
                    object_id_field,
                    record: &record,
                },
                limits,
            )?;
            return Ok(ActionOutcome::Replayed { record });
        }

        let base_values = catalog
            .effective_object_values(object_type, &preparation.effective_input)
            .map_err(|error| {
                ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                    reason: format!("the effective values for {}: {error}", object_type.as_str()),
                })
            })?;
        let scope = scope_from_values(declaration, &base_values)
            .map_err(|reason| ActionRefusal::Decision(DecisionRefusal::InvalidOutput { reason }))?;
        let candidate = prepare_candidate(
            host,
            catalog,
            object_type,
            series_owner,
            key_name,
            key_properties,
            declaration,
            &scope,
            call.turn.now.as_millis(),
        )
        .map_err(ActionRefusal::Decision)?;

        let mut generated_input = preparation.effective_input.clone();
        generated_input.insert(
            object_id_field.to_owned(),
            JsonValue::String(candidate.target.key.clone()),
        );
        validate_action_input::<H::Error>(call.action, &generated_input)?;
        let target = candidate.target.clone();
        let state = candidate.target_state.clone();
        let slice = load_action_slice(
            &ActionSliceContext {
                host,
                catalog,
                call,
                effective_input: &generated_input,
                authorizer,
            },
            &target,
            &state,
            SliceMeter::for_profile(&ResourceLimitProfile::norn_limits_v1()),
        )?;
        let mut receipt_evidence_bytes = preparation.receipt_evidence_bytes;
        let precondition_outcomes = check_action_preconditions(
            call.action,
            &generated_input,
            &slice.values,
            &mut preparation.remaining_fuel,
            &mut receipt_evidence_bytes,
            usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
        )?;
        let record = ActionCallRecord {
            action_id: call.action.action_id.clone(),
            action_artifact_id: Some(call.action_artifact_id.to_owned()),
            action_version: Some(call.action_version.to_owned()),
            effect_kind: Some("upsert_input_object".to_owned()),
            row_operations: Vec::new(),
            object: target.clone(),
            input: call.input.clone(),
            effective_input: Some(generated_input.clone()),
            derived_values: preparation.derived_values.clone(),
            precondition_outcomes,
            actor: call.actor.clone(),
            turn: call.turn.id.clone(),
            transition: String::new(),
            from: String::new(),
            to: String::new(),
        };
        check_action_record_size::<H::Error>(call.action, &record, limits)?;
        let changes = vec![FactChange::Create {
            object_type: object_type.clone(),
            key: target.key.clone(),
            values: generated_input.clone(),
        }];
        let pending = Pending {
            effective_input: generated_input,
            target,
            slice: slice.facts,
            reference_only: slice.reference_only,
            link_reads: slice.link_reads,
            precondition_reads: slice.precondition_keys,
            record_key: record_key.clone(),
            recorded,
            record,
            series_candidate: Some(candidate),
            remaining_fuel: preparation.remaining_fuel,
        };
        let outcome = commit_effect(
            host,
            catalog,
            call,
            authorizer,
            changes,
            pending,
            EnvelopePolicy {
                limits,
                meter: meter.as_deref_mut(),
            },
        );
        match outcome {
            Err(ActionRefusal::Decision(DecisionRefusal::Stale { .. })) => {}
            Err(refusal @ ActionRefusal::Decision(DecisionRefusal::NotPending { .. })) => {
                // A consumed delivery is not retried, but an exact concurrent
                // winner may have committed this call under the same key.
                let raced = host.load(&record_key).map_err(host_failure)?;
                if let Some(record) = recorded_generated_call(
                    &raced,
                    &call.action.action_id,
                    object_type,
                    &call.input,
                    &call.actor,
                    &call.idempotency_key,
                )? {
                    validate_generated_replay_input(
                        host,
                        catalog,
                        call,
                        &mut preparation,
                        authorizer,
                        GeneratedReplay {
                            object_id_field,
                            record: &record,
                        },
                        limits,
                    )?;
                    return Ok(ActionOutcome::Replayed { record });
                }
                return Err(refusal);
            }
            result => return result,
        }
    }
    Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
        reason: "naming-series allocation exceeded its 128 compare-and-swap retries".to_owned(),
    }))
}

#[derive(Clone, Copy)]
struct GeneratedReplay<'a> {
    object_id_field: &'a str,
    record: &'a ActionCallRecord,
}

fn validate_generated_replay_input<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    preparation: &mut ActionInputPreparation<'_>,
    authorizer: &dyn ActionAuthorizer,
    replay: GeneratedReplay<'_>,
    limits: &EnvelopeLimits,
) -> Result<(), ActionRefusal<H::Error>> {
    let mut input = preparation.effective_input.clone();
    input.insert(
        replay.object_id_field.to_owned(),
        JsonValue::String(replay.record.object.key.clone()),
    );
    validate_action_input::<H::Error>(call.action, &input)?;
    let state = authorize_target(host, call, &input, authorizer, &replay.record.object)?;
    let slice = load_action_slice(
        &ActionSliceContext {
            host,
            catalog,
            call,
            effective_input: &input,
            authorizer,
        },
        &replay.record.object,
        &state,
        SliceMeter::for_profile(&ResourceLimitProfile::norn_limits_v1()),
    )?;
    check_action_preconditions(
        call.action,
        &input,
        &slice.values,
        &mut preparation.remaining_fuel,
        &mut preparation.receipt_evidence_bytes,
        usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
    )?;
    Ok(())
}

fn recorded_generated_call<X>(
    recorded: &FactState,
    action_id: &str,
    object_type: &ObjectTypeName,
    input: &JsonObject,
    actor: &str,
    key: &str,
) -> Result<Option<ActionCallRecord>, ActionRefusal<X>> {
    if recorded.version == crate::cell_host::FactVersion::ABSENT {
        return Ok(None);
    }
    let Some(values) = &recorded.values else {
        return Err(ActionRefusal::IdempotencyConflict {
            key: key.to_owned(),
        });
    };
    let Some(record) = ActionCallRecord::from_values(values) else {
        return Err(ActionRefusal::IdempotencyConflict {
            key: key.to_owned(),
        });
    };
    if record.binds_same_generated_call(action_id, object_type, input, actor) {
        Ok(Some(record))
    } else {
        Err(ActionRefusal::IdempotencyConflict {
            key: key.to_owned(),
        })
    }
}

fn check_kind<X>(
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeName,
    object_kind: &str,
) -> Result<(), ActionRefusal<X>> {
    if catalog
        .lineage(object_type)
        .iter()
        .any(|ancestor| ancestor.as_str() == object_kind)
    {
        Ok(())
    } else {
        Err(ActionRefusal::KindMismatch {
            expected: object_kind.to_owned(),
            actual: object_type.as_str().to_owned(),
        })
    }
}

/// A call that passed every check up to the transition.
struct Pending {
    effective_input: JsonObject,
    remaining_fuel: u64,
    target: FactKey,
    slice: FactSlice,
    reference_only: bool,
    link_reads: Vec<LinkReadExpectation>,
    precondition_reads: BTreeSet<FactKey>,
    record_key: FactKey,
    recorded: FactState,
    record: ActionCallRecord,
    series_candidate: Option<SeriesCandidate>,
}

/// Per-decision limits and, when present, the shared aggregate turn meter.
struct EnvelopePolicy<'a> {
    limits: &'a EnvelopeLimits,
    meter: Option<&'a mut TurnEnvelopeMeter>,
}

fn host_failure<X>(error: X) -> ActionRefusal<X> {
    ActionRefusal::Decision(DecisionRefusal::Host(error))
}

/// Loads the target and authorizes the call; existence never leaks.
fn authorize_target<H: CellHost + ?Sized>(
    host: &H,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
) -> Result<FactState, ActionRefusal<H::Error>> {
    crate::check_fact_key_identity(target)
        .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
    let state = host.load(target).map_err(host_failure)?;
    authorize_current_target(call, effective_input, authorizer, target, &state)?;
    Ok(state)
}

/// Authorizes one snapshot of the committed target, preserving absence
/// concealment for current-state checks.
fn authorize_current_target<X>(
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
    state: &FactState,
) -> Result<(), ActionRefusal<X>> {
    let verdict = authorizer.authorize(&ActionAuthzRequest {
        action: call.action,
        scope: &call.scope,
        actor: &call.actor,
        input: effective_input,
        target,
        target_values: state.values.as_ref(),
    });
    match (verdict, state.exists()) {
        (Ok(()), true) => Ok(()),
        (Err(ActionDenial::Forbidden { reason }), true) => {
            Err(ActionRefusal::PermissionDenied { reason })
        }
        (Err(ActionDenial::Invisible), _)
        | (Ok(()) | Err(ActionDenial::Forbidden { .. }), false) => {
            Err(ActionRefusal::TargetNotFound)
        }
    }
}

/// Loads an upsert target and authorizes its current state when it exists.
/// A new create is authorized after its prepared values are available.
fn authorize_upsert_target<H: CellHost + ?Sized>(
    host: &H,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
) -> Result<FactState, ActionRefusal<H::Error>> {
    crate::check_fact_key_identity(target)
        .map_err(|error| ActionRefusal::Decision(DecisionRefusal::from_commit(error)))?;
    let state = host.load(target).map_err(host_failure)?;
    // A create has no resource state to inspect yet. Its identity/scope and
    // proposed-resource authorization is checked after the prepared write is
    // available, without sending an empty resource to the policy engine.
    if !state.exists() {
        return Ok(state);
    }
    authorize_current_target(call, effective_input, authorizer, target, &state)?;
    Ok(state)
}

/// Prepares an action's changes and commits them with the call record.
#[allow(
    clippy::too_many_lines,
    reason = "business changes, counter state, receipt, and race replay share one commit boundary"
)]
fn commit_effect<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    changes: Vec<FactChange>,
    pending: Pending,
    envelope: EnvelopePolicy<'_>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let EnvelopePolicy { limits, meter } = envelope;
    let Pending {
        effective_input,
        mut remaining_fuel,
        target,
        slice,
        reference_only,
        link_reads,
        precondition_reads,
        record_key,
        recorded,
        record,
        series_candidate,
    } = pending;
    let generated_key = series_candidate.is_some();
    let additional_events = usize::from(generated_key);
    let fact_event_count = changes
        .len()
        .checked_add(additional_events)
        .ok_or_else(|| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: "the action output event count overflowed".to_owned(),
            })
        })?;

    let mut counts =
        check_action_envelope::<H::Error>(limits, fact_event_count, call.action.commands.len())?;
    let commands = build_action_commands::<H::Error>(
        call.action,
        &effective_input,
        &format!("{}/{}", call.action.action_id, call.idempotency_key),
        &mut remaining_fuel,
    )?;
    let output = norn_ir::DecisionOutput {
        facts: changes,
        commands,
        timers: Vec::new(),
    };
    let mut prepared = prepare_decision_with_limits(catalog, &slice, &call.turn, &output, limits)?;
    let mut created_links = Vec::new();
    for binding in &call.action.create_links {
        let target_id = effective_input
            .get(&binding.target_id_field)
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ActionRefusal::InvalidInput {
                field: binding.target_id_field.clone(),
            })?;
        let linked_target = FactKey::new(&binding.target_type, target_id);
        if !slice.get(&linked_target).is_some_and(FactState::exists) {
            return Err(ActionRefusal::InvalidInput {
                field: binding.target_id_field.clone(),
            });
        }
        let key = LinkKey::new(target.clone(), &binding.link, linked_target);
        let expected = host.load_link(&key).map_err(host_failure)?.version;
        created_links.push(LinkWrite {
            key,
            expected,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        });
    }

    authorize_proposed_target(
        catalog,
        call,
        &effective_input,
        authorizer,
        &target,
        &mut prepared.writes,
    )?;
    let record = action_record_with_transition(record, &prepared.events);
    // Transition metadata is added after the earlier precondition/evidence
    // checks, so include the finalized receipt in the aggregate output bound.
    check_action_record_size::<H::Error>(call.action, &record, limits)?;
    let mut events = prepared.events;
    if let Some(candidate) = &series_candidate {
        events.push(candidate.event.clone());
    }
    let action_call = format!("{}/{}", call.action.action_id, call.idempotency_key);
    append_command_events(&mut events, &prepared.effects, &action_call);
    counts.events = events.len();
    check_decision_envelope(limits, counts).map_err(|exceeded| {
        ActionRefusal::Decision(DecisionRefusal::EnvelopeExceeded {
            kind: exceeded.kind,
            limit: exceeded.limit,
            actual: exceeded.actual,
        })
    })?;
    let mut writes = prepared.writes;
    let mut reads = prepared.reads;
    if let Some(candidate) = series_candidate {
        writes.push(candidate.counter_write);
        reads.extend(candidate.skipped_reads);
    }
    writes.push(FactWrite {
        key: record_key.clone(),
        expected: recorded.version,
        mutation: FactMutation::Put {
            values: record.to_values(),
        },
    });
    let commit = CellCommit {
        consumes: prepared.consumes,
        reads,
        writes,
        events: events.into_iter().map(Into::into).collect(),
        effects: prepared.effects.into_iter().map(widen_effect).collect(),
        links: created_links,
        link_reads,
    };
    check_action_turn_meter::<H::Error>(meter.as_deref(), counts)?;
    let quantity = super::action_quantity::prepare_quantity_action(call, &effective_input)?;
    // Precision granted after full snapshots were loaded cannot erase their
    // value-dependent admission. Revocation refuses before any write rather
    // than using empty values under newly dependent authorization.
    if reference_only && !authorizer.supports_action_reference_lineage(call.action) {
        return Err(ActionRefusal::TargetNotFound);
    }
    let reference_reads = (reference_only && !generated_key)
        .then(|| reference_read_proof(catalog, call.action, authorizer, &commit, &target))
        .flatten();
    let result = if let Some(request) = quantity {
        let identity = norn_ir::canonical::canonical_json_of(&(
            "action.quantity/1",
            &call.action.action_id,
            &call.idempotency_key,
        ))
        .map_err(|_encoding_error| ActionRefusal::InvalidInput {
            field: "idempotencyKey".to_owned(),
        })?;
        let key = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&identity);
        if let Some(proof) = reference_reads {
            host.commit_action_references(commit, proof, Some((&request, &key)))
        } else {
            host.commit_action_quantity(commit, &request, &key)
        }
    } else if let Some(proof) = reference_reads {
        host.commit_action_references(commit, proof, None)
    } else {
        host.commit(commit)
    };
    match result {
        Ok(receipt) => {
            if let Some(meter) = meter {
                meter.charge(counts);
            }
            Ok(ActionOutcome::Committed { receipt, record })
        }
        Err(error @ CommitError::InvariantViolation { .. }) => Err(ActionRefusal::Decision(
            ledger_action_invariant_refusal(host, call, error),
        )),
        Err(error) => action_commit_conflict(
            host,
            call,
            authorizer,
            error,
            &ActionReplayState {
                effective_input: &effective_input,
                record_key: &record_key,
                record: &record,
                precondition_reads: &precondition_reads,
                generated_key,
            },
        ),
    }
}

/// Kernel-owned attribution for direct reference validation. It cannot cross a
/// serialized commit boundary or be constructed by an action caller.
#[derive(Debug)]
pub struct ActionReferenceReads {
    reads: Vec<FactExpectation>,
    references: Vec<FactExpectation>,
    property_reads: Vec<(FactExpectation, Vec<String>)>,
    writes: Vec<FactWrite>,
    links: Vec<LinkWrite>,
    link_reads: Vec<LinkReadExpectation>,
    event_count: usize,
}

impl ActionReferenceReads {
    /// Exact-version references whose existence, rather than values, was used.
    #[must_use]
    pub fn reference_reads(&self) -> &[FactExpectation] {
        &self.references
    }

    /// Exact property operands proved by the kernel's expression analysis.
    /// Empty properties preserve an independent patch target's identity only.
    #[must_use]
    pub fn property_reads(&self) -> &[(FactExpectation, Vec<String>)] {
        &self.property_reads
    }

    /// Bind the one-use proof to the kernel-prepared transaction.
    #[must_use]
    pub fn validates<E, C, I>(&self, commit: &CellCommit<E, C, I>) -> bool {
        self.reads == commit.reads
            && self.writes == commit.writes
            && self.links == commit.links
            && self.link_reads == commit.link_reads
            && self.event_count == commit.events.len()
            && commit.effects.is_empty()
            && commit.consumes.is_none()
    }

    /// Add ordinary CAS/lineage guards without changing any authorized output.
    /// An overlapping guard restores whole-object lineage for that source.
    pub fn with_read_guards<E, C, I>(
        mut self,
        commit: &mut CellCommit<E, C, I>,
        guards: &[FactExpectation],
    ) -> Option<Self> {
        if !self.validates(commit) {
            return None;
        }
        for guard in guards {
            if let Some(previous) = commit.reads.iter().find(|read| read.key == guard.key) {
                if previous.expected != guard.expected {
                    return None;
                }
            } else if commit.writes.iter().any(|write| write.key == guard.key) {
                return None;
            } else {
                commit.reads.push(guard.clone());
            }
            self.references.retain(|read| read.key != guard.key);
            self.property_reads
                .retain(|(read, _)| read.key != guard.key);
        }
        self.reads.clone_from(&commit.reads);
        Some(self)
    }
}

fn reference_action(
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
) -> bool {
    // Closed creates use identity-only links. Patches preserve their complete
    // target while attributing exact expression operands. Unknown selectors,
    // optional references, commands and policy dependencies stay conservative.
    let Some(ActionEffectProgram::UpsertInputObject {
        object_kind,
        object_id_field,
        write_mode,
        ..
    }) = &action.effect_program
    else {
        return false;
    };
    let patch = matches!(write_mode, UpsertWriteMode::Upsert);
    if !authorizer.supports_action_reference_lineage(action)
        || !matches!(write_mode, UpsertWriteMode::CreateOnly | UpsertWriteMode::Upsert)
        || !action.commands.is_empty()
        || (!patch && !action.derive.is_empty())
        || action.slice.is_empty()
        // A patch reads the full target to preserve its other values. Only
        // caller-input assignments with a required direct target slice qualify.
        // Invariants and lifecycles may make admission depend on stored values.
        || (patch
            && (!action.create_links.is_empty()
                || action.quantity_effect.is_some()
                || !catalog.lifecycles(&target.object_type).is_empty()
                || catalog.ancestry(&target.object_type).iter().any(|ty| ty.accounting_leg.is_some())
                || patch_read_properties(catalog, action, target).is_none()))
        || action.slice.iter().any(|entry| {
            entry.optional
                || !matches!(&entry.key, SliceKey::Property { from, property } if from == "input" && if patch {
                    entry.object_type.as_str() == object_kind.as_str() && property.as_str() == object_id_field.as_str()
                } else {
                    action.create_links.iter().any(|link| link.target_type == entry.object_type.as_str() && link.target_id_field == property.as_str())
                })
        })
        || (!patch && action.preconditions.iter().any(|expression| {
            !norn_expr::expression_reads(&expression.0, "slice")
                .is_ok_and(|reads| !reads.whole_root && reads.fields.is_empty())
        }))
    {
        return false;
    }
    true
}

fn patch_read_properties(
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    target: &FactKey,
) -> Option<Vec<String>> {
    let effective = catalog.effective_type(&target.object_type)?;
    let mut properties = BTreeSet::new();
    // Derivations run before the slice is loaded and consume only caller
    // input and earlier derived values. No stored values enter that phase.
    for expression in &action.preconditions {
        for path in norn_expr::expression_read_paths(&expression.0, "slice").ok()? {
            let [alias, property] = path.as_slice() else {
                return None;
            };
            if !action.slice.iter().any(|entry| entry.name == *alias) {
                return None;
            }
            effective.property(property)?;
            properties.insert(property.clone());
        }
    }
    // Admission also evaluates effective invariants against the final object.
    // Their operands remain classified even when they are unrelated to an
    // action's explicit preconditions or supplied by the caller this time.
    for invariant in catalog.invariants(&target.object_type).ok()? {
        for path in
            norn_expr::expression_read_paths(invariant.invariant.expression.as_str(), "self")
                .ok()?
        {
            let [property] = path.as_slice() else {
                return None;
            };
            effective.property(property)?;
            properties.insert(property.clone());
        }
    }
    for property in &effective.properties {
        if !property.computed
            && matches!(
                &property.kind,
                norn_checker::EffectiveKind::Base(
                    norn_ir::PropertyType::Date {} | norn_ir::PropertyType::Timestamp { .. }
                )
            )
        {
            properties.insert(property.id.as_str().to_owned());
        }
    }
    Some(properties.into_iter().collect())
}

fn reference_read_proof<E, C, I>(
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    authorizer: &dyn ActionAuthorizer,
    commit: &CellCommit<E, C, I>,
    target: &FactKey,
) -> Option<ActionReferenceReads> {
    if !reference_action(catalog, action, authorizer, target)
        || !commit.effects.is_empty()
        || commit.consumes.is_some()
        || !commit.link_reads.is_empty()
        || commit
            .writes
            .iter()
            .any(|write| write.expected.0 != 0 && write.key != *target)
    {
        return None;
    }
    // The decider folds a written slice target's read into its write CAS.
    // Retain that exact reference as well as ordinary non-written reads.
    let mut references = commit.reads.clone();
    references.extend(
        commit
            .writes
            .iter()
            .filter(|write| write.expected.0 != 0)
            .map(|write| FactExpectation {
                key: write.key.clone(),
                expected: write.expected,
            }),
    );
    if references.is_empty() {
        return None;
    }
    let property_reads = if matches!(
        action.effect_program,
        Some(ActionEffectProgram::UpsertInputObject {
            write_mode: UpsertWriteMode::Upsert,
            ..
        })
    ) {
        let reference = references.iter().find(|read| read.key == *target)?.clone();
        vec![(reference, patch_read_properties(catalog, action, target)?)]
    } else {
        Vec::new()
    };
    Some(ActionReferenceReads {
        reads: commit.reads.clone(),
        references,
        property_reads,
        writes: commit.writes.clone(),
        links: commit.links.clone(),
        link_reads: commit.link_reads.clone(),
        event_count: commit.events.len(),
    })
}

fn action_record_with_transition(
    record: ActionCallRecord,
    events: &[FactEvent],
) -> ActionCallRecord {
    events
        .iter()
        .find_map(|event| match event {
            FactEvent::Transitioned { from, to, .. } => Some(ActionCallRecord {
                from: from.clone(),
                to: to.clone(),
                ..record.clone()
            }),
            FactEvent::CommandEnqueued { .. }
            | FactEvent::NamingSeriesAllocated { .. }
            | FactEvent::NamingSeriesCounterSeeded { .. }
            | FactEvent::Created { .. }
            | FactEvent::Set { .. }
            | FactEvent::Modified { .. }
            | FactEvent::Deleted { .. }
            | FactEvent::InvariantRefused { .. } => None,
        })
        .unwrap_or(record)
}

fn check_action_turn_meter<X>(
    meter: Option<&TurnEnvelopeMeter>,
    counts: EnvelopeCounts,
) -> Result<(), ActionRefusal<X>> {
    if let Some(meter) = meter {
        meter.check(counts).map_err(|exceeded| {
            ActionRefusal::Decision(DecisionRefusal::TurnEnvelopeExceeded {
                kind: exceeded.kind,
                limit: exceeded.limit,
                actual: exceeded.actual,
            })
        })?;
    }
    Ok(())
}

struct ActionReplayState<'a> {
    effective_input: &'a JsonObject,
    record_key: &'a FactKey,
    record: &'a ActionCallRecord,
    precondition_reads: &'a BTreeSet<FactKey>,
    generated_key: bool,
}

fn action_commit_conflict<H: CellHost + ?Sized>(
    host: &H,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    error: CommitError<H::Error>,
    state: &ActionReplayState<'_>,
) -> Result<ActionOutcome, ActionRefusal<H::Error>> {
    let ActionReplayState {
        effective_input,
        record_key,
        record,
        precondition_reads,
        generated_key,
    } = *state;
    if generated_key {
        // A generated candidate may lose the counter CAS to a different key.
        // The allocation loop reloads the call record and revalidates replay
        // against the winner's exact key before trying a new candidate.
        return Err(precondition_read_refusal(&error, precondition_reads)
            .unwrap_or_else(|| ActionRefusal::Decision(DecisionRefusal::from_commit(error))));
    }
    if matches!(
        &error,
        CommitError::Conflict { .. }
            | CommitError::LinkReadConflict { .. }
            | CommitError::UnsupportedLinkRead { .. }
            | CommitError::NotPending { .. }
    ) {
        // A concurrent call committed first (it changed the target,
        // or consumed the same delivery): replay it if it was this
        // same call, else report the original refusal.
        let raced = host.load(record_key).map_err(host_failure)?;
        if let Some(outcome) = replay(&raced, record, &call.idempotency_key)? {
            // The winner may have changed authorization-relevant
            // values. A replay must use current authorization too.
            authorize_target(host, call, effective_input, authorizer, &record.object)?;
            return Ok(outcome);
        }
    }
    Err(precondition_read_refusal(&error, precondition_reads)
        .unwrap_or_else(|| ActionRefusal::Decision(DecisionRefusal::from_commit(error))))
}

fn precondition_read_refusal<X>(
    error: &CommitError<X>,
    precondition_reads: &BTreeSet<FactKey>,
) -> Option<ActionRefusal<X>> {
    let stale_fact = match error {
        CommitError::Conflict { key, .. } if precondition_reads.contains(key) => Some(key.clone()),
        CommitError::Conflict { key, .. } => precondition_reads
            .iter()
            .find(|read| crate::snapshot_observe::tombstone_key(read) == *key)
            .cloned(),
        CommitError::OwnedRowsProtected { .. }
        | CommitError::DuplicateKey { .. }
        | CommitError::NotPending { .. }
        | CommitError::DuplicateLink { .. }
        | CommitError::IdentifierTooLong { .. }
        | CommitError::LinkConflict { .. }
        | CommitError::LinkReadConflict { .. }
        | CommitError::UnsupportedLinkRead { .. }
        | CommitError::DuplicateLinkRead { .. }
        | CommitError::LinkReadTooLarge { .. }
        | CommitError::InvalidLinkRead { .. }
        | CommitError::DanglingLink { .. }
        | CommitError::LinkTarget { .. }
        | CommitError::LinkCardinality { .. }
        | CommitError::LinkRequired { .. }
        | CommitError::TypeMismatch { .. }
        | CommitError::EffectiveValues { .. }
        | CommitError::TraversalLimitExceeded { .. }
        | CommitError::AccountingPosting { .. }
        | CommitError::InvariantViolation { .. }
        | CommitError::InvariantBudgetExceeded { .. }
        | CommitError::InvariantModel { .. }
        | CommitError::ObservationAuthorization
        | CommitError::EventLimitExceeded { .. }
        | CommitError::Overflow
        | CommitError::Host(_) => None,
    };
    let stale_link = match error {
        CommitError::LinkReadConflict { source_key, link }
        | CommitError::UnsupportedLinkRead { source_key, link } => {
            Some((source_key.clone(), link.clone()))
        }
        CommitError::OwnedRowsProtected { .. }
        | CommitError::DuplicateKey { .. }
        | CommitError::NotPending { .. }
        | CommitError::Conflict { .. }
        | CommitError::DuplicateLink { .. }
        | CommitError::IdentifierTooLong { .. }
        | CommitError::LinkConflict { .. }
        | CommitError::DuplicateLinkRead { .. }
        | CommitError::LinkReadTooLarge { .. }
        | CommitError::InvalidLinkRead { .. }
        | CommitError::DanglingLink { .. }
        | CommitError::LinkTarget { .. }
        | CommitError::LinkCardinality { .. }
        | CommitError::LinkRequired { .. }
        | CommitError::TypeMismatch { .. }
        | CommitError::EffectiveValues { .. }
        | CommitError::TraversalLimitExceeded { .. }
        | CommitError::AccountingPosting { .. }
        | CommitError::InvariantViolation { .. }
        | CommitError::InvariantBudgetExceeded { .. }
        | CommitError::InvariantModel { .. }
        | CommitError::ObservationAuthorization
        | CommitError::EventLimitExceeded { .. }
        | CommitError::Overflow
        | CommitError::Host(_) => None,
    };
    if let Some(key) = stale_fact {
        return Some(ActionRefusal::StalePreconditionRead { key, link: None });
    }
    stale_link.map(|(key, link)| ActionRefusal::StalePreconditionRead {
        key,
        link: Some(link),
    })
}

fn append_command_events(
    events: &mut Vec<FactEvent>,
    effects: &[crate::effect::Effect<JsonObject, JsonObject>],
    action_call: &str,
) {
    for effect in effects {
        if let crate::effect::Effect::Perform(command) = effect {
            events.push(FactEvent::CommandEnqueued {
                command_id: command.id.0.clone(),
                generation: command.generation.0,
                idempotency_key: command.idempotency_key.0.clone(),
                action_call: action_call.to_owned(),
                payload: command.payload.clone(),
            });
        }
    }
}

/// Canonicalizes and authorizes the target's proposed post-commit values.
/// The same canonical value map is retained in the write that will be
/// committed, so policy and storage see an identical resource.
fn authorize_proposed_target<X>(
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    target: &FactKey,
    writes: &mut [FactWrite],
) -> Result<(), ActionRefusal<X>> {
    let Some(write) = writes.iter_mut().find(|write| write.key == *target) else {
        return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("the prepared action does not write its target {target}"),
        }));
    };
    let FactMutation::Put { values } = &mut write.mutation else {
        return Err(ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("the prepared action does not put its target {target}"),
        }));
    };
    let canonical = catalog
        .effective_object_values(&target.object_type, values)
        .map_err(|error| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: format!("the effective values for {target}: {error}"),
            })
        })?;
    *values = canonical;
    match authorizer.authorize(&ActionAuthzRequest {
        action: call.action,
        scope: &call.scope,
        actor: &call.actor,
        input: effective_input,
        target,
        target_values: Some(values),
    }) {
        Ok(()) => Ok(()),
        // Invisibility continues to conceal the target, even for a create.
        // A policy denial is a real authorization refusal because the
        // proposed resource values are now available to decide against.
        Err(ActionDenial::Invisible) => Err(ActionRefusal::TargetNotFound),
        Err(ActionDenial::Forbidden { reason }) => Err(ActionRefusal::PermissionDenied { reason }),
    }
}

fn ledger_action_invariant_refusal<H>(
    host: &mut H,
    call: &ActionCall<'_>,
    error: CommitError<H::Error>,
) -> DecisionRefusal<H::Error>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
{
    let (key, owner, id, message, outcome) = match error {
        CommitError::InvariantViolation {
            key,
            owner,
            id,
            message,
            outcome,
        } => (key, owner, id, message, outcome),
        error @ (CommitError::OwnedRowsProtected { .. }
        | CommitError::DuplicateKey { .. }
        | CommitError::NotPending { .. }
        | CommitError::Conflict { .. }
        | CommitError::DuplicateLink { .. }
        | CommitError::IdentifierTooLong { .. }
        | CommitError::LinkConflict { .. }
        | CommitError::LinkReadConflict { .. }
        | CommitError::UnsupportedLinkRead { .. }
        | CommitError::DuplicateLinkRead { .. }
        | CommitError::LinkReadTooLarge { .. }
        | CommitError::InvalidLinkRead { .. }
        | CommitError::DanglingLink { .. }
        | CommitError::LinkTarget { .. }
        | CommitError::LinkCardinality { .. }
        | CommitError::LinkRequired { .. }
        | CommitError::TypeMismatch { .. }
        | CommitError::EffectiveValues { .. }
        | CommitError::TraversalLimitExceeded { .. }
        | CommitError::AccountingPosting { .. }
        | CommitError::InvariantBudgetExceeded { .. }
        | CommitError::InvariantModel { .. }
        | CommitError::ObservationAuthorization
        | CommitError::EventLimitExceeded { .. }
        | CommitError::Overflow
        | CommitError::Host(_)) => return DecisionRefusal::from_commit(error),
    };
    let refusal = DecisionRefusal::InvariantViolation {
        key: key.clone(),
        owner: owner.clone(),
        id: id.clone(),
        message: message.clone(),
        outcome: outcome.clone(),
    };
    let attempt_id = format!("{}/{}", call.action.action_id, call.idempotency_key);
    let mut refusal_commit = CellCommit::empty();
    refusal_commit.consumes.clone_from(&call.turn.consumes);
    refusal_commit.events.push(
        FactEvent::InvariantRefused {
            key: *key,
            owner,
            id,
            message,
            outcome: *outcome,
            attempt_id,
        }
        .into(),
    );
    match host.commit(refusal_commit) {
        Ok(_) => refusal,
        Err(error) => DecisionRefusal::from_commit(error),
    }
}

/// Checks the output envelope, including one event per committed command.
fn check_action_envelope<X>(
    limits: &EnvelopeLimits,
    fact_change_count: usize,
    command_count: usize,
) -> Result<EnvelopeCounts, ActionRefusal<X>> {
    let event_count = fact_change_count
        .checked_add(command_count)
        .ok_or_else(|| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: "the action output event count overflowed".to_owned(),
            })
        })?;
    let counts = EnvelopeCounts::new(event_count, command_count, 0);
    check_decision_envelope(limits, counts).map_err(|exceeded| {
        ActionRefusal::Decision(DecisionRefusal::EnvelopeExceeded {
            kind: exceeded.kind,
            limit: exceeded.limit,
            actual: exceeded.actual,
        })
    })?;
    Ok(counts)
}

/// Evaluates the declared outbound commands over the action input. The
/// expressions remain inert IR until this admitted action is actually
/// committed; the returned records are then converted by the shared decision
/// path into `perform` effects in the same commit as the transition.
fn build_action_commands<X>(
    action: &ActionRoot,
    input: &JsonObject,
    action_call: &str,
    remaining_fuel: &mut u64,
) -> Result<Vec<JsonObject>, ActionRefusal<X>> {
    if action.commands.is_empty() {
        return Ok(Vec::new());
    }
    let invalid = |reason: String| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("action command: {reason}"),
        })
    };
    let input = input
        .iter()
        .map(|(name, value)| expr_value(value).map(|value| (name.clone(), value)))
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(invalid)?;
    let context = ExprValue::object([("input", ExprValue::Object(input))]);
    action
        .commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let mut record = JsonObject::new();
            record.insert(
                "type".to_owned(),
                RuntimeValue::String("norn.connector.command/v1".to_owned()),
            );
            record.insert(
                "connector".to_owned(),
                RuntimeValue::String(command.connector.clone()),
            );
            let operation = action_command_operation_name(command.operation);
            record.insert(
                "operation".to_owned(),
                RuntimeValue::String(operation.to_owned()),
            );
            record.insert(
                "doctype".to_owned(),
                RuntimeValue::String(command.doctype.clone()),
            );
            if let Some(name) = &command.name {
                let value = evaluate_action_expression(name.as_str(), &context, remaining_fuel)
                    .map_err(&invalid)?;
                if !matches!(value, RuntimeValue::String(_)) {
                    return Err(invalid(format!(
                        "command {index} name must evaluate to a string"
                    )));
                }
                record.insert("name".to_owned(), value);
            } else if matches!(
                &command.operation,
                ActionCommandOperation::Submit
                    | ActionCommandOperation::Update
                    | ActionCommandOperation::Cancel
            ) {
                return Err(invalid(format!(
                    "command {index} requires a name expression"
                )));
            }
            if let Some(value) = evaluate_action_expected_modified(
                index,
                command
                    .expected_modified
                    .as_ref()
                    .map(norn_ir::ZenExpression::as_str),
                &context,
                remaining_fuel,
            )? {
                record.insert("expectedModified".to_owned(), value);
            }
            let mut fields = JsonObject::new();
            for (field, expression) in &command.fields {
                fields.insert(
                    field.clone(),
                    evaluate_action_expression(expression.as_str(), &context, remaining_fuel)
                        .map_err(&invalid)?,
                );
            }
            record.insert("fields".to_owned(), RuntimeValue::Object(fields));
            if let Some(child_merges) =
                build_action_child_merges(index, command, &context, remaining_fuel)?
            {
                record.insert("childMerges".to_owned(), child_merges);
            }
            record.insert(
                "actionId".to_owned(),
                RuntimeValue::String(action.action_id.clone()),
            );
            record.insert(
                "actionCall".to_owned(),
                RuntimeValue::String(action_call.to_owned()),
            );
            // The normal decision commit derives the command id and external
            // idempotency key from the action turn and this stable index.
            Ok(record)
        })
        .collect()
}

fn action_command_operation_name(operation: ActionCommandOperation) -> &'static str {
    match operation {
        ActionCommandOperation::Submit => "submit",
        ActionCommandOperation::Create => "create",
        ActionCommandOperation::Update => "update",
        ActionCommandOperation::Cancel => "cancel",
    }
}

/// Evaluates a command's guarded source version, when present.
fn evaluate_action_expected_modified<X>(
    command_index: usize,
    source: Option<&str>,
    context: &ExprValue,
    remaining_fuel: &mut u64,
) -> Result<Option<RuntimeValue>, ActionRefusal<X>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let invalid = |reason: String| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("action command: {reason}"),
        })
    };
    let value = evaluate_action_expression(source, context, remaining_fuel).map_err(&invalid)?;
    if !matches!(&value, RuntimeValue::String(version) if !version.trim().is_empty()) {
        return Err(invalid(format!(
            "command {command_index} expectedModified must evaluate to non-empty text"
        )));
    }
    Ok(Some(value))
}

/// Evaluates and encodes the child-table merge declarations for one command.
fn build_action_child_merges<X>(
    command_index: usize,
    command: &ActionCommand,
    context: &ExprValue,
    remaining_fuel: &mut u64,
) -> Result<Option<RuntimeValue>, ActionRefusal<X>> {
    if command.child_merges.is_empty() {
        return Ok(None);
    }
    let invalid = |reason: String| {
        ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
            reason: format!("action command: {reason}"),
        })
    };
    let mut child_merges = Vec::with_capacity(command.child_merges.len());
    for merge in &command.child_merges {
        let ownership_value =
            evaluate_action_expression(merge.ownership_value.as_str(), context, remaining_fuel)
                .map_err(&invalid)?;
        if !matches!(
            &ownership_value,
            RuntimeValue::String(owner) if !owner.trim().is_empty()
        ) {
            return Err(invalid(format!(
                "command {command_index} child merge {} ownershipValue must be non-empty text",
                merge.field
            )));
        }
        let rows = evaluate_action_expression(merge.rows.as_str(), context, remaining_fuel)
            .map_err(&invalid)?;
        let RuntimeValue::Array(rows) = rows else {
            return Err(invalid(format!(
                "command {command_index} child merge {} rows must evaluate to an array",
                merge.field
            )));
        };
        if rows
            .iter()
            .any(|row| !matches!(row, RuntimeValue::Object(_)))
        {
            return Err(invalid(format!(
                "command {command_index} child merge {} rows must contain only objects",
                merge.field
            )));
        }
        child_merges.push(RuntimeValue::Object(JsonObject::from([
            (
                "field".to_owned(),
                RuntimeValue::String(merge.field.clone()),
            ),
            (
                "ownershipField".to_owned(),
                RuntimeValue::String(merge.ownership_field.clone()),
            ),
            ("ownershipValue".to_owned(), ownership_value),
            (
                "rowKeyField".to_owned(),
                RuntimeValue::String(merge.row_key_field.clone()),
            ),
            ("rows".to_owned(), RuntimeValue::Array(rows)),
        ])));
    }
    Ok(Some(RuntimeValue::Array(child_merges)))
}

/// Evaluates one action command expression with the input under `input`.
fn evaluate_action_expression(
    source: &str,
    context: &ExprValue,
    remaining_fuel: &mut u64,
) -> Result<RuntimeValue, String> {
    let expression = Expression::compile(source).map_err(|error| error.to_string())?;
    let attempt = expression.evaluate_metered(context, EvaluationBudget::new(*remaining_fuel));
    *remaining_fuel = (*remaining_fuel).saturating_sub(attempt.units_used);
    let evaluated = attempt.result.map_err(|error| error.to_string())?;
    expression_value_to_runtime(evaluated.value())
}

/// Checks the action input against the closed schema admitted on its action
/// artifact. The public action route is one caller, but this check stays in
/// the kernel so direct `commit_action` callers cannot smuggle undeclared
/// properties into an upsert.
fn validate_action_input<X>(
    action: &ActionRoot,
    input: &JsonObject,
) -> Result<(), ActionRefusal<X>> {
    let schema = norn_ir::JsonValue::Object(action.input_schema.clone());
    let value = norn_expr::Value::try_from(&RuntimeValue::Object(input.clone())).map_err(
        |_conversion| ActionRefusal::InvalidInput {
            field: "$input".to_owned(),
        },
    )?;
    let parsed =
        norn_expr::schema::Schema::parse_action(&schema, "$input").map_err(|violation| {
            ActionRefusal::InvalidInput {
                field: violation.path,
            }
        })?;
    parsed
        .check(&value, "$input")
        .map_err(|violation| ActionRefusal::InvalidInput {
            field: violation.path,
        })
}

/// Runs declarative input derivations in order, then validates the exact
/// effective input that authorization, preconditions, and writes will use.
/// This validation-only token is not bound to an action artifact version and
/// is refused by [`commit_action_with_preparation`]. The caller's object
/// remains untouched for idempotency and delegation binding.
///
/// # Errors
///
/// Refuses missing required input, invalid derived values or final input,
/// expression failures, and input or receipt size limits.
pub fn prepare_action_input<'a, X>(
    action: &'a ActionRoot,
    raw_input: &'a JsonObject,
) -> Result<ActionInputPreparation<'a>, ActionRefusal<X>> {
    prepare_action_input_inner(action, raw_input, None, None)
}

/// Prepares one action call's input, binding the opaque result to the action
/// artifact version and exact raw input that the kernel will commit.
///
/// # Errors
///
/// Refuses missing required input, invalid derived values or final input,
/// expression failures, and input or receipt size limits.
pub fn prepare_action_input_for_call<'a, X>(
    action: &'a ActionRoot,
    raw_input: &'a JsonObject,
    action_artifact_id: &'a str,
    action_version: &'a str,
) -> Result<ActionInputPreparation<'a>, ActionRefusal<X>> {
    prepare_action_input_inner(
        action,
        raw_input,
        Some(action_artifact_id),
        Some(action_version),
    )
}

fn prepare_action_input_inner<'a, X>(
    action: &'a ActionRoot,
    raw_input: &'a JsonObject,
    action_artifact_id: Option<&'a str>,
    action_version: Option<&'a str>,
) -> Result<ActionInputPreparation<'a>, ActionRefusal<X>> {
    let profile = ResourceLimitProfile::norn_limits_v1();
    if !has_action_preparation_features(action) {
        validate_action_input(action, raw_input)?;
        return Ok(ActionInputPreparation {
            action,
            raw_input,
            action_artifact_id,
            action_version,
            effective_input: raw_input.clone(),
            derived_values: Vec::new(),
            remaining_fuel: profile.turn.fuel_units,
            receipt_evidence_bytes: 0,
            precondition_outcomes: Vec::new(),
        });
    }
    let max_input_bytes = usize::try_from(profile.slice.max_canonical_bytes).unwrap_or(usize::MAX);
    let max_receipt_bytes =
        usize::try_from(profile.envelope.max_output_bytes).unwrap_or(usize::MAX);
    check_canonical_object_bound::<X>("caller input", raw_input, max_input_bytes)?;
    let raw_bytes = canonical_object_bytes::<X>(raw_input)?;
    if raw_bytes > max_receipt_bytes {
        return Err(ActionRefusal::InputBoundExceeded {
            subject: "action call receipt input".to_owned(),
            actual_bytes: raw_bytes,
            max_bytes: max_receipt_bytes,
        });
    }
    let mut effective_input = raw_input.clone();
    for derivation in &action.derive {
        if derivation.input_required && !raw_input.contains_key(&derivation.field) {
            return Err(ActionRefusal::InvalidInput {
                field: derivation.field.clone(),
            });
        }
        if !derivation.input_required && !effective_input.contains_key(&derivation.field) {
            effective_input.insert(derivation.field.clone(), RuntimeValue::Null);
        }
    }
    check_canonical_object_bound::<X>("effective input", &effective_input, max_input_bytes)?;
    let effective_bytes = if action.derive.is_empty() {
        0 // The receipt retains only raw input when no derivation ran.
    } else {
        canonical_object_bytes::<X>(&effective_input)?
    };
    let mut receipt_evidence_bytes = raw_bytes
        .saturating_add(effective_bytes)
        .saturating_add(128);
    if receipt_evidence_bytes > max_receipt_bytes {
        return Err(ActionRefusal::InputBoundExceeded {
            subject: "action call receipt evidence".to_owned(),
            actual_bytes: receipt_evidence_bytes,
            max_bytes: max_receipt_bytes,
        });
    }
    let mut derived_values = Vec::with_capacity(action.derive.len());
    let mut remaining_fuel = profile.turn.fuel_units;

    for derivation in &action.derive {
        let (value, next_receipt_bytes) = prepare_action_derivation(
            derivation,
            &effective_input,
            &mut remaining_fuel,
            receipt_evidence_bytes,
        )?;
        receipt_evidence_bytes = next_receipt_bytes;
        if receipt_evidence_bytes > max_receipt_bytes {
            return Err(ActionRefusal::InputBoundExceeded {
                subject: "action call receipt evidence".to_owned(),
                actual_bytes: receipt_evidence_bytes,
                max_bytes: max_receipt_bytes,
            });
        }
        effective_input.insert(derivation.field.clone(), value.clone());
        derived_values.push(ActionDerivedValue {
            field: derivation.field.clone(),
            value,
        });
        check_canonical_object_bound::<X>("effective input", &effective_input, max_input_bytes)?;
    }

    validate_action_input(action, &effective_input)?;
    Ok(ActionInputPreparation {
        action,
        raw_input,
        action_artifact_id,
        action_version,
        effective_input,
        derived_values,
        remaining_fuel,
        receipt_evidence_bytes,
        precondition_outcomes: Vec::new(),
    })
}

fn prepare_action_derivation<X>(
    derivation: &norn_ir::ActionInputDerivation,
    effective_input: &JsonObject,
    remaining_fuel: &mut u64,
    receipt_evidence_bytes: usize,
) -> Result<(RuntimeValue, usize), ActionRefusal<X>> {
    let context =
        action_expression_context(effective_input, &JsonObject::new()).map_err(|reason| {
            ActionRefusal::DerivationFailed {
                field: derivation.field.clone(),
                expression: derivation.expression.as_str().to_owned(),
                reason,
            }
        })?;
    let expression = Expression::compile(derivation.expression.as_str()).map_err(|error| {
        ActionRefusal::DerivationFailed {
            field: derivation.field.clone(),
            expression: derivation.expression.as_str().to_owned(),
            reason: error.to_string(),
        }
    })?;
    let attempt = expression.evaluate_metered(&context, EvaluationBudget::new(*remaining_fuel));
    *remaining_fuel = remaining_fuel.saturating_sub(attempt.units_used);
    let evaluated = attempt
        .result
        .map_err(|error| ActionRefusal::DerivationFailed {
            field: derivation.field.clone(),
            expression: derivation.expression.as_str().to_owned(),
            reason: error.to_string(),
        })?;
    let value = expression_value_to_runtime(evaluated.value()).map_err(|reason| {
        ActionRefusal::DerivationFailed {
            field: derivation.field.clone(),
            expression: derivation.expression.as_str().to_owned(),
            reason,
        }
    })?;
    let encoded = norn_ir::canonical::canonical_json_of(&ActionDerivedValueRef {
        field: &derivation.field,
        value: &value,
    })
    .map_err(|error| ActionRefusal::DerivationFailed {
        field: derivation.field.clone(),
        expression: derivation.expression.as_str().to_owned(),
        reason: error.to_string(),
    })?;
    let value_bytes = norn_ir::canonical::canonical_json_of(&value)
        .map_err(|error| ActionRefusal::DerivationFailed {
            field: derivation.field.clone(),
            expression: derivation.expression.as_str().to_owned(),
            reason: error.to_string(),
        })?
        .len();
    let effective_growth = if let Some(previous) = effective_input.get(&derivation.field) {
        let previous_bytes = norn_ir::canonical::canonical_json_of(previous)
            .map_err(|error| ActionRefusal::DerivationFailed {
                field: derivation.field.clone(),
                expression: derivation.expression.as_str().to_owned(),
                reason: error.to_string(),
            })?
            .len();
        (
            value_bytes >= previous_bytes,
            value_bytes.abs_diff(previous_bytes),
        )
    } else {
        let field_bytes = norn_ir::canonical::canonical_json_of(&derivation.field)
            .map_err(|error| ActionRefusal::DerivationFailed {
                field: derivation.field.clone(),
                expression: derivation.expression.as_str().to_owned(),
                reason: error.to_string(),
            })?
            .len();
        (
            true,
            field_bytes
                .saturating_add(1)
                .saturating_add(value_bytes)
                .saturating_add(usize::from(!effective_input.is_empty())),
        )
    };
    let receipt_evidence_bytes = if effective_growth.0 {
        receipt_evidence_bytes.saturating_add(effective_growth.1)
    } else {
        receipt_evidence_bytes.saturating_sub(effective_growth.1)
    }
    .saturating_add(encoded.len())
    .saturating_add(1);
    Ok((value, receipt_evidence_bytes))
}

fn canonical_object_bytes<X>(object: &JsonObject) -> Result<usize, ActionRefusal<X>> {
    norn_ir::canonical::canonical_json_of(object)
        .map(|bytes| bytes.len())
        .map_err(|error| ActionRefusal::DerivationFailed {
            field: "$input".to_owned(),
            expression: String::new(),
            reason: error.to_string(),
        })
}

fn check_canonical_object_bound<X>(
    subject: &str,
    object: &JsonObject,
    max_bytes: usize,
) -> Result<(), ActionRefusal<X>> {
    let actual_bytes = canonical_object_bytes::<X>(object)?;
    if actual_bytes > max_bytes {
        return Err(ActionRefusal::InputBoundExceeded {
            subject: subject.to_owned(),
            actual_bytes,
            max_bytes,
        });
    }
    Ok(())
}

fn action_expression_context(input: &JsonObject, slice: &JsonObject) -> Result<ExprValue, String> {
    let mut context = expression_object(input)?
        .into_iter()
        .filter(|(name, _)| name != "input" && name != "slice")
        .collect::<BTreeMap<_, _>>();
    let input = expression_object(input)?;
    let slice = expression_object(slice)?;
    context.insert("input".to_owned(), ExprValue::Object(input));
    context.insert("slice".to_owned(), ExprValue::Object(slice));
    Ok(ExprValue::Object(context))
}

fn expression_object(values: &JsonObject) -> Result<BTreeMap<String, ExprValue>, String> {
    values
        .iter()
        .map(|(name, value)| expr_value(value).map(|value| (name.clone(), value)))
        .collect()
}

fn has_action_preparation_features(action: &ActionRoot) -> bool {
    !action.derive.is_empty() || !action.slice.is_empty() || !action.preconditions.is_empty()
}

fn check_action_record_size<X>(
    action: &ActionRoot,
    record: &ActionCallRecord,
    limits: &EnvelopeLimits,
) -> Result<(), ActionRefusal<X>> {
    // Existing actions without the new preparation phases retain their
    // admitted input/receipt behavior, including command-only actions.
    if !has_action_preparation_features(action)
        && !matches!(
            action.effect_program,
            Some(ActionEffectProgram::MutateParentWithRows { .. })
        )
    {
        return Ok(());
    }
    let values = RuntimeValue::Object(record.to_values());
    let actual_bytes = norn_ir::canonical::canonical_json_of(&values)
        .map_err(|_error| ActionRefusal::InputBoundExceeded {
            subject: "action call receipt".to_owned(),
            actual_bytes: usize::MAX,
            max_bytes: usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX),
        })?
        .len();
    let max_bytes = usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX);
    if actual_bytes > max_bytes {
        return Err(ActionRefusal::InputBoundExceeded {
            subject: "action call receipt".to_owned(),
            actual_bytes,
            max_bytes,
        });
    }
    Ok(())
}

/// Converts an expression value without routing exact decimals through a
/// binary JSON number.
fn expression_value_to_runtime(value: &ExprValue) -> Result<RuntimeValue, String> {
    Ok(match value {
        ExprValue::Null => RuntimeValue::Null,
        ExprValue::Bool(flag) => RuntimeValue::Bool(*flag),
        ExprValue::Number(number) => RuntimeValue::Number(
            Decimal::parse(number.as_str()).map_err(|error| error.to_string())?,
        ),
        ExprValue::String(text) => RuntimeValue::String(text.clone()),
        ExprValue::Array(values) => RuntimeValue::Array(
            values
                .iter()
                .map(expression_value_to_runtime)
                .collect::<Result<_, _>>()?,
        ),
        ExprValue::Object(values) => RuntimeValue::Object(
            values
                .iter()
                .map(|(name, value)| {
                    expression_value_to_runtime(value).map(|value| (name.clone(), value))
                })
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// Command identity and scope fail-closed, then every required scope key.
fn check_scope<X>(call: &ActionCall<'_>) -> Result<(), ActionRefusal<X>> {
    check_action_command(&ActionCommandShape {
        org_id: call.scope.get("orgId").map(String::as_str),
        action_id: &call.action.action_id,
        action_version: Some(call.action_version),
        idempotency_key: &call.idempotency_key,
    })
    .map_err(ActionRefusal::Command)?;
    if call.action_artifact_id.trim().is_empty() {
        return Err(ActionRefusal::Command(ActionCommandRejection::BlankField));
    }
    for key in &call.action.scope_required {
        if call
            .scope
            .get(key)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(ActionRefusal::MissingScope { key: key.clone() });
        }
    }
    Ok(())
}

/// The facts, link sets, and expression values read by one action call.
struct LoadedActionSlice {
    facts: FactSlice,
    reference_only: bool,
    values: JsonObject,
    link_reads: Vec<LinkReadExpectation>,
    precondition_keys: BTreeSet<FactKey>,
}

struct ActionSliceContext<'a, 'model, 'call, H: CellHost + ?Sized> {
    host: &'a H,
    catalog: &'a TypeCatalog<'model>,
    call: &'a ActionCall<'call>,
    effective_input: &'a JsonObject,
    authorizer: &'a dyn ActionAuthorizer,
}

struct ActionSliceState {
    facts: FactSlice,
    values: JsonObject,
    link_reads: Vec<LinkReadExpectation>,
    precondition_keys: BTreeSet<FactKey>,
    entry_keys: BTreeMap<String, Option<FactKey>>,
    entry_depths: BTreeMap<String, u32>,
    observed_records: u32,
    meter: SliceMeter,
    read_types: BTreeSet<ObjectTypeName>,
    limits: crate::SliceLimits,
}

fn action_slice_key<X>(
    entry: &SliceDeclaration,
    effective_input: &JsonObject,
    values: &JsonObject,
    entry_depths: &BTreeMap<String, u32>,
) -> Result<(u32, Option<RuntimeValue>), ActionRefusal<X>> {
    Ok(match &entry.key {
        SliceKey::Property { from, property } if from == "input" => {
            let depth = 0;
            let value = effective_input.get(property.as_str()).cloned();
            (depth, value)
        }
        SliceKey::Property { from, property } => {
            let depth = entry_depths
                .get(from)
                .copied()
                .ok_or_else(|| ActionRefusal::SliceUnavailable {
                    entry: entry.name.clone(),
                    reason: format!("source entry {from:?} has no readable depth"),
                })?
                .saturating_add(1);
            let key = values
                .get(from)
                .and_then(RuntimeValue::as_object)
                .and_then(|object| object.get(property.as_str()))
                .cloned();
            (depth, key)
        }
        SliceKey::Link { from, .. } => {
            let depth = entry_depths
                .get(from)
                .copied()
                .ok_or_else(|| ActionRefusal::SliceUnavailable {
                    entry: entry.name.clone(),
                    reason: format!("link source entry {from:?} has no readable depth"),
                })?
                .saturating_add(1);
            (depth, None)
        }
        SliceKey::TriggerObject
        | SliceKey::TriggerBefore
        | SliceKey::TriggerAfter
        | SliceKey::TriggerChanges => {
            return Err(ActionRefusal::SliceUnavailable {
                entry: entry.name.clone(),
                reason: "action slices cannot reference rule trigger keys".to_owned(),
            });
        }
    })
}

fn load_action_slice_entry<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    state: &mut ActionSliceState,
    entry: &SliceDeclaration,
    reference_only: bool,
) -> Result<(), ActionRefusal<H::Error>> {
    let ActionSliceContext {
        host,
        catalog,
        call,
        effective_input,
        authorizer,
    } = *context;
    let (depth, key) =
        action_slice_key(entry, effective_input, &state.values, &state.entry_depths)?;
    if matches!(entry.key, SliceKey::Link { .. }) {
        return load_action_slice_links(context, state, entry, depth);
    }
    let ActionSliceState {
        facts,
        values,
        precondition_keys,
        entry_keys,
        entry_depths,
        observed_records,
        meter,
        read_types,
        limits,
        ..
    } = state;

    let Some(key) = key else {
        if entry.optional {
            entry_keys.insert(entry.name.clone(), None);
            entry_depths.insert(entry.name.clone(), depth);
            return Ok(());
        }
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry.name.clone(),
            reason: "the key property is absent".to_owned(),
        });
    };
    let key = match key {
        RuntimeValue::Null if entry.optional => {
            entry_keys.insert(entry.name.clone(), None);
            entry_depths.insert(entry.name.clone(), depth);
            return Ok(());
        }
        RuntimeValue::String(key) if !key.trim().is_empty() => key,
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Number(_)
        | RuntimeValue::String(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => {
            return Err(ActionRefusal::SliceUnavailable {
                entry: entry.name.clone(),
                reason: "the key property is not a non-blank string".to_owned(),
            });
        }
    };
    let requested = FactKey::new(entry.object_type.as_str(), key);
    if read_types.insert(entry.object_type.clone()) {
        authorize_slice_type(call, effective_input, authorizer, &entry.object_type)?;
    }
    let state = load_action_fact(
        host,
        catalog,
        call,
        effective_input,
        authorizer,
        &entry.name,
        &entry.object_type,
        &requested,
        depth,
        facts,
        meter,
        observed_records,
        limits,
        read_types,
        reference_only,
    )?;
    precondition_keys.insert(known_fact_key(facts, catalog, &requested));
    let Some(object) = state.values.as_ref() else {
        return Err(ActionRefusal::SliceObjectMissing {
            entry: entry.name.clone(),
        });
    };
    let concrete_key = facts
        .keys()
        .find(|fact| {
            fact.key == requested.key
                && catalog
                    .lineage(&fact.object_type)
                    .contains(&entry.object_type)
        })
        .cloned()
        .unwrap_or(requested);
    entry_keys.insert(entry.name.clone(), Some(concrete_key));
    entry_depths.insert(entry.name.clone(), depth);
    values.insert(entry.name.clone(), RuntimeValue::Object(object.clone()));
    Ok(())
}

fn load_action_slice_links<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    state: &mut ActionSliceState,
    entry: &SliceDeclaration,
    depth: u32,
) -> Result<(), ActionRefusal<H::Error>> {
    let ActionSliceContext {
        host,
        catalog,
        call,
        effective_input,
        authorizer,
    } = *context;
    let ActionSliceState {
        facts,
        values,
        link_reads,
        precondition_keys,
        entry_keys,
        entry_depths,
        observed_records,
        meter,
        read_types,
        limits,
    } = state;
    let SliceKey::Link { from, link } = &entry.key else {
        return Ok(());
    };
    let Some(source_key) = entry_keys.get(from).and_then(Option::as_ref) else {
        // A list sourced from an omitted optional entry is an empty
        // list; there is no existing source whose adjacency can change.
        values.insert(entry.name.clone(), RuntimeValue::Array(Vec::new()));
        entry_keys.insert(entry.name.clone(), None);
        entry_depths.insert(entry.name.clone(), depth);
        return Ok(());
    };
    if read_types.insert(entry.object_type.clone()) {
        authorize_slice_type(call, effective_input, authorizer, &entry.object_type)?;
    }
    let targets = action_link_snapshot(
        context,
        source_key,
        link,
        &entry.name,
        limits,
        *observed_records,
        meter.bytes(),
    )?;
    let mut items = Vec::with_capacity(targets.len());
    for target in &targets {
        if !catalog
            .lineage(&target.key.target.object_type)
            .contains(&entry.object_type)
        {
            return Err(ActionRefusal::SliceUnavailable {
                entry: entry.name.clone(),
                reason: "a link target has a type outside the admitted target type".to_owned(),
            });
        }
        let state = load_action_fact(
            host,
            catalog,
            call,
            effective_input,
            authorizer,
            &entry.name,
            &entry.object_type,
            &target.key.target,
            depth,
            facts,
            meter,
            observed_records,
            limits,
            read_types,
            false,
        )?;
        precondition_keys.insert(known_fact_key(facts, catalog, &target.key.target));
        let Some(object) = state.values.as_ref() else {
            return Err(ActionRefusal::SliceObjectMissing {
                entry: entry.name.clone(),
            });
        };
        items.push(RuntimeValue::Object(object.clone()));
    }
    record_action_link_read(link_reads, source_key, link, targets)?;
    values.insert(entry.name.clone(), RuntimeValue::Array(items));
    entry_keys.insert(entry.name.clone(), None);
    entry_depths.insert(entry.name.clone(), depth);
    Ok(())
}

fn record_action_link_read<X>(
    link_reads: &mut Vec<LinkReadExpectation>,
    source_key: &FactKey,
    link: &str,
    targets: Vec<crate::cell_host::LinkReadTarget>,
) -> Result<(), ActionRefusal<X>> {
    if let Some(previous) = link_reads
        .iter()
        .find(|read: &&LinkReadExpectation| read.source == *source_key && read.link == link)
    {
        if previous.targets != targets {
            return Err(ActionRefusal::StalePreconditionRead {
                key: source_key.clone(),
                link: Some(link.to_owned()),
            });
        }
    } else {
        link_reads.push(LinkReadExpectation {
            source: source_key.clone(),
            link: link.to_owned(),
            targets,
        });
    }
    Ok(())
}

fn action_link_snapshot<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    source_key: &FactKey,
    link: &str,
    entry_name: &str,
    limits: &crate::SliceLimits,
    observed_records: u32,
    read_bytes: u64,
) -> Result<Vec<crate::cell_host::LinkReadTarget>, ActionRefusal<H::Error>> {
    let host = context.host;
    let catalog = context.catalog;
    let max_records = usize::try_from(limits.max_records).unwrap_or(usize::MAX);
    let remaining_records =
        max_records.saturating_sub(usize::try_from(observed_records).unwrap_or(usize::MAX));
    let max_targets = remaining_records.saturating_add(1);
    let max_key_bytes = limits.max_canonical_bytes.saturating_sub(read_bytes);
    let targets = host
        .read_link_targets(source_key, link, max_targets, max_key_bytes)
        .map_err(host_failure)?
        .ok_or_else(|| ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: "the host cannot provide a complete bounded link snapshot".to_owned(),
        })?;
    if targets.len() > remaining_records {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: format!(
                "link target count exceeds the remaining {remaining_records} slice records"
            ),
        });
    }
    let model = crate::CommitModel::new(catalog);
    let invalid_order = targets.windows(2).any(|pair| {
        matches!(pair, [left, right] if model.canonical_link(&left.key) >= model.canonical_link(&right.key))
    });
    let invalid_identity = targets
        .iter()
        .any(|target| target.key.source != *source_key || target.key.link != link);
    if invalid_order || invalid_identity {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: "the host returned a non-canonical link target snapshot".to_owned(),
        });
    }
    Ok(targets)
}

/// Loads and authorizes the action's declared slice under the resource profile.
/// Every distinct fact snapshot is retained once, and duplicate aliases must
/// observe the same version and values.
fn load_action_slice<H: CellHost + ?Sized>(
    context: &ActionSliceContext<'_, '_, '_, H>,
    action_target: &FactKey,
    target_state: &FactState,
    meter: SliceMeter,
) -> Result<LoadedActionSlice, ActionRefusal<H::Error>> {
    let catalog = context.catalog;
    let call = context.call;
    let limits = ResourceLimitProfile::norn_limits_v1().slice;
    let reserved_bytes = meter.bytes();
    let reference_only = context.host.supports_reference_reads()
        && reference_action(catalog, call.action, context.authorizer, action_target);
    // Keep the separately authorized target snapshot out of the slice cache
    // until declared entries have passed their read-type gates. If an entry
    // aliases the target, it must be loaded through the same bounded read and
    // object authorization path as every other slice fact.
    let facts = FactSlice::new();
    let values = JsonObject::new();
    let link_reads = Vec::new();
    let precondition_keys = BTreeSet::new();
    let entry_keys = BTreeMap::<String, Option<FactKey>>::new();
    let entry_depths = BTreeMap::<String, u32>::new();
    let observed_records = meter.records();
    let read_types = BTreeSet::<ObjectTypeName>::new();
    let mut state = ActionSliceState {
        facts,
        values,
        link_reads,
        precondition_keys,
        entry_keys,
        entry_depths,
        observed_records,
        meter,
        read_types,
        limits,
    };
    for entry in &call.action.slice {
        // A precise patch still loads the complete target to preserve its
        // unmodified fields. The closed effect never consults those values
        // for a predicate or derives a new value from them.
        let identity_only = reference_only
            && matches!(
                call.action.effect_program,
                Some(ActionEffectProgram::UpsertInputObject {
                    write_mode: UpsertWriteMode::CreateOnly,
                    ..
                })
            );
        load_action_slice_entry(context, &mut state, entry, identity_only)?;
    }
    let ActionSliceState {
        mut facts,
        values,
        link_reads,
        precondition_keys,
        limits,
        ..
    } = state;
    reconcile_action_target_snapshot::<H::Error>(catalog, &mut facts, action_target, target_state)?;

    let actual_bytes = norn_ir::canonical::canonical_json_of(&values)
        .map_err(|error| ActionRefusal::SliceUnavailable {
            entry: "$slice".to_owned(),
            reason: error.to_string(),
        })?
        .len();
    // An undeclared slice contributes no materialized input; do not charge
    // the empty object's braces against existing owned-row-only calls.
    let combined_bytes = reserved_bytes.saturating_add(if call.action.slice.is_empty() {
        0
    } else {
        u64::try_from(actual_bytes).unwrap_or(u64::MAX)
    });
    if combined_bytes > limits.max_canonical_bytes {
        return Err(ActionRefusal::SliceUnavailable {
            entry: "$slice".to_owned(),
            reason: format!(
                "combined canonical slice size {combined_bytes} exceeds {} bytes",
                limits.max_canonical_bytes
            ),
        });
    }
    Ok(LoadedActionSlice {
        facts,
        reference_only,
        values,
        link_reads,
        precondition_keys,
    })
}

/// Reconciles a declared slice read of the action target with the separately
/// authorized target snapshot, preserving both type-gated reads and CAS.
fn reconcile_action_target_snapshot<X>(
    catalog: &TypeCatalog<'_>,
    facts: &mut FactSlice,
    action_target: &FactKey,
    target_state: &FactState,
) -> Result<(), ActionRefusal<X>> {
    let model = crate::CommitModel::new(catalog);
    let target_identity = model.canonical(action_target);
    let same_target = |key: &FactKey| model.canonical(key) == target_identity;
    if let Some((_, observed)) = facts.iter().find(|(key, _)| same_target(key)) {
        if observed.version != target_state.version || observed.values != target_state.values {
            return Err(ActionRefusal::StalePreconditionRead {
                key: action_target.clone(),
                link: None,
            });
        }
        // Keep the concrete, authorized slice identity and snapshot in the
        // prepared fact cache. Its read version is also recorded as a
        // precondition read by the declared entry.
    } else {
        // The action target still participates in decision preparation and
        // write CAS even when the declared slice does not read it.
        facts.insert(action_target.clone(), target_state.clone());
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the read is bounded, authorized, and inserted into one prepared slice"
)]
fn load_action_fact<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    entry_name: &str,
    declared_type: &ObjectTypeName,
    requested: &FactKey,
    relation_depth: u32,
    facts: &mut FactSlice,
    meter: &mut SliceMeter,
    observed_records: &mut u32,
    limits: &crate::SliceLimits,
    read_types: &mut BTreeSet<ObjectTypeName>,
    reference_only: bool,
) -> Result<FactState, ActionRefusal<H::Error>> {
    if reference_only && !authorizer.supports_action_reference_lineage(call.action) {
        return Err(ActionRefusal::TargetNotFound);
    }
    if relation_depth > limits.max_relation_depth {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: format!(
                "relation depth {relation_depth} exceeds {}",
                limits.max_relation_depth
            ),
        });
    }
    if read_types.insert(declared_type.clone()) {
        authorize_slice_type(call, effective_input, authorizer, declared_type)?;
    }
    let model = crate::CommitModel::new(catalog);
    let canonical = model.canonical(requested);
    if let Some((previous_key, previous_state)) = facts
        .iter()
        .find(|(key, _)| model.canonical(key) == canonical)
    {
        // Reuse the first authorized snapshot for aliases of the same fact.
        // A single CAS identity must never acquire a later alias's newer
        // version or values while the action is still being prepared.
        if !catalog
            .lineage(&previous_key.object_type)
            .contains(&requested.object_type)
        {
            return Err(ActionRefusal::TargetNotFound);
        }
        if authorizer
            .authorize_read(&ActionReadAuthzRequest {
                action: call.action,
                scope: &call.scope,
                actor: &call.actor,
                input: effective_input,
                declared_type,
                target: previous_key,
                target_values: previous_state.values.as_ref(),
            })
            .is_err()
        {
            return Err(ActionRefusal::TargetNotFound);
        }
        admit_action_fact_record::<H::Error>(
            previous_state.values.as_ref(),
            meter,
            observed_records,
            relation_depth,
            limits,
            entry_name,
        )?;
        return Ok(previous_state.clone());
    }
    if meter.records() >= limits.max_records {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: format!("the slice exceeds {} object records", limits.max_records),
        });
    }
    let remaining_bytes = limits.max_canonical_bytes.saturating_sub(meter.bytes());
    let bounded = if reference_only {
        host.load_reference(requested, remaining_bytes)
    } else {
        host.load_bounded(requested, remaining_bytes)
    }
    .map_err(host_failure)?
    // Unsupported and over-bound snapshots are indistinguishable from an
    // unreadable object. Do not expose row existence or size before
    // object-level authorization can run.
    .ok_or(ActionRefusal::TargetNotFound)?;
    let fact_key = bounded.concrete_type.as_ref().map_or_else(
        || requested.clone(),
        |kind| FactKey::new(kind.as_str(), requested.key.clone()),
    );
    if let Some(concrete) = &bounded.concrete_type
        && !catalog.lineage(concrete).contains(&requested.object_type)
    {
        return Err(ActionRefusal::TargetNotFound);
    }
    let target_values = bounded.state.values.as_ref();
    if let Err(_denial) = authorizer.authorize_read(&ActionReadAuthzRequest {
        action: call.action,
        scope: &call.scope,
        actor: &call.actor,
        input: effective_input,
        declared_type,
        target: &fact_key,
        target_values,
    }) {
        // Read denial and invisibility share the same response so the action
        // cannot probe protected object identities or values.
        return Err(ActionRefusal::TargetNotFound);
    }
    admit_action_fact_record::<H::Error>(
        bounded.state.values.as_ref(),
        meter,
        observed_records,
        relation_depth,
        limits,
        entry_name,
    )?;
    facts.insert(fact_key, bounded.state.clone());
    Ok(bounded.state)
}

fn admit_action_fact_record<X>(
    values: Option<&JsonObject>,
    meter: &mut SliceMeter,
    observed_records: &mut u32,
    relation_depth: u32,
    limits: &crate::SliceLimits,
    entry_name: &str,
) -> Result<(), ActionRefusal<X>> {
    let Some(values) = values else {
        return Ok(());
    };
    let bytes = norn_ir::canonical::canonical_json_of(values)
        .map_err(|error| ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: error.to_string(),
        })?
        .len();
    meter
        .admit_record(u64::try_from(bytes).unwrap_or(u64::MAX), relation_depth)
        .map_err(|error| ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: error.to_string(),
        })?;
    *observed_records = (*observed_records).saturating_add(1);
    if *observed_records > limits.max_records {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: format!("the slice exceeds {} object records", limits.max_records),
        });
    }
    Ok(())
}

fn known_fact_key(facts: &FactSlice, catalog: &TypeCatalog<'_>, requested: &FactKey) -> FactKey {
    let model = crate::CommitModel::new(catalog);
    let canonical = model.canonical(requested);
    facts
        .keys()
        .find(|key| model.canonical(key) == canonical)
        .cloned()
        .unwrap_or_else(|| requested.clone())
}

fn authorize_slice_type<X>(
    call: &ActionCall<'_>,
    effective_input: &JsonObject,
    authorizer: &dyn ActionAuthorizer,
    object_type: &ObjectTypeName,
) -> Result<(), ActionRefusal<X>> {
    if authorizer
        .authorize_read_type(&ActionReadTypeAuthzRequest {
            action: call.action,
            scope: &call.scope,
            actor: &call.actor,
            input: effective_input,
            object_type,
        })
        .is_err()
    {
        return Err(ActionRefusal::TargetNotFound);
    }
    Ok(())
}

/// Evaluates every declared action precondition over the derived input and the
/// authorized slice, carrying forward the derivations' remaining fuel.
fn check_action_preconditions<X>(
    action: &ActionRoot,
    input: &JsonObject,
    slice: &JsonObject,
    remaining_fuel: &mut u64,
    receipt_evidence_bytes: &mut usize,
    max_receipt_bytes: usize,
) -> Result<Vec<ActionPreconditionOutcome>, ActionRefusal<X>> {
    let context = action_expression_context(input, slice).map_err(|reason| {
        ActionRefusal::PreconditionFailed {
            precondition: action
                .preconditions
                .first()
                .map_or_else(String::new, |expression| expression.as_str().to_owned()),
            reason: Some(reason),
        }
    })?;
    let mut outcomes = Vec::with_capacity(action.preconditions.len());
    for precondition in &action.preconditions {
        let source = precondition.as_str();
        let refusal = |reason| ActionRefusal::PreconditionFailed {
            precondition: source.to_owned(),
            reason,
        };
        let expression =
            Expression::compile(source).map_err(|error| refusal(Some(error.to_string())))?;
        let attempt = expression.evaluate_metered(&context, EvaluationBudget::new(*remaining_fuel));
        *remaining_fuel = (*remaining_fuel).saturating_sub(attempt.units_used);
        let evaluated = attempt
            .result
            .map_err(|error| refusal(Some(error.to_string())))?;
        match evaluated.value() {
            ExprValue::Bool(true) => {
                let encoded =
                    norn_ir::canonical::canonical_json_of(&ActionPreconditionOutcomeRef {
                        expression: source,
                        satisfied: true,
                    })
                    .map_err(|error| refusal(Some(error.to_string())))?;
                let overhead = if outcomes.is_empty() { 24 } else { 1 };
                let next_bytes = (*receipt_evidence_bytes)
                    .saturating_add(encoded.len())
                    .saturating_add(overhead);
                if next_bytes > max_receipt_bytes {
                    return Err(ActionRefusal::InputBoundExceeded {
                        subject: "action call receipt evidence".to_owned(),
                        actual_bytes: next_bytes,
                        max_bytes: max_receipt_bytes,
                    });
                }
                *receipt_evidence_bytes = next_bytes;
                outcomes.push(ActionPreconditionOutcome {
                    expression: source.to_owned(),
                    satisfied: true,
                });
            }
            ExprValue::Bool(false) => return Err(refusal(None)),
            ExprValue::Null
            | ExprValue::Number(_)
            | ExprValue::String(_)
            | ExprValue::Array(_)
            | ExprValue::Object(_) => {
                return Err(refusal(Some(
                    "the expression did not evaluate to a boolean".to_owned(),
                )));
            }
        }
    }
    Ok(outcomes)
}

/// The replay of an already committed call under the same key, or `None`
/// when no call was committed under it.
fn replay<X>(
    recorded: &FactState,
    call: &ActionCallRecord,
    key: &str,
) -> Result<Option<ActionOutcome>, ActionRefusal<X>> {
    let Some(values) = &recorded.values else {
        return Ok(None);
    };
    match ActionCallRecord::from_values(values) {
        Some(record) if record.binds_same_call(call) => {
            Ok(Some(ActionOutcome::Replayed { record }))
        }
        _ => Err(ActionRefusal::IdempotencyConflict {
            key: key.to_owned(),
        }),
    }
}

#[cfg(test)]
mod reference_lineage_tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "kernel proof assertions"
    )]
    use super::*;

    #[test]
    fn reference_proof_rejects_changed_outputs_and_keeps_added_guards_full() {
        let reference = FactExpectation {
            key: FactKey::new("example.Customer", "customer"),
            expected: FactVersion(1),
        };
        let commit = CellCommit::<(), (), ()> {
            reads: vec![reference.clone()],
            writes: vec![FactWrite {
                key: FactKey::new("example.Order", "order"),
                expected: FactVersion(0),
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        };
        let proof = || ActionReferenceReads {
            reads: commit.reads.clone(),
            references: commit.reads.clone(),
            property_reads: vec![(reference.clone(), vec!["contact".to_owned()])],
            writes: commit.writes.clone(),
            links: Vec::new(),
            link_reads: Vec::new(),
            event_count: 0,
        };
        let mut changed = commit.clone();
        changed.writes[0].mutation = FactMutation::Put {
            values: BTreeMap::from([(
                "copiedContact".to_owned(),
                RuntimeValue::String("private".to_owned()),
            )]),
        };
        assert!(!proof().validates(&changed));
        changed = commit.clone();
        changed.reads[0].expected = FactVersion(2);
        assert!(!proof().validates(&changed));
        let mut guarded = commit.clone();
        let guarded_proof = proof()
            .with_read_guards(&mut guarded, std::slice::from_ref(&reference))
            .expect("additional full guard");
        assert!(guarded_proof.reference_reads().is_empty());
        assert!(guarded_proof.property_reads().is_empty());
        assert!(guarded_proof.validates(&guarded));
        let mismatch = FactExpectation {
            key: commit.reads[0].key.clone(),
            expected: FactVersion(3),
        };
        assert!(
            proof()
                .with_read_guards(&mut commit.clone(), &[mismatch])
                .is_none()
        );
    }
}
