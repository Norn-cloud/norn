//! Core 08 Fork Unit 2b (E4, #1947, `FORK-EVIDENCE-1` Lemma L6): pin-bound
//! fork writes under storage version 11 and auditable simulated Decision Card
//! evidence that outlives its fork while never passing as `main` evidence.

#![cfg(feature = "native")]

mod query_support;

use std::collections::BTreeMap;

use norn_cell::compute::{QueryContext, Watermark};
use norn_cell::engine::NativeEngine;
use norn_cell::export::{ExportError, ExportManifest, ExportRecord};
use norn_cell::{
    CellError, CellStore, ComparisonBaseline, ForkCommitEvidence, MAIN_WORLD, Model,
    SimulatedDecisionCardEvidence, SqliteCell, verify_fork_commit_evidence,
    verify_main_export_record, verify_main_receipt,
};
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::{QuestionPlan, admit};
use norn_ir::{
    ArtifactEnvelope, Decimal, MetricDefinition, ObjectTypeDef, QuestionIR, RuntimeValue as V,
};
use norn_kernel::{
    CellCommit, CellHost, FactKey, FactMutation, FactVersion, FactWrite, LogicalTime,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type SimCell = SqliteCell<SimEvent, SimCommand, SimInput>;

const SCOPE: &str = "org_demo/dev";
const CLOCK: u64 = 1_792_972_800_000;
const CARD_TYPE: &str = "demo.DecisionCard";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum SimEvent {
    LotRecorded { key: String },
    CardSaved { key: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SimCommand {
    key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SimInput {
    key: String,
}

fn decision_card_type() -> Result<ObjectTypeDef, Box<dyn std::error::Error>> {
    Ok(serde_json::from_value(json!({
        "name": CARD_TYPE,
        "properties": [
            { "id": "title", "type": { "kind": "text" } },
            { "id": "simulatedEvidence", "type": { "kind": "json-ext" } }
        ]
    }))?)
}

fn plan_and_model() -> Result<(Model, QuestionPlan), Box<dyn std::error::Error>> {
    let conversion = query_support::object_type();
    let card = decision_card_type()?;
    let ontology: Value =
        serde_json::from_str(include_str!("../../../fixtures/ir/metrics/ontology.json"))?;
    let mut artifact = ontology
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .ok_or("missing entity artifact in metrics fixture")?;
    artifact["artifactId"] = json!("demo.Conversion.entity.v2");
    artifact["name"] = json!(query_support::TYPE);
    artifact["namespace"] = json!("demo");
    artifact["provenance"] = json!({"source":"import","sourcePath":"synthetic:fork-e4-evidence"});
    artifact["payload"] = json!({"entityVersion":"2.0","objectType":conversion});
    let artifact: ArtifactEnvelope = serde_json::from_value(artifact)?;
    let catalog =
        ScopedView::uniform(&[artifact], &std::collections::BTreeSet::new()).into_catalog();
    let model = Model::from_definitions(&[conversion, card], &[])?;

    let mut metric: MetricDefinition = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/net-sales.definition.json"
    ))?;
    metric.name = "demo.recovered_total".into();
    metric.label = "Sum of recovered".to_owned();
    metric.type_params.expr = Some("recovered".into());
    if let Some(aggregate) = metric.type_params.metric_aggregation_params.as_mut() {
        aggregate.semantic_model = norn_ir::ObjectTypeName(query_support::TYPE.into());
        aggregate.agg_time_dimension = "occurred_at".into();
    }
    metric.norn.currency = None;
    metric.norn.default_filters.clear();
    metric.norn.time_dimension = Some(serde_json::from_value(json!({"property": "occurred_at"}))?);

    let mut question: QuestionIR = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))?;
    question.name = "demo_recovered_weekly".to_owned();
    question.object_set = norn_ir::ObjectTypeName(query_support::TYPE.into());
    question.comparison = None;
    question.metrics = serde_json::from_value(json!([{"metric": "demo.recovered_total"}]))?;
    question.dimensions =
        serde_json::from_value(json!([{"path": {"property": "occurred_at"}, "grain": "week"}]))?;
    question.filters = serde_json::from_value(
        json!([{"path": {"property": "metal"}, "operator": "eq", "value": "gold"}]),
    )?;
    question.time_window.start = norn_ir::UtcTimestamp("2026-07-06T00:00:00Z".into());
    question.time_window.end = norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into());
    question.freshness = norn_ir::metric::FreshnessPolicy::Live {
        max_age_ms: norn_ir::WholeNumber(5_000),
    };

    let plan = admit(&[metric], &question, &catalog, 100)
        .map_err(|err| format!("admit sum plan: {err:?}"))?;
    Ok((model, plan))
}

