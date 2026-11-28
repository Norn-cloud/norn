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
//!   target, and missing required `createLinks` / `synchronousRule` bindings).
//! - Gap B: constitutional core-law non-defeasibility (including normative
//!   `defeatedBy` precedence direction), effective core-law enrollment, and
//!   bounded core-law preservation with concrete
//!   [`CounterexampleWitness::CoreLawViolation`] counterexamples and honest
//!   `core.laws.preservation_unknown` diagnostics when predicates or external
//!   preconditions cannot be statically decided.
//! - Zero-regression checks over real `registry/core`, `registry/packs/canon`,
//!   and `registry/packs/compute-units` artifacts.

#[path = "support/core_preservation_fixtures.rs"]
mod fixtures;

use std::collections::{BTreeMap, BTreeSet};

use fixtures::{
    boolean_entity_envelope, check_over_core, core_predicate_law, custom_widened_hold_interface,
    decode_envelopes, deontic_rule_json, errors_of, find_mut, load_pack_values,
    pack_with_place_sync_rule, read_json, repo_root, zero_hold_literal_rule,
};
use norn_checker::core_preservation::codes as core_codes;
use norn_checker::interfaces::codes as interface_codes;
use norn_checker::object_types::codes as object_codes;
use norn_checker::{
    CounterexampleWitness, Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticWitness,
    EntityArtifactSetContext, WitnessMove, WitnessSide, WitnessStep, check_artifact_set,
};
use norn_ir::PropertyTypeName;
use serde_json::json;

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
        }) if child_concept == "compute.QuotaHold"
            && parent_interface == "core.Hold"
            && link_id == "resource"
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
    let findings = check_over_core(vec![custom_widened_hold_interface()]);
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
}

#[test]
fn normative_defeated_by_precedence_admits_yielding_to_core_and_refuses_defeating_core() {
    let registry = BTreeSet::<PropertyTypeName>::new();
    let entity_id = "core.SalesOrder.entity";
    let subject = "core.SalesOrder";

    // Direction 1 (LAWFUL): downstream rule yields (`defeatedBy`) to a `core.*` rule -> ADMITTED.
    let yielding_rules = json!([
        deontic_rule_json(
            "acme.sales_custom",
            subject,
            entity_id,
            0,
            "obligation",
            "always",
            &["core.sales_guard"],
        ),
        deontic_rule_json(
            "core.sales_guard",
            subject,
            entity_id,
            1,
            "prohibition",
            "never",
            &[],
        ),
    ]);
    let lawful_entity = decode_envelopes(vec![boolean_entity_envelope(
        entity_id,
        subject,
        yielding_rules,
    )]);
    let lawful_findings = check_artifact_set(
        &lawful_entity,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    );
    assert!(
        errors_of(&lawful_findings).is_empty(),
        "downstream rule lawfully yielding to core.* via defeatedBy must be admitted: {lawful_findings:#?}"
    );

    // Direction 2 (ATTACK): `core.*` rule lists downstream rule in `defeatedBy` -> REFUSED.
    let defeating_rules = json!([
        deontic_rule_json(
            "acme.sales_override",
            subject,
            entity_id,
            0,
            "obligation",
            "always",
            &[],
        ),
        deontic_rule_json(
            "core.sales_guard",
            subject,
            entity_id,
            1,
            "prohibition",
            "never",
            &["acme.sales_override"],
        ),
    ]);
    let attack_entity = decode_envelopes(vec![boolean_entity_envelope(
        entity_id,
        subject,
        defeating_rules,
    )]);
    let attack_findings = check_artifact_set(
        &attack_entity,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    );
    let diag = attack_findings
        .iter()
        .find(|d| d.code == core_codes::CONSTITUTIONAL_DEFEAT)
        .unwrap_or_else(|| panic!("expected constitutional_defeat, got {attack_findings:#?}"));
    assert!(matches!(
        diag.witness
            .as_ref()
            .and_then(|w| w.counterexample.as_deref()),
        Some(CounterexampleWitness::CoreLawWeakening {
            offending_law_id,
            core_law_id,
            ..
        }) if offending_law_id == "acme.sales_override" && core_law_id == "core.sales_guard"
    ));
}

