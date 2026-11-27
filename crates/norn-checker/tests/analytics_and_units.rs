//! End-to-end `norn-checker` verification for `registry/packs/compute-units`
//! and `registry/packs/analytics`:
//!
//! 1. Full admission over `registry/core/artifacts/*` with zero errors.
//! 2. `VAL-UNIT-1` + `VAL-RATE` dimensional checking on
//!    `Rate<USD, {token}> × Quantity<{token}> → Money<USD>` (positive admission,
//!    unit-mismatch refusal on `{GPU}.s`, unknown/unpinned unit refusal, and
//!    `units.physical` + multi-catalog + fail-closed malformed/overlapping pin checks).
//! 3. `LIFE-SOUND-1..8` structural checks and `PROOF-LIFECYCLE-2/3` bounded
//!    graph checks on `bi.CertifiedMetric` and `analytics.Study`, including
//!    `LAW_BLOCKED_EXIT` when `publish_requires_diagnostics` blocks `publish`
//!    under `diagnostics_passed = false`.
//! 4. `LAW-EXAMPLE-1` evaluation over `bi.MetricSlice` (`BI-ADD-1` zero
//!    denominator, exact & rounded scale-6 ratios, and unweighted-average refusal).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use norn_checker::law_examples::{Verdict, evaluate as evaluate_law};
use norn_checker::lifecycle_soundness::codes as soundness_codes;
use norn_checker::obligations::lifecycle::finite::Bounds;
use norn_checker::obligations::{ApprovedContract, DefinitionSource};
use norn_checker::{ArtifactValidation, Diagnostic, EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, DiagnosticSeverity, LawIR};
use serde_json::{Value, json};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[allow(clippy::panic, reason = "test fixture helper")]
fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} should be valid JSON: {error}", path.display()))
}

#[allow(clippy::expect_used, clippy::panic, reason = "test fixture helper")]
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

#[allow(clippy::expect_used, reason = "test fixture helper")]
fn decode_envelopes(values: Vec<Value>) -> Vec<ArtifactEnvelope> {
    values
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("artifact envelope decodes"))
        .collect()
}

fn load_pack(pack: &str) -> Vec<ArtifactEnvelope> {
    decode_envelopes(load_pack_values(pack))
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
                    "id": "confidential.finance",
                    "label": "Confidential finance",
                    "severity": "high",
                    "category": "confidential.finance",
                    "mask": { "kind": "redacted" }
                },
                {
                    "id": "pii.health",
                    "label": "Health PII",
                    "severity": "critical",
                    "category": "pii.health",
                    "mask": { "kind": "refuse" }
                }
            ]
        }
    }));
    values
}

fn core_base() -> Vec<ArtifactEnvelope> {
    decode_envelopes(core_base_values())
}

fn check_over_core(artifacts: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    check_artifact_set(
        artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
    )
}

#[allow(clippy::panic, reason = "test fixture helper")]
fn find_artifact(pack: &[Value], id: &str) -> Value {
    pack.iter()
        .find(|artifact| artifact.get("artifactId").and_then(Value::as_str) == Some(id))
        .unwrap_or_else(|| panic!("missing {id}"))
        .clone()
}

#[allow(clippy::expect_used, reason = "test fixture helper")]
fn lifecycle_source(artifacts: &[Value], base: &[Value]) -> DefinitionSource {
    let artifacts_json = String::from_utf8(
        norn_canonical::canonical_json_exact_of(artifacts).expect("canonical artifacts"),
    )
    .expect("UTF-8");
    let base_json =
        String::from_utf8(norn_canonical::canonical_json_exact_of(base).expect("canonical base"))
            .expect("UTF-8");
    DefinitionSource::parse(&artifacts_json, &base_json).expect("exact definition source")
}

#[allow(clippy::expect_used, reason = "test fixture helper")]
fn approve(source: &DefinitionSource) -> ApprovedContract {
    ApprovedContract::approve(
        source,
        &BTreeSet::new(),
        json!({"expr": "norn-zen/7"}),
        json!({"store": "empty"}),
        json!({"level": "bounded"}),
    )
    .expect("approved contract")
}

