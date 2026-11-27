//! Bounded lifecycle input snapshots; no backend verdict is simulated.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "bounded source fixtures"
)]
use super::lifecycle::finite::{Batch, Bounds};
use super::*;

fn source(entity_root: bool) -> DefinitionSource {
    definition_source(&declarations(entity_root))
}
fn declarations(entity_root: bool) -> Value {
    let mut declarations: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/jewelry_demo.json"))
            .expect("actual authored source");
    if entity_root {
        // A separate supported test declaration; the actual App-root pack stays Unknown.
        declarations[1]["payload"]["scope"]["root"] = json!({"kind":"entity","id":"demo.Melt"});
        for artifact in declarations.as_array_mut().expect("artifacts") {
            artifact["provenance"]["sourcePath"] = json!("tests/lifecycle_finite");
        }
        declarations[1]["payload"]["provenance"]["sourcePath"] = json!("tests/lifecycle_finite");
    }
    declarations
}
fn definition_source(declarations: &Value) -> DefinitionSource {
    let units: Value = serde_json::from_str(include_str!(
        "../../../../registry/core/artifacts/core.units.json"
    ))
    .expect("units");
    DefinitionSource::parse(
        &String::from_utf8(norn_canonical::canonical_json_exact_of(declarations).expect("source"))
            .expect("UTF8"),
        &String::from_utf8(norn_canonical::canonical_json_exact_of(&vec![units]).expect("base"))
            .expect("UTF8"),
    )
    .expect("exact source")
}
fn bounds() -> Bounds {
    Bounds {
        object_type: "demo.Melt".into(),
        property: "status".into(),
        seed: 0,
        frozen: BTreeMap::from([
            ("id".into(), vec!["\"synthetic\"".into()]),
            ("input_fine_g".into(), vec!["2".into()]),
            ("output_fine_g".into(), vec!["1".into()]),
            ("loss_fine_g".into(), vec!["0".into(), "1".into()]),
        ]),
    }
}
fn batch(source: &DefinitionSource, bounds: &Bounds) -> Batch {
    super::lifecycle_tests::authority(source)
        .lifecycle_inputs(source, &BTreeSet::new(), bounds)
        .expect("fresh challenge")
}
#[test]
fn lifecycle_inputs_are_exact_complete_and_source_bound() {
    let prepared = batch(&source(true), &bounds());
    let snapshot = prepared.snapshot.expect("separate supported declaration");
    assert_eq!(snapshot.nodes.len(), 10);
    assert_eq!(snapshot.machine.states.len(), 5);
    assert_eq!(snapshot.laws.len(), 1);
    assert!(
        matches!(&snapshot.fields["input_fine_g"],norn_ir::PropertyType::Decimal{scale,unit:Some(unit),..} if scale.0==3 && unit.code=="g")
    );
    for state in &snapshot.machine.states {
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .filter(|row| row["status"] == norn_ir::RuntimeValue::String(state.clone()))
                .count(),
            2
        );
    }
    let repeated = batch(&source(true), &bounds());
    assert_eq!(prepared.challenge.sha256(), repeated.challenge.sha256());
    assert_eq!(prepared.bounds_json, repeated.bounds_json);
    let profile: Value = serde_json::from_str(&prepared.bounds_json).expect("pinned profile");
    assert_eq!(
        prepared.bounds_json.as_bytes(),
        norn_canonical::canonical_json_exact_of(&profile).expect("canonical profile")
    );
}
#[test]
fn lifecycle_input_refusals_never_clip_the_domain() {
    assert!(
        batch(&source(false), &bounds())
            .snapshot
            .expect_err("actual App root stays Unknown")
            .contains("UnsupportedUntilB2")
    );
    let mut profile = bounds();
    profile
        .frozen
        .insert("loss_fine_g".into(), vec!["0.0001".into()]);
    assert!(batch(&source(true), &profile).snapshot.is_err());
    profile = bounds();
    profile.frozen.remove("id");
    assert!(
        batch(&source(true), &profile)
            .snapshot
            .expect_err("no implicit context")
            .contains("missing explicit")
    );
    profile = bounds();
    profile
        .frozen
        .insert("status".into(), vec!["\"posted\"".into()]);
    assert!(
        batch(&source(true), &profile)
            .snapshot
            .expect_err("no enum clipping")
            .contains("extra frozen")
    );
    profile = bounds();
    profile
        .frozen
        .insert("id".into(), vec!["\"synthetic\"".into(); 600]);
    let reason = batch(&source(true), &profile)
        .snapshot
        .expect_err("full product refused");
    assert_eq!(
        reason,
        "empty or excessive frozen context domain (4096 nodes)"
    );
    profile = bounds();
    profile
        .frozen
        .insert("id".into(), vec![format!("\"{}\"", "x".repeat(4096))]);
    profile.frozen.insert(
        "input_fine_g".into(),
        (0..128).map(|i| i.to_string()).collect(),
    );
    assert!(
        batch(&source(true), &profile)
            .snapshot
            .expect_err("byte product refuses as a whole")
            .contains("budget")
    );
    let source = source(true);
    profile = bounds();
    profile.object_type = "x".repeat(10_001);
    assert_eq!(
        super::lifecycle_tests::authority(&source)
            .lifecycle_inputs(&source, &BTreeSet::new(), &profile)
            .expect_err("oversized selector"),
        ChallengeError::Limit
    );
}
// Separately attributed chart-only declarations isolate the finite input boundary.
fn cap_source(empty_enum: bool) -> DefinitionSource {
    let mut declarations: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/jewelry_demo.json"))
            .expect("source");
    declarations.as_array_mut().expect("artifacts").truncate(1);
    declarations[0]["provenance"]["sourcePath"] = json!("tests/lifecycle_finite_caps");
    let object = &mut declarations[0]["payload"]["objectType"];
    let status = object["properties"]
        .as_array_mut()
        .expect("properties")
        .iter_mut()
        .find(|p| p["id"] == "status")
        .expect("status");
    status["type"]["values"] = if empty_enum {
        json!([])
    } else {
        json!([{"value":"only"}])
    };
    status["default"] = json!("only");
    object["lifecycles"] =
        json!([{"property":"status","initial":["only"],"terminal":["only"],"transitions":[]}]);
    definition_source(&declarations)
}
fn cap_bounds(inputs: u32, outputs: u32) -> Bounds {
    let mut profile = bounds();
    profile.frozen.insert(
        "input_fine_g".into(),
        (0..inputs).map(|i| i.to_string()).collect(),
    );
    profile.frozen.insert(
        "output_fine_g".into(),
        (0..outputs).map(|i| i.to_string()).collect(),
    );
    profile
        .frozen
        .insert("loss_fine_g".into(), vec!["0".into()]);
    profile
}
#[test]
fn lifecycle_inputs_accept_exactly_4096_nodes() {
    let snapshot = batch(&cap_source(false), &cap_bounds(64, 64))
        .snapshot
        .expect("64 by 64 by one state");
    assert_eq!(snapshot.nodes.len(), 4096);
    let unique: BTreeSet<_> = snapshot
        .nodes
        .iter()
        .map(|row| norn_canonical::serialize_json_verbatim(row).expect("row"))
        .collect();
    assert_eq!(unique.len(), 4096);
}
#[test]
fn lifecycle_inputs_refuse_the_complete_4097_node_product() {
    let reason = batch(&cap_source(false), &cap_bounds(17, 241))
        .snapshot
        .expect_err("17 by 241 by one state");
    assert_eq!(
        reason,
        "empty or excessive frozen context domain (4096 nodes)"
    );
}
#[test]
fn lifecycle_inputs_reject_an_empty_domain() {
    let mut profile = cap_bounds(1, 1);
    profile
        .frozen
        .get_mut("loss_fine_g")
        .expect("domain")
        .clear();
    assert_eq!(
        batch(&cap_source(false), &profile)
            .snapshot
            .expect_err("empty domain"),
        "empty or excessive frozen context domain (4096 nodes)"
    );
}
#[test]
fn lifecycle_inputs_reject_an_empty_enum_at_admission() {
    let source = cap_source(true);
    let registry = BTreeSet::new();
    let diagnostics = crate::check_artifact_set(
        &source.artifacts,
        &crate::EntityArtifactSetContext {
            registry: &registry,
            base: &source.base,
            tenant: None,
        },
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code.as_str() == crate::property_types::codes::EMPTY_ENUM)
    );
    assert_eq!(
        ApprovedContract::approve(
            &source,
            &registry,
            json!({"expr":"norn-zen/6"}),
            json!({"store":"empty"}),
            json!({"level":"bounded"})
        )
        .expect_err("empty enum"),
        ChallengeError::NotAdmitted
    );
}