#[test]
fn required_core_link_binding_rejects_generic_rule_and_mismatched_source_id_field() {
    // 1. Missing required link in `createLinks` -> REFUSED with CoreLink witness.
    let mut missing = load_pack_values("compute-units");
    let place = find_mut(&mut missing, "compute.quota_hold.place.action");
    place["payload"]["createLinks"]
        .as_array_mut()
        .expect("createLinks array")
        .retain(|binding| binding["link"] != "resource");
    let findings = check_over_core(missing);
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

    // 2. `synchronousRule` with generic `outputSchema.properties.links` (empty array, does not
    //    bind required `resource` link) -> REFUSED.
    let generic_rule_pack = pack_with_place_sync_rule(
        "compute.generic_links.rule",
        "compute.rule.generic_links",
        "[]",
        false,
    );
    let findings = check_over_core(generic_rule_pack);
    assert!(
        findings
            .iter()
            .any(|d| d.code == core_codes::MISSING_REQUIRED_LINK_BINDING),
        "generic synchronousRule declaring outputSchema.properties.links without binding resource must be refused: {findings:#?}"
    );

    // 3. `createLinks` with explicit `source` pointing to a DIFFERENT `idField` -> REFUSED.
    let mut wrong_source_pack = load_pack_values("compute-units");
    let place = find_mut(&mut wrong_source_pack, "compute.quota_hold.place.action");
    place["payload"]["inputSchema"]["properties"]["other_hold_id"] = json!({ "type": "string" });
    for binding in place["payload"]["createLinks"]
        .as_array_mut()
        .expect("createLinks array")
    {
        if binding["link"] == "resource" {
            binding["source"] = json!({
                "objectType": "compute.QuotaHold",
                "idField": "other_hold_id"
            });
        }
    }
    let findings = check_over_core(wrong_source_pack);
    assert!(
        findings
            .iter()
            .any(|d| d.code == core_codes::MISSING_REQUIRED_LINK_BINDING),
        "createLinks explicit source pointing to a different idField must be refused: {findings:#?}"
    );

    // 4. `createLinks` binding the created object (`source` omitted) and exact `link_id` -> ADMITTED.
    let admitted_findings = check_over_core(load_pack_values("compute-units"));
    assert!(
        errors_of(&admitted_findings).is_empty(),
        "createLinks binding created object and exact required link_ids must be admitted: {admitted_findings:#?}"
    );

    // 5. `synchronousRule` unconditionally emitting `{ op: 'add', sourceType: 'compute.QuotaHold',
    //    sourceKey: event.input.id, link: 'resource', ... }` satisfies `resource` link binding.
    let guaranteed_rule_pack = pack_with_place_sync_rule(
        "compute.bind_resource.rule",
        "compute.rule.bind_resource",
        "[{ op: 'add', sourceType: 'compute.QuotaHold', sourceKey: event.input.id, link: 'resource', targetType: 'compute.TokenPool', targetKey: event.input.resource_id }]",
        true,
    );
    let findings = check_over_core(guaranteed_rule_pack);
    assert!(
        !findings
            .iter()
            .any(|d| d.code == core_codes::MISSING_REQUIRED_LINK_BINDING),
        "guaranteed synchronousRule emitting add for created object must satisfy required link binding: {findings:#?}"
    );
}

#[test]
fn action_derive_and_fire_transition_violations_emit_counterexample_witnesses() {
    // 1. `upsert_input_object` + `derive` violating inherited core invariant.
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

    // 2. `fire_transition` + `setFields` violating core invariant.
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
    release["payload"]["effectProgram"]["setFields"] = json!({ "quantity": "bad_quantity" });
    let findings = check_over_core(pack);
    let diag = findings
        .iter()
        .find(|d| d.code == core_codes::PRESERVATION_VIOLATED)
        .unwrap_or_else(|| panic!("expected preservation_violated, got {findings:#?}"));
    let witness = diag.witness.as_ref().expect("witness attached");
    assert_eq!(witness.steps.len(), 1);
    assert_eq!(witness.steps[0].moves[0].side, WitnessSide::Backend);
    assert_eq!(witness.steps[0].moves[0].label, "release");
    assert!(matches!(
        witness.counterexample.as_deref(),
        Some(CounterexampleWitness::CoreLawViolation {
            action_id,
            subject_type,
            core_law_id,
            state_assignment,
            ..
        }) if action_id == "compute.quota_hold.release"
            && subject_type == "compute.QuotaHold"
            && core_law_id == "core.Hold.positive_quantity"
            && state_assignment.get("quantity").map(String::as_str) == Some("0")
    ));
}