fn study_bounds(train: &str, holdout: &str, diag: Vec<String>) -> Bounds {
    Bounds {
        object_type: "analytics.Study".into(),
        property: "publication_status".into(),
        seed: 0,
        frozen: BTreeMap::from([
            ("id".into(), vec!["\"study-1\"".into()]),
            ("study_code".into(), vec!["\"SCRAP-OLS\"".into()]),
            ("population_digest".into(), vec!["\"22cee202\"".into()]),
            ("method_digest".into(), vec!["\"ols-v1\"".into()]),
            ("receipt_digest".into(), vec!["\"fn-receipt-001\"".into()]),
            ("train_cutoff_at".into(), vec![format!("\"{train}\"")]),
            ("holdout_start_at".into(), vec![format!("\"{holdout}\"")]),
            ("diagnostics_passed".into(), diag),
        ]),
    }
}

fn study_subset() -> Vec<Value> {
    load_pack_values("analytics")
        .into_iter()
        .filter(|artifact| {
            matches!(
                artifact.get("artifactId").and_then(Value::as_str),
                Some(
                    "analytics.Analyst.entity"
                        | "bi.CertifiedMetric.entity"
                        | "analytics.Study.entity"
                        | "analytics.Study.temporal_split_ordered.law"
                        | "analytics.Study.publish_requires_diagnostics.law"
                )
            )
        })
        .collect()
}

#[test]
fn compute_units_and_analytics_packs_admit_cleanly_over_core() {
    let base = core_base();
    let mut artifacts = load_pack("compute-units");
    artifacts.extend(load_pack("analytics"));
    assert_eq!(artifacts.len(), 48);

    for artifact in &artifacts {
        let validated = artifact.validate();
        assert!(
            validated.is_ok(),
            "{}: {validated:#?}",
            artifact.artifact_id
        );
    }

    let findings = check_over_core(&artifacts, &base);
    let errors: Vec<_> = findings
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .collect();
    assert_eq!(errors, Vec::<&_>::new(), "unexpected errors: {errors:#?}");

    let lifecycle_findings: Vec<_> = findings
        .iter()
        .filter(|d| d.code.as_str().starts_with("lifecycle."))
        .collect();
    assert_eq!(
        lifecycle_findings,
        Vec::<&_>::new(),
        "unexpected lifecycle findings: {lifecycle_findings:#?}"
    );

    let law_ids: BTreeSet<&str> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == norn_ir::ArtifactKind::Law)
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    let law_findings: Vec<_> = findings
        .iter()
        .filter(|d| {
            d.artifact_id
                .as_deref()
                .is_some_and(|id| law_ids.contains(id))
        })
        .collect();
    assert_eq!(
        law_findings,
        Vec::<&_>::new(),
        "unexpected law findings: {law_findings:#?}"
    );
}

#[test]
fn val_rate_token_pricing_admits_exact_product() {
    let base = core_base();
    let admitted = load_pack("compute-units");
    let findings = check_over_core(&admitted, &base);
    assert!(
        findings
            .iter()
            .all(|d| d.severity != DiagnosticSeverity::Error),
        "{findings:#?}"
    );
    assert!(
        findings.iter().any(|d| {
            d.code.as_str() == "core.values.literal_unit"
                && d.severity == DiagnosticSeverity::Info
                && d.message.contains("{token}")
        }),
        "expected Info literal_unit naming {{token}}: {findings:#?}"
    );
}