#[test]
fn lifecycle_graph_preserves_unknown_reasons_and_refuses_duplicate_rows() {
    let unsupported = source(false);
    assert!(
        super::lifecycle_tests::authority(&unsupported)
            .lifecycle_graph(&unsupported, &BTreeSet::new(), &bounds())
            .expect("fresh source")
            .graph
            .expect_err("actual App-root law stays Unknown")
            .contains("UnsupportedUntilB2")
    );
    let source = source(true);
    let mut profile = bounds();
    profile
        .frozen
        .insert("id".into(), vec!["\"synthetic\"".into(); 2]);
    assert_eq!(
        super::lifecycle_tests::authority(&source)
            .lifecycle_graph(&source, &BTreeSet::new(), &profile)
            .expect("source")
            .graph
            .expect_err("no duplicate node identities"),
        "duplicate finite scalar valuations"
    );
}

/// One row per state, so only native budgets, not global work, can refuse these graphs.
fn single_row_batch(declarations: &Value) -> super::lifecycle::graph::Batch {
    let source = definition_source(declarations);
    let mut profile = bounds();
    profile
        .frozen
        .insert("loss_fine_g".into(), vec!["1".into()]);
    profile
        .frozen
        .insert("id".into(), vec![format!("\"{}\"", costly_id())]);
    super::lifecycle_tests::authority(&source)
        .lifecycle_graph(&source, &BTreeSet::new(), &profile)
        .expect("fresh source")
}
/// Closures are Unknown, so cost comes from comparing a near-limit frozen literal.
fn costly_id() -> String {
    "x".repeat(9_000)
}
fn costly(comparisons: usize) -> String {
    format!(
        "len([{}]) > 0",
        vec!["self.id == self.id"; comparisons].join(", ")
    )
}
fn units(source: &str) -> u64 {
    let context = norn_expr::Value::object([(
        "self",
        norn_expr::Value::object([("id", norn_expr::Value::from(costly_id()))]),
    )]);
    norn_expr::Expression::compile(source)
        .expect("compiled")
        .evaluate(&context, norn_expr::EvaluationBudget::new(2_000_000))
        .expect("within global work")
        .units_used()
}
#[test]
fn a_guard_beyond_the_native_budget_makes_the_graph_unknown() {
    let guard = costly(75);
    let cost = units(&guard);
    assert!((1_000_001..1_900_000).contains(&cost), "{cost}");
    let mut declarations = declarations(true);
    assert!(single_row_batch(&declarations).graph.is_ok());
    declarations[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][0]["guards"] =
        json!([guard]);
    let reason = single_row_batch(&declarations)
        .graph
        .expect_err("native never commits this edge");
    assert!(
        reason.contains("transition measure") && reason.contains("1000000"),
        "{reason}"
    );
}
#[test]
fn a_false_guard_stops_later_guards_like_native() {
    let mut declarations = declarations(true);
    declarations[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][0]["guards"] =
        json!(["false", "1"]);
    let graph = single_row_batch(&declarations)
        .graph
        .expect("native refuses GuardFalse first");
    assert!(graph.edges.iter().all(|edge| edge.transition != "measure"));
}
#[test]
fn lifecycle_graph_pins_native_evaluation_limits() {
    let source = source(true);
    let batch = super::lifecycle_tests::authority(&source)
        .lifecycle_graph(&source, &BTreeSet::new(), &bounds())
        .expect("source");
    let profile: Value = serde_json::from_str(&batch.bounds_json).expect("pinned profile");
    assert_eq!(profile["maxGuardUnits"], 1_000_000);
    assert_eq!(profile["maxRowLawUnits"], 1_000_000);
}

