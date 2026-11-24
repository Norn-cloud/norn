use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{ActionRoot, KeyName, ObjectTypeName, RuntimeValue, TraceContext};
use serde::{Deserialize, Serialize};

use crate::action::ActionCommandRejection;
use crate::cell_host::{
    CellCommit, CommitReceipt, FactExpectation, FactKey, FactState, FactVersion, FactWrite,
    JsonObject, JsonValue, LinkReadExpectation, LinkWrite, PredicateReadExpectation,
};
use crate::decision_commit::{DecisionRefusal, DecisionTurn, FactSlice};
use crate::envelope::TurnEnvelopeMeter;
use crate::limits::EnvelopeLimits;
use crate::naming_series::SeriesCandidate;

/// Object type of the kernel's idempotency records of action calls (registered by host, #2911).
pub const ACTION_CALL_TYPE: &str = "norn.ActionCall";

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
    /// Optional provenance trace context supplied by the caller.
    pub trace: Option<TraceContext>,
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionDerivedValue {
    /// The derived field.
    pub field: String,
    /// The exact runtime value written to the effective input. Preserve source
    /// text and use an exact-number JSON parser; JavaScript `JSON.parse` may
    /// lose precision.
    pub value: RuntimeValue,
}

/// The successful outcome of one action precondition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionPreconditionOutcome {
    /// The expression evaluated.
    pub expression: String,
    /// Whether it held (successful records contain only `true`).
    pub satisfied: bool,
}

/// One effective-dated parameter resolved and pinned during an action commit
/// (D07, PARAM-EXACT-1, PARAM-BOUND-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionParameterPin {
    /// The declared parameter slice entry name.
    pub entry: String,
    /// The stable parameter key.
    pub setting_key: String,
    /// The declared effective timestamp evaluated for this selection.
    pub effective_at: String,
    /// The selected setting fact's concrete object type and key.
    pub setting: FactKey,
    /// The committed fact version of the selected setting at commit time.
    pub version: u64,
    /// Canonical Norn1 SHA-256 digest (`sha256:...`) of the selected setting's
    /// exact values at commit time.
    pub digest: String,
}

impl ActionParameterPin {
    fn to_runtime_value(&self) -> JsonValue {
        let string = |value: &str| JsonValue::String(value.to_owned());
        JsonValue::Object(JsonObject::from([
            ("entry".to_owned(), string(&self.entry)),
            ("settingKey".to_owned(), string(&self.setting_key)),
            ("effectiveAt".to_owned(), string(&self.effective_at)),
            (
                "setting".to_owned(),
                JsonValue::Object(JsonObject::from([
                    (
                        "objectType".to_owned(),
                        string(self.setting.object_type.as_str()),
                    ),
                    ("key".to_owned(), string(&self.setting.key)),
                ])),
            ),
            (
                "version".to_owned(),
                JsonValue::Number(norn_ir::Decimal::from(self.version)),
            ),
            ("digest".to_owned(), string(&self.digest)),
        ]))
    }
}

/// The result of applying ordered derivations and validating the final input.
#[derive(Debug, PartialEq)]
pub struct ActionInputPreparation<'a> {
    pub(super) action: &'a ActionRoot,
    pub(super) raw_input: &'a JsonObject,
    pub(super) action_artifact_id: Option<&'a str>,
    pub(super) action_version: Option<&'a str>,
    pub(super) actor: Option<String>,
    pub(super) effective_input: JsonObject,
    pub(super) derived_values: Vec<ActionDerivedValue>,
    pub(super) remaining_fuel: u64,
    pub(super) receipt_evidence_bytes: usize,
    pub(super) precondition_outcomes: Vec<ActionPreconditionOutcome>,
}

