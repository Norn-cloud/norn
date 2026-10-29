//! Lifecycle writes of decisions and actions (T3-LIFE-2, #2673; ruling
//! 2026-09-25 §2): "The checker verifies that every rule that writes that
//! property only emits declared transitions."
//!
//! # The fact change (T2)
//!
//! A decide graph's `facts` items are [`norn_ir::FactChange`] records. The
//! graph's `outputSchema` declares the item schema as one closed record over
//! [`norn_ir::FACT_CHANGE_MEMBERS`] (`validate_fact_change_schema`, run with
//! the decision contract; [`codes::INVALID_FACT_CHANGE`]):
//!
//! - `op`, `objectType`, and `key` are declared and required; `key` is a
//!   string.
//! - `op` is a string `enum` of the ops the decision emits, and `objectType`
//!   a string `enum` of the object types it writes.
//! - Every member an emitted op carries is declared (`create`: `values`;
//!   `set`: `property`, `value`; `transition`: `lifecycle`, `transition`;
//!   `patch`: `patch`, #3080), and no other member is.
//! - `property`, `lifecycle`, and `transition` are string `enum`s, and
//!   `values` and `patch` are closed records.
//!
//! The evaluator validates every emitted record against this schema
//! (T3-DEC-2), so the enums bound what a decision can write whatever its
//! expressions compute: a computed transition id is constrained by the
//! `transition` enum, which must be a subset of the declared ids (below).
//! A **literal** fact-change value in an expression site is also checked
//! directly, per record (`fact_literals.rs`, over the object literals
//! `literals.rs` reads).
//!
//! # The writes (T3, [`check_lifecycle_write_set`])
//!
//! Over a resolved artifact set, each candidate decide graph's fact changes
//! are checked against the **effective** lifecycles of the object types they
//! name (declared or inherited, [`crate::EffectiveObjectType::lifecycles`]).
//! Every combination the schema admits is checked, because the closed schema
//! cannot correlate an object type with a property:
//!
//! - every `objectType` value names an object type of the set or the base
//!   ([`codes::UNKNOWN_FACT_OBJECT_TYPE`]) the rule's scope can see
//!   ([`crate::artifact_set::codes::INCOMPATIBLE_SCOPE`]);
//! - `set` never names a lifecycle property, and no `patch` member is one
//!   ([`codes::DIRECT_WRITE`]): a status changes only through a declared
//!   transition, so the lifecycle is the single owner of the machine;
//! - `transition` is checked per target object type (#3080): each
//!   `lifecycle` value is declared by at least one written type
//!   ([`codes::NOT_A_LIFECYCLE`]), and each `transition` id by that
//!   lifecycle of every written type that declares it
//!   ([`codes::UNKNOWN_TRANSITION`]); a written type without the lifecycle
//!   is not a transition target;
//! - `create` writes a lifecycle property only as an `enum` of states
//!   ([`codes::UNCONSTRAINED_STATE`]), each an initial state
//!   ([`crate::check_lifecycle_writes`], `lifecycle.validation.undeclared_write`).
//!   A property it omits takes its default, which is an initial state;
//! - `delete` never names a type with a lifecycle
//!   ([`codes::DELETE_LIFECYCLE_OBJECT`]): delete then create of the same key
//!   would reset its status without a transition. Deleting lifecycle-owned
//!   objects waits for an explicit deletion/recreation policy;
//! - in every expression site that feeds the facts (not `commands` or
//!   `timers`), each fact-change record's literal `op`, `objectType`,
//!   `property`, `lifecycle`, and `transition`, and each whole-literal
//!   decision-table output cell of a column named like one of them, is
//!   admitted by that member's enum
//!   ([`codes::FACT_LITERAL_OUTSIDE_SCHEMA`]); a literal `transition` is
//!   declared by the lifecycle the record's literal `lifecycle` (or, when
//!   computed, each `lifecycle` enum value) names on its literal
//!   `objectType` (or each resolved one)
//!   ([`codes::UNDECLARED_TRANSITION_LITERAL`]); a literal created state is
//!   admitted by its `values` enum and is an initial state;
//! - a type whose effective lifecycles cannot be resolved (the catalog is
//!   blocked by a finding on an artifact it depends on) fails closed
//!   ([`codes::UNRESOLVED_LIFECYCLES`]): its status writes cannot be checked.
//!
//! Actions follow the same rule ([`crate::action_set`]): `upsert_input_object`
//! may modify, so an input schema property that is a lifecycle property of
//! the target is a [`codes::DIRECT_WRITE`], and a created object takes its
//! default initial state. An action changes a status only through the
//! `fire_transition` effect (T3-LIFE-3, #2867), which names the lifecycle
//! ([`codes::NOT_A_LIFECYCLE`]) and a transition id its effective lifecycle
//! declares ([`codes::UNKNOWN_TRANSITION`]), exactly like a `transition` fact
//! change; [`crate::check_action_firing`] checks one call against an object's
//! current state.

