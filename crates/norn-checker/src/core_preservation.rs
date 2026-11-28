//! Core-ontology link binding, constitutional non-defeasibility, effective
//! core-law enrollment, and bounded core-law preservation with structured
//! counterexample witnesses (Track #4).
//!
//! Runs automatically as part of [`crate::check_artifact_set`] (default on, no
//! opt-in flag). Evidence boundaries are explicit:
//! - constitutional non-defeasibility, effective core-law enrollment, and
//!   required `core.*` link bindings on `create_only` actions are **structural**;
//! - action derivation/default/`setFields`, `synchronousRule` literal writes,
//!   and finite lifecycle soundness under governing `core.*` laws are
//!   **bounded** / **exhaustive-finite** within fixed state and work budgets,
//!   and emit [`codes::PRESERVATION_UNKNOWN`] (never a silent pass) if any
//!   budget is exhausted.

mod eval;
mod lifecycle;
mod writers;

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{Expression, Type};
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, ArtifactPayload, EntityRoot,
    InterfaceDef, InterfaceName, Invariant, JsonValue, LawIR, LawModality, LawResponse,
    UpsertWriteMode,
};

use crate::entity_artifacts::{Decoded, EntityArtifactSetContext, decode_v2};
use crate::entity_scopes::ScopedViews;
use crate::{
    Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, DiagnosticWitness,
    EffectiveCatalog, EffectiveObjectType, Span,
};

/// Stable `core.laws.*` preservation and constitutional-integrity codes.
pub mod codes {
    /// A downstream artifact attempts to weaken, shadow, except, or defeat a
    /// `core.*` constitutional law.
    pub const CONSTITUTIONAL_DEFEAT: &str = "core.laws.constitutional_defeat";
    /// A concept specializing `core.*` fails to enroll an applicable `core.*`
    /// law in its effective law closure or fails type binding on the subtype.
    pub const ENROLLMENT_GAP: &str = "core.laws.enrollment_gap";
    /// A `create_only` action targeting a `core.*`-specializing concept omits
    /// a `required: true` core interface link binding.
    pub const MISSING_REQUIRED_LINK_BINDING: &str = "core.laws.missing_required_link_binding";
    /// An action, `setFields` patch, `synchronousRule` writer, or lifecycle
    /// transition violates an inherited `core.*` law or leaves a reachable
    /// state with zero law-satisfying exits.
    pub const PRESERVATION_VIOLATED: &str = "core.laws.preservation_violated";
    /// Bounded finite core-law preservation exploration exhausted its work or
    /// state budget and failed closed rather than passing silently.
    pub const PRESERVATION_UNKNOWN: &str = "core.laws.preservation_unknown";
}

pub(crate) fn is_core_name(name: &str) -> bool {
    name.starts_with("core.")
}

pub(crate) fn decode_law(artifact: &ArtifactEnvelope) -> Option<LawIR> {
    if !matches!(artifact.payload, ArtifactPayload::Law(_)) {
        return None;
    }
    serde_json::to_value(artifact.payload.to_json_value())
        .and_then(serde_json::from_value)
        .ok()
}

pub(crate) fn decode_action(artifact: &ArtifactEnvelope) -> Option<ActionRoot> {
    if !matches!(artifact.payload, ArtifactPayload::Action(_)) {
        return None;
    }
    serde_json::to_value(artifact.payload.to_json_value())
        .and_then(serde_json::from_value)
        .ok()
}

pub(crate) fn finding(
    code: &str,
    artifact: &ArtifactEnvelope,
    pointer: &str,
    message: impl Into<String>,
    witness: Option<DiagnosticWitness>,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
        witness,
    }
}

/// Governing core constraint on a concrete `core.*`-specializing object type:
/// either a `core.*` `LawIR` predicate or a `core.*` interface invariant.
#[derive(Clone)]
pub(crate) struct GoverningCoreRule {
    pub(crate) id: String,
    pub(crate) condition: Option<String>,
    pub(crate) expression: String,
    /// `true` for `always` laws and integrity invariants; `false` for `never`.
    pub(crate) expected: bool,
}