#[test]
fn synchronous_rule_literal_write_violating_core_invariant_emits_counterexample() {
    let mut pack = load_pack_values("compute-units");
    pack.push(zero_hold_literal_rule());
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
fn lifecycle_core_law_blocked_exit_and_unevaluated_exit_emit_honest_verdicts() {
    // 1. Concrete blocked exit emits PRESERVATION_VIOLATED with shortest-run witness.
    let mut blocked_pack = load_pack_values("compute-units");
    blocked_pack.push(core_predicate_law(
        "core.Hold.stay_open.law",
        "core.Hold.stay_open",
        "core.Hold",
        "core.Hold.interface",
        "self.status == 'open'",
    ));
    let findings = check_over_core(blocked_pack);
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
    assert!(matches!(
        diag.witness
            .as_ref()
            .and_then(|w| w.counterexample.as_deref()),
        Some(CounterexampleWitness::CoreLawViolation {
            action_id,
            subject_type,
            core_law_id,
            state_assignment,
            trace,
            ..
        }) if action_id == "<lifecycle>"
            && subject_type == "compute.QuotaHold"
            && core_law_id == "core.Hold.stay_open"
            && state_assignment.get("status").map(String::as_str) == Some("\"open\"")
            && trace.iter().any(|step| step.starts_with("blocked:"))
    ));

    // 2. Unevaluated destination (`EvalStop::Other` via invalid timestamp on non-open states)
    //    must NEVER silently pass and must emit PRESERVATION_UNKNOWN (not false VIOLATED).
    let mut unknown_pack = load_pack_values("compute-units");
    unknown_pack.push(core_predicate_law(
        "core.Hold.unevaluated_exit.law",
        "core.Hold.unevaluated_exit",
        "core.Hold",
        "core.Hold.interface",
        "self.status == 'open' ? true : (timestamp(self.status) > timestamp('2026-01-01T00:00:00Z'))",
    ));
    let findings = check_over_core(unknown_pack);
    assert!(
        findings.iter().any(|d| {
            d.code == core_codes::PRESERVATION_UNKNOWN
                && d.artifact_id.as_deref() == Some("compute.QuotaHold.entity")
        }),
        "unevaluated lifecycle exit must emit preservation_unknown, got {findings:#?}"
    );
    assert!(
        !findings.iter().any(|d| {
            d.code == core_codes::PRESERVATION_VIOLATED
                && d.artifact_id.as_deref() == Some("compute.QuotaHold.entity")
        }),
        "unevaluated lifecycle exit must not emit false concrete preservation_violated: {findings:#?}"
    );
}

#[test]
fn unevaluated_writer_law_and_unchecked_external_precondition_emit_preservation_unknown() {
    // 1. Writer encountering `EvalStop::Other` emits PRESERVATION_UNKNOWN (never silent pass).
    let mut eval_unknown_pack = load_pack_values("compute-units");
    let create = find_mut(
        &mut eval_unknown_pack,
        "compute.capacity_commitment.create.action",
    );
    create["payload"]["derive"] = json!([
        {
            "field": "unit",
            "expression": "'not-a-timestamp'",
            "inputRequired": false
        }
    ]);
    eval_unknown_pack.push(core_predicate_law(
        "core.Commitment.eval_unknown.law",
        "core.Commitment.eval_unknown",
        "core.Commitment",
        "core.Commitment.interface",
        "timestamp(self.unit ?? 'not-a-timestamp') > timestamp('2026-01-01T00:00:00Z')",
    ));
    let findings = check_over_core(eval_unknown_pack);
    assert!(
        findings.iter().any(|d| {
            d.code == core_codes::PRESERVATION_UNKNOWN
                && d.artifact_id.as_deref() == Some("compute.capacity_commitment.create.action")
        }),
        "unevaluated core law predicate on writer must emit preservation_unknown: {findings:#?}"
    );

    // 2. Unchecked external (`slice.*`) precondition with a violating post-state emits
    //    PRESERVATION_UNKNOWN instead of a false concrete PRESERVATION_VIOLATED witness.
    let mut pre_unknown_pack = load_pack_values("compute-units");
    let create = find_mut(
        &mut pre_unknown_pack,
        "compute.capacity_commitment.create.action",
    );
    create["payload"]["derive"] = json!([
        {
            "field": "quantity",
            "expression": "0",
            "inputRequired": false
        }
    ]);
    create["payload"]["preconditions"] = json!(["slice.provider.id != ''"]);
    let findings = check_over_core(pre_unknown_pack);
    assert!(
        findings.iter().any(|d| {
            d.code == core_codes::PRESERVATION_UNKNOWN
                && d.artifact_id.as_deref() == Some("compute.capacity_commitment.create.action")
        }),
        "unchecked external precondition must downgrade candidate violation to preservation_unknown: {findings:#?}"
    );
    assert!(
        !findings.iter().any(|d| {
            d.code == core_codes::PRESERVATION_VIOLATED
                && d.artifact_id.as_deref() == Some("compute.capacity_commitment.create.action")
        }),
        "unchecked external precondition must NOT emit false concrete preservation_violated: {findings:#?}"
    );

    // 3. Statically-evaluable refusing precondition skips the rejected input cleanly.
    let mut pre_refused_pack = load_pack_values("compute-units");
    let create = find_mut(
        &mut pre_refused_pack,
        "compute.capacity_commitment.create.action",
    );
    create["payload"]["derive"] = json!([
        {
            "field": "quantity",
            "expression": "0",
            "inputRequired": false
        }
    ]);
    create["payload"]["preconditions"] = json!(["input.quantity > 0"]);
    let findings = check_over_core(pre_refused_pack);
    assert!(
        !findings.iter().any(|d| {
            (d.code == core_codes::PRESERVATION_VIOLATED
                || d.code == core_codes::PRESERVATION_UNKNOWN)
                && d.artifact_id.as_deref() == Some("compute.capacity_commitment.create.action")
        }),
        "refusing precondition must reject the violating input cleanly: {findings:#?}"
    );
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