use std::collections::BTreeMap;

use norn_ir::{
    ArtifactEnvelope, ArtifactKind, DiagnosticRemediation, DiagnosticSeverity, FACT_CHANGE_MEMBERS,
    FACT_CHANGE_REQUIRED_MEMBERS, FactChangeOp, JsonValue,
};

use crate::action_set::Model;
use crate::artifact_set::scope_finding;
use crate::entity_artifacts::EntityArtifactSetContext;
use crate::entity_instance::EffectiveLifecycle;
use crate::envelope::invalid_diagnostic;
use crate::{Diagnostic, DiagnosticCode, LifecycleWrite, Span, check_lifecycle_writes};

/// Stable codes of lifecycle writes.
pub mod codes {
    /// A decide graph's `facts` item schema is not the canonical fact change.
    pub const INVALID_FACT_CHANGE: &str = "rule.validation.invalid_fact_change";
    /// A fact change names an object type the set and the base do not
    /// define.
    pub const UNKNOWN_FACT_OBJECT_TYPE: &str = "artifact_set.decision.unknown_fact_object_type";
    /// A decision `set` or an action's input writes a lifecycle property
    /// directly instead of through a declared transition.
    pub const DIRECT_WRITE: &str = "artifact_set.lifecycle.direct_write";
    /// A `transition` fact change or a `fire_transition` action effect names
    /// a property with no lifecycle.
    pub const NOT_A_LIFECYCLE: &str = "artifact_set.lifecycle.not_a_lifecycle";
    /// A `transition` fact change or a `fire_transition` action effect names
    /// a transition its (effective) lifecycle does not declare.
    pub const UNKNOWN_TRANSITION: &str = "artifact_set.lifecycle.unknown_transition";
    /// A `create` fact change writes a lifecycle property without an `enum`
    /// of the states it may create.
    pub const UNCONSTRAINED_STATE: &str = "artifact_set.lifecycle.unconstrained_state";
    /// A decision may delete objects of a type with a lifecycle.
    pub const DELETE_LIFECYCLE_OBJECT: &str = "artifact_set.lifecycle.delete_lifecycle_object";
    /// A literal `op`, `objectType`, `property`, `lifecycle`, `transition`,
    /// or created state in an expression site is not admitted by its enum in
    /// the facts item schema.
    pub const FACT_LITERAL_OUTSIDE_SCHEMA: &str =
        "artifact_set.decision.fact_literal_outside_schema";
    /// A literal transition id in an expression site is not declared by the
    /// lifecycle its fact change names.
    pub const UNDECLARED_TRANSITION_LITERAL: &str =
        "artifact_set.lifecycle.undeclared_transition_literal";
    /// A decision or an action writes a type whose effective lifecycles
    /// cannot be resolved.
    pub const UNRESOLVED_LIFECYCLES: &str = "artifact_set.lifecycle.unresolved_lifecycles";
    /// A decide fact writes a property that is absent, computed, or whose
    /// exact value violates the target type's effective stored contract.
    pub const INVALID_STORED_PROPERTY_WRITE: &str =
        "artifact_set.decision.invalid_stored_property_write";
    /// A decide fact writes a type whose effective stored-property contracts
    /// cannot be resolved.
    pub const UNRESOLVED_STORED_PROPERTY_CONTRACT: &str =
        "artifact_set.decision.unresolved_stored_property_contract";
}

mod fact_literals;
mod literals;

const ITEMS: &str = "payload.outputSchema.properties.facts.items";
const ITEMS_POINTER: &str = "/payload/outputSchema/properties/facts/items";

