//! BENCH-ITEM-1: opt-in observed import of link-free canonical slices.
#![cfg(feature = "native")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "benchmark driver"
)]
use norn_cell::{CellStore, InstalledDefinitionInput, Model, SqliteCell, StoreOptions};
use norn_checker::{ArtifactRef, EntityArtifactSetContext};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile, RuntimeValue as Value};
use norn_kernel::{LogicalTime, ObservationId, Observed, Provenance};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::BufRead,
    time::Instant,
};

struct Target {
    entity: &'static str,
    key: &'static str,
    fields: &'static [&'static str],
    properties: &'static [(&'static str, &'static str)],
}

const ITEM: Target = Target {
    entity: "canon.Item",
    key: "product_id",
    fields: &["product_id", "label"],
    properties: &[
        ("id", "input.product_id"),
        ("code", "input.product_id"),
        ("label", "input.label"),
    ],
};
const PERSON: Target = Target {
    entity: "canon.Party",
    key: "id",
    fields: &["id"],
    properties: &[("display_name", "input.id"), ("kind", "'person'")],
};
const ORGANIZATION: Target = Target {
    entity: "canon.Party",
    key: "id",
    fields: &["id"],
    properties: &[("display_name", "input.id"), ("kind", "'organization'")],
};

const LINE: Target = Target {
    entity: "bench.RetailLine",
    key: "id",
    fields: &[
        "id",
        "invoice",
        "code",
        "quantity",
        "price",
        "customer",
        "country",
        "occurred_at",
    ],
    properties: &[
        ("id", "input.id"),
        ("invoice", "input.invoice"),
        ("code", "input.code"),
        ("quantity", "input.quantity"),
        ("price", "input.price"),
        ("customer", "input.customer"),
        ("country", "input.country"),
        ("occurred_at", "timestamp(input.occurred_at)"),
    ],
};

fn retail_line() -> (serde_json::Value, serde_json::Value, Vec<serde_json::Value>) {
    let wrap = |id: &str,
                kind: &str,
                payload: serde_json::Value,
                dependencies: serde_json::Value| {
        json!({"artifactId":id,"dependencies":dependencies,"irVersion":"0.1","kind":kind,"lifecycle":"draft",
            "name":id,"namespace":"bench","payload":payload,"version":"1.0.0",
            "provenance":{"source":"system","sourcePath":"crates/norn-cell/tests/bench_import.rs"},
            "scope":{"environmentId":"dev","orgId":"org_demo"}})
    };
    let text = json!({"kind":"text"});
    let unit = json!({"system":"ucum","code":"1"});
    let properties = json!([{"id":"id","type":text},{"id":"invoice","type":text},{"id":"code","type":text},
        {"id":"quantity","type":{"kind":"decimal","scale":0,"unit":unit,"additivity":{"kind":"additive"}}},
        {"id":"price","type":{"kind":"decimal","scale":3,"unit":unit,"additivity":{"kind":"non_additive"}}},
        {"id":"customer","type":text},{"id":"country","type":text},
        {"id":"occurred_at","type":{"kind":"timestamp","timezone":"utc"}}]);
    let entity = wrap(
        "bench.RetailLine.entity",
        "entity",
        json!({"entityVersion":"2.0","objectType":{"name":"bench.RetailLine","implements":["bench.RetailSource"],
            "identity":[{"name":"primary","properties":["id"]}],"properties":properties}}),
        json!([{"artifactId":"core.units","kind":"entity","required":true,"version":"1.0.0"}]),
    );
    let contract = wrap(
        "bench.RetailSource.interface",
        "entity",
        json!({"entityVersion":"2.0","interface":{"name":"bench.RetailSource","properties":[{"id":"id","type":text}]}}),
        json!([]),
    );
    let law = |id: &str, severity: &str, expression: &str| {
        wrap(
            &format!("bench.{id}.law"),
            "law",
            json!({"id":id,"lawVersion":"0.1","modality":{"kind":"always"},"owner":"bench",
                "provenance":{"source":"system","sourcePath":"crates/norn-cell/tests/bench_import.rs"},
                "rationale":id,"response":{"kind":"predicate","expression":expression},
                "scope":{"objectTypes":["bench.RetailLine"],"root":{"kind":"entity","id":"bench.RetailLine"}},
                "severity":severity,"subject":"bench.RetailLine"}),
            json!([{"artifactId":"bench.RetailLine.entity","kind":"entity","required":true,"version":"1.0.0"}]),
        )
    };
    let laws = vec![
        law("retail_price_nonnegative", "error", "self.price >= 0"),
        law("retail_customer_present", "warning", "self.customer != ''"),
        law(
            "retail_sign",
            "warning",
            "self.quantity > 0 or startsWith(self.invoice, 'C')",
        ),
    ];
    (entity, contract, laws)
}