#[test]
fn val_rate_token_pricing_refuses_mismatched_unpinned_and_unknown_units() {
    let base = core_base();
    let original = load_pack_values("compute-units");
    let check_mutated = |mutate: fn(&mut Value)| -> Vec<Diagnostic> {
        let mut mutated = original.clone();
        let action = mutated
            .iter_mut()
            .find(|artifact| artifact["artifactId"] == "compute.usage.price.action")
            .expect("price action");
        mutate(action);
        check_over_core(&decode_envelopes(mutated), &base)
    };

    // Negative 1: dimensional mismatch — Rate<USD, {token}> * Quantity<{GPU}.s> must refuse.
    let mismatch_findings = check_mutated(|action| {
        action["payload"]["inputSchema"]["properties"]["token_quantity"]["nornType"]["unit"]["code"] =
            json!("{GPU}.s");
    });
    assert!(
        mismatch_findings.iter().any(|d| {
            d.severity == DiagnosticSeverity::Error
                && d.artifact_id.as_deref() == Some("compute.usage.price.action")
                && d.code.as_str() == "action.validation.invalid_payload"
                && d.message.contains("core.values.unit_mismatch")
        }),
        "expected VAL-RATE unit_mismatch on Rate<USD,{{token}}> * Quantity<{{GPU}}.s>: {mismatch_findings:#?}"
    );

    // Negative 2: unpinned compute unit catalog refuses with core.values.unit_mismatch.
    let unpinned_findings = check_mutated(|action| {
        action["dependencies"] = json!([]);
    });
    assert!(
        unpinned_findings.iter().any(|d| {
            d.severity == DiagnosticSeverity::Error
                && d.artifact_id.as_deref() == Some("compute.usage.price.action")
                && d.code.as_str() == "core.values.unit_mismatch"
        }),
        "expected unit_mismatch when units.compute pin is absent: {unpinned_findings:#?}"
    );

    // Negative 3: unknown unit code `{bogus}` refuses even when the catalog is pinned.
    let bogus_findings = check_mutated(|action| {
        action["payload"]["inputSchema"]["properties"]["token_quantity"]["nornType"]["unit"]["code"] =
            json!("{bogus}");
    });
    assert!(
        bogus_findings.iter().any(|d| {
            d.severity == DiagnosticSeverity::Error
                && d.artifact_id.as_deref() == Some("compute.usage.price.action")
                && d.code.as_str() == "core.values.unit_mismatch"
        }),
        "expected unit_mismatch on unknown unit {{bogus}}: {bogus_findings:#?}"
    );
}

#[test]
fn val_unit_admission_covers_physical_and_multi_catalog_pins() {
    let base = core_base();
    let pack = load_pack_values("compute-units");
    let compute_catalog = find_artifact(&pack, "units.compute.UnitDefinition.entity");
    let physical_catalog = find_artifact(&pack, "units.physical.UnitDefinition.entity");
    let token_pool_template = find_artifact(&pack, "compute.TokenPool.entity");

    // 1. Pinning `units.physical.UnitDefinition.entity` admits `kW.h`...
    let mut physical_ok = token_pool_template.clone();
    physical_ok["dependencies"] = json!([{
        "artifactId": "units.physical.UnitDefinition.entity",
        "kind": "entity",
        "required": true,
        "version": "0.1.0"
    }]);
    physical_ok["payload"]["objectType"]["properties"][6]["type"]["unit"]["code"] = json!("kW.h");
    let findings = check_over_core(
        &decode_envelopes(vec![physical_catalog.clone(), physical_ok.clone()]),
        &base,
    );
    let errors: Vec<_> = findings
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .collect();
    assert_eq!(errors, Vec::<&_>::new(), "unexpected errors: {errors:#?}");

    // ...and refuses `{token}` when only `units.physical` is pinned.
    let mut physical_refuses_token = physical_ok;
    physical_refuses_token["payload"]["objectType"]["properties"][6]["type"]["unit"]["code"] =
        json!("{token}");
    let findings = check_over_core(
        &decode_envelopes(vec![physical_catalog, physical_refuses_token]),
        &base,
    );
    assert!(
        findings.iter().any(|d| {
            d.severity == DiagnosticSeverity::Error
                && d.artifact_id.as_deref() == Some("compute.TokenPool.entity")
                && d.code.as_str() == "core.values.unit_mismatch"
        }),
        "expected unit_mismatch for {{token}} under physical-only pin: {findings:#?}"
    );

    // 2. Pinning both `core.units` and `units.compute.UnitDefinition.entity` admits both `g` and `{token}`.
    let mut multi_pin = token_pool_template;
    multi_pin["dependencies"] = json!([
        {
            "artifactId": "core.units",
            "kind": "entity",
            "required": true,
            "version": "1.0.0"
        },
        {
            "artifactId": "units.compute.UnitDefinition.entity",
            "kind": "entity",
            "required": true,
            "version": "0.1.0"
        }
    ]);
    multi_pin["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({
            "id": "mass_g",
            "type": {
                "kind": "decimal",
                "scale": 3,
                "unit": { "code": "g", "system": "ucum" },
                "additivity": { "kind": "additive" }
            }
        }));
    let findings = check_over_core(&decode_envelopes(vec![compute_catalog, multi_pin]), &base);
    let errors: Vec<_> = findings
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .collect();
    assert_eq!(errors, Vec::<&_>::new(), "unexpected errors: {errors:#?}");
}