/// The `facts` item schema of a decide graph's output schema: its
/// `properties` and `required` members, when it is a list of records.
struct FactSchema<'s> {
    properties: &'s BTreeMap<String, JsonValue>,
    required: Vec<&'s str>,
}

impl<'s> FactSchema<'s> {
    fn of(output_schema: &'s JsonValue) -> Option<Self> {
        let facts = member(Some(output_schema), "properties")
            .and_then(|properties| member(Some(properties), "facts"))?;
        if type_of(Some(facts)) != Some("array") {
            return None;
        }
        let items = member(Some(facts), "items")?;
        if type_of(Some(items)) != Some("object") {
            return None;
        }
        Some(Self {
            properties: member(Some(items), "properties")?.as_object()?,
            required: member(Some(items), "required")
                .and_then(JsonValue::as_array)
                .unwrap_or_default()
                .iter()
                .filter_map(JsonValue::as_string)
                .collect(),
        })
    }

    fn get(&self, name: &str) -> Option<&'s JsonValue> {
        self.properties.get(name)
    }

    /// The string `enum` of member `name`.
    fn values(&self, name: &str) -> Option<Vec<&'s str>> {
        string_enum(self.get(name))
    }

    /// The ops of the `op` enum that parse.
    fn ops(&self) -> Vec<FactChangeOp> {
        self.values("op")
            .unwrap_or_default()
            .into_iter()
            .filter_map(FactChangeOp::parse)
            .collect()
    }
}

fn member<'s>(schema: Option<&'s JsonValue>, name: &str) -> Option<&'s JsonValue> {
    schema?.as_object()?.get(name)
}

fn type_of(schema: Option<&JsonValue>) -> Option<&str> {
    member(schema, "type")?.as_string()
}

/// The values of a `string` schema's `enum`, when every one is a string.
fn string_enum(schema: Option<&JsonValue>) -> Option<Vec<&str>> {
    if type_of(schema) != Some("string") {
        return None;
    }
    let values = member(schema, "enum")?.as_array()?;
    let strings: Vec<&str> = values.iter().filter_map(JsonValue::as_string).collect();
    (!strings.is_empty() && strings.len() == values.len()).then_some(strings)
}

/// Check a decide graph's `facts` item schema against the canonical fact
/// change (T2). See the module docs. A `facts` member that is not a list of
/// records is the decision contract's finding, not this one's.
pub(crate) fn validate_fact_change_schema(
    output_schema: Option<&JsonValue>,
    diagnostics: &mut Vec<norn_ir::Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(schema) = output_schema.and_then(FactSchema::of) else {
        return;
    };
    for (message, path) in member_findings(&schema)
        .into_iter()
        .chain(op_findings(&schema))
    {
        diagnostics.push(invalid_diagnostic(
            codes::INVALID_FACT_CHANGE,
            message,
            path,
            artifact_id,
        ));
    }
}

/// `(message, dotted path)` of every member rule the item schema breaks.
fn member_findings(schema: &FactSchema<'_>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in schema.properties.keys() {
        if !FACT_CHANGE_MEMBERS.contains(&name.as_str()) {
            out.push((
                format!(
                    "'{name}' is not a fact change member; a fact change carries {FACT_CHANGE_MEMBERS:?}"
                ),
                format!("{ITEMS}.properties.{name}"),
            ));
        }
    }
    for name in FACT_CHANGE_REQUIRED_MEMBERS {
        if schema.get(name).is_none() {
            out.push((
                format!("a fact change declares '{name}'"),
                format!("{ITEMS}.properties"),
            ));
        } else if !schema.required.contains(&name) {
            out.push((
                format!("every fact change carries '{name}': it must be required"),
                format!("{ITEMS}.required"),
            ));
        }
    }
    if schema.get("key").is_some() && type_of(schema.get("key")) != Some("string") {
        out.push((
            "a fact change's 'key' is a string".to_owned(),
            format!("{ITEMS}.properties.key"),
        ));
    }
    for (name, what) in [
        ("op", "the ops the decision emits"),
        ("objectType", "the object types the decision writes"),
        ("property", "the properties the decision sets"),
        (
            "lifecycle",
            "the lifecycle properties the decision transitions",
        ),
        ("transition", "the transition ids the decision fires"),
    ] {
        let Some(declared) = schema.get(name) else {
            continue;
        };
        // A malformed `enum` is the closed-schema check's finding.
        let has_enum =
            type_of(Some(declared)) == Some("string") && member(Some(declared), "enum").is_some();
        if !has_enum {
            out.push((
                format!("a fact change's '{name}' is a string enum of {what}"),
                format!("{ITEMS}.properties.{name}"),
            ));
        }
    }
    for name in ["values", "patch"] {
        if schema.get(name).is_some() && type_of(schema.get(name)) != Some("object") {
            out.push((
                format!("a fact change's '{name}' is a closed record of property values"),
                format!("{ITEMS}.properties.{name}"),
            ));
        }
    }
    out
}