fn registry(path: &str) -> serde_json::Value {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry");
    serde_json::from_str(&std::fs::read_to_string(root.join(path)).expect("registry artifact"))
        .expect("artifact JSON")
}

fn artifacts(target: &Target) -> (Vec<ArtifactEnvelope>, Vec<ArtifactEnvelope>) {
    let entity_id = format!("{}.entity", target.entity);
    let staged = target.entity == "bench.RetailLine";
    let (contract, contract_path) = if target.entity == "canon.Item" {
        (
            "canon.Catalog.interface",
            "packs/canon/artifacts/canon.Catalog.interface.json",
        )
    } else {
        (
            "core.Agent.interface",
            "core/artifacts/core.Agent.interface.json",
        )
    };
    let (entity, interface, laws) = if staged {
        retail_line()
    } else {
        (
            registry(&format!("packs/canon/artifacts/{entity_id}.json")),
            registry(contract_path),
            Vec::new(),
        )
    };
    let contract = if staged {
        "bench.RetailSource.interface"
    } else {
        contract
    };
    let fields: Vec<_> = target
        .fields
        .iter()
        .enumerate()
        .map(|(ordinal, name)| {
            let kind = match *name {
                "quantity" | "price" => "decimal",
                _ => "string",
            };
            json!({"dataset":"Rows","name":name,"nativeType":"text","ordinal":ordinal,
                "required":true,"typeCandidate":{"origin":"declared","type":kind}})
        })
        .collect();
    let scope = json!({"environmentId":"dev","orgId":"org_demo"});
    let provenance =
        json!({"source":"system","sourcePath":"crates/norn-cell/tests/bench_import.rs"});
    let source: ArtifactEnvelope = serde_json::from_value(json!({
        "artifactId":"bench.retail.source","dependencies":[],"irVersion":"0.1","kind":"source_schema",
        "lifecycle":"draft","name":"Retail extract","namespace":"bench","provenance":provenance,
        "scope":scope,"version":"1.0.0","payload":{"adapter":{"name":"bench.jsonl","version":"1.0.0"},
        "datasets":[{"id":"Rows","kind":"table","nativeName":"Rows"}],"fields":fields,
        "format":"norn.source-schema/1","observedAt":"2026-11-04T00:00:00Z",
        "rawMetadata":{"uri":"https://github.com/Norn-cloud/norn/blob/main/scripts/bench_retail.py","sha256":"0000000000000000000000000000000000000000000000000000000000000000"},
        "source":{"system":"Retail","instance":"public"}}
    }))
    .expect("source schema");
    let pin = ArtifactRef::of(&source).expect("source pin").sha256;
    let properties: Vec<_> = target
        .properties
        .iter()
        .map(|(property, value)| json!({"property":property,"value":value}))
        .collect();
    let subject = format!("input.{}", target.key);
    let dependency = |id: &str, version: &str, kind: &str| json!({"artifactId":id,"version":version,"kind":kind,"required":true});
    let contract_version = interface["version"].clone();
    let mapping: ArtifactEnvelope = serde_json::from_value(json!({
        "artifactId":"bench.retail.mapping","irVersion":"0.1","kind":"mapping","lifecycle":"draft",
        "name":"Retail mapping","namespace":"bench","provenance":provenance,"scope":scope,"version":"1.0.0",
        "dependencies":[dependency("bench.retail.source","1.0.0","source_schema"),
            dependency(&entity_id,entity["version"].as_str().expect("version"),"entity"),
            dependency(contract,contract_version.as_str().expect("version"),"entity")],
        "payload":{"extensions":{"norn.mapping.generic/1.0.0":{"parity":"unqualified"}},
            "format":"norn.mapping/1","interfaces":[contract],
            "maps":[{"id":"row","source":{"dataset":"Rows"},"subject":subject,"sourceRecord":subject,
                "target":{"entity":entity_id,"interface":contract},"properties":properties}],
            "source":{"schemaId":"bench.retail.source","version":"1.0.0","sha256":pin}}
    }))
    .expect("mapping");
    let candidate = [entity, interface]
        .into_iter()
        .map(|v| serde_json::from_value(v).expect("artifact"))
        .chain([source, mapping])
        .chain(
            laws.into_iter()
                .map(|v| serde_json::from_value(v).expect("law")),
        )
        .collect();
    let base = vec![
        serde_json::from_value(registry("core/artifacts/core.value_types.entity.json"))
            .expect("core types"),
        serde_json::from_value(registry("core/artifacts/core.units.json")).expect("core units"),
    ];
    (candidate, base)
}