#[test]
fn val_unit_admission_fails_closed_on_malformed_and_overlapping_catalogs() {
    let base = core_base();
    let pack = load_pack_values("compute-units");
    let compute_catalog = find_artifact(&pack, "units.compute.UnitDefinition.entity");
    let token_pool_template = find_artifact(&pack, "compute.TokenPool.entity");

    // 1. Malformed catalog (nullable `code`) fails closed even when `core.units` is also pinned.
    let mut multi_pin = token_pool_template.clone();
    multi_pin["dependencies"] = json!([
        {
            "artifactId": "core.units",
            "kind": "entity",
            "required": true,
            "version": "1.0.0"
        },
        {
            "artifactId": "units.compute.UnitDefinition.entity",
            "kind": "entity",
            "required": true,
            "version": "0.1.0"
        }
    ]);
    let mut malformed_catalog = compute_catalog.clone();
    malformed_catalog["payload"]["objectType"]["properties"][0]["constraints"] =
        json!({ "nullable": true });
    let findings = check_over_core(&decode_envelopes(vec![malformed_catalog, multi_pin]), &base);
    assert!(
        findings.iter().any(|d| {
            d.severity == DiagnosticSeverity::Error
                && d.artifact_id.as_deref() == Some("compute.TokenPool.entity")
                && d.code.as_str() == "core.values.unit_mismatch"
        }),
        "expected unit_mismatch when a pinned catalog is malformed: {findings:#?}"
    );

    // 2. Overlapping catalog id `units.UnitDefinition.entity` (empty namespace segment) is rejected.
    let mut overlapping_catalog = compute_catalog;
    overlapping_catalog["artifactId"] = json!("units.UnitDefinition.entity");
    overlapping_catalog["payload"]["objectType"]["name"] = json!("units.UnitDefinition");
    let mut overlapping_pin = token_pool_template;
    overlapping_pin["dependencies"] = json!([{
        "artifactId": "units.UnitDefinition.entity",
        "kind": "entity",
        "required": true,
        "version": "0.1.0"
    }]);
    let findings = check_over_core(
        &decode_envelopes(vec![overlapping_catalog, overlapping_pin]),
        &base,
    );
    assert!(
        findings.iter().any(|d| {
            d.severity == DiagnosticSeverity::Error
                && d.artifact_id.as_deref() == Some("compute.TokenPool.entity")
                && d.code.as_str() == "core.values.unit_mismatch"
        }),
        "expected unit_mismatch for overlapping catalog id units.UnitDefinition.entity: {findings:#?}"
    );
}

#[test]
fn proof_lifecycle_certified_metric_is_sound_with_resting_and_cancelled_endings() {
    let subset: Vec<Value> = load_pack_values("analytics")
        .into_iter()
        .filter(|artifact| {
            matches!(
                artifact["artifactId"].as_str(),
                Some(
                    "analytics.Analyst.entity"
                        | "bi.CertifiedMetric.entity"
                        | "bi.CertifiedMetric.freshness_sla_positive.law"
                )
            )
        })
        .collect();
    assert_eq!(subset.len(), 3);

    let source = lifecycle_source(&subset, &core_base_values());
    let bounds = Bounds {
        object_type: "bi.CertifiedMetric".into(),
        property: "status".into(),
        seed: 0,
        frozen: BTreeMap::from([
            ("id".into(), vec!["\"m-1\"".into()]),
            ("metric_name".into(), vec!["\"scrap.yield\"".into()]),
            ("additivity".into(), vec!["\"non_additive\"".into()]),
            ("grain".into(), vec!["\"conversion_id\"".into()]),
            ("unit_code".into(), vec!["\"1\"".into()]),
            ("freshness_sla_ms".into(), vec!["5000".into()]),
        ]),
    };

    let batch = approve(&source)
        .lifecycle_graph(&source, &BTreeSet::new(), &bounds)
        .expect("certified metric lifecycle graph");
    let graph = batch.graph.expect("bounded graph builds");
    assert_eq!(graph.initial.len(), 1);
    assert_eq!(graph.blocked, Vec::new());
    assert_eq!(graph.soundness().expect("soundness"), Vec::new());
    let transitions: BTreeSet<&str> = graph.edges.iter().map(|e| e.transition.as_str()).collect();
    assert_eq!(transitions, BTreeSet::from(["certify", "deprecate"]));
}