fn open_sim_cell(store: CellStore) -> Result<SimCell, CellError> {
    let mut cell = SqliteCell::new(store, SCOPE, |cmd: &norn_kernel::Command<SimCommand>| {
        SimInput {
            key: cmd.payload.key.clone(),
        }
    })?;
    cell.advance_to(LogicalTime(CLOCK));
    Ok(cell)
}

fn main_context() -> QueryContext {
    QueryContext {
        world: MAIN_WORLD.to_owned(),
        scope: SCOPE.to_owned(),
        population_pin: "pop:e4-fork-evidence".to_owned(),
        authorization_epoch: 1,
        mapping_pin: "mapping:e4-v1".to_owned(),
        now_ms: CLOCK,
        source_cursors: BTreeMap::from([(
            "erp".to_owned(),
            BTreeMap::from([("lots".to_owned(), "cursor/10".to_owned())]),
        )]),
        watermarks: BTreeMap::from([(
            "erp".to_owned(),
            BTreeMap::from([(
                "lots".to_owned(),
                Watermark {
                    observed: "cursor/10".to_owned(),
                    heartbeat_ms: Some(CLOCK),
                    complete: true,
                },
            )]),
        )]),
    }
}

fn commit_conversion(cell: &mut SimCell, key: &str, recovered_mg: i64, time_ms: u64) -> TestResult {
    cell.advance_to(LogicalTime(time_ms));
    let expected = FactVersion(cell.store().read().version(
        MAIN_WORLD,
        query_support::TYPE,
        key,
    )?);
    let values = BTreeMap::from([
        (
            "input".to_owned(),
            V::from(Decimal::parse(&format!("{recovered_mg}e-3"))?),
        ),
        (
            "recovered".to_owned(),
            V::from(Decimal::parse(&format!("{recovered_mg}e-3"))?),
        ),
        ("loss".to_owned(), V::from(Decimal::parse("0e-3")?)),
        ("occurred_at".to_owned(), V::from("2026-10-19T12:00:00Z")),
        ("metal".to_owned(), V::from("gold")),
    ]);
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new(query_support::TYPE, key),
            expected,
            mutation: FactMutation::Put { values },
        }],
        events: vec![SimEvent::LotRecorded {
            key: key.to_owned(),
        }],
        ..CellCommit::empty()
    })
    .map_err(|err| format!("commit conversion {key}: {err:?}"))?;
    Ok(())
}

struct SeededForks {
    main: SimCell,
    d1: SimCell,
    s1: SimCell,
    cell_id: norn_ir::cell_id::CellId,
    main_export: ExportRecord,
    d1_ev: ForkCommitEvidence,
    s1_ev: ForkCommitEvidence,
}

fn seed_nested_forks(model: Model) -> Result<SeededForks, Box<dyn std::error::Error>> {
    let mut main = open_sim_cell(CellStore::open_in_memory(model)?)?;
    commit_conversion(&mut main, "lot-main", 100_000, CLOCK)?;
    let main_receipt = main.store().read().receipt_at(1)?.ok_or("main receipt 1")?;
    verify_main_receipt(&main_receipt)?;
    let cell_id = main.store().cell_id()?.ok_or("main cell id missing")?;
    let main_export = main
        .store()
        .export_record(1)?
        .ok_or("main export 1 missing")?;
    verify_main_export_record(&main_export)?;

    let d1_base = main.store().verified_head()?;
    let mut d1 = open_sim_cell(main.store().fork("decision-1", &d1_base)?)?;
    commit_conversion(&mut d1, "lot-d1-a", 40_000, CLOCK + 1)?;
    commit_conversion(&mut d1, "lot-d1-b", 15_000, CLOCK + 2)?;
    let d1_ev = d1
        .store()
        .export_fork_commit_evidence(2)?
        .ok_or("d1 commit 2 evidence missing")?;

    let s1_base = d1.store().verified_head()?;
    let mut s1 = open_sim_cell(d1.store().fork("scenario-1", &s1_base)?)?;
    commit_conversion(&mut s1, "lot-s1", 25_000, CLOCK + 3)?;
    let s1_ev = s1
        .store()
        .export_fork_commit_evidence(4)?
        .ok_or("s1 commit 4 evidence missing")?;

    Ok(SeededForks {
        main,
        d1,
        s1,
        cell_id,
        main_export,
        d1_ev,
        s1_ev,
    })
}