fn category(error: &impl std::fmt::Display) -> String {
    error.to_string().chars().take(96).collect()
}

fn percentile(sorted: &[u128], p: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)]
}

/// Weekly United Kingdom quantity through the admitted question engine.
fn country_weekly_quantity(
    store: &mut CellStore,
    artifacts: &[ArtifactEnvelope],
) -> serde_json::Value {
    let catalog = norn_checker::EffectiveCatalog::from_artifacts(artifacts);
    let mut metric: norn_ir::MetricDefinition = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/net-sales.definition.json"
    ))
    .expect("metric");
    metric.name = "bench.quantity".into();
    metric.label = "Quantity".into();
    metric.type_params.expr = Some("quantity".into());
    let aggregate = metric
        .type_params
        .metric_aggregation_params
        .as_mut()
        .expect("aggregate");
    aggregate.semantic_model = norn_ir::ObjectTypeName("bench.RetailLine".into());
    aggregate.agg_time_dimension = "occurred_at".into();
    metric.norn.currency = None;
    metric.norn.default_filters.clear();
    metric.norn.time_dimension =
        Some(serde_json::from_value(json!({"property":"occurred_at"})).expect("path"));
    let mut question: norn_ir::QuestionIR = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))
    .expect("question");
    question.name = "bench.country_weekly_quantity".into();
    question.object_set = norn_ir::ObjectTypeName("bench.RetailLine".into());
    question.comparison = None;
    question.metrics =
        serde_json::from_value(json!([{"metric":"bench.quantity"}])).expect("selection");
    question.dimensions =
        serde_json::from_value(json!([{"path":{"property":"occurred_at"},"grain":"week"}]))
            .expect("dimension");
    question.filters = serde_json::from_value(
        json!([{"path":{"property":"country"},"operator":"eq","value":"United Kingdom"}]),
    )
    .expect("predicate");
    question.time_window.start = norn_ir::UtcTimestamp("2009-01-01T00:00:00Z".into());
    question.time_window.end = norn_ir::UtcTimestamp("2012-01-01T00:00:00Z".into());
    let items = ResourceLimitProfile::norn_limits_v1().slice.max_records;
    let plan = match norn_checker::metric::compute::admit(&[metric], &question, &catalog, items) {
        Ok(plan) => plan,
        Err(error) => return json!({"admitted":false,"refusal":format!("{error:?}")}),
    };
    let now = 1_793_750_400_000_u64;
    let context = norn_cell::compute::QueryContext {
        world: "main".into(),
        scope: "org_demo/dev".into(),
        population_pin: "uci-lines-sample".into(),
        authorization_epoch: 1,
        mapping_pin: "bench.retail.mapping".into(),
        now_ms: now,
        source_cursors: BTreeMap::from([(
            "Retail".into(),
            BTreeMap::from([("sample".into(), "end".into())]),
        )]),
        watermarks: BTreeMap::from([(
            "Retail".into(),
            BTreeMap::from([(
                "sample".into(),
                norn_cell::compute::Watermark {
                    observed: "end".into(),
                    heartbeat_ms: Some(now),
                    complete: true,
                },
            )]),
        )]),
    };
    match store.evaluate_question(&plan, context, |_| Ok(true)) {
        Ok(result) => json!({"admitted":true,"complete":result.complete,"rows":result.rows.iter()
            .map(|(bucket, row)| (bucket.clone(), row.totals.first().map(norn_ir::Decimal::canonical)))
            .collect::<BTreeMap<_, _>>()}),
        Err(error) => json!({"admitted":true,"refusal":format!("{error:?}")}),
    }
}