/// Check core constitutional non-defeasibility, effective core-law enrollment,
/// required core-link bindings on create actions, and bounded core-law
/// preservation across writers and lifecycles (T3).
#[must_use]
pub fn check_core_preservation_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let base = context.base;
    let interfaces = collect_interfaces(base, artifacts);
    let base_laws: Vec<(&ArtifactEnvelope, LawIR)> = base
        .iter()
        .filter_map(|artifact| decode_law(artifact).map(|law| (artifact, law)))
        .collect();
    let candidate_laws: Vec<(&ArtifactEnvelope, LawIR)> = artifacts
        .iter()
        .filter_map(|artifact| decode_law(artifact).map(|law| (artifact, law)))
        .collect();

    let mut views = ScopedViews::new(artifacts, base, context.registry);
    let mut findings = Vec::new();

    check_constitutional_non_defeasibility(
        artifacts,
        &base_laws,
        &candidate_laws,
        &mut views,
        &mut findings,
    );
    check_core_law_enrollment(
        artifacts,
        &base_laws,
        &candidate_laws,
        &mut views,
        &mut findings,
    );
    check_required_core_link_bindings(artifacts, &interfaces, &mut views, &mut findings);
    writers::check_core_writer_preservation(
        artifacts,
        &interfaces,
        &base_laws,
        &candidate_laws,
        &mut views,
        &mut findings,
    );
    lifecycle::check_core_lifecycle_preservation(
        artifacts,
        &interfaces,
        &base_laws,
        &candidate_laws,
        &mut views,
        &mut findings,
    );

    findings
}

fn collect_interfaces(
    base: &[ArtifactEnvelope],
    artifacts: &[ArtifactEnvelope],
) -> BTreeMap<InterfaceName, InterfaceDef> {
    let mut interfaces = BTreeMap::new();
    for artifact in base.iter().chain(artifacts.iter()) {
        if let Decoded::Root(root) = decode_v2(artifact)
            && let EntityRoot::V2Interface(iface) = *root
        {
            interfaces
                .entry(iface.interface.name.clone())
                .or_insert(iface.interface);
        }
    }
    interfaces
}

pub(crate) fn interface_lineage<'a>(
    start: &'a InterfaceName,
    interfaces: &'a BTreeMap<InterfaceName, InterfaceDef>,
) -> Vec<&'a InterfaceDef> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    let mut stack = vec![start];
    while let Some(name) = stack.pop() {
        if !seen.insert(name) {
            continue;
        }
        if let Some(def) = interfaces.get(name) {
            result.push(def);
            for parent in def.extends.iter().rev() {
                stack.push(parent);
            }
        }
    }
    result
}

pub(crate) fn governing_core_rules(
    object_type: &EffectiveObjectType,
    catalog: &EffectiveCatalog,
    interfaces: &BTreeMap<InterfaceName, InterfaceDef>,
    base_laws: &[(&ArtifactEnvelope, LawIR)],
    candidate_laws: &[(&ArtifactEnvelope, LawIR)],
) -> Vec<GoverningCoreRule> {
    let mut rules = Vec::new();
    let mut seen_ids = BTreeSet::new();

    for iface_name in &object_type.interfaces {
        if !is_core_name(iface_name.as_str()) {
            continue;
        }
        for def in interface_lineage(iface_name, interfaces) {
            if !is_core_name(def.name.as_str()) {
                continue;
            }
            for invariant in &def.invariants {
                push_core_invariant(&def.name, invariant, &mut seen_ids, &mut rules);
            }
        }
    }

    let type_name = object_type.name.as_str();
    for (_, law) in base_laws.iter().chain(candidate_laws.iter()) {
        if !is_core_name(&law.id) && !is_core_name(&law.subject) {
            continue;
        }
        let applies = catalog
            .target_types(&law.subject)
            .is_some_and(|targets| targets.contains(type_name));
        if !applies {
            continue;
        }
        let expected = match law.modality {
            LawModality::Always {} => true,
            LawModality::Never {} => false,
            LawModality::Eventually {} | LawModality::Until {} => continue,
        };
        if let LawResponse::Predicate { expression } = &law.response
            && seen_ids.insert(law.id.clone())
        {
            rules.push(GoverningCoreRule {
                id: law.id.clone(),
                condition: law.condition.as_ref().map(|c| c.as_str().to_owned()),
                expression: expression.as_str().to_owned(),
                expected,
            });
        }
    }

    rules
}

