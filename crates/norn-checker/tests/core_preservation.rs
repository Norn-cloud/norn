#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::pedantic,
    missing_docs,
    reason = "integration tests for Track #4 core-link completeness and core-law preservation"
)]

//! End-to-end tests for Track #4:
//! - Gap A: explicit core-link and specialization completeness with structured
//!   [`CounterexampleWitness::CoreLink`] witnesses (direct, transitive parent
//!   interface, incompatible target, widened cardinality, dangling `core.*`
//!   target, and missing required `createLinks` bindings).
//! - Gap B: constitutional core-law non-defeasibility, effective core-law
//!   enrollment, and bounded core-law preservation with concrete
//!   [`CounterexampleWitness::CoreLawViolation`] counterexamples across
//!   `upsert_input_object` / `derive`, `fire_transition` + `setFields`,
//!   `synchronousRule` literal fact writers, and finite lifecycle soundness.
//! - Zero-regression checks over real `registry/core`, `registry/packs/canon`,
//!   and `registry/packs/compute-units` artifacts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use norn_checker::core_preservation::codes as core_codes;
use norn_checker::interfaces::codes as interface_codes;
use norn_checker::object_types::codes as object_codes;
use norn_checker::{
    CounterexampleWitness, Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticWitness,
    EntityArtifactSetContext, WitnessMove, WitnessSide, WitnessStep, check_artifact_set,
};
use norn_ir::{ArtifactEnvelope, PropertyTypeName};
use serde_json::{Value, json};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} should be valid JSON: {error}", path.display()))
}

fn decode_envelopes(values: Vec<Value>) -> Vec<ArtifactEnvelope> {
    values
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("artifact envelope decodes"))
        .collect()
}

fn load_pack_values(pack: &str) -> Vec<Value> {
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

fn core_base_values() -> Vec<Value> {
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
        "core.Order.interface.json",
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
                    "description": "Financial amount or billing metadata"
                },
                {
                    "id": "pii.personal",
                    "label": "Personal PII",
                    "severity": "high",
                    "description": "Personal contact or identity metadata"
                }
            ]
        }
    }));
    values
}

fn check_over_core(artifacts: Vec<Value>) -> Vec<Diagnostic> {
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

fn errors_of(findings: &[Diagnostic]) -> Vec<&Diagnostic> {
    findings.iter().filter(|d| d.is_error()).collect()
}

fn find_mut<'a>(values: &'a mut [Value], artifact_id: &str) -> &'a mut Value {
    values
        .iter_mut()
        .find(|value| value["artifactId"] == artifact_id)
        .unwrap_or_else(|| panic!("fixture {artifact_id} not found"))
}

#[test]
fn real_canon_and_compute_units_packs_pass_cleanly_over_core_laws() {
    let canon_findings = check_over_core(load_pack_values("canon"));
    assert!(
        errors_of(&canon_findings).is_empty(),
        "canon pack should admit cleanly over core interfaces and laws: {canon_findings:#?}"
    );

    let compute_findings = check_over_core(load_pack_values("compute-units"));
    assert!(
        errors_of(&compute_findings).is_empty(),
        "compute-units pack should admit cleanly over core interfaces and laws: {compute_findings:#?}"
    );
}