#[test]
fn every_frozen_choice_keeps_the_complete_chart() {
    for loss in 0_u8..5 {
        let mut profile = bounds();
        profile
            .frozen
            .insert("loss_fine_g".into(), vec![loss.to_string()]);
        let snapshot = batch(&source(true), &profile).snapshot.expect("bounded");
        assert_eq!(
            snapshot.nodes.len(),
            snapshot.machine.states.len(),
            "loss {loss}"
        );
        let expected = norn_ir::RuntimeValue::from_json_text(loss.to_string()).expect("exact");
        for row in snapshot.nodes {
            assert_eq!(&row["loss_fine_g"], &expected, "loss {loss}");
        }
    }
}

/// LIFE-SOUND-4: with no recorded loss, conservation refuses `post` from every approved row,
/// so approval is a planted dead end whose minimal blocking core is the conservation law.
#[test]
fn a_law_blocked_exit_names_its_smallest_blocking_law_set() {
    let source = source(true);
    let graph = super::lifecycle_tests::authority(&source)
        .lifecycle_graph(&source, &BTreeSet::new(), &bounds())
        .expect("source")
        .graph
        .expect("bounded graph");
    let findings = graph.soundness().expect("rows carry states");
    assert_eq!(findings.len(), 1, "{findings:?}");
    let finding = &findings[0];
    assert_eq!(
        finding.code,
        crate::lifecycle_soundness::codes::LAW_BLOCKED_EXIT
    );
    assert_eq!(finding.state, "approved");
    assert_eq!(finding.run, ["measure", "approve"]);
    let (transition, laws) = finding.blocked.as_ref().expect("a core");
    assert_eq!(transition, "post");
    assert_eq!(laws.len(), 1);
    assert!(laws[0].contains("fine_mass_conservation"), "{laws:?}");
    assert_eq!(
        graph.inputs.nodes[finding.row]["loss_fine_g"],
        norn_ir::RuntimeValue::from_json_text("0").expect("exact")
    );
    // Recording the loss makes every approved row postable: no dead end remains.
    let mut profile = bounds();
    profile
        .frozen
        .insert("loss_fine_g".into(), vec!["1".into()]);
    let sound = super::lifecycle_tests::authority(&source)
        .lifecycle_graph(&source, &BTreeSet::new(), &profile)
        .expect("source")
        .graph
        .expect("bounded graph");
    assert_eq!(sound.soundness().expect("states"), Vec::new());
}