#[test]
fn proof_lifecycle_study_with_withhold_remedy_is_sound_and_rejects_overlap() {
    let subset = study_subset();
    assert_eq!(subset.len(), 5);

    let source = lifecycle_source(&subset, &core_base_values());
    let full_batch = approve(&source)
        .lifecycle_graph(
            &source,
            &BTreeSet::new(),
            &study_bounds(
                "2026-09-28T00:00:00Z",
                "2026-09-28T00:00:00Z",
                vec!["false".into(), "true".into()],
            ),
        )
        .expect("study graph");
    let full_graph = full_batch.graph.expect("bounded graph builds");
    assert_eq!(
        full_graph.soundness().expect("soundness"),
        Vec::new(),
        "withhold remedy keeps the full Study lifecycle sound even when diagnostics fail"
    );
    assert_eq!(full_graph.blocked.len(), 1, "{:#?}", full_graph.blocked);
    assert_eq!(full_graph.blocked[0].transition, "publish");
    assert_eq!(
        full_graph.blocked[0].laws,
        vec!["analytics.Study.publish_requires_diagnostics.law"]
    );
    assert_eq!(
        full_graph.inputs.nodes[full_graph.blocked[0].from]["diagnostics_passed"],
        norn_ir::RuntimeValue::Bool(false)
    );

    // Overlapping temporal split (train_cutoff_at > holdout_start_at): no lawful initial state.
    let leaky_graph = approve(&source)
        .lifecycle_graph(
            &source,
            &BTreeSet::new(),
            &study_bounds(
                "2026-10-05T00:00:00Z",
                "2026-09-28T00:00:00Z",
                vec!["true".into()],
            ),
        )
        .expect("leaky graph")
        .graph
        .expect("bounded graph builds");
    assert!(
        leaky_graph.initial.is_empty(),
        "temporal_split_ordered must reject overlapping train/holdout split"
    );
}

#[test]
fn proof_lifecycle_study_without_withhold_reports_law_blocked_exit() {
    let mut no_withhold = study_subset();
    let study_entity = no_withhold
        .iter_mut()
        .find(|artifact| artifact["artifactId"] == "analytics.Study.entity")
        .expect("study entity");
    let props = study_entity["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties");
    let status_prop = props
        .iter_mut()
        .find(|prop| prop["id"] == "publication_status")
        .expect("publication_status");
    status_prop["type"]["values"] = json!([
        {"value": "preregistered"},
        {"value": "evaluated"},
        {"value": "published"}
    ]);
    study_entity["payload"]["objectType"]["lifecycles"][0] = json!({
        "property": "publication_status",
        "initial": ["preregistered"],
        "terminal": ["published"],
        "immutable": ["published"],
        "endings": [{"state": "published", "outcome": "completed"}],
        "transitions": [
            {"id": "evaluate", "from": ["preregistered"], "to": "evaluated"},
            {"id": "publish", "from": ["evaluated"], "to": "published"}
        ]
    });

    let mutant_source = lifecycle_source(&no_withhold, &core_base_values());
    let blocked_graph = approve(&mutant_source)
        .lifecycle_graph(
            &mutant_source,
            &BTreeSet::new(),
            &study_bounds(
                "2026-09-28T00:00:00Z",
                "2026-09-28T00:00:00Z",
                vec!["false".into()],
            ),
        )
        .expect("mutant graph")
        .graph
        .expect("bounded mutant graph");
    let findings = blocked_graph.soundness().expect("mutant soundness");
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].code, soundness_codes::LAW_BLOCKED_EXIT);
    assert_eq!(findings[0].state, "evaluated");
    assert_eq!(findings[0].run, vec!["evaluate"]);
    assert_eq!(
        findings[0].blocked,
        Some((
            "publish".to_owned(),
            vec!["analytics.Study.publish_requires_diagnostics.law".to_owned()]
        ))
    );

    let unblocked_graph = approve(&mutant_source)
        .lifecycle_graph(
            &mutant_source,
            &BTreeSet::new(),
            &study_bounds(
                "2026-09-28T00:00:00Z",
                "2026-09-28T00:00:00Z",
                vec!["true".into()],
            ),
        )
        .expect("unblocked graph")
        .graph
        .expect("bounded unblocked graph");
    assert_eq!(unblocked_graph.soundness().expect("soundness"), Vec::new());
}