fn push_core_invariant(
    interface_name: &InterfaceName,
    invariant: &Invariant,
    seen_ids: &mut BTreeSet<String>,
    rules: &mut Vec<GoverningCoreRule>,
) {
    let rule_id = format!("{}.{}", interface_name.as_str(), invariant.id.as_str());
    if seen_ids.insert(rule_id.clone()) {
        rules.push(GoverningCoreRule {
            id: rule_id,
            condition: None,
            expression: invariant.expression.as_str().to_owned(),
            expected: true,
        });
    }
}

/// Constitutional non-defeasibility: downstream packs cannot shadow, replace,
/// add exceptions to, defeat, or directly contradict a `core.*` law.
fn check_constitutional_non_defeasibility(
    artifacts: &[ArtifactEnvelope],
    base_laws: &[(&ArtifactEnvelope, LawIR)],
    candidate_laws: &[(&ArtifactEnvelope, LawIR)],
    views: &mut ScopedViews<'_>,
    findings: &mut Vec<Diagnostic>,
) {
    let core_laws_by_id: BTreeMap<&str, &LawIR> = base_laws
        .iter()
        .filter(|(_, law)| is_core_name(&law.id))
        .map(|(_, law)| (law.id.as_str(), law))
        .collect();
    let all_core_laws: Vec<&LawIR> = base_laws
        .iter()
        .chain(candidate_laws.iter())
        .map(|(_, law)| law)
        .filter(|law| is_core_name(&law.id))
        .collect();

    for (artifact, law) in candidate_laws {
        check_candidate_law_defeat(
            artifact,
            law,
            &core_laws_by_id,
            &all_core_laws,
            views,
            findings,
        );
    }

    for artifact in artifacts {
        if !is_core_name(&artifact.artifact_id) {
            check_normative_extension_defeat(artifact, findings);
        }
    }
}

fn check_candidate_law_defeat(
    artifact: &ArtifactEnvelope,
    law: &LawIR,
    core_laws_by_id: &BTreeMap<&str, &LawIR>,
    all_core_laws: &[&LawIR],
    views: &mut ScopedViews<'_>,
    findings: &mut Vec<Diagnostic>,
) {
    let is_core_artifact = is_core_name(&artifact.artifact_id);

    if is_core_name(&law.id) && !is_core_artifact {
        let reason = format!(
            "non-core artifact {:?} cannot declare or shadow constitutional law {:?}",
            artifact.artifact_id, law.id
        );
        findings.push(defeat_finding(
            artifact,
            "/payload/id",
            &law.id,
            &law.id,
            reason,
        ));
    }

    if let Some(installed) = core_laws_by_id.get(law.id.as_str())
        && *installed != law
    {
        let reason = if installed.exceptions == law.exceptions {
            format!(
                "installed core law {:?} is constitutional and cannot be weakened or replaced",
                installed.id
            )
        } else {
            format!(
                "installed core law {:?} is non-defeasible; downstream artifacts cannot add or alter its exceptions",
                installed.id
            )
        };
        findings.push(defeat_finding(
            artifact,
            "/payload/id",
            &law.id,
            &installed.id,
            reason,
        ));
    }

    for (index, exception) in law.exceptions.iter().enumerate() {
        if is_core_name(&exception.overrides)
            && (!is_core_artifact || core_laws_by_id.contains_key(exception.overrides.as_str()))
        {
            let reason = format!(
                "exception {:?} attempts to override constitutional core law {:?}",
                exception.id, exception.overrides
            );
            findings.push(defeat_finding(
                artifact,
                &format!("/payload/exceptions/{index}/overrides"),
                &law.id,
                &exception.overrides,
                reason,
            ));
        }
    }

    if !is_core_artifact {
        let catalog = views.of(artifact).catalog();
        let candidate_targets = catalog.target_types(&law.subject);
        for core_law in all_core_laws {
            if core_law.id == law.id || !contradicts_core_modality(core_law, law) {
                continue;
            }
            let overlaps = core_law.subject == law.subject
                || match (catalog.target_types(&core_law.subject), &candidate_targets) {
                    (Some(core_t), Some(cand_t)) => !core_t.is_disjoint(cand_t),
                    (Some(_), None) | (None, Some(_) | None) => false,
                };
            if overlaps {
                let reason = format!(
                    "downstream law {:?} directly contradicts constitutional core law {:?} on subject {:?}",
                    law.id, core_law.id, law.subject
                );
                findings.push(defeat_finding(
                    artifact,
                    "/payload/modality",
                    &law.id,
                    &core_law.id,
                    reason,
                ));
            }
        }
    }
}

