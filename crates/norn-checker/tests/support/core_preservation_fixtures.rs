//! Shared fixture loaders and builders for `tests/core_preservation.rs`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use norn_checker::{Diagnostic, EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, PropertyTypeName};
use serde_json::{Value, json};

pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub(crate) fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} should be valid JSON: {error}", path.display()))
}

pub(crate) fn decode_envelopes(values: Vec<Value>) -> Vec<ArtifactEnvelope> {
    values
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("artifact envelope decodes"))
        .collect()
}

pub(crate) fn load_pack_values(pack: &str) -> Vec<Value> {
    let dir = repo_root()
        .join("registry/packs")
        .join(pack)
        .join("artifacts");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{} should exist: {error}", dir.display()))
        .map(|entry| entry.expect("artifact entry").path())
        .collect();
    paths.sort();
    paths.iter().map(|path| read_json(path)).collect()
}

pub(crate) fn core_base_values() -> Vec<Value> {
    let core = repo_root().join("registry/core/artifacts");
    let mut values: Vec<Value> = [
        "core.Agent.interface.json",
        "core.AgentRole.interface.json",
        "core.Resource.interface.json",
        "core.Commitment.interface.json",
        "core.Hold.interface.json",
        "core.EconomicEvent.interface.json",
        "core.LedgerEffect.interface.json",
        "core.Claim.interface.json",
        "core.HoldableResource.interface.json",
        "core.ValuedEvent.interface.json",
        "core.ValuedLedgerEffect.interface.json",
        "core.ReversibleEvent.interface.json",
        "core.TimeInterval.interface.json",
        "core.EconomicEvent.reversal_matches_original.law.json",
        "core.Hold.measure_matches_resource.law.json",
        "core.HoldableResource.open_holds_within_quantity.law.json",
        "core.ReversibleEvent.no_reversal_of_reversal.law.json",
        "core.ReversibleEvent.reversal_flag_requires_link.law.json",
        "core.ValuedEvent.effects_balance.law.json",
        "core.ValuedLedgerEffect.currency_matches_event.law.json",
        "core.value_types.entity.json",
        "std.value_types.entity.json",
        "core.units.json",
        "core.time.json",
    ]
    .iter()
    .map(|name| read_json(&core.join(name)))
    .collect();
    values.push(read_json(
        &repo_root().join("registry/privacy/artifacts/privacy.markings.entity.json"),
    ));
    values.push(json!({
        "irVersion": "0.1",
        "artifactId": "core.extra_markings.entity",
        "kind": "entity",
        "namespace": "core",
        "name": "Core extra markings",
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "system" },
        "payload": {
            "entityVersion": "2.0",
            "markings": [
                {
                    "id": "pii.financial",
                    "label": "Financial PII",
                    "severity": "high",
                    "category": "pii.financial",
                    "mask": { "kind": "redacted" }
                },
                {
                    "id": "pii.personal",
                    "label": "Personal PII",
                    "severity": "high",
                    "category": "pii.personal",
                    "mask": { "kind": "redacted" }
                }
            ]
        }
    }));
    values
}

pub(crate) fn check_over_core(artifacts: Vec<Value>) -> Vec<Diagnostic> {
    let base = decode_envelopes(core_base_values());
    let candidates = decode_envelopes(artifacts);
    let registry = BTreeSet::<PropertyTypeName>::new();
    check_artifact_set(
        &candidates,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
    )
}

pub(crate) fn errors_of(findings: &[Diagnostic]) -> Vec<&Diagnostic> {
    findings.iter().filter(|d| d.is_error()).collect()
}

pub(crate) fn find_mut<'a>(values: &'a mut [Value], artifact_id: &str) -> &'a mut Value {
    values
        .iter_mut()
        .find(|value| value["artifactId"] == artifact_id)
        .unwrap_or_else(|| panic!("fixture {artifact_id} not found"))
}