#[test]
fn missing_widened_incompatible_and_dangling_core_links_emit_core_link_witness() {
    // 1. Missing required core link `resource` on `compute.QuotaHold` (implements `core.Hold`).
    let mut missing = load_pack_values("compute-units");
    let hold = find_mut(&mut missing, "compute.QuotaHold.entity");
    hold["payload"]["objectType"]["links"]
        .as_array_mut()
        .expect("links array")
        .retain(|link| link["id"] != "resource");
    let findings = check_over_core(missing);
    let diag = findings
        .iter()
        .find(|d| d.code == object_codes::MISSING_INTERFACE_LINK)
        .unwrap_or_else(|| panic!("expected missing_interface_link, got {findings:#?}"));
    match diag
        .witness
        .as_ref()
        .and_then(|w| w.counterexample.as_deref())
    {
        Some(CounterexampleWitness::CoreLink {
            child_concept,
            parent_interface,
            link_id,
            expected_contract,
            actual_contract,
        }) => {
            assert_eq!(child_concept, "compute.QuotaHold");
            assert_eq!(parent_interface, "core.Hold");
            assert_eq!(link_id, "resource");
            assert!(expected_contract.contains("required=true"));
            assert_eq!(actual_contract, "missing");
        }
        other => panic!("expected CoreLink witness, got {other:#?}"),
    }

    // 2. Widened cardinality (`to: many`, `required: false`) on `resource`.
    let mut widened = load_pack_values("compute-units");
    let hold = find_mut(&mut widened, "compute.QuotaHold.entity");
    for link in hold["payload"]["objectType"]["links"]
        .as_array_mut()
        .expect("links array")
    {
        if link["id"] == "resource" {
            link["cardinality"] = json!({ "to": "many", "from": "many", "required": false });
        }
    }
    let findings = check_over_core(widened);
    let diag = findings
        .iter()
        .find(|d| d.code == object_codes::INTERFACE_LINK_WIDENS_CARDINALITY)
        .unwrap_or_else(|| panic!("expected interface_link_widens_cardinality, got {findings:#?}"));
    match diag
        .witness
        .as_ref()
        .and_then(|w| w.counterexample.as_deref())
    {
        Some(CounterexampleWitness::CoreLink {
            child_concept,
            parent_interface,
            link_id,
            expected_contract,
            actual_contract,
        }) => {
            assert_eq!(child_concept, "compute.QuotaHold");
            assert_eq!(parent_interface, "core.Hold");
            assert_eq!(link_id, "resource");
            assert!(expected_contract.contains("to=one"));
            assert!(actual_contract.contains("to=many"));
        }
        other => panic!("expected CoreLink witness, got {other:#?}"),
    }

    // 3. Incompatible target (`compute.Agent` does not implement `core.Resource`).
    let mut mismatch = load_pack_values("compute-units");
    let hold = find_mut(&mut mismatch, "compute.QuotaHold.entity");
    for link in hold["payload"]["objectType"]["links"]
        .as_array_mut()
        .expect("links array")
    {
        if link["id"] == "resource" {
            link["target"] = json!("compute.Agent");
        }
    }
    let findings = check_over_core(mismatch);
    let diag = findings
        .iter()
        .find(|d| d.code == object_codes::INTERFACE_LINK_TARGET_MISMATCH)
        .unwrap_or_else(|| panic!("expected interface_link_target_mismatch, got {findings:#?}"));
    assert!(matches!(
        diag.witness
            .as_ref()
            .and_then(|w| w.counterexample.as_deref()),
        Some(CounterexampleWitness::CoreLink {
            child_concept,
            parent_interface,
            link_id,
            ..
        }) if child_concept == "compute.QuotaHold" && parent_interface == "core.Hold" && link_id == "resource"
    ));

    // 4. Dangling `core.*` link target.
    let mut dangling = load_pack_values("compute-units");
    let hold = find_mut(&mut dangling, "compute.QuotaHold.entity");
    hold["payload"]["objectType"]["links"]
        .as_array_mut()
        .expect("links array")
        .push(json!({
            "id": "phantom_core_ref",
            "target": "core.NonExistentConcept",
            "cardinality": { "to": "one", "from": "many", "required": false }
        }));
    let findings = check_over_core(dangling);
    let diag = findings
        .iter()
        .find(|d| d.code == object_codes::UNKNOWN_LINK_TARGET)
        .unwrap_or_else(|| panic!("expected unknown_link_target, got {findings:#?}"));
    assert!(matches!(
        diag.witness
            .as_ref()
            .and_then(|w| w.counterexample.as_deref()),
        Some(CounterexampleWitness::CoreLink {
            child_concept,
            parent_interface,
            link_id,
            ..
        }) if child_concept == "compute.QuotaHold"
            && parent_interface == "core.NonExistentConcept"
            && link_id == "phantom_core_ref"
    ));
}

#[test]
fn transitive_interface_link_widening_emits_core_link_witness() {
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
    let findings = check_over_core(vec![custom_iface]);
    let diag = findings
        .iter()
        .find(|d| d.code == interface_codes::INVALID_INHERITANCE)
        .unwrap_or_else(|| panic!("expected invalid_inheritance, got {findings:#?}"));
    match diag
        .witness
        .as_ref()
        .and_then(|w| w.counterexample.as_deref())
    {
        Some(CounterexampleWitness::CoreLink {
            child_concept,
            parent_interface,
            link_id,
            expected_contract,
            actual_contract,
        }) => {
            assert_eq!(child_concept, "acme.CustomHold");
            assert_eq!(parent_interface, "core.Hold");
            assert_eq!(link_id, "resource");
            assert!(expected_contract.contains("to=one"));
            assert!(actual_contract.contains("to=many"));
        }
        other => panic!("expected CoreLink witness on widened parent interface, got {other:#?}"),
    }
}