#[test]
fn pin_bound_fork_writes_tag_receipts_rows_and_entries_across_nesting_depths() -> TestResult {
    let (model, _plan) = plan_and_model()?;
    let seeded = seed_nested_forks(model)?;
    let d1_pin = seeded.d1.store().fork_pin().cloned().ok_or("d1 pin")?;
    let s1_pin = seeded.s1.store().fork_pin().cloned().ok_or("s1 pin")?;
    assert_eq!(d1_pin.main_horizon, 1);
    assert_eq!(s1_pin.main_horizon, 1);
    assert_eq!(s1_pin.parent, "decision-1");

    let main_receipt = seeded
        .main
        .store()
        .read()
        .receipt_at(1)?
        .ok_or("main receipt 1")?;
    assert!(!main_receipt.is_fork_minted());
    assert_eq!(main_receipt.fork_pin(), None);
    assert_eq!(
        main_receipt
            .body
            .written
            .first()
            .and_then(|row| row.fork_world.as_deref()),
        None
    );

    verify_fork_commit_evidence(&seeded.d1_ev, &seeded.cell_id)?;
    assert!(seeded.d1_ev.record.receipt.is_fork_minted());
    assert_eq!(seeded.d1_ev.commit_pin, d1_pin);
    assert_eq!(seeded.d1_ev.exporting_pin, d1_pin);
    assert!(seeded.d1_ev.lineage_chain.is_empty());
    assert_eq!(
        seeded
            .d1_ev
            .record
            .receipt
            .body
            .written
            .first()
            .and_then(|row| row.fork_world.as_deref()),
        Some("decision-1")
    );
    assert_eq!(
        seeded
            .d1_ev
            .record
            .entries
            .first()
            .and_then(|entry| entry.fork_pin.as_ref()),
        Some(&d1_pin)
    );

    verify_fork_commit_evidence(&seeded.s1_ev, &seeded.cell_id)?;
    assert_eq!(seeded.s1_ev.commit_pin, s1_pin);
    assert_eq!(seeded.s1_ev.exporting_pin, s1_pin);
    assert!(seeded.s1_ev.lineage_chain.is_empty());

    // Exporting earlier inherited commit 2 from S_1 (base = commit 3) carries
    // commit 3 in `lineage_chain`, cryptographically proving membership in S_1.
    let inherited_earlier = seeded
        .s1
        .store()
        .export_fork_commit_evidence(2)?
        .ok_or("s1 inherited commit 2 evidence missing")?;
    verify_fork_commit_evidence(&inherited_earlier, &seeded.cell_id)?;
    assert_eq!(inherited_earlier.commit_pin, d1_pin);
    assert_eq!(inherited_earlier.exporting_pin, s1_pin);
    assert_eq!(inherited_earlier.lineage_chain.len(), 1);

    // Exporting inherited parent tip commit 3 needs no extra lineage links.
    let inherited_tip = seeded
        .s1
        .store()
        .export_fork_commit_evidence(3)?
        .ok_or("s1 inherited commit 3 evidence missing")?;
    verify_fork_commit_evidence(&inherited_tip, &seeded.cell_id)?;
    assert!(inherited_tip.lineage_chain.is_empty());

    assert!(seeded.s1.store().export_record(1)?.is_some());
    assert!(matches!(
        seeded.s1.store().export_record(2),
        Err(CellError::ForkEvidenceForbidden)
    ));
    assert!(matches!(
        seeded.s1.store().export_record(4),
        Err(CellError::ForkEvidenceForbidden)
    ));
    Ok(())
}

