//! The `flow` retained-definition kind: an automation drawn on the canvas,
//! lowered by `lowerFlow` into exactly two kernel artifacts, its Run entity
//! (whose lifecycle is the flow's state machine) and its `decide` rule.
//!
//! A flow is installed into the cell's kernel set: the checker admits the
//! active set with this flow's previous artifacts replaced, the Run type
//! enters the model, and one governed commit installs the new set with the
//! flow's revision. The artifacts are retained verbatim, so the run driver
//! reads them back from the active set. Retiring keeps them (run history
//! stays interpretable) and only marks the flow `retired`.
//!
//! Payload `{flow: {id, version}, artifacts: [<Run entity>, <decide rule>],
//! layout?}`.

use std::collections::BTreeMap;

use norn_ir::{ArtifactEnvelope, ArtifactKind, RuntimeValue};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::kernel_set::{self, AdmittedSet, KernelRefusal};
use crate::refusal::{Refusal, RefusalDiagnostic};

/// Stable `definition.flow_*` codes.
pub mod codes {
    /// The payload is not the flow shape, or the artifacts are not exactly
    /// `lowerFlow`'s output.
    pub const SHAPE_INVALID: &str = "definition.flow_shape_invalid";
    /// The definition id is not `flow.id`.
    pub const ID_MISMATCH: &str = "definition.flow_id_mismatch";
    /// An artifact the flow does not own.
    pub const ARTIFACT_FOREIGN: &str = "definition.flow_artifact_foreign";
    /// A flow may not declare actions.
    pub const ACTION_DECLARED: &str = "definition.flow_action_declared";
    /// A `perform` names no registered built-in and no installed binding.
    pub const FUNCTION_UNKNOWN: &str = "definition.flow_function_unknown";
    /// An artifact scoped outside the cell.
    pub const SCOPE_CONFLICT: &str = "definition.flow_scope_conflict";
    /// Retiring a flow while a run may still be live.
    pub const RUNS_LIVE: &str = "definition.flow_runs_live";
}

/// What a flow's install, read and list report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlowSummary {
    /// The flow id, also the definition id.
    pub flow_id: String,
    /// The flow's authored version.
    pub version: String,
    /// The `decide` rule's artifact id.
    pub rule_artifact_id: String,
    /// The Run object type.
    pub run_type: String,
}

/// Registered built-in functions: `(function, version)` resolves.
pub type Builtins = fn(&str, u32) -> bool;