fn defeat_finding(
    artifact: &ArtifactEnvelope,
    pointer: &str,
    offending_law_id: &str,
    core_law_id: &str,
    reason: String,
) -> Diagnostic {
    finding(
        codes::CONSTITUTIONAL_DEFEAT,
        artifact,
        pointer,
        reason.as_str(),
        Some(DiagnosticWitness::core_law_weakening(
            artifact.artifact_id.as_str(),
            offending_law_id,
            core_law_id,
            reason,
        )),
    )
}

fn contradicts_core_modality(core_law: &LawIR, candidate: &LawIR) -> bool {
    let opposite = matches!(
        (&core_law.modality, &candidate.modality),
        (LawModality::Always {}, LawModality::Never {})
            | (LawModality::Never {}, LawModality::Always {})
    );
    if !opposite {
        return false;
    }
    let same_condition = match (&core_law.condition, &candidate.condition) {
        (None, None) => true,
        (Some(a), Some(b)) => a.as_str().trim() == b.as_str().trim(),
        (Some(_), None) | (None, Some(_)) => false,
    };
    if !same_condition {
        return false;
    }
    let (
        LawResponse::Predicate { expression: core_e },
        LawResponse::Predicate { expression: cand_e },
    ) = (&core_law.response, &candidate.response)
    else {
        return false;
    };
    core_e.as_str().trim() == cand_e.as_str().trim()
}

fn check_normative_extension_defeat(artifact: &ArtifactEnvelope, findings: &mut Vec<Diagnostic>) {
    let payload_json = artifact.payload.to_json_value();
    let Some(normative) = payload_json
        .as_object()
        .and_then(|object| object.get("extensions"))
        .and_then(JsonValue::as_object)
        .and_then(|extensions| extensions.get("norn.normative/1.0.0"))
    else {
        return;
    };
    let rules: Vec<&JsonValue> = match normative {
        JsonValue::Array(items) => items.iter().collect(),
        JsonValue::Object(obj) => match obj.get("rules") {
            Some(JsonValue::Array(items)) => items.iter().collect(),
            Some(
                JsonValue::Null
                | JsonValue::Bool(_)
                | JsonValue::Number(_)
                | JsonValue::String(_)
                | JsonValue::Object(_),
            )
            | None => vec![normative],
        },
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::String(_) => {
            Vec::new()
        }
    };
    for (index, rule) in rules.into_iter().enumerate() {
        let Some(obj) = rule.as_object() else {
            continue;
        };
        let rule_id = obj
            .get("id")
            .and_then(JsonValue::as_string)
            .unwrap_or(artifact.artifact_id.as_str());
        let pointer = format!("/payload/extensions/norn.normative~11.0.0/{index}");
        if let Some(JsonValue::Array(defeated)) = obj.get("defeatedBy") {
            for target in defeated.iter().filter_map(JsonValue::as_string) {
                if is_core_name(target) {
                    let reason = format!(
                        "normative rule {rule_id:?} lists constitutional core law {target:?} in defeatedBy"
                    );
                    findings.push(defeat_finding(artifact, &pointer, rule_id, target, reason));
                }
            }
        }
        if let Some(JsonValue::Array(exceptions)) = obj.get("exceptions") {
            for exception in exceptions {
                if let Some(overrides) = exception
                    .as_object()
                    .and_then(|ex| ex.get("overrides"))
                    .and_then(JsonValue::as_string)
                    && is_core_name(overrides)
                {
                    let reason = format!(
                        "normative rule {rule_id:?} declares an exception overriding constitutional core law {overrides:?}"
                    );
                    findings.push(defeat_finding(
                        artifact, &pointer, rule_id, overrides, reason,
                    ));
                }
            }
        }
        let is_permission = obj
            .get("modality")
            .and_then(JsonValue::as_string)
            .is_some_and(|m| m.eq_ignore_ascii_case("permission"));
        if is_permission
            && let Some(law_id) = obj
                .get("law")
                .and_then(JsonValue::as_object)
                .and_then(|l| l.get("id"))
                .and_then(JsonValue::as_string)
            && is_core_name(law_id)
        {
            let reason = format!(
                "normative permission {rule_id:?} cannot defeat or relax constitutional core law {law_id:?}"
            );
            findings.push(defeat_finding(artifact, &pointer, rule_id, law_id, reason));
        }
    }
}