#[test]
fn constitutional_core_law_defeat_and_shadowing_are_refused_with_witness() {
    // 1. Downstream non-core artifact attempting to shadow a `core.*` law id.
    let mut shadow = read_json(
        &repo_root().join("registry/core/artifacts/core.Hold.measure_matches_resource.law.json"),
    );
    shadow["artifactId"] = json!("acme.shadow_hold.law");
    shadow["namespace"] = json!("acme");
    shadow["payload"]["owner"] = json!("acme");
    shadow["payload"]["response"]["expression"] = json!("true");
    let findings = check_over_core(vec![shadow]);
    let diag = findings
        .iter()
        .find(|d| d.code == core_codes::CONSTITUTIONAL_DEFEAT)
        .unwrap_or_else(|| panic!("expected constitutional_defeat, got {findings:#?}"));
    assert!(matches!(
        diag.witness
            .as_ref()
            .and_then(|w| w.counterexample.as_deref()),
        Some(CounterexampleWitness::CoreLawWeakening {
            offending_artifact,
            core_law_id,
            ..
        }) if offending_artifact == "acme.shadow_hold.law"
            && core_law_id == "core.Hold.measure_matches_resource"
    ));

    // 2. Downstream law declaring an exception overriding a `core.*` law id.
    let mut except_core = read_json(
        &repo_root().join("registry/core/artifacts/core.Hold.measure_matches_resource.law.json"),
    );
    except_core["artifactId"] = json!("acme.relax_hold.law");
    except_core["namespace"] = json!("acme");
    except_core["payload"]["id"] = json!("acme.relax_hold");
    except_core["payload"]["owner"] = json!("acme");
    except_core["payload"]["exceptions"] = json!([
        {
            "id": "acme.allow_mismatch",
            "overrides": "core.Hold.measure_matches_resource",
            "condition": "true",
            "rationale": "attempt to bypass core measure law",
            "source": {
                "artifactId": "acme.relax_hold.law",
                "version": "0.1.0",
                "path": "/payload/exceptions/0"
            }
        }
    ]);
    let findings = check_over_core(vec![except_core]);
    assert!(
        findings
            .iter()
            .any(|d| d.code == core_codes::CONSTITUTIONAL_DEFEAT),
        "expected constitutional_defeat for exception overriding core law: {findings:#?}"
    );

    // 3. Downstream law contradicting core modality (`never` vs `always` on same predicate).
    let mut contradict =
        read_json(&repo_root().join(
            "registry/core/artifacts/core.ReversibleEvent.reversal_flag_requires_link.law.json",
        ));
    contradict["artifactId"] = json!("acme.contradict_reversal.law");
    contradict["namespace"] = json!("acme");
    contradict["payload"]["id"] = json!("acme.contradict_reversal");
    contradict["payload"]["owner"] = json!("acme");
    contradict["payload"]["modality"] = json!({ "kind": "never" });
    let findings = check_over_core(vec![contradict]);
    assert!(
        findings
            .iter()
            .any(|d| d.code == core_codes::CONSTITUTIONAL_DEFEAT),
        "expected constitutional_defeat for contradicting core law: {findings:#?}"
    );

    // 4. Normative extension slot (`norn.normative/1.0.0`) attempting to defeat a `core.*` law.
    let mut pack = load_pack_values("compute-units");
    let action = find_mut(&mut pack, "compute.token_pool.create.action");
    action["payload"]["extensions"] = json!({
        "norn.normative/1.0.0": [
            {
                "id": "acme.defeat_core_hold",
                "modality": "obligation",
                "defeatedBy": ["core.Hold.measure_matches_resource"]
            }
        ]
    });
    let findings = check_over_core(pack);
    assert!(
        findings
            .iter()
            .any(|d| d.code == core_codes::CONSTITUTIONAL_DEFEAT),
        "expected constitutional_defeat for normative defeatedBy core law: {findings:#?}"
    );
}

