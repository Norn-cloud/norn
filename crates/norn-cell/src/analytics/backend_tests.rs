use std::collections::BTreeMap;

use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{MetricDefinition, ObjectTypeDef, QuestionIR};
use serde_json::json;

use super::*;
use crate::compute::{Engine as ComputeEngine, QueryContext};
use crate::{CellError, CellStore, MAIN_WORLD, Model};

type Fixture = (CellStore, QuestionPlan, TenantId);

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let object: ObjectTypeDef = serde_json::from_value(json!({
        "name": "demo.Order",
        "properties": [
            {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
            {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}}
        ]
    }))?;
    let entity = serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": "demo.Order", "kind": "entity",
        "namespace": "demo", "name": "demo.Order", "version": "1.0.0",
        "lifecycle": "active", "scope": {"orgId": "org_demo", "environmentId": "dev"},
        "dependencies": [], "provenance": {"source": "authoring_sdk"},
        "payload": {"entityVersion": "2.0", "objectType": object}
    }))?;
    let metric: MetricDefinition = serde_json::from_value(json!({
        "name": "demo.total_amount", "label": "Total amount", "type": "simple",
        "type_params": {"expr": "amount", "metric_aggregation_params": {
            "semantic_model": "demo.Order", "agg": "sum", "agg_time_dimension": "occurred_at"}},
        "x-norn": {"irVersion": "0.1", "additivity": {"kind": "additive"},
            "timeDimension": {"property": "occurred_at"}, "owner": "finance", "certified": true}
    }))?;
    let question: QuestionIR = serde_json::from_value(json!({
        "irVersion": "0.1", "name": "demo.order_total", "objectSet": "demo.Order",
        "aggregate": [{"metric": "demo.total_amount"}],
        "window": {"start": "2026-01-01T00:00:00Z", "end": "2026-12-31T00:00:00Z"},
        "freshness": {"kind": "live", "maxAgeMs": 5000}, "calendar": "iso_utc"
    }))?;
    let catalog = EffectiveCatalog::from_artifacts(&[entity]);
    let plan = norn_checker::metric::compute::admit(&[metric], &question, &catalog, 100)
        .map_err(|diags| format!("{diags:?}"))?;
    let store = CellStore::open_in_memory(Model::from_definitions(&[object], &[])?)?;
    Ok((store, plan, TenantId::of_scope("org_demo/dev")))
}

fn context() -> QueryContext {
    QueryContext {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        population_pin: "test".to_owned(),
        authorization_epoch: 1,
        mapping_pin: "test".to_owned(),
        now_ms: 10_000,
        source_cursors: BTreeMap::new(),
        watermarks: BTreeMap::new(),
    }
}

fn answer(provenance: SnapshotProvenance) -> EvaluateResponse {
    EvaluateResponse {
        population: 0,
        excluded: Vec::new(),
        buckets: Vec::new(),
        unlinked: 0,
        provenance,
    }
}

/// QUERY-FRESH-1: the cell accepts an engine's answer only for exactly
/// the snapshot it prepared, read exactly, and reports that engine.
#[test]
fn an_answer_is_accepted_only_for_the_snapshot_prepared() -> Result<(), Box<dyn std::error::Error>>
{
    let (store, plan, tenant) = fixture()?;
    let prepared = store.analytics_prepare(&plan, context(), &tenant)?;
    let Source::Mirror {
        world,
        table,
        frontier,
        scope,
        ..
    } = prepared.request().source.clone()
    else {
        return Err("a mirror source".into());
    };
    let read = ReadSnapshot::Mirror {
        world,
        table: table.clone(),
        frontier,
        scope,
        model_pin: store.model().model_hash().to_owned(),
        schema_pin: store.analytics_table(&table)?.pin,
    };
    let exact = SnapshotProvenance {
        engine: AnalyticalEngineId::DataFusion,
        tenant: tenant.clone(),
        snapshot: read,
        freshness: Freshness::Exact,
    };
    let edit = |f: fn(&mut SnapshotProvenance)| {
        let mut p = exact.clone();
        f(&mut p);
        p
    };
    let snapshot = |f: fn(&mut ReadSnapshot)| {
        let mut p = exact.clone();
        f(&mut p.snapshot);
        p
    };
    let mirror_field = |f: fn(&mut String, &mut u64, &mut String, &mut String)| {
        let mut p = exact.clone();
        if let ReadSnapshot::Mirror {
            world,
            frontier,
            model_pin,
            schema_pin,
            ..
        } = &mut p.snapshot
        {
            f(world, frontier, model_pin, schema_pin);
        }
        p
    };
    let stale = [
        edit(|p| p.tenant = TenantId::of_scope("org_other/dev")),
        edit(|p| p.freshness = Freshness::Stale),
        edit(|p| p.freshness = Freshness::Fresh),
        mirror_field(|world, _, _, _| *world = "branch".into()),
        mirror_field(|_, frontier, _, _| *frontier = u64::MAX),
        mirror_field(|_, _, model, _| *model = "another model".into()),
        mirror_field(|_, _, _, schema| *schema = "another schema".into()),
        snapshot(|s| {
            *s = ReadSnapshot::Dataset {
                table: "demo.Order".into(),
                load: "load-1".into(),
            }
        }),
    ];
    for provenance in stale {
        let prepared = store.analytics_prepare(&plan, context(), &tenant)?;
        assert!(
            matches!(
                store.analytics_answer(prepared, answer(provenance.clone())),
                Err(CellError::ComputeRefused("core.compute.stale_input"))
            ),
            "{provenance:?}"
        );
    }
    let prepared = store.analytics_prepare(&plan, context(), &tenant)?;
    let result = store.analytics_answer(prepared, answer(exact))?;
    assert_eq!(result.engine, ComputeEngine::DataFusion);
    Ok(())
}

/// D136 / Core 03: a historical read (a valid-time `as_of` or a knowledge
/// cutoff's model) needs `exact_bitemporal`; a current read needs nothing.
#[test]
fn historical_reads_need_exact_bitemporal() -> Result<(), Box<dyn std::error::Error>> {
    let (store, plan, tenant) = fixture()?;
    let prepared = store.analytics_prepare(&plan, context(), &tenant)?;
    let current = prepared.request().query.clone();
    let as_of = Lowered {
        as_of: Some(crate::analytics::AsOf {
            column: "occurred_at".into(),
            periods: Vec::new(),
        }),
        ..current.clone()
    };
    let mut at_cutoff = prepared.request().source.clone();
    if let Source::Mirror { model, .. } = &mut at_cutoff {
        *model = Some(store.model().model_hash().to_owned());
    }
    let live = &prepared.request().source;
    let none = EngineCapabilities::default();
    for (caps, query, source, answers) in [
        (none, &current, live, true),
        (none, &as_of, live, false),
        (none, &current, &at_cutoff, false),
        (EngineCapabilities::DUCKDB, &as_of, &at_cutoff, true),
    ] {
        assert_eq!(caps.answers(query, source), answers, "{query:?} {source:?}");
    }
    Ok(())
}

/// A caller the population gate denies gets the cell's own empty answer:
/// no engine call, so no engine is named.
#[test]
fn a_denied_population_is_answered_by_the_cell() -> Result<(), Box<dyn std::error::Error>> {
    let (store, plan, tenant) = fixture()?;
    let prepared = store.analytics_prepare(&plan, context(), &tenant)?;
    let result = store.analytics_empty_authorized_answer(prepared)?;
    assert_eq!(result.engine, ComputeEngine::Cell);
    assert!(result.rows.is_empty());
    Ok(())
}