#[test]
#[ignore = "requires a test-time JSONL extract outside the checkout"]
fn real_rows_through_retained_mapping_and_cell() {
    let path = std::env::var("NORN_BENCH_ROWS").expect("test-time rows path");
    let dataset = std::env::var("NORN_BENCH_DATASET").expect("dataset name");
    let limit: usize = std::env::var("NORN_BENCH_LIMIT")
        .unwrap_or_else(|_| "1000000".into())
        .parse()
        .expect("positive limit");
    let target = match dataset.rsplit('-').next() {
        Some("items" | "products") => &ITEM,
        Some("sellers") => &ORGANIZATION,
        Some("sample") => &LINE,
        _ => &PERSON,
    };
    let (artifacts, base) = artifacts(target);
    let sources: Vec<Value> = artifacts
        .iter()
        .map(|a| Value::from_json(serde_json::to_value(a).expect("wire")).expect("source"))
        .collect();
    let base_sources: Vec<Value> = base
        .iter()
        .map(|a| Value::from_json(serde_json::to_value(a).expect("wire")).expect("base"))
        .collect();
    let registry = BTreeSet::new();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&sources, &base_sources).expect("receipt sources"),
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("receipt");
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let wire = checked
        .receipt
        .to_canonical_value()
        .expect("receipt canonical value");
    let receipt =
        String::from_utf8(norn_ir::canonical::serialize(&wire).expect("receipt canonicalizes"))
            .expect("UTF-8 receipt");
    let values = serde_json::to_value(&artifacts).expect("definitions");
    let object =
        serde_json::from_value(values[0]["payload"]["objectType"].clone()).expect("object");
    let interface =
        serde_json::from_value(values[1]["payload"]["interface"].clone()).expect("interface");
    let types: Vec<_> = serde_json::from_value(
        serde_json::to_value(&base).expect("base")[0]["payload"]["propertyTypes"].clone(),
    )
    .expect("value types");
    let model = Model::from_definitions_with_interfaces(&[object], &types, &[interface])
        .expect("model")
        .with_governing_laws(&artifacts)
        .expect("governing laws");
    let directory = tempfile::tempdir().expect("temporary cell");
    let database = directory.path().join("rows.sqlite");
    let mut store =
        CellStore::open(&database, model.clone(), StoreOptions::default()).expect("SQLite");
    store
        .install_definitions(
            model.clone(),
            InstalledDefinitionInput {
                canonical_set: serde_json::to_string(
                    &artifacts.iter().chain(&base).collect::<Vec<_>>(),
                )
                .expect("canonical set"),
                registry: Vec::new(),
                receipt_sha256: norn_checker::admission_receipt_sha256(&receipt)
                    .expect("receipt hash"),
                admission_receipt: Some(receipt),
                readmission: None,
                diff: json!({"benchmark":"link-free slice only"}),
                installed_at: 1,
                layout: None,
            },
            Some(0),
            "org_demo/dev",
        )
        .expect("install checked mapping");
    let input = norn_cell::mapping_bundle::MappingInput {
        mapping: "bench.retail.mapping".into(),
        dataset: "Rows".into(),
        connector_instance: "public".into(),
        partition: dataset.clone(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-04T00:00:00Z".into()),
    };
    let mut cell = SqliteCell::<
        norn_kernel::ObservationEvent,
        BTreeMap<String, Value>,
        BTreeMap<String, Value>,
    >::new(
        CellStore::open(&database, model, StoreOptions::default()).expect("processing connection"),
        "org_demo/dev",
        |_| BTreeMap::new(),
    )
    .expect("cell");
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("rows"));
    let (mut capture, mut process) = (Vec::new(), Vec::new());
    let mut outcomes = BTreeMap::<String, u64>::new();
    let (mut rows, mut matched, mut mismatched) = (0_u64, 0_u64, 0_u64);
    let mut ids = Vec::new();
    let started = Instant::now();
    for (index, line) in file.lines().take(limit).enumerate() {
        rows += 1;
        let text = line.expect("source line");
        let payload = Value::from_json_text(&text).expect("source JSON");
        let id = payload
            .get(target.key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let hash = norn_ir::canonical::HashProfile::Norn1
            .hash_json(&text)
            .expect("payload digest");
        let observed = Observed {
            id: ObservationId(format!("row-{index}")),
            provenance: Provenance {
                source: "Retail".into(),
                source_id: id.clone(),
                mapping_version: "pipeline/1".into(),
                observed_at: LogicalTime::from_millis(1_793_750_400_000),
            },
            input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>"Retail",
            "source_id"=>id.clone(), "event_id"=>format!("row-{index}"), "source_version"=>"2",
            "pipeline_version"=>"pipeline/1", "kind"=>"upsert", "extracted_at"=>1_793_750_400_000u64,
            "payload"=>payload.clone(), "payload_digest"=>hash},
        };
        let at = Instant::now();
        let bundle = match store.inject_mapping_bundle(&observed, &input) {
            Ok((bundle, _)) => bundle,
            Err(error) => {
                *outcomes
                    .entry(format!("capture: {}", category(&error)))
                    .or_default() += 1;
                continue;
            }
        };
        capture.push(at.elapsed().as_micros());
        let at = Instant::now();
        match cell.process_mapping_bundle(&bundle) {
            Ok(true) => *outcomes.entry("processed".into()).or_default() += 1,
            Ok(false) => *outcomes.entry("not pending".into()).or_default() += 1,
            Err(error) => {
                *outcomes
                    .entry(format!("process: {}", category(&error)))
                    .or_default() += 1;
                continue;
            }
        }
        process.push(at.elapsed().as_micros());
        let actual = cell
            .store()
            .read()
            .object("main", target.entity, &id)
            .expect("read");
        ids.push(id.clone());
        let expected = (target.entity == "bench.RetailLine" && actual.is_some())
            || target.properties.iter().all(|(property, source)| {
                let wanted = source.strip_prefix("input.").map_or_else(
                    || Some(source.trim_matches('\'')),
                    |field| payload.get(field).and_then(Value::as_str),
                );
                actual.as_ref().is_some_and(|object| {
                    object.values.get(*property).and_then(Value::as_str) == wanted
                })
            });
        if expected {
            matched += 1;
        } else {
            mismatched += 1;
        }
        if rows.is_multiple_of(500) {
            println!(
                "BENCH-IMPORT-WINDOW {}",
                json!({"dataset":dataset,"rows":rows,"elapsed_s":started.elapsed().as_secs_f64()})
            );
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    let (mut findings, mut quarantined) = (BTreeMap::<String, u64>::new(), 0_u64);
    if target.entity == "bench.RetailLine" {
        let reader = cell.store().read();
        for id in &ids {
            for finding in reader
                .observation_findings("main", target.entity, id)
                .expect("findings")
            {
                let rule = serde_json::to_value(&finding.rule_ref).expect("finding rule");
                let name = rule["lawId"]
                    .as_str()
                    .or_else(|| rule["law_id"].as_str())
                    .unwrap_or("other")
                    .to_owned();
                *findings.entry(name).or_default() += 1;
            }
            let record = reader
                .object("main", target.entity, id)
                .expect("read")
                .expect("object");
            quarantined += u64::from(
                reader
                    .observation_is_quarantined("main", &record)
                    .expect("quarantine"),
            );
        }
    }
    capture.sort_unstable();
    process.sort_unstable();
    let answer = (target.entity == "bench.RetailLine").then(|| {
        let pool: Vec<_> = artifacts.iter().chain(&base).cloned().collect();
        country_weekly_quantity(&mut store, &pool)
    });
    let rate = rows as f64 / seconds;
    println!(
        "BENCH-IMPORT {}",
        json!({"dataset":dataset,"rows":rows,"seconds":seconds,"rows_per_second":rate,
        "capture_us":{"p50":percentile(&capture,50),"p95":percentile(&capture,95),"p99":percentile(&capture,99)},
        "process_us":{"p50":percentile(&process,50),"p95":percentile(&process,95),"p99":percentile(&process,99)},
        "outcomes":outcomes,"readback_matched":matched,"readback_mismatched":mismatched,
        "findings":findings,"quarantined":quarantined,"question":answer})
    );
}