/// A fact transition a governing law refuses is reported, not silently dropped.
#[test]
fn a_law_refusing_a_fact_is_reported() {
    let mut declarations = declarations(true);
    declarations[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][3]["mode"] =
        json!("fact");
    let source = definition_source(&declarations);
    let graph = super::lifecycle_tests::authority(&source)
        .lifecycle_graph(&source, &BTreeSet::new(), &bounds())
        .expect("source")
        .graph
        .expect("bounded graph");
    let codes: Vec<_> = graph
        .soundness()
        .expect("states")
        .into_iter()
        .map(|finding| finding.code)
        .collect();
    assert_eq!(
        codes,
        [
            crate::lifecycle_soundness::codes::LAW_BLOCKED_EXIT,
            crate::lifecycle_soundness::codes::FACT_REFUSED
        ]
    );
}

fn core_commitment_base() -> Vec<Value> {
    let mut base: Vec<Value> = [
        include_str!("../../../../registry/privacy/artifacts/privacy.markings.entity.json"),
        include_str!("../../../../registry/core/artifacts/core.value_types.entity.json"),
        include_str!("../../../../registry/core/artifacts/std.value_types.entity.json"),
        include_str!("../../../../registry/core/artifacts/core.Agent.interface.json"),
        include_str!("../../../../registry/core/artifacts/core.Resource.interface.json"),
        include_str!("../../../../registry/core/artifacts/core.EconomicEvent.interface.json"),
        include_str!("../../../../registry/core/artifacts/core.Commitment.interface.json"),
        include_str!("../../../../registry/core/artifacts/core.Hold.interface.json"),
        include_str!("../../../../registry/core/artifacts/core.LedgerEffect.interface.json"),
        include_str!("../../../../registry/core/artifacts/core.Claim.interface.json"),
    ]
    .into_iter()
    .map(|raw| serde_json::from_str(raw).expect("core artifact"))
    .collect();
    base.push(json!({
        "irVersion": "0.1", "kind": "entity",
        "artifactId": "test.CoreMarkings.entity", "version": "0.1.0",
        "namespace": "test", "name": "CoreMarkings", "lifecycle": "active",
        "scope": {}, "dependencies": [],
        "provenance": { "source": "system", "sourcePath": "tests/core_commitment_life" },
        "payload": {
            "entityVersion": "2.0",
            "markings": [
                { "id": "pii.financial", "label": "Financial PII", "category": "pii.financial", "severity": "high", "mask": { "kind": "refuse" } },
                { "id": "confidential.finance", "label": "Confidential finance", "category": "confidential.finance", "severity": "high", "mask": { "kind": "refuse" } },
                { "id": "pii.health", "label": "Health PII", "category": "pii.health", "severity": "critical", "mask": { "kind": "refuse" } }
            ]
        }
    }));
    base
}