#[test]
fn lemma_l6_refuses_fork_evidence_as_main_at_depths_one_and_two() -> TestResult {
    let (model, _plan) = plan_and_model()?;
    let seeded = seed_nested_forks(model)?;

    assert!(matches!(
        seeded.main.store().export_fork_commit_evidence(1),
        Err(CellError::WorldSimulationForbidden)
    ));
    assert!(matches!(
        seeded.d1.store().export_fork_commit_evidence(1),
        Err(CellError::ForkInvalidWorld)
    ));

    let manifest = ExportManifest::empty().advance(&seeded.main_export)?;
    for evidence in [&seeded.d1_ev, &seeded.s1_ev] {
        assert!(matches!(
            verify_main_receipt(&evidence.record.receipt),
            Err(CellError::ForkEvidenceForbidden)
        ));
        assert!(matches!(
            verify_main_export_record(&evidence.record),
            Err(CellError::ForkEvidenceForbidden)
        ));
        assert!(matches!(
            evidence.record.advance(&manifest),
            Err(ExportError::Cell(CellError::ForkEvidenceForbidden))
        ));
        assert!(matches!(
            evidence.record.validate_manifest(&manifest),
            Err(ExportError::Cell(CellError::ForkEvidenceForbidden))
        ));

        let mut stripped = evidence.record.receipt.clone();
        stripped.body.fork_pin = None;
        for row in &mut stripped.body.written {
            row.fork_world = None;
        }
        assert!(matches!(
            verify_main_receipt(&stripped),
            Err(CellError::LedgerCorrupt { .. })
        ));

        let chain = [seeded.main_export.clone(), evidence.record.clone()];
        let tip = evidence.record.hash()?;
        assert!(matches!(
            CellStore::rebuild_export_history(NativeEngine::open_in_memory()?, &chain, &tip),
            Err(CellError::ForkEvidenceForbidden)
        ));
    }
    Ok(())
}

#[test]
fn same_world_id_sibling_commit_cannot_pose_as_inherited_ancestry() -> TestResult {
    let (model, _plan) = plan_and_model()?;
    let seeded = seed_nested_forks(model)?;
    let s1_inherited = seeded
        .s1
        .store()
        .export_fork_commit_evidence(2)?
        .ok_or("s1 inherited commit 2")?;

    // Construct a distinct fork with the EXACT SAME world ID ("decision-1") and
    // identical `ForkPin` as `d1`, but a different commit 2 history.
    let base = seeded.main.store().verified_head()?;
    let mut alt_d1 = open_sim_cell(seeded.main.store().fork("decision-1", &base)?)?;
    commit_conversion(&mut alt_d1, "lot-evil", 99_000, CLOCK + 1)?;
    let alt_ev = alt_d1
        .store()
        .export_fork_commit_evidence(2)?
        .ok_or("alt_d1 commit 2")?;
    assert_eq!(alt_ev.commit_pin, s1_inherited.commit_pin);

    // Substituting `alt_d1`'s commit 2 under `s1`'s lineage chain must fail
    // cryptographic hash chaining into `s1.base`.
    let mut forged = s1_inherited;
    forged.record = alt_ev.record;
    assert!(matches!(
        verify_fork_commit_evidence(&forged, &seeded.cell_id),
        Err(CellError::LedgerCorrupt { .. } | CellError::ForkInvalidWorld)
    ));
    Ok(())
}

#[test]
fn simulated_decision_card_evidence_persists_on_main_and_outlives_discarded_fork() -> TestResult {
    let (model, plan) = plan_and_model()?;
    let mut main = open_sim_cell(CellStore::open_in_memory(model)?)?;
    commit_conversion(&mut main, "lot-main", 100_000, CLOCK)?;
    let cell_id = main.store().cell_id()?.ok_or("main cell id missing")?;
    let main_answer = main
        .store()
        .evaluate_question(&plan, main_context(), |_| Ok(true))?;

    let d1_base = main.store().verified_head()?;
    let mut d1 = open_sim_cell(main.store().fork("decision-1", &d1_base)?)?;
    commit_conversion(&mut d1, "lot-d1", 50_000, CLOCK + 1)?;
    let d1_answer = d1
        .store()
        .evaluate_question_in_fork(&plan, main_context(), |_| Ok(true))?;
    let commit_ev = d1
        .store()
        .export_fork_commit_evidence(2)?
        .ok_or("d1 commit 2 evidence")?;

    let bundle = SimulatedDecisionCardEvidence::build(
        &cell_id,
        &d1_answer,
        Some(ComparisonBaseline::Main(&main_answer)),
        vec![commit_ev],
    )?;
    let encoded = bundle.to_runtime_value()?;

    main.advance_to(LogicalTime(CLOCK + 2));
    main.commit(CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new(CARD_TYPE, "card-1"),
            expected: FactVersion(0),
            mutation: FactMutation::Put {
                values: BTreeMap::from([
                    ("title".to_owned(), V::from("Expedite supplier batch")),
                    ("simulatedEvidence".to_owned(), encoded),
                ]),
            },
        }],
        events: vec![SimEvent::CardSaved {
            key: "card-1".to_owned(),
        }],
        ..CellCommit::empty()
    })
    .map_err(|err| format!("persist decision card: {err:?}"))?;

    d1.discard_fork()?;
    assert!(matches!(
        d1.store().verified_head(),
        Err(CellError::ForkClosed { .. })
    ));
    assert!(matches!(
        d1.store().export_fork_commit_evidence(2),
        Err(CellError::ForkClosed { .. })
    ));

    let stored = main
        .store()
        .read()
        .object(MAIN_WORLD, CARD_TYPE, "card-1")?
        .ok_or("persisted decision card missing")?;
    let raw = stored
        .values
        .get("simulatedEvidence")
        .ok_or("simulatedEvidence field missing")?;
    let decoded = SimulatedDecisionCardEvidence::from_runtime_value(raw)?;
    assert_eq!(decoded, bundle);
    decoded.verify(&cell_id)?;
    Ok(())
}