impl ActionInputPreparation<'_> {
    pub(super) fn recorded_effective_input(&self) -> Option<JsonObject> {
        (!self.action.derive.is_empty() || self.effective_input != *self.raw_input)
            .then(|| self.effective_input.clone())
    }

    pub(super) fn matches_call(&self, call: &ActionCall<'_>) -> bool {
        self.action == call.action
            && self.raw_input == &call.input
            && self.action_artifact_id == Some(call.action_artifact_id)
            && self.action_version == Some(call.action_version)
            && self.actor.as_deref() == Some(call.actor.as_str())
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
pub(super) struct ActionDerivedValueRef<'a> {
    pub(super) field: &'a str,
    pub(super) value: &'a RuntimeValue,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ActionPreconditionOutcomeRef<'a> {
    pub(super) expression: &'a str,
    pub(super) satisfied: bool,
}

/// The idempotency record of a committed call (the values of its
/// [`ACTION_CALL_TYPE`] object).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    /// TXN-RULE-1: exact writer closure, action, scope and world identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_binding: Option<String>,
    /// The target object.
    pub object: FactKey,
    /// The call's input.
    pub input: JsonObject,
    /// The final input after ordered derivations. Legacy records use `input`.
    /// Preserve source text and use an exact-number JSON parser for its values;
    /// JavaScript `JSON.parse` may lose precision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_input: Option<JsonObject>,
    /// The values produced by each derivation, in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_values: Vec<ActionDerivedValue>,
    /// Successful precondition results, in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub precondition_outcomes: Vec<ActionPreconditionOutcome>,
    /// Effective-dated parameter pins resolved by declared parameter slices
    /// (D07, PARAM-EXACT-1, PARAM-BOUND-1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameter_pins: Vec<ActionParameterPin>,
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
    /// Optional provenance trace context supplied by the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,
}