fn core_commitment_source(base: &[Value], extra_invariants: Value) -> DefinitionSource {
    let party = json!({
        "irVersion": "0.1", "kind": "entity",
        "artifactId": "test.Party.entity", "version": "0.1.0",
        "namespace": "test", "name": "Party", "lifecycle": "active",
        "scope": {}, "dependencies": [],
        "provenance": { "source": "system", "sourcePath": "tests/core_commitment_life" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.Party",
                "role": "thing",
                "implements": ["core.Agent"],
                "properties": [
                    { "id": "display_name", "type": { "kind": "ref", "name": "core.display_name" } },
                    { "id": "kind", "type": { "kind": "enum", "values": [
                        { "value": "person" }, { "value": "organization" }, { "value": "system" }
                    ] } }
                ]
            }
        }
    });
    let commitment = json!({
        "irVersion": "0.1", "kind": "entity",
        "artifactId": "test.OrderCommitment.entity", "version": "0.1.0",
        "namespace": "test", "name": "OrderCommitment", "lifecycle": "active",
        "scope": {}, "dependencies": [],
        "provenance": { "source": "system", "sourcePath": "tests/core_commitment_life" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.OrderCommitment",
                "role": "happening",
                "implements": ["core.Commitment"],
                "properties": [
                    { "id": "status", "type": { "kind": "enum", "values": [
                        { "value": "requested" }, { "value": "promised" },
                        { "value": "fulfilled" }, { "value": "cancelled" }, { "value": "breached" }
                    ] }, "default": "requested" },
                    { "id": "due_at", "type": { "kind": "ref", "name": "iso8601.timestamp" } },
                    { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } },
                    { "id": "unit", "type": { "kind": "ref", "name": "ucum.unit_code" }, "constraints": { "nullable": true } },
                    { "id": "currency", "type": { "kind": "ref", "name": "core.code" }, "constraints": { "nullable": true } }
                ],
                "links": [
                    { "id": "provider", "target": "test.Party", "cardinality": { "to": "one", "from": "many", "required": true } },
                    { "id": "receiver", "target": "test.Party", "cardinality": { "to": "one", "from": "many", "required": true } },
                    { "id": "resource", "target": "core.Resource", "cardinality": { "to": "one", "from": "many" } }
                ],
                "invariants": extra_invariants,
                "lifecycles": [{
                    "property": "status",
                    "initial": ["requested"],
                    "terminal": ["fulfilled", "cancelled", "breached"],
                    "immutable": ["fulfilled", "cancelled", "breached"],
                    "endings": [
                        { "state": "fulfilled", "outcome": "completed" },
                        { "state": "cancelled", "outcome": "cancelled" },
                        { "state": "breached", "outcome": "breached_visible" }
                    ],
                    "transitions": [
                        { "id": "promise", "from": ["requested"], "to": "promised" },
                        { "id": "fulfil", "from": ["promised"], "to": "fulfilled" },
                        { "id": "cancel", "from": ["requested", "promised"], "to": "cancelled" },
                        { "id": "breach_due", "from": ["requested", "promised"], "to": "breached",
                          "trigger": { "kind": "timer", "millis": "86400000" }, "mode": "fact" }
                    ]
                }]
            }
        }
    });
    let artifacts = json!([party, commitment]);
    DefinitionSource::parse(
        &String::from_utf8(norn_canonical::canonical_json_exact_of(&artifacts).expect("artifacts"))
            .expect("UTF8"),
        &String::from_utf8(norn_canonical::canonical_json_exact_of(&base).expect("base"))
            .expect("UTF8"),
    )
    .expect("core commitment source")
}