/// Verify that every candidate concrete concept specializing `core.*` enrolls
/// every applicable `core.*` law in its effective law closure and binds the
/// law's predicates cleanly against the concrete subtype environment.
fn check_core_law_enrollment(
    artifacts: &[ArtifactEnvelope],
    base_laws: &[(&ArtifactEnvelope, LawIR)],
    candidate_laws: &[(&ArtifactEnvelope, LawIR)],
    views: &mut ScopedViews<'_>,
    findings: &mut Vec<Diagnostic>,
) {
    let core_laws: Vec<&LawIR> = base_laws
        .iter()
        .chain(candidate_laws.iter())
        .map(|(_, law)| law)
        .filter(|law| is_core_name(&law.id))
        .collect();
    if core_laws.is_empty() {
        return;
    }

    for artifact in artifacts {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        let EntityRoot::V2(object_root) = *root else {
            continue;
        };
        let type_name = object_root.object_type.name.as_str();
        let view = views.of(artifact);
        let catalog = view.catalog();
        let Some(Ok(effective)) = catalog.object_type_named(type_name) else {
            continue;
        };
        let core_ifaces: BTreeSet<&str> = effective
            .interfaces
            .iter()
            .map(InterfaceName::as_str)
            .filter(|name| is_core_name(name))
            .collect();
        let lineage: BTreeSet<&str> = effective.lineage.iter().map(|n| n.as_str()).collect();
        if core_ifaces.is_empty() && !lineage.iter().any(|n| is_core_name(n)) {
            continue;
        }
        let bound_env = view
            .env()
            .and_then(|env| env.clone().bind_self(&effective.name).ok());
        for law in &core_laws {
            let subject = law.subject.as_str();
            if !core_ifaces.contains(subject) && !lineage.contains(subject) {
                continue;
            }
            if !catalog
                .target_types(subject)
                .is_some_and(|targets| targets.contains(type_name))
            {
                let reason = format!(
                    "concept {type_name:?} specializes {subject:?} but failed to enroll core law {:?} in its effective law closure",
                    law.id
                );
                findings.push(finding(
                    codes::ENROLLMENT_GAP,
                    artifact,
                    "/payload/objectType/implements",
                    reason.as_str(),
                    Some(DiagnosticWitness::core_law_weakening(
                        artifact.artifact_id.as_str(),
                        type_name,
                        law.id.as_str(),
                        reason,
                    )),
                ));
                continue;
            }
            let Some(env) = bound_env.as_ref() else {
                continue;
            };
            check_law_binding_on_subtype(artifact, type_name, law, env, findings);
        }
    }
}

fn check_law_binding_on_subtype(
    artifact: &ArtifactEnvelope,
    type_name: &str,
    law: &LawIR,
    env: &norn_expr::ZenTypeEnv,
    findings: &mut Vec<Diagnostic>,
) {
    let mut exprs: Vec<(&str, &str)> = Vec::new();
    if let Some(cond) = &law.condition {
        exprs.push(("condition", cond.as_str()));
    }
    if let LawResponse::Predicate { expression } = &law.response {
        exprs.push(("response", expression.as_str()));
    }
    for (slot, source) in exprs {
        let Ok(compiled) = Expression::compile(source) else {
            continue;
        };
        if let Err(errors) = compiled.check_typed(env, &Type::Bool) {
            let detail = errors.first().map_or_else(
                || "type error binding core law on subtype".to_owned(),
                |err| err.message.clone(),
            );
            let reason = format!(
                "inherited core law {:?} ({slot}) fails to bind on specializing concept {type_name:?}: {detail}",
                law.id
            );
            findings.push(finding(
                codes::ENROLLMENT_GAP,
                artifact,
                "/payload/objectType/implements",
                reason.as_str(),
                Some(DiagnosticWitness::core_law_weakening(
                    artifact.artifact_id.as_str(),
                    type_name,
                    law.id.as_str(),
                    reason,
                )),
            ));
            break;
        }
    }
}