/// `(message, dotted path)` of every op rule the item schema breaks: each op
/// is known, and the declared members are exactly those the emitted ops
/// carry.
fn op_findings(schema: &FactSchema<'_>) -> Vec<(String, String)> {
    let Some(ops) = schema.values("op") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut carried: Vec<&str> = Vec::new();
    for (index, value) in ops.iter().enumerate() {
        let Some(op) = FactChangeOp::parse(value) else {
            out.push((
                format!(
                    "'{value}' is not a fact change op: create, set, delete, transition, or patch"
                ),
                format!("{ITEMS}.properties.op.enum[{index}]"),
            ));
            continue;
        };
        for name in op.members() {
            if !carried.contains(name) {
                carried.push(name);
            }
            if schema.get(name).is_none() {
                out.push((
                    format!("the decision emits '{value}', which carries '{name}'"),
                    format!("{ITEMS}.properties"),
                ));
            }
        }
    }
    for name in schema.properties.keys() {
        let name = name.as_str();
        if FACT_CHANGE_MEMBERS.contains(&name)
            && !FACT_CHANGE_REQUIRED_MEMBERS.contains(&name)
            && !carried.contains(&name)
        {
            out.push((
                format!("no op the decision emits carries '{name}'"),
                format!("{ITEMS}.properties.{name}"),
            ));
        }
    }
    out
}

/// Check the lifecycle writes of every candidate decide graph over the
/// resolved set of `artifacts` and the installed base and registry of
/// `context` (T3). See the module docs; actions are checked by
/// [`crate::action_set`].
#[must_use]
pub fn check_lifecycle_write_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let decisions = fact_decisions(artifacts);
    if decisions.is_empty() {
        return Vec::new();
    }
    let model = Model::new(artifacts, context.base, context.registry, true);
    let mut findings = Vec::new();
    for (artifact, output) in &decisions {
        let Some(schema) = FactSchema::of(output) else {
            continue;
        };
        let ops = schema.ops();
        let mut resolved: Vec<fact_literals::Resolved<'_>> = Vec::new();
        for (index, kind) in schema
            .values("objectType")
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            let pointer = enum_pointer("objectType", index);
            let Some(defined) = model.object_type(kind) else {
                findings.push(finding(
                    codes::UNKNOWN_FACT_OBJECT_TYPE,
                    format!(
                        "the decision writes {kind:?}, which is not an object type of the artifact set or the installed base"
                    ),
                    artifact,
                    &pointer,
                ));
                continue;
            };
            findings.extend(scope_finding(artifact, defined.artifact, &pointer));
            let lifecycles = match &defined.lifecycles {
                Ok(lifecycles) => lifecycles.as_slice(),
                Err(reason) => {
                    findings.push(finding(
                        codes::UNRESOLVED_LIFECYCLES,
                        format!(
                            "the decision writes {kind:?}, whose lifecycles cannot be resolved ({reason}); its status writes cannot be checked, so they fail closed"
                        ),
                        artifact,
                        &pointer,
                    ));
                    continue;
                }
            };
            let effective_object_type = match defined.effective_object_type.as_ref() {
                Some(Ok(effective)) => effective,
                unresolved => {
                    let reason = unresolved
                        .and_then(|contract| contract.as_ref().err())
                        .map_or("the effective catalog does not define it", String::as_str);
                    findings.push(finding(
                        codes::UNRESOLVED_STORED_PROPERTY_CONTRACT,
                        format!(
                            "the decision writes {kind:?}, whose effective stored-property contracts cannot be resolved ({reason}); its property writes cannot be checked, so they fail closed"
                        ),
                        artifact,
                        &pointer,
                    ));
                    continue;
                }
            };
            resolved.push(fact_literals::Resolved {
                kind,
                lifecycles,
                effective_object_type,
            });
            let target = Target {
                artifact,
                schema: &schema,
                kind,
                lifecycles,
            };
            if ops.contains(&FactChangeOp::Delete) {
                findings.extend(target.delete_findings());
            }
            if ops.contains(&FactChangeOp::Set) {
                findings.extend(target.set_findings());
            }
            if ops.contains(&FactChangeOp::Patch) {
                findings.extend(target.patch_findings());
            }
            if ops.contains(&FactChangeOp::Create) {
                findings.extend(target.create_findings());
            }
        }
        if ops.contains(&FactChangeOp::Transition) {
            findings.extend(transition_findings(artifact, &schema, &resolved));
        }
        findings.extend(fact_literals::literal_findings(
            artifact, &schema, &resolved,
        ));
    }
    findings
}