#[test]
fn create_only_action_missing_required_core_link_binding_emits_witness() {
    let mut pack = load_pack_values("compute-units");
    let place = find_mut(&mut pack, "compute.quota_hold.place.action");
    place["payload"]["createLinks"]
        .as_array_mut()
        .expect("createLinks array")
        .retain(|binding| binding["link"] != "resource");
    let findings = check_over_core(pack);
    let diag = findings
        .iter()
        .find(|d| d.code == core_codes::MISSING_REQUIRED_LINK_BINDING)
        .unwrap_or_else(|| panic!("expected missing_required_link_binding, got {findings:#?}"));
    assert!(matches!(
        diag.witness
            .as_ref()
            .and_then(|w| w.counterexample.as_deref()),
        Some(CounterexampleWitness::CoreLink {
            child_concept,
            parent_interface,
            link_id,
            ..
        }) if child_concept == "compute.QuotaHold"
            && parent_interface == "core.Hold"
            && link_id == "resource"
    ));
}

#[test]
fn action_derive_violating_inherited_core_invariant_emits_counterexample_witness() {
    let mut pack = load_pack_values("compute-units");
    let create = find_mut(&mut pack, "compute.capacity_commitment.create.action");
    create["payload"]["derive"] = json!([
        {
            "field": "quantity",
            "expression": "0",
            "inputRequired": false
        }
    ]);
    let findings = check_over_core(pack);
    let diag = findings
        .iter()
        .find(|d| d.code == core_codes::PRESERVATION_VIOLATED)
        .unwrap_or_else(|| panic!("expected preservation_violated, got {findings:#?}"));
    match diag
        .witness
        .as_ref()
        .and_then(|w| w.counterexample.as_deref())
    {
        Some(CounterexampleWitness::CoreLawViolation {
            action_id,
            writer_id,
            subject_type,
            core_law_id,
            state_assignment,
            ..
        }) => {
            assert_eq!(action_id, "compute.capacity_commitment.create");
            assert_eq!(writer_id, "derive:quantity");
            assert_eq!(subject_type, "compute.CapacityCommitment");
            assert_eq!(core_law_id, "core.Commitment.positive_quantity");
            assert_eq!(
                state_assignment.get("quantity").map(String::as_str),
                Some("0")
            );
        }
        other => panic!("expected CoreLawViolation witness, got {other:#?}"),
    }
}

#[test]
fn fire_transition_set_fields_violating_core_invariant_emits_step_and_counterexample() {
    let mut pack = load_pack_values("compute-units");
    let release = find_mut(&mut pack, "compute.quota_hold.release.action");
    release["payload"]["inputSchema"]["properties"]["bad_quantity"] = json!({ "type": "number" });
    release["payload"]["derive"] = json!([
        {
            "field": "bad_quantity",
            "expression": "0",
            "inputRequired": false
        }
    ]);
    release["payload"]["effectProgram"]["setFields"] = json!({
        "quantity": "bad_quantity"
    });
    let findings = check_over_core(pack);
    let diag = findings
        .iter()
        .find(|d| d.code == core_codes::PRESERVATION_VIOLATED)
        .unwrap_or_else(|| panic!("expected preservation_violated, got {findings:#?}"));
    let witness = diag.witness.as_ref().expect("witness attached");
    assert_eq!(witness.steps.len(), 1);
    assert_eq!(witness.steps[0].moves[0].side, WitnessSide::Backend);
    assert_eq!(witness.steps[0].moves[0].label, "release");
    match witness.counterexample.as_deref() {
        Some(CounterexampleWitness::CoreLawViolation {
            action_id,
            subject_type,
            core_law_id,
            state_assignment,
            ..
        }) => {
            assert_eq!(action_id, "compute.quota_hold.release");
            assert_eq!(subject_type, "compute.QuotaHold");
            assert_eq!(core_law_id, "core.Hold.positive_quantity");
            assert_eq!(
                state_assignment.get("quantity").map(String::as_str),
                Some("0")
            );
        }
        other => panic!("expected CoreLawViolation witness, got {other:#?}"),
    }
}

#[test]
fn synchronous_rule_literal_write_violating_core_invariant_emits_counterexample() {
    let mut pack = load_pack_values("compute-units");
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
    pack.push(rule);
    let findings = check_over_core(pack);
    let diag = findings
        .iter()
        .find(|d| d.code == core_codes::PRESERVATION_VIOLATED)
        .unwrap_or_else(|| {
            panic!("expected preservation_violated from rule write, got {findings:#?}")
        });
    assert!(matches!(
        diag.witness
            .as_ref()
            .and_then(|w| w.counterexample.as_deref()),
        Some(CounterexampleWitness::CoreLawViolation {
            writer_id,
            subject_type,
            core_law_id,
            state_assignment,
            ..
        }) if writer_id == "compute.zero_hold.rule"
            && subject_type == "compute.QuotaHold"
            && core_law_id == "core.Hold.positive_quantity"
            && state_assignment.get("quantity").map(String::as_str) == Some("0")
    ));
}