pub(crate) fn core_predicate_law(
    artifact_id: &str,
    law_id: &str,
    subject: &str,
    dep_artifact_id: &str,
    expression: &str,
) -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": "law",
        "namespace": "core",
        "name": law_id,
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [
            {
                "artifactId": dep_artifact_id,
                "kind": "entity",
                "version": "0.2.0",
                "required": true
            }
        ],
        "provenance": { "source": "system" },
        "payload": {
            "lawVersion": "0.1",
            "id": law_id,
            "owner": "core",
            "severity": "error",
            "rationale": "Test constitutional core law",
            "provenance": { "source": "system" },
            "scope": {
                "root": { "kind": "pack", "id": "core" },
                "objectTypes": [subject]
            },
            "subject": subject,
            "modality": { "kind": "always" },
            "response": {
                "kind": "predicate",
                "expression": expression
            }
        }
    })
}

pub(crate) fn deontic_rule_json(
    law_id: &str,
    subject: &str,
    artifact_id: &str,
    index: usize,
    modality: &str,
    law_modality: &str,
    defeated_by: &[&str],
) -> Value {
    json!({
        "law": {
            "lawVersion": "0.1",
            "id": law_id,
            "owner": if law_id.starts_with("core.") { "core" } else { "acme" },
            "severity": "error",
            "rationale": "Normative precedence regression test rule",
            "provenance": { "source": "system" },
            "scope": {
                "root": { "kind": "company", "id": "acme" },
                "objectTypes": [subject]
            },
            "subject": subject,
            "modality": { "kind": law_modality },
            "response": {
                "kind": "predicate",
                "expression": "self.paid"
            }
        },
        "modality": modality,
        "authorityId": "authority.test",
        "authorityVersion": "1.0.0",
        "source": {
            "artifactId": artifact_id,
            "pointer": format!("/payload/extensions/norn.normative~11.0.0/{index}")
        },
        "defeatedBy": defeated_by
    })
}

pub(crate) fn boolean_entity_envelope(artifact_id: &str, object_name: &str, rules: Value) -> Value {
    let namespace = object_name.split('.').next().unwrap_or("core");
    json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": "entity",
        "namespace": namespace,
        "name": object_name,
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "system" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": object_name,
                "properties": [
                    { "id": "paid", "type": { "kind": "boolean" } }
                ]
            },
            "extensions": {
                "norn.normative/1.0.0": rules
            }
        }
    })
}

pub(crate) fn sync_links_rule(
    artifact_id: &str,
    rule_id: &str,
    links_expr: &str,
    required_links: bool,
) -> Value {
    let mut rule =
        read_json(&repo_root().join("fixtures/rule-ir-v2/valid/purchase-order-approval.rule.json"));
    rule["artifactId"] = json!(artifact_id);
    rule["namespace"] = json!("compute");
    rule["lifecycle"] = json!("active");
    rule["scope"] = json!({});
    rule["dependencies"] = json!([
        {
            "artifactId": "compute.QuotaHold.entity",
            "kind": "entity",
            "version": "0.1.0",
            "required": true
        },
        {
            "artifactId": "compute.TokenPool.entity",
            "kind": "entity",
            "version": "0.1.0",
            "required": true
        }
    ]);
    rule["payload"]["ruleId"] = json!(rule_id);
    rule["payload"]["contract"] = json!("decide");
    rule["payload"]["outputSchema"] = json!({
        "type": "object",
        "properties": {
            "links": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["add"] },
                        "sourceType": { "type": "string", "enum": ["compute.QuotaHold"] },
                        "sourceKey": { "type": "string" },
                        "link": { "type": "string", "enum": ["resource"] },
                        "targetType": { "type": "string", "enum": ["compute.TokenPool"] },
                        "targetKey": { "type": "string" }
                    },
                    "required": ["op", "sourceType", "sourceKey", "link", "targetType", "targetKey"],
                    "additionalProperties": false
                }
            }
        },
        "required": if required_links { json!(["links"]) } else { json!([]) },
        "additionalProperties": false
    });
    rule["payload"]["graph"] = json!({
        "nodes": [
            { "id": "in", "name": "Input", "type": "inputNode", "position": { "x": 0, "y": 0 } },
            {
                "id": "expr",
                "name": "Emit links",
                "type": "expressionNode",
                "position": { "x": 100, "y": 0 },
                "content": {
                    "expressions": [
                        {
                            "id": "e1",
                            "key": "links",
                            "value": links_expr
                        }
                    ]
                }
            },
            { "id": "out", "name": "Output", "type": "outputNode", "position": { "x": 200, "y": 0 } }
        ],
        "edges": [
            { "id": "e_in", "sourceId": "in", "targetId": "expr" },
            { "id": "e_out", "sourceId": "expr", "targetId": "out" }
        ]
    });
    rule
}