/// Verify that every candidate `create_only` action targeting a concept
/// specializing a `core.*` interface explicitly binds every `required: true`
/// link declared on those `core.*` interfaces (via `createLinks` or a
/// synchronous rule emitting links).
fn check_required_core_link_bindings(
    artifacts: &[ArtifactEnvelope],
    interfaces: &BTreeMap<InterfaceName, InterfaceDef>,
    views: &mut ScopedViews<'_>,
    findings: &mut Vec<Diagnostic>,
) {
    for artifact in artifacts {
        let Some(root) = decode_action(artifact) else {
            continue;
        };
        let Some(ActionEffectProgram::UpsertInputObject {
            object_kind,
            write_mode: UpsertWriteMode::CreateOnly,
            ..
        }) = &root.effect_program
        else {
            continue;
        };
        let view = views.of(artifact);
        let Some(Ok(effective)) = view.catalog().object_type_named(object_kind) else {
            continue;
        };
        let required_core_links = required_core_interface_links(effective, interfaces);
        if required_core_links.is_empty() {
            continue;
        }
        let rule_emits_links = root.synchronous_rule.as_ref().is_some_and(|rule_ref| {
            synchronous_rule_declares_links(view.pool(), &rule_ref.artifact_id, &rule_ref.version)
        });
        if rule_emits_links {
            continue;
        }
        let bound_links: BTreeSet<&str> = root
            .create_links
            .iter()
            .filter(|binding| {
                binding
                    .source
                    .as_ref()
                    .is_none_or(|src| src.object_type == *object_kind)
            })
            .map(|binding| binding.link.as_str())
            .collect();
        for (link_id, (iface_name, target_name)) in required_core_links {
            if bound_links.contains(link_id.as_str()) {
                continue;
            }
            let expected =
                format!("createLinks binding for required link {link_id} -> {target_name}");
            let actual = format!("unbound in create_only action {}", root.action_id);
            findings.push(finding(
                codes::MISSING_REQUIRED_LINK_BINDING,
                artifact,
                "/payload/createLinks",
                format!(
                    "create_only action {:?} creates {:?} (implementing {:?}) without binding required core link {:?} -> {:?}",
                    root.action_id, object_kind, iface_name, link_id, target_name
                ),
                Some(DiagnosticWitness::core_link(
                    object_kind.as_str(),
                    iface_name,
                    link_id,
                    expected,
                    actual,
                )),
            ));
        }
    }
}

fn required_core_interface_links(
    effective: &EffectiveObjectType,
    interfaces: &BTreeMap<InterfaceName, InterfaceDef>,
) -> BTreeMap<String, (String, String)> {
    let mut required = BTreeMap::new();
    for iface_name in &effective.interfaces {
        if !is_core_name(iface_name.as_str()) {
            continue;
        }
        for def in interface_lineage(iface_name, interfaces) {
            if !is_core_name(def.name.as_str()) {
                continue;
            }
            for link in &def.links {
                if link.cardinality.required {
                    required
                        .entry(link.id.as_str().to_owned())
                        .or_insert_with(|| {
                            (
                                def.name.as_str().to_owned(),
                                link.target.as_str().to_owned(),
                            )
                        });
                }
            }
        }
    }
    required
}

pub(crate) fn synchronous_rule_declares_links(
    pool: &[ArtifactEnvelope],
    artifact_id: &str,
    version: &str,
) -> bool {
    let Some(rule) = pool.iter().find(|artifact| {
        artifact.kind() == ArtifactKind::Rule
            && artifact.artifact_id == artifact_id
            && artifact.version == version
    }) else {
        return false;
    };
    rule.payload
        .to_json_value()
        .as_object()
        .and_then(|payload| payload.get("outputSchema"))
        .and_then(JsonValue::as_object)
        .and_then(|schema| schema.get("properties"))
        .and_then(JsonValue::as_object)
        .is_some_and(|props| props.contains_key("links"))
}