/// Whether `id` is dotted lower-case segments, as `lowerFlow` requires.
#[must_use]
pub fn valid_flow_id(id: &str) -> bool {
    let segments: Vec<&str> = id.split('.').collect();
    segments.len() >= 2
        && segments.iter().all(|segment| {
            let mut bytes = segment.bytes();
            bytes.next().is_some_and(|first| first.is_ascii_lowercase())
                && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// The Run type `lowerFlow` derives: the namespace, then the last segment in
/// PascalCase (split on `_`) followed by `Run`.
#[must_use]
pub fn run_type(flow_id: &str) -> String {
    let (namespace, name) = flow_id.rsplit_once('.').unwrap_or(("", flow_id));
    let pascal: String = name
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect();
    format!("{namespace}.{pascal}Run")
}

/// The two artifact ids a flow owns.
#[must_use]
pub fn owned_ids(flow_id: &str) -> (String, String) {
    (
        format!("{}.entity", run_type(flow_id)),
        format!("{flow_id}.flow.rule"),
    )
}

/// A checked flow: its two artifacts and summary.
#[derive(Debug, Clone)]
pub struct Flow {
    /// The Run entity, then the rule.
    pub artifacts: Vec<ArtifactEnvelope>,
    /// Their indices in the submitted `artifacts`, in the same order.
    pub indices: [usize; 2],
    /// What the install reports.
    pub summary: FlowSummary,
}

/// An artifact payload as plain JSON for structural reads (numbers are not
/// judged here; the checker admits the exact values).
fn json(payload: &norn_ir::JsonValue) -> Value {
    serde_json::to_value(payload).unwrap_or(Value::Null)
}

fn error(code: &str, message: impl Into<String>, pointer: impl Into<String>) -> RefusalDiagnostic {
    RefusalDiagnostic::error(code, message, pointer)
}

/// Every finding for a flow payload (its canonical text) installed as `id`,
/// against the registered built-ins and the active bindings' versions, and
/// the flow when there are none.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one flow's shape, ownership, scope and perform rules, checked together in payload order"
)]
pub fn check(
    id: &str,
    canonical: &str,
    cell_scope: &crate::scope::CellScope,
    builtins: Option<Builtins>,
    bindings: &BTreeMap<String, u64>,
) -> (Vec<RefusalDiagnostic>, Option<Flow>) {
    let mut findings = Vec::new();
    let (Ok(value), Ok(exact)) = (
        serde_json::from_str::<Value>(canonical),
        RuntimeValue::from_json_text(canonical),
    ) else {
        findings.push(error(codes::SHAPE_INVALID, "a flow is a JSON object", ""));
        return (findings, None);
    };
    let Some(object) = value.as_object() else {
        findings.push(error(codes::SHAPE_INVALID, "a flow is an object", ""));
        return (findings, None);
    };
    for key in object
        .keys()
        .filter(|key| !["flow", "artifacts", "layout"].contains(&key.as_str()))
    {
        findings.push(error(
            codes::SHAPE_INVALID,
            format!("unknown member {key:?}"),
            format!("/{key}"),
        ));
    }
    let flow_id = value
        .pointer("/flow/id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let version = value
        .pointer("/flow/version")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !valid_flow_id(flow_id) {
        findings.push(error(
            codes::SHAPE_INVALID,
            "flow.id is dotted lower-case segments (namespace.name)",
            "/flow/id",
        ));
    } else if flow_id != id {
        findings.push(error(
            codes::ID_MISMATCH,
            "the definition id is flow.id",
            "/flow/id",
        ));
    }
    if version.trim().is_empty() {
        findings.push(error(
            codes::SHAPE_INVALID,
            "flow.version is a non-blank string",
            "/flow/version",
        ));
    }
    let Some(RuntimeValue::Array(sources)) = exact.get("artifacts") else {
        findings.push(error(
            codes::SHAPE_INVALID,
            "artifacts is lowerFlow's artifact list",
            "/artifacts",
        ));
        return (findings, None);
    };
    let (entity_id, rule_id) = owned_ids(flow_id);
    let mut artifacts = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let at = format!("/artifacts/{index}");
        let Ok(artifact) = norn_ir::runtime::decode::<ArtifactEnvelope>(source) else {
            findings.push(error(codes::SHAPE_INVALID, "not an artifact envelope", at));
            continue;
        };
        if norn_checker::scope_conflict(&cell_scope.as_scope_ref(), &artifact.scope).is_some() {
            findings.push(error(
                codes::SCOPE_CONFLICT,
                "the artifact is scoped outside this cell",
                format!("{at}/scope"),
            ));
        }
        if artifact.kind() == ArtifactKind::Action {
            findings.push(error(
                codes::ACTION_DECLARED,
                "a flow references existing actions; it declares none",
                format!("{at}/kind"),
            ));
        } else if artifact.artifact_id != entity_id && artifact.artifact_id != rule_id {
            findings.push(error(
                codes::ARTIFACT_FOREIGN,
                format!("a flow owns exactly {entity_id} and {rule_id}"),
                format!("{at}/artifactId"),
            ));
        }
        artifacts.push((index, artifact));
    }
    let find = |wanted: &str, kind: ArtifactKind| {
        artifacts
            .iter()
            .filter(|(_, artifact)| artifact.artifact_id == wanted && artifact.kind() == kind)
            .collect::<Vec<_>>()
    };
    let (entities, rules) = (
        find(&entity_id, ArtifactKind::Entity),
        find(&rule_id, ArtifactKind::Rule),
    );
    let (entity_index, entity, rule_index, rule) = match (entities.as_slice(), rules.as_slice()) {
        ([entity], [rule]) if sources.len() == 2 => (entity.0, &entity.1, rule.0, &rule.1),
        _ => {
            findings.push(error(
                codes::SHAPE_INVALID,
                format!("artifacts are exactly the Run entity {entity_id} and the rule {rule_id}"),
                "/artifacts",
            ));
            return (findings, None);
        }
    };
    let rule_at = format!("/artifacts/{rule_index}");
    if rule.version != version {
        findings.push(error(
            codes::SHAPE_INVALID,
            "the rule's version is flow.version",
            format!("{rule_at}/version"),
        ));
    }
    let rule_payload = json(&rule.payload.to_json_value());
    if rule_payload.get("contract").and_then(Value::as_str) != Some("decide") {
        findings.push(error(
            codes::SHAPE_INVALID,
            "the rule's contract is decide",
            format!("{rule_at}/payload/contract"),
        ));
    }
    let entity_deps: Vec<_> = rule
        .dependencies
        .iter()
        .filter(|dependency| dependency.kind == ArtifactKind::Entity)
        .collect();
    if !matches!(entity_deps.as_slice(), [only] if only.artifact_id == entity_id) {
        findings.push(error(
            codes::SHAPE_INVALID,
            format!("the rule depends on exactly one entity, {entity_id}"),
            format!("{rule_at}/dependencies"),
        ));
    }
    let lifecycles = json(&entity.payload.to_json_value())
        .pointer("/objectType/lifecycles")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let initial = lifecycles
        .first()
        .and_then(|lifecycle| lifecycle["initial"].as_array())
        .is_some_and(|initial| !initial.is_empty());
    if lifecycles.len() != 1 || !initial {
        findings.push(error(
            codes::SHAPE_INVALID,
            "the Run type has exactly one lifecycle with an initial state",
            format!("/artifacts/{entity_index}/payload/objectType/lifecycles"),
        ));
    }
    match norn_checker::flow_performs::perform_refs(&rule_payload) {
        Err((pointer, message)) => findings.push(error(
            codes::SHAPE_INVALID,
            format!("expression does not parse: {message}"),
            format!("{rule_at}/payload{pointer}"),
        )),
        Ok(performs) => {
            for perform in performs {
                let at = format!("{rule_at}/payload{}", perform.pointer);
                let resolved = perform.name.as_ref().is_some_and(|(function, version)| {
                    let builtin = version.parse::<u32>().is_ok_and(|number| {
                        builtins.is_some_and(|resolve| resolve(function, number))
                    });
                    builtin
                        || bindings
                            .get(function)
                            .is_some_and(|bound| bound.to_string() == *version)
                });
                if !resolved {
                    let named = perform.name.map_or_else(
                        || "a function that is not a literal".to_owned(),
                        |(f, v)| format!("{f}@{v}"),
                    );
                    findings.push(error(
                        codes::FUNCTION_UNKNOWN,
                        format!("this step performs {named}: neither a registered built-in nor an installed binding at that version"),
                        at,
                    ));
                }
            }
        }
    }
    let flow = Flow {
        artifacts: vec![entity.clone(), rule.clone()],
        indices: [entity_index, rule_index],
        summary: FlowSummary {
            flow_id: flow_id.to_owned(),
            version: version.to_owned(),
            rule_artifact_id: rule_id,
            run_type: run_type(flow_id),
        },
    };
    (findings, Some(flow))
}

/// The binding version a flow's performs pin `binding_id` to, if any.
#[must_use]
pub fn pinned_version(canonical: &str, binding_id: &str) -> Option<String> {
    let value: Value = serde_json::from_str(canonical).ok()?;
    let flow_id = value.pointer("/flow/id")?.as_str()?;
    let (_, rule_id) = owned_ids(flow_id);
    let rule = value["artifacts"]
        .as_array()?
        .iter()
        .find(|artifact| artifact["artifactId"].as_str() == Some(rule_id.as_str()))?;
    norn_checker::flow_performs::perform_refs(&rule["payload"])
        .ok()?
        .into_iter()
        .find_map(|perform| {
            perform
                .name
                .filter(|(function, _)| function == binding_id)
                .map(|(_, version)| version)
        })
}

/// The active kernel set with `flow`'s artifacts in place of any it already
/// owns, admitted. Findings on the flow's artifacts point into the payload.
///
/// # Errors
/// The checker's refusal, as `definition.refused` diagnostics.
pub fn rebuild(active_set: &str, flow: &Flow) -> Result<AdmittedSet, Refusal> {
    let owned: Vec<&str> = flow
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    let mut artifacts: Vec<ArtifactEnvelope> = kernel_set::retained_artifacts(active_set)
        .map_err(Refusal::host)?
        .into_iter()
        .filter(|artifact| !owned.contains(&artifact.artifact_id.as_str()))
        .collect();
    artifacts.extend(flow.artifacts.iter().cloned());
    kernel_set::admit_kernel_set(&artifacts).map_err(|refusal| {
        let diagnostics = match refusal {
            KernelRefusal::Refused(findings) => findings
                .iter()
                .map(|finding| {
                    let index = finding
                        .artifact_id
                        .as_deref()
                        .and_then(|id| owned.iter().position(|owned| *owned == id))
                        .and_then(|at| flow.indices.get(at).copied());
                    let mut diagnostic =
                        RefusalDiagnostic::from_checker(std::slice::from_ref(finding), "")
                            .pop()
                            .unwrap_or_else(|| {
                                RefusalDiagnostic::error(codes::SHAPE_INVALID, "", "")
                            });
                    diagnostic.span = index.map(|index| crate::refusal::RefusalSpan {
                        pointer: format!(
                            "/artifacts/{index}{}",
                            finding
                                .span
                                .as_ref()
                                .map_or("", |span| span.pointer.as_str())
                        ),
                    });
                    diagnostic
                })
                .collect(),
            KernelRefusal::Invalid(detail) => vec![RefusalDiagnostic {
                code: codes::SHAPE_INVALID.to_owned(),
                severity: "error".to_owned(),
                message: detail,
                law: None,
                span: None,
            }],
        };
        crate::definition_routes::refused_with(diagnostics)
    })
}