pub(crate) fn custom_widened_hold_interface() -> Value {
    let mut custom_iface =
        read_json(&repo_root().join("registry/core/artifacts/core.Hold.interface.json"));
    custom_iface["artifactId"] = json!("acme.CustomHold.interface");
    custom_iface["namespace"] = json!("acme");
    custom_iface["name"] = json!("CustomHold");
    custom_iface["payload"]["interface"]["name"] = json!("acme.CustomHold");
    custom_iface["payload"]["interface"]["extends"] = json!(["core.Hold"]);
    custom_iface["payload"]["interface"]["properties"] = json!([]);
    custom_iface["payload"]["interface"]["invariants"] = json!([]);
    custom_iface["payload"]["interface"]["lifecycles"] = json!([]);
    custom_iface["payload"]["interface"]["links"] = json!([
        {
            "id": "resource",
            "target": "core.Resource",
            "cardinality": { "to": "many", "from": "many", "required": false }
        }
    ]);
    custom_iface
}

pub(crate) fn zero_hold_literal_rule() -> Value {
    let mut rule =
        read_json(&repo_root().join("fixtures/rule-ir-v2/valid/purchase-order-approval.rule.json"));
    rule["artifactId"] = json!("compute.zero_hold.rule");
    rule["namespace"] = json!("compute");
    rule["scope"] = json!({});
    rule["dependencies"] = json!([
        {
            "artifactId": "compute.QuotaHold.entity",
            "kind": "entity",
            "version": "0.1.0",
            "required": true
        }
    ]);
    rule["payload"]["ruleId"] = json!("compute.rule.zero_hold");
    rule["payload"]["outputSchema"] = json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["set"] },
                        "objectType": { "type": "string", "enum": ["compute.QuotaHold"] },
                        "key": { "type": "string" },
                        "property": { "type": "string", "enum": ["quantity"] },
                        "value": { "type": "number" }
                    },
                    "required": ["op", "objectType", "key", "property", "value"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["facts"],
        "additionalProperties": false
    });
    rule["payload"]["graph"] = json!({
        "nodes": [
            { "id": "in", "name": "Input", "type": "inputNode", "position": { "x": 0, "y": 0 } },
            {
                "id": "expr",
                "name": "Emit zero quantity",
                "type": "expressionNode",
                "position": { "x": 100, "y": 0 },
                "content": {
                    "expressions": [
                        {
                            "id": "e1",
                            "key": "facts",
                            "value": "[{ op: 'set', objectType: 'compute.QuotaHold', key: 'h1', property: 'quantity', value: 0 }]"
                        }
                    ]
                }
            },
            { "id": "out", "name": "Output", "type": "outputNode", "position": { "x": 200, "y": 0 } }
        ],
        "edges": [
            { "id": "e_in", "sourceId": "in", "targetId": "expr" },
            { "id": "e_out", "sourceId": "expr", "targetId": "out" }
        ]
    });
    rule
}

pub(crate) fn pack_with_place_sync_rule(
    rule_artifact_id: &str,
    rule_id: &str,
    links_expr: &str,
    required_links: bool,
) -> Vec<Value> {
    let mut pack = load_pack_values("compute-units");
    let rule = sync_links_rule(rule_artifact_id, rule_id, links_expr, required_links);
    let place = find_mut(&mut pack, "compute.quota_hold.place.action");
    place["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": rule_artifact_id,
            "kind": "rule",
            "version": "0.1.0",
            "required": true
        }));
    place["payload"]["createLinks"]
        .as_array_mut()
        .expect("createLinks array")
        .retain(|binding| binding["link"] != "resource");
    place["payload"]["synchronousRule"] = json!({
        "artifactId": rule_artifact_id,
        "version": "0.1.0"
    });
    pack.push(rule);
    pack
}