impl ActionCallRecord {
    /// Generated JSON Schema of this record's wire contract.
    /// The kernel's schema correspondence test regenerates and checks it.
    #[must_use]
    pub const fn schema_json() -> &'static str {
        include_str!("../../schema/action_call_record.schema.json")
    }

    pub(super) fn for_transition(
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
            rule_binding: None,
            object: target.clone(),
            input: call.input.clone(),
            effective_input: preparation.recorded_effective_input(),
            derived_values: preparation.derived_values.clone(),
            precondition_outcomes: Vec::new(),
            parameter_pins: Vec::new(),
            actor: call.actor.clone(),
            turn: call.turn.id.clone(),
            transition: transition.as_str().to_owned(),
            from: String::new(),
            to: String::new(),
            row_operations: Vec::new(),
            trace: call.trace.clone(),
        }
    }

    pub(super) fn binds_same_call(&self, other: &Self) -> bool {
        crate::replay_decision::same_call(&self.call_identity(), &other.call_identity())
    }

    const fn call_identity(
        &self,
    ) -> crate::replay_decision::CallIdentity<
        &String,
        &FactKey,
        &JsonObject,
        (&Option<String>, &Option<TraceContext>),
    > {
        crate::replay_decision::CallIdentity {
            action: &self.action_id,
            object: &self.object,
            input: &self.input,
            actor: &self.actor,
            rule_binding: (&self.rule_binding, &self.trace),
        }
    }

    pub(super) fn binds_same_generated_call(
        &self,
        action_id: &str,
        object_type: &ObjectTypeName,
        input: &JsonObject,
        actor: &str,
        trace: Option<&TraceContext>,
    ) -> bool {
        self.action_id == action_id
            && self.object.object_type == *object_type
            && self.input == *input
            && self.actor == actor
            && self.trace.as_ref() == trace
    }

    pub(super) fn to_values(&self) -> JsonObject {
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
        if !self.parameter_pins.is_empty() {
            values.insert(
                "parameterPins".to_owned(),
                JsonValue::Array(
                    self.parameter_pins
                        .iter()
                        .map(ActionParameterPin::to_runtime_value)
                        .collect(),
                ),
            );
        }
        if let Some(action_artifact_id) = &self.action_artifact_id {
            values.insert("actionArtifactId".to_owned(), string(action_artifact_id));
        }
        if let Some(action_version) = &self.action_version {
            values.insert("actionVersion".to_owned(), string(action_version));
        }
        if let Some(binding) = &self.rule_binding {
            values.insert("ruleBinding".to_owned(), string(binding));
        }
        if let Some(effect_kind) = &self.effect_kind {
            values.insert("effectKind".to_owned(), string(effect_kind));
        }
        if let Some(trace) = &self.trace {
            values.insert("trace".to_owned(), trace.to_runtime_value());
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
        let parameter_pins = match values.get("parameterPins") {
            None | Some(JsonValue::Null) => Vec::new(),
            Some(value) => {
                let decoded: Vec<ActionParameterPin> = norn_ir::runtime::decode(value).ok()?;
                if !decoded.iter().all(super::parameter::validate_parameter_pin) {
                    return None;
                }
                decoded
            }
        };
        let trace = match values.get("trace") {
            None | Some(JsonValue::Null) => None,
            Some(value) => {
                let decoded: TraceContext = norn_ir::runtime::decode(value).ok()?;
                decoded.validate().ok()?;
                Some(decoded)
            }
        };
        Some(Self {
            action_id: string("actionId")?,
            action_artifact_id,
            action_version,
            effect_kind,
            rule_binding: match values.get("ruleBinding") {
                Some(JsonValue::String(value)) => Some(value.clone()),
                None | Some(JsonValue::Null) => None,
                _ => return None,
            },
            object: FactKey::new(object_type.clone(), key.clone()),
            input: input.clone(),
            effective_input,
            derived_values,
            precondition_outcomes,
            parameter_pins,
            actor: string("actor")?,
            turn: string("turn")?,
            transition: string("transition")?,
            from: string("from")?,
            to: string("to")?,
            row_operations,
            trace,
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
        /// Exact target version at `receipt.seq` (written or CAS-verified read).
        target_version: FactVersion,
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
    /// A synchronous writer escaped its closed declared effect set.
    #[error("synchronous action writer refused: {reason}")]
    UncheckedWriter {
        /// The refused writer contract.
        reason: String,
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
    /// An aggregate input matched more objects than its declared bound.
    #[error("aggregate {aggregate:?} matched more than {max_items} objects")]
    AggregateBoundExceeded {
        /// The declared aggregate.
        aggregate: String,
        /// Its declared bound.
        max_items: u32,
    },
    /// An aggregate input's matches outgrew the slice's byte bound.
    #[error("aggregate {aggregate:?} matched more than {max_bytes} canonical bytes")]
    AggregateBytesExceeded {
        /// The declared aggregate.
        aggregate: String,
        /// The slice's canonical byte bound.
        max_bytes: u64,
    },
    /// An aggregate input could not be read or folded exactly.
    #[error("aggregate {aggregate:?} was refused: {reason}")]
    AggregateUnavailable {
        /// The declared aggregate.
        aggregate: String,
        /// The safe refusal detail.
        reason: String,
    },
    /// A visible, required single-object slice entry does not exist.
    #[error("required action slice entry {entry:?} is missing")]
    SliceObjectMissing {
        /// The declared entry.
        entry: String,
    },
    /// PARAM-EXACT-1 / PARAM-BOUND-1: effective-dated parameter resolution refused.
    #[error("parameter slice {entry:?} refused ({reason})")]
    ParameterRefused {
        /// The declared parameter slice entry.
        entry: String,
        /// The canonical parameter resolution error.
        error: norn_checker::parameter::ParameterError,
        /// Safe refusal detail.
        reason: String,
    },
    /// A precondition's fact or adjacency read changed before commit.
    #[error("action precondition read changed before commit at {key}")]
    StalePreconditionRead {
        /// The stale object or adjacency source.
        key: FactKey,
        /// The link name when the stale read was a complete adjacency set.
        link: Option<String>,
    },
    /// The objects an aggregate input folded over changed before commit.
    #[error("aggregate read over {object_type:?} changed before commit")]
    StaleAggregateRead {
        /// The aggregate's object type.
        object_type: ObjectTypeName,
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
    /// RECEIPT-EFFECT-1: the referenced receipt cannot be consumed by this
    /// call (unknown, replayed, bound to another run or generation, not an
    /// accepted success, or its result does not fit the declared bindings).
    #[error("the action's receipt was refused: {}", reason.code())]
    ReceiptRefused {
        /// Why.
        reason: crate::receipt_binding::ReceiptRefusal,
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
            Self::UncheckedWriter { .. } => "core.txn.unchecked_writer",
            Self::InvalidInput { .. } => "action.invalid_input",
            Self::DerivationFailed { .. } => "action.derivation_failed",
            Self::InputBoundExceeded { .. } => "action.input_bound_exceeded",
            Self::ExpressionBudgetExceeded { .. } => "action.expression_budget_exceeded",
            Self::SliceUnavailable { .. } => "action.slice_unavailable",
            Self::AggregateBoundExceeded { .. } => "action.aggregate_bound_exceeded",
            Self::AggregateBytesExceeded { .. } => "action.aggregate_bytes_exceeded",
            Self::AggregateUnavailable { .. } => "action.aggregate_unavailable",
            Self::SliceObjectMissing { .. } => "action.slice_object_missing",
            Self::ParameterRefused { error, .. } => error.code(),
            Self::StalePreconditionRead { .. } => "action.precondition_stale_read",
            Self::StaleAggregateRead { .. } => "action.aggregate_stale_read",
            Self::PreconditionFailed { .. } => "action.precondition_failed",
            Self::TargetNotFound => "action.target_not_found",
            Self::PermissionDenied { .. } => "action.permission_denied",
            Self::KindMismatch { .. } => "artifact_set.action.effect_kind_mismatch",
            Self::IdempotencyConflict { .. } => "action.idempotency_conflict",
            Self::WriteModeFailed { .. } => "action.write_mode_failed",
            Self::ReceiptRefused { reason } => reason.code(),
            Self::Decision(refusal) => refusal.code(),
        }
    }
}

impl<X> From<DecisionRefusal<X>> for ActionRefusal<X> {
    fn from(refusal: DecisionRefusal<X>) -> Self {
        Self::Decision(refusal)
    }
}

impl crate::replay_decision::RecordedCall for ActionCallRecord {
    fn same_call(&self, call: &Self) -> bool {
        self.binds_same_call(call)
    }
}

/// A call that passed every check up to the transition.
pub(super) struct Pending {
    pub(super) effective_input: JsonObject,
    pub(super) remaining_fuel: u64,
    pub(super) target: FactKey,
    pub(super) slice: FactSlice,
    pub(super) reference_only: bool,
    pub(super) link_reads: Vec<LinkReadExpectation>,
    pub(super) predicate_reads: Vec<PredicateReadExpectation>,
    pub(super) precondition_reads: BTreeSet<FactKey>,
    pub(super) record_key: FactKey,
    pub(super) recorded: FactState,
    pub(super) record: ActionCallRecord,
    pub(super) series_candidate: Option<SeriesCandidate>,
    pub(super) decision_links: Vec<norn_ir::LinkChange>,
    /// RECEIPT-EFFECT-1: the receipt delivery this commit consumes.
    pub(super) receipt: Option<crate::result_reconciliation::Acceptance>,
}

/// Per-decision limits and, when present, the shared aggregate turn meter.
pub(super) struct EnvelopePolicy<'a> {
    pub(super) limits: &'a EnvelopeLimits,
    pub(super) meter: Option<&'a mut TurnEnvelopeMeter>,
}

/// Kernel-owned attribution for direct reference validation. It cannot cross a
/// serialized commit boundary or be constructed by an action caller.
#[derive(Debug)]
pub struct ActionReferenceReads {
    pub(super) reads: Vec<FactExpectation>,
    pub(super) references: Vec<FactExpectation>,
    pub(super) property_reads: Vec<(FactExpectation, Vec<String>)>,
    pub(super) writes: Vec<FactWrite>,
    pub(super) links: Vec<LinkWrite>,
    pub(super) link_reads: Vec<LinkReadExpectation>,
    pub(super) event_count: usize,
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
            && commit.predicate_reads.is_empty()
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