#[test]
fn simulated_decision_card_evidence_refuses_tampering_and_missing_tip() -> TestResult {
    let (model, plan) = plan_and_model()?;
    let mut main = open_sim_cell(CellStore::open_in_memory(model)?)?;
    commit_conversion(&mut main, "lot-main", 100_000, CLOCK)?;
    let cell_id = main.store().cell_id()?.ok_or("main cell id missing")?;
    let main_answer = main
        .store()
        .evaluate_question(&plan, main_context(), |_| Ok(true))?;

    let d1_base = main.store().verified_head()?;
    let mut d1 = open_sim_cell(main.store().fork("decision-1", &d1_base)?)?;
    commit_conversion(&mut d1, "lot-d1", 50_000, CLOCK + 1)?;
    let d1_answer = d1
        .store()
        .evaluate_question_in_fork(&plan, main_context(), |_| Ok(true))?;
    let commit_ev = d1
        .store()
        .export_fork_commit_evidence(2)?
        .ok_or("d1 commit 2 evidence")?;

    // Empty supporting commits when `question_revision.commits > main_horizon` must refuse.
    assert!(matches!(
        SimulatedDecisionCardEvidence::build(
            &cell_id,
            &d1_answer,
            Some(ComparisonBaseline::Main(&main_answer)),
            Vec::new(),
        ),
        Err(CellError::ForkInvalidWorld)
    ));

    // Mutated `rows` under the original `lineage_ref` must refuse at build time.
    let mut tampered_answer = d1_answer.clone();
    for row in tampered_answer.result.rows.values_mut() {
        row.value = Some(Decimal::parse("999.000")?);
    }
    assert!(matches!(
        SimulatedDecisionCardEvidence::build(
            &cell_id,
            &tampered_answer,
            Some(ComparisonBaseline::Main(&main_answer)),
            vec![commit_ev.clone()],
        ),
        Err(CellError::ForkInvalidWorld)
    ));

    let bundle = SimulatedDecisionCardEvidence::build(
        &cell_id,
        &d1_answer,
        Some(ComparisonBaseline::Main(&main_answer)),
        vec![commit_ev],
    )?;

    let mut wrong_delta = bundle.clone();
    if let Some(compare) = wrong_delta.comparison.as_mut() {
        for delta in compare.deltas.values_mut() {
            delta.delta_value = Some(Decimal::parse("999.000")?);
        }
    }
    assert!(matches!(
        wrong_delta.verify(&cell_id),
        Err(CellError::ForkInvalidWorld)
    ));

    let mut wrong_digest = bundle.clone();
    wrong_digest.evidence_digest = "tampered".to_owned();
    assert!(matches!(
        wrong_digest.verify(&cell_id),
        Err(CellError::ForkEvidenceForbidden)
    ));

    let mut mutated_commit = bundle;
    if let Some(first) = mutated_commit.commit_evidence.first_mut() {
        first.record.batch.logical_time = CLOCK + 999;
    }
    assert!(matches!(
        mutated_commit.verify(&cell_id),
        Err(CellError::LedgerCorrupt { .. })
    ));
    Ok(())
}