#[test]
fn metric_slice_laws_admit_exact_and_rounded_ratios_and_refuse_invalid() {
    let analytics = load_pack_values("analytics");
    let law_by_id = |id: &str| -> LawIR {
        let artifact = find_artifact(&analytics, id);
        serde_json::from_value(artifact["payload"].clone()).expect("LawIR decodes")
    };
    let slice = |id: &str, num: Value, den: Value, val: Value| {
        json!({
            "id": id,
            "slice_key": "2026-W43:gold",
            "additivity": "non_additive",
            "unit_code": "g",
            "effective_at": "2026-10-26T00:00:00Z",
            "known_at": "2026-10-26T00:05:00Z",
            "numerator": num,
            "denominator": den,
            "value": val
        })
    };

    let den_non_zero = law_by_id("bi.MetricSlice.denominator_non_zero.law");
    let ratio_consistent = law_by_id("bi.MetricSlice.non_additive_ratio_consistent.law");

    let valid_exact = slice("s-1", json!(294), json!(300), json!(0.98));
    assert_eq!(
        evaluate_law(&den_non_zero, &valid_exact),
        Ok(Verdict::Holds)
    );
    assert_eq!(
        evaluate_law(&ratio_consistent, &valid_exact),
        Ok(Verdict::Holds)
    );

    // Non-terminating ratio 1/3 rounded to scale 6 (0.333333) holds; off by 1 ULP (0.333332) fails.
    let valid_third = slice("s-third", json!(1), json!(3), json!(0.333_333));
    assert_eq!(
        evaluate_law(&ratio_consistent, &valid_third),
        Ok(Verdict::Holds)
    );
    let bad_third = slice("s-third-bad", json!(1), json!(3), json!(0.333_332));
    assert_eq!(
        evaluate_law(&ratio_consistent, &bad_third),
        Ok(Verdict::Violated)
    );

    let zero_den = slice("s-2", json!(0), json!(0), json!(0));
    assert_eq!(
        evaluate_law(&den_non_zero, &zero_den),
        Ok(Verdict::Violated)
    );
    assert_eq!(
        evaluate_law(&ratio_consistent, &zero_den),
        Ok(Verdict::Violated)
    );

    // Classic BI bug: unweighted average of 1/1 and 0/3 gives 0.50 instead of 1/4 = 0.25.
    let bad_avg_ratio = slice("s-3", json!(1), json!(4), json!(0.5));
    assert_eq!(
        evaluate_law(&ratio_consistent, &bad_avg_ratio),
        Ok(Verdict::Violated)
    );
}

#[test]
#[allow(clippy::expect_used, clippy::panic, reason = "integration test")]
fn analytics_study_evaluate_pins_admitted_diagnostics_function_manifest() {
    let manifest_path =
        repo_root().join("registry/packs/analytics/diagnostics.function-manifest.json");
    let manifest_text = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", manifest_path.display()));
    let manifest = norn_checker::function::decode_function_manifest(&manifest_text)
        .unwrap_or_else(|findings| panic!("diagnostics manifest should decode: {findings:#?}"));
    let admitted = norn_checker::function::admit_function_manifest(&manifest)
        .unwrap_or_else(|findings| panic!("diagnostics manifest should admit: {findings:#?}"));

    let evaluate_action = read_json(
        &repo_root()
            .join("registry/packs/analytics/artifacts/analytics.study.evaluate.action.json"),
    );
    let pinned_digest =
        evaluate_action["payload"]["effectProgram"]["acceptReceipt"]["manifestDigest"]
            .as_str()
            .expect("analytics.study.evaluate must pin acceptReceipt.manifestDigest");
    assert_eq!(
        admitted.digest.0, pinned_digest,
        "analytics.study.evaluate.action must pin the admitted diagnostics FunctionManifest digest"
    );
}