/// PROOF-LIFECYCLE-2/3 + LIFE-SOUND-4 on a real `core.Commitment` implementer
/// with inherited integrity invariants (`positive_quantity`, `single_measure`)
/// and nullable `unit`/`currency` fields.
#[test]
fn core_commitment_with_invariants_and_nullable_measures_runs_life_sound_4() {
    let base = core_commitment_base();
    let make_source = |extra_invariants: Value| core_commitment_source(&base, extra_invariants);

    let make_bounds = |unit: &str, currency: &str, qty: &str| Bounds {
        object_type: "test.OrderCommitment".into(),
        property: "status".into(),
        seed: 0,
        frozen: BTreeMap::from([
            ("due_at".into(), vec!["\"2026-12-01T00:00:00Z\"".into()]),
            ("quantity".into(), vec![qty.into()]),
            ("unit".into(), vec![unit.into()]),
            ("currency".into(), vec![currency.into()]),
        ]),
    };

    // 1. Valid measure (unit = "each", currency = null, quantity = 5): sound with zero dead ends.
    let sound_source = make_source(json!([]));
    let graph = super::lifecycle_tests::authority(&sound_source)
        .lifecycle_graph(
            &sound_source,
            &BTreeSet::new(),
            &make_bounds("\"each\"", "null", "5"),
        )
        .expect("challenge")
        .graph
        .expect("core.Commitment implementer must lower cleanly");
    assert_eq!(graph.inputs.invariants.len(), 2);
    assert_eq!(graph.initial.len(), 1);
    assert_eq!(graph.soundness().expect("states"), Vec::new());

    // 2. Violating `core.Commitment`'s inherited `single_measure` invariant (both null) refuses initial rows.
    let bad_measure = super::lifecycle_tests::authority(&sound_source)
        .lifecycle_graph(
            &sound_source,
            &BTreeSet::new(),
            &make_bounds("null", "null", "5"),
        )
        .expect("challenge")
        .graph
        .expect("graph builds");
    assert!(
        bad_measure.initial.is_empty(),
        "inherited single_measure invariant must exclude unlawful initial rows"
    );

    // 3. An integrity invariant that blocks every exit from `promised` is caught as LAW_BLOCKED_EXIT.
    let blocked_source = make_source(json!([
        {
            "id": "no_terminal_small_qty",
            "expression": "(self.status != 'fulfilled' and self.status != 'cancelled' and self.status != 'breached') or self.quantity >= 10",
            "message": "small commitments cannot finish"
        }
    ]));
    let blocked_graph = super::lifecycle_tests::authority(&blocked_source)
        .lifecycle_graph(
            &blocked_source,
            &BTreeSet::new(),
            &make_bounds("\"each\"", "null", "5"),
        )
        .expect("challenge")
        .graph
        .expect("blocked graph builds");
    let findings = blocked_graph.soundness().expect("states");
    assert!(
        findings.iter().any(
            |f| f.code == crate::lifecycle_soundness::codes::LAW_BLOCKED_EXIT
                && f.blocked.as_ref().is_some_and(|(_, laws)| laws
                    .iter()
                    .any(|id| id == "invariant:test.OrderCommitment:no_terminal_small_qty"))
        ),
        "expected LAW_BLOCKED_EXIT naming the blocking integrity invariant, got {findings:?}"
    );
}