#[test]
fn lifecycle_core_law_blocked_exit_emits_shortest_run_counterexample() {
    let mut pack = load_pack_values("compute-units");
    let blocking_law = json!({
        "irVersion": "0.1",
        "artifactId": "core.Hold.stay_open.law",
        "kind": "law",
        "namespace": "core",
        "name": "Hold stay open test law",
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [
            {
                "artifactId": "core.Hold.interface",
                "kind": "entity",
                "version": "0.1.0",
                "required": true
            }
        ],
        "provenance": { "source": "system" },
        "payload": {
            "lawVersion": "0.1",
            "id": "core.Hold.stay_open",
            "owner": "core",
            "severity": "error",
            "rationale": "Test constitutional law blocking every exit from open",
            "provenance": { "source": "system" },
            "scope": {
                "root": { "kind": "pack", "id": "core" },
                "objectTypes": ["core.Hold"]
            },
            "subject": "core.Hold",
            "modality": { "kind": "always" },
            "response": {
                "kind": "predicate",
                "expression": "self.status == 'open'"
            }
        }
    });
    pack.push(blocking_law);
    let findings = check_over_core(pack);
    let diag = findings
        .iter()
        .find(|d| {
            d.code == core_codes::PRESERVATION_VIOLATED
                && d.artifact_id.as_deref() == Some("compute.QuotaHold.entity")
        })
        .unwrap_or_else(|| {
            panic!(
                "expected lifecycle preservation_violated on compute.QuotaHold, got {findings:#?}"
            )
        });
    match diag
        .witness
        .as_ref()
        .and_then(|w| w.counterexample.as_deref())
    {
        Some(CounterexampleWitness::CoreLawViolation {
            action_id,
            subject_type,
            core_law_id,
            state_assignment,
            trace,
            ..
        }) => {
            assert_eq!(action_id, "<lifecycle>");
            assert_eq!(subject_type, "compute.QuotaHold");
            assert_eq!(core_law_id, "core.Hold.stay_open");
            assert_eq!(
                state_assignment.get("status").map(String::as_str),
                Some("\"open\"")
            );
            assert!(
                trace.iter().any(|step| step.starts_with("blocked:")),
                "expected blocked transition in trace: {trace:?}"
            );
        }
        other => panic!("expected CoreLawViolation lifecycle witness, got {other:#?}"),
    }
}

#[test]
fn diagnostic_witness_json_keeps_step_only_format_and_adds_counterexample_additively() {
    let step_only = Diagnostic {
        code: DiagnosticCode::new(core_codes::PRESERVATION_VIOLATED),
        severity: DiagnosticSeverity::Error,
        message: "step only".to_owned(),
        span: None,
        artifact_id: None,
        remediation: None,
        witness: Some(DiagnosticWitness::from_steps(vec![WitnessStep {
            edge: 0,
            moves: vec![WitnessMove {
                side: WitnessSide::Backend,
                label: "release".to_owned(),
            }],
        }])),
    };
    let step_json = step_only.to_json();
    assert!(step_json["witness"]["steps"].is_array());
    assert!(step_json["witness"].get("counterexample").is_none());

    let mut state = BTreeMap::new();
    state.insert("quantity".to_owned(), "0".to_owned());
    let with_cx = Diagnostic {
        witness: Some(DiagnosticWitness::core_law_violation(
            Vec::new(),
            "compute.capacity_commitment.create",
            "derive:quantity",
            "compute.CapacityCommitment",
            "core.Commitment.positive_quantity",
            state,
            BTreeMap::new(),
            vec!["derive:quantity".to_owned()],
            "quantity must be positive",
        )),
        ..step_only
    };
    let cx_json = with_cx.to_json();
    assert_eq!(
        cx_json["witness"]["counterexample"]["kind"],
        "core_law_violation"
    );
    assert_eq!(
        cx_json["witness"]["counterexample"]["coreLawId"],
        "core.Commitment.positive_quantity"
    );
    assert_eq!(
        cx_json["witness"]["counterexample"]["stateAssignment"]["quantity"],
        "0"
    );
}