fn fact_decisions(artifacts: &[ArtifactEnvelope]) -> Vec<(&ArtifactEnvelope, JsonValue)> {
    artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Rule)
        .filter_map(|artifact| {
            let payload = artifact.payload.to_json_value();
            let object = payload.as_object()?;
            let decide = object.get("contract").and_then(JsonValue::as_string) == Some("decide")
                && object.contains_key("graph");
            let output = object.get("outputSchema")?.clone();
            (decide && FactSchema::of(&output).is_some()).then_some((artifact, output))
        })
        .collect()
}

/// `transition` names lifecycle properties and transition ids they declare,
/// checked per target object type (#3080): a lifecycle in the `lifecycle`
/// enum must be declared by at least one object type the decision writes
/// ([`codes::NOT_A_LIFECYCLE`]), and every id in the `transition` enum by
/// that lifecycle of **every** written type that declares it
/// ([`codes::UNKNOWN_TRANSITION`]). A written type without the lifecycle
/// (a created ledger row, a patched balance) is not a transition target, so
/// it no longer forces every type of the decision to share one machine. The
/// kernel resolves each transition against its object's own type at commit,
/// and literal records are checked per record (`fact_literals.rs`).
fn transition_findings(
    artifact: &ArtifactEnvelope,
    schema: &FactSchema<'_>,
    resolved: &[fact_literals::Resolved<'_>],
) -> Vec<Diagnostic> {
    let transitions = schema.values("transition").unwrap_or_default();
    let mut findings = Vec::new();
    for (index, property) in schema
        .values("lifecycle")
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let declaring: Vec<(&str, &EffectiveLifecycle)> = resolved
            .iter()
            .filter_map(|resolved| {
                resolved
                    .lifecycles
                    .iter()
                    .find(|effective| effective.lifecycle.property.as_str() == property)
                    .map(|effective| (resolved.kind, effective))
            })
            .collect();
        // A type whose lifecycles could not be resolved already failed closed
        // (`unresolved_lifecycles`); it may be the one that declares this
        // lifecycle, so no lifecycle is reported missing on its account.
        let every_type_resolved =
            resolved.len() == schema.values("objectType").unwrap_or_default().len();
        if declaring.is_empty() && every_type_resolved {
            let kinds: Vec<&str> = resolved.iter().map(|resolved| resolved.kind).collect();
            findings.push(finding(
                codes::NOT_A_LIFECYCLE,
                format!("{property:?} of {kinds:?} has no declared lifecycle to transition"),
                artifact,
                &enum_pointer("lifecycle", index),
            ));
            continue;
        }
        if declaring.is_empty() {
            continue;
        }
        for (transition_index, id) in transitions.iter().enumerate() {
            for (kind, effective) in &declaring {
                let declared = effective
                    .lifecycle
                    .transitions
                    .iter()
                    .any(|transition| transition.id.as_str() == *id);
                if !declared {
                    findings.push(finding(
                        codes::UNKNOWN_TRANSITION,
                        format!(
                            "{id:?} is not a declared transition of the {property:?} lifecycle of {kind:?}"
                        ),
                        artifact,
                        &enum_pointer("transition", transition_index),
                    ));
                }
            }
        }
    }
    findings
}

fn enum_pointer(name: &str, index: usize) -> String {
    format!("{ITEMS_POINTER}/properties/{name}/enum/{index}")
}

/// One object type a decision's fact changes may write.
struct Target<'s> {
    artifact: &'s ArtifactEnvelope,
    schema: &'s FactSchema<'s>,
    kind: &'s str,
    lifecycles: &'s [EffectiveLifecycle],
}

impl Target<'_> {
    /// `delete` never names a type with a lifecycle.
    fn delete_findings(&self) -> Option<Diagnostic> {
        if self.lifecycles.is_empty() {
            return None;
        }
        let kind = self.kind;
        let index = self
            .schema
            .values("op")
            .unwrap_or_default()
            .iter()
            .position(|op| *op == FactChangeOp::Delete.as_str())
            .unwrap_or_default();
        Some(finding(
            codes::DELETE_LIFECYCLE_OBJECT,
            format!(
                "the decision may delete {kind:?}, which has a declared lifecycle; delete then create would reset its status without a transition"
            ),
            self.artifact,
            &enum_pointer("op", index),
        ))
    }

    fn lifecycle(&self, property: &str) -> Option<&EffectiveLifecycle> {
        self.lifecycles
            .iter()
            .find(|lifecycle| lifecycle.lifecycle.property.as_str() == property)
    }

    /// `set` never names a lifecycle property.
    fn set_findings(&self) -> Vec<Diagnostic> {
        let kind = self.kind;
        self.schema
            .values("property")
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .filter(|(_, property)| self.lifecycle(property).is_some())
            .map(|(index, property)| {
                finding(
                    codes::DIRECT_WRITE,
                    format!(
                        "the decision sets {property:?} of {kind:?}, which its lifecycle owns; emit a transition fact change instead"
                    ),
                    self.artifact,
                    &enum_pointer("property", index),
                )
            })
            .collect()
    }

    /// `patch` never names a lifecycle property (#3080): every member of the
    /// `patch` record is checked against this type, like a `set` property.
    fn patch_findings(&self) -> Vec<Diagnostic> {
        let kind = self.kind;
        let Some(members) =
            member(self.schema.get("patch"), "properties").and_then(JsonValue::as_object)
        else {
            return Vec::new();
        };
        members
            .keys()
            .filter(|property| self.lifecycle(property).is_some())
            .map(|property| {
                finding(
                    codes::DIRECT_WRITE,
                    format!(
                        "the decision may patch {property:?} of {kind:?}, which its lifecycle owns; emit a transition fact change instead"
                    ),
                    self.artifact,
                    &format!("{ITEMS_POINTER}/properties/patch/properties/{property}"),
                )
            })
            .collect()
    }

    /// `create` writes a lifecycle property only as an enum of initial
    /// states (the write hook, [`check_lifecycle_writes`]).
    fn create_findings(&self) -> Vec<Diagnostic> {
        let kind = self.kind;
        let values = member(self.schema.get("values"), "properties").and_then(JsonValue::as_object);
        let mut findings = Vec::new();
        for effective in self.lifecycles {
            let property = effective.lifecycle.property.as_str();
            let Some(value_schema) = values.and_then(|values| values.get(property)) else {
                continue;
            };
            let pointer = format!("{ITEMS_POINTER}/properties/values/properties/{property}");
            let Some(states) = string_enum(Some(value_schema)) else {
                findings.push(finding(
                    codes::UNCONSTRAINED_STATE,
                    format!(
                        "the decision creates {kind:?} with lifecycle property {property:?}, which must be a string enum of initial states"
                    ),
                    self.artifact,
                    &pointer,
                ));
                continue;
            };
            let writes: Vec<LifecycleWrite<'_>> = states
                .iter()
                .enumerate()
                .map(|(index, state)| LifecycleWrite {
                    from: None,
                    to: state,
                    at: Span::pointer(format!("{pointer}/enum/{index}")),
                })
                .collect();
            findings.extend(
                check_lifecycle_writes(&effective.lifecycle, &writes)
                    .into_iter()
                    .map(|mut diagnostic| {
                        diagnostic.artifact_id = Some(self.artifact.artifact_id.clone());
                        diagnostic.remediation = Some(DiagnosticRemediation::FixDefinition);
                        diagnostic
                    }),
            );
        }
        findings
    }
}

pub(crate) fn finding(
    code: &str,
    message: String,
    artifact: &ArtifactEnvelope,
    pointer: &str,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}
