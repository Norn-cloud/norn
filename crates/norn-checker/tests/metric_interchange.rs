#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test assertions and fixture mutations"
)]

use norn_checker::metric::codes;
use norn_checker::metric_interchange::{
    OntologyBinding, admit_metric_ir, export_osi, generate_metric_ir, import_metricflow, import_osi,
};
use norn_checker::{EffectiveCatalog, check_metrics_and_questions};
use norn_ir::{
    ArtifactEnvelope, MetricDefinition, MetricIR, ObjectTypeName, PropertyTypeName, QuestionIR,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const ONTOLOGY: &str = include_str!("../../../fixtures/ir/metrics/ontology.json");
const SALES: &str = include_str!("../../../fixtures/ir/metrics/net-sales.definition.json");
const CUSTOMERS: &str =
    include_str!("../../../fixtures/ir/metrics/distinct-customers.definition.json");

fn catalog() -> EffectiveCatalog {
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(ONTOLOGY).expect("ontology");
    EffectiveCatalog::from_artifacts_with_registry(
        &artifacts,
        &BTreeSet::from([PropertyTypeName("iso4217.currency_code".into())]),
    )
}
fn definition(source: &str) -> MetricDefinition {
    serde_json::from_str(source).expect("metric")
}
fn schema() -> jsonschema::Validator {
    let directory = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR")
        .map(std::path::PathBuf::from)
        .expect("acquire pinned inputs with scripts/importers/erpnext_runtime.py --output EXTERNAL_SCRATCH --interop-only; set NORN_UPSTREAM_FIXTURE_DIR");
    let path = directory
        .join("osi-schema.json")
        .canonicalize()
        .expect("pinned OSI schema exists");
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    assert!(
        !path.starts_with(repo),
        "upstream schema must remain external"
    );
    let bytes = std::fs::read(path).expect("read pinned OSI schema");
    let lock: Value = serde_json::from_str(include_str!(
        "../../../scripts/importers/erpnext-runtime.lock.json"
    ))
    .expect("runtime input pins");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        lock["interop"]["osi-schema.json"]["sha256"],
        "OSI schema must match acquisition pin"
    );
    let schema: Value = serde_json::from_slice(&bytes).expect("schema");
    jsonschema::validator_for(&schema).expect("pinned validator")
}
fn binding() -> OntologyBinding {
    let mut policy = definition(SALES).norn;
    policy.currency = None;
    policy.default_filters.clear();
    OntologyBinding {
        datasets: BTreeMap::from([
            (
                "invoice_items".into(),
                ObjectTypeName("erpnext.SalesInvoiceItem".into()),
            ),
            (
                "invoices".into(),
                ObjectTypeName("erpnext.SalesInvoiceItem".into()),
            ),
        ]),
        fields: BTreeMap::from([
            (
                "invoice_items.id".into(),
                serde_json::from_value(json!({"property":"_norn_identity"})).expect("identity"),
            ),
            (
                "invoice_items.weight".into(),
                serde_json::from_value(json!({"property":"weight"})).expect("amount"),
            ),
            (
                "invoice_items.posting_date".into(),
                policy.time_dimension.clone().expect("time"),
            ),
            (
                "invoices.weight".into(),
                serde_json::from_value(json!({"property":"weight"})).expect("amount"),
            ),
        ]),
        links: BTreeMap::from([(
            "invoice_items.invoice".into(),
            norn_ir::LinkId("parent_sales_invoice".into()),
        )]),
        policies: BTreeMap::from([("sales_weight".into(), policy)]),
    }
}

#[test]
fn erpnext_metric_files_strip_to_pinned_osi_and_admit_by_regeneration() {
    let validator = schema();
    for (name, source) in [("net-sales", SALES), ("distinct-customers", CUSTOMERS)] {
        let definition = definition(source);
        let doc = generate_metric_ir("erpnext", std::slice::from_ref(&definition), &catalog())
            .expect("generate");
        let exported = export_osi(&doc).expect("export");
        let errors: Vec<_> = validator
            .iter_errors(&exported)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!exported.to_string().contains("x-norn"));
        let flow: Value = serde_json::from_str(
            exported["semantic_model"][0]["metrics"][0]["custom_extensions"][0]["data"]
                .as_str()
                .expect("data"),
        )
        .expect("DBT");
        assert_eq!(flow["type"], "simple");
        assert!(flow.get("x-norn").is_none());
        assert_eq!(
            admit_metric_ir(&doc, &catalog()).expect("native reimport"),
            vec![definition]
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../fixtures/ir/metrics/{name}.metric.json"));
        if std::env::var_os("NORN_UPDATE_METRIC_GOLDENS").is_some() {
            std::fs::write(
                &path,
                format!("{}\n", serde_json::to_string_pretty(&doc).expect("encode")),
            )
            .expect("golden");
        }
        let golden: MetricIR =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("file")).expect("doc");
        assert_eq!(doc, golden, "generated ontology/calculation golden drift");
        let imported: MetricIR = serde_json::from_str(&exported.to_string()).expect("OSI");
        let binding = OntologyBinding {
            policies: BTreeMap::from([(
                doc.semantic_model[0].metrics[0].name.clone(),
                doc.semantic_model[0].metrics[0]
                    .norn
                    .clone()
                    .expect("policy"),
            )]),
            ..Default::default()
        };
        assert_eq!(
            import_osi(&imported, &binding, &catalog()).expect("near-identity import"),
            doc
        );
    }
}

#[test]
fn ontology_projection_preserves_each_direction_and_rejects_authored_changes() {
    let doc = generate_metric_ir("erpnext", &[definition(SALES)], &catalog()).expect("generate");
    let links = &doc.semantic_model[0].datasets;
    let items = links
        .iter()
        .find(|d| d.name == "erpnext.SalesInvoice::items")
        .expect("items");
    assert_eq!(
        items.norn.as_ref().expect("link").cardinality.to,
        norn_ir::Multiplicity::Many
    );
    assert!(!items.unique_keys.contains(&vec!["_from_identity".into()]));
    assert!(items.unique_keys.contains(&vec!["_to_identity".into()]));
    let parent = links
        .iter()
        .find(|d| d.name == "erpnext.SalesInvoiceItem::parent_sales_invoice")
        .expect("parent");
    assert!(parent.norn.as_ref().expect("link").cardinality.required);
    assert!(parent.unique_keys.contains(&vec!["_from_identity".into()]));
    let mut changed = doc.clone();
    changed.semantic_model[0].relationships[0].to_columns = vec!["name".into()];
    assert!(
        admit_metric_ir(&changed, &catalog())
            .expect_err("reject")
            .iter()
            .any(|d| d.code == codes::ONTOLOGY_PROJECTION)
    );
    let mut changed = doc;
    changed.semantic_model[0].metrics[0].expression.dialects[0].expression =
        "SUM(unrelated.amount)".into();
    assert!(
        admit_metric_ir(&changed, &catalog())
            .expect_err("reject")
            .iter()
            .any(|d| d.code == codes::ONTOLOGY_PROJECTION)
    );
}

#[test]
fn osi_and_metricflow_samples_import_with_explicit_ontology_binding_or_precise_gaps() {
    let osi: MetricIR = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/osi/sample-osi.json"
    ))
    .expect("sample");
    assert!(schema().is_valid(&serde_json::to_value(&osi).expect("json")));
    let mf: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/osi/sample-metricflow.json"
    ))
    .expect("sample");
    for doc in [
        import_osi(&osi, &binding(), &catalog()).expect("OSI import"),
        import_metricflow(&mf, &binding(), &catalog()).expect("MF import"),
    ] {
        assert!(schema().is_valid(&export_osi(&doc).expect("export")));
        assert!(admit_metric_ir(&doc, &catalog()).is_ok());
    }
    let errors = import_metricflow(&mf, &OntologyBinding::default(), &catalog())
        .expect_err("missing bindings");
    assert_eq!(
        errors[0].span.as_ref().expect("span").pointer,
        "/semantic_models/0/name"
    );
    let mut bad = osi;
    bad.semantic_model[0].metrics[0].expression.dialects[0].expression =
        "SUM(invoices.weight) OVER (PARTITION BY hidden)".into();
    let errors = import_osi(&bad, &binding(), &catalog()).expect_err("opaque SQL");
    assert_eq!(
        errors[0].span.as_ref().expect("span").pointer,
        "/semantic_model/0/metrics/0/expression/dialects/0/expression"
    );
    let mut bad = mf;
    bad["semantic_models"][0]["entities"][0]["expr"] = json!("weight");
    assert!(
        import_metricflow(&bad, &binding(), &catalog())
            .expect_err("unproved key")
            .iter()
            .any(|d| d.code == codes::IMPORT_GAP)
    );
}

#[test]
fn advanced_calculations_report_the_specific_unadmitted_contract() {
    for (kind, pointer) in [
        ("cumulative", "cumulative_type_params"),
        ("derived", "expr"),
        ("conversion", "conversion_type_params"),
    ] {
        let mut value: Value = serde_json::from_str(SALES).expect("metric");
        value["type"] = json!(kind);
        let metric: MetricDefinition = serde_json::from_str(&value.to_string()).expect("typed");
        let errors = check_metrics_and_questions(&[metric], &[], &catalog());
        assert!(errors.iter().any(|d| d.code == codes::UNSUPPORTED_SEMANTICS
            && d.span.as_ref().expect("span").pointer
                == format!("/metrics/0/type_params/{pointer}")));
    }
}

#[test]
fn object_set_traversal_filters_source_and_aggregates_the_distinct_terminal_population() {
    let source = include_str!("../../../fixtures/ir/metrics/sales-by-store-day.question.json");
    let mut question: Value = serde_json::from_str(source).expect("question");
    question["objectSet"] = json!("erpnext.SalesInvoice");
    question["searchAround"] = json!(["items"]);
    question["filter"] =
        json!([{"path":{"property":"docstatus"},"operator":"eq","value":"Submitted"}]);
    let typed: QuestionIR = serde_json::from_str(&question.to_string()).expect("query");
    assert!(check_metrics_and_questions(&[definition(SALES)], &[typed], &catalog()).is_empty());
    question["filter"][0]["path"]["property"] = json!("net_amount");
    let typed: QuestionIR = serde_json::from_str(&question.to_string()).expect("query");
    assert!(
        check_metrics_and_questions(&[definition(SALES)], &[typed], &catalog())
            .iter()
            .any(|d| d.code == codes::UNKNOWN_PATH)
    );
    let mut policy = definition(SALES).norn;
    policy.currency = None;
    policy.default_filters.clear();
    question["filter"] = json!([]);
    question["aggregate"] = json!([{"metric":"erpnext.inline_count","inline":{"name":"erpnext.inline_count","agg":"count","aggTimeDimension":policy.time_dimension,"x-norn":policy}}]);
    let typed: QuestionIR = serde_json::from_str(&question.to_string()).expect("inline query");
    assert!(check_metrics_and_questions(&[], &[typed], &catalog()).is_empty());
}

#[test]
fn measure_import_never_discards_options_and_reports_nested_decode_paths() {
    let sample: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/osi/sample-metricflow.json"
    ))
    .expect("sample");
    let mut extra = sample.clone();
    extra["metrics"][0]["type_params"]["fill_nulls_with"] = json!(0);
    let errors =
        import_metricflow(&extra, &binding(), &catalog()).expect_err("cannot discard fill policy");
    assert_eq!(
        errors[0].span.as_ref().expect("span").pointer,
        "/metrics/0/type_params/fill_nulls_with"
    );
    let mut malformed = sample;
    malformed["semantic_models"][0]["measures"][0]["agg"] = json!("opaque_udf");
    let errors =
        import_metricflow(&malformed, &binding(), &catalog()).expect_err("invalid operator");
    assert_eq!(
        errors[0].span.as_ref().expect("span").pointer,
        "/semantic_models/0/measures/0/agg"
    );
}

#[test]
fn generated_unit_and_currency_semantics_cannot_be_authored_or_changed() {
    let sales = definition(SALES);
    let doc =
        generate_metric_ir("erpnext", std::slice::from_ref(&sales), &catalog()).expect("generate");
    let semantics = doc.semantic_model[0].metrics[0]
        .norn
        .as_ref()
        .expect("norn")
        .derived
        .as_ref()
        .expect("derived");
    assert_eq!(
        semantics.currency_source.as_ref(),
        sales.norn.currency.as_ref().map(|c| &c.property)
    );
    let mut changed = doc;
    changed.semantic_model[0].metrics[0]
        .norn
        .as_mut()
        .expect("norn")
        .derived
        .as_mut()
        .expect("derived")
        .currency_source = None;
    assert!(
        admit_metric_ir(&changed, &catalog())
            .expect_err("reject altered source")
            .iter()
            .any(|d| d.code == codes::ONTOLOGY_PROJECTION)
    );
    let mut authored = sales;
    authored.norn.derived = Some(norn_ir::metric::MetricPropertySemantics {
        unit: None,
        currency_source: None,
    });
    assert!(
        check_metrics_and_questions(&[authored], &[], &catalog())
            .iter()
            .any(|d| d.code == codes::INVALID_POLICY)
    );
}

#[test]
fn inline_output_names_are_local_and_other_questions_cannot_reference_them() {
    let mut query: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))
    .expect("query");
    let mut policy = definition(SALES).norn;
    policy.currency = None;
    policy.default_filters.clear();
    query["aggregate"] = json!([{"metric":"item_count","inline":{"name":"item_count","agg":"count","aggTimeDimension":policy.time_dimension,"x-norn":policy}}]);
    let first: QuestionIR = serde_json::from_str(&query.to_string()).expect("first");
    let mut second = first.clone();
    second.name = "erpnext.other_inline".into();
    second.comparison = Some(norn_ir::metric::QuestionComparison::Plan {
        question: first.name.clone(),
        mapping: vec![],
    });
    assert!(
        check_metrics_and_questions(&[], &[first.clone(), second.clone()], &catalog()).is_empty()
    );
    second.metrics[0].inline = None;
    assert!(
        check_metrics_and_questions(&[], &[first, second], &catalog())
            .iter()
            .any(|d| d.code == codes::UNKNOWN_METRIC)
    );
}

#[test]
fn metricflow_non_additive_dimensions_keep_snapshot_governance_and_resolve_series_keys() {
    let mut metric = definition(SALES);
    let time_name = metric
        .type_params
        .metric_aggregation_params
        .as_ref()
        .expect("agg")
        .agg_time_dimension
        .clone();
    metric.norn.additivity = norn_ir::metric::MetricAdditivity::SemiAdditive {
        over: norn_ir::metric::AdditivityAxis::Time,
        aggregate: norn_ir::metric::Reaggregate::Last,
    };
    metric
        .type_params
        .metric_aggregation_params
        .as_mut()
        .expect("agg")
        .non_additive_dimension = Some(norn_ir::metric::NonAdditiveDimension {
        name: time_name,
        window_choice: norn_ir::metric::WindowChoice::Max,
        window_groupings: vec!["name".into()],
    });
    assert!(check_metrics_and_questions(std::slice::from_ref(&metric), &[], &catalog()).is_empty());
    let question: QuestionIR = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))
    .expect("query");
    assert!(
        check_metrics_and_questions(std::slice::from_ref(&metric), &[question], &catalog())
            .iter()
            .any(|d| d.code == codes::SNAPSHOT_AGGREGATION)
    );
    metric
        .type_params
        .metric_aggregation_params
        .as_mut()
        .expect("agg")
        .non_additive_dimension
        .as_mut()
        .expect("snapshot")
        .window_groupings = vec!["missing".into()];
    assert!(
        check_metrics_and_questions(&[metric], &[], &catalog())
            .iter()
            .any(|d| d.code == codes::UNKNOWN_PATH)
    );
}

#[test]
fn modern_metricflow_aggregates_bind_fields_and_time_without_measure_reauthoring() {
    let input = json!({"metrics":[{"name":"sales_weight","label":"Weight","type":"simple","type_params":{"expr":"weight","metric_aggregation_params":{"semantic_model":"invoice_items","agg":"sum","agg_time_dimension":"posting_date"}}}]});
    let doc = import_metricflow(&input, &binding(), &catalog()).expect("modern import");
    assert!(schema().is_valid(&export_osi(&doc).expect("export")));
    assert!(admit_metric_ir(&doc, &catalog()).is_ok());
    let flow: Value =
        serde_json::from_str(&doc.semantic_model[0].metrics[0].custom_extensions[0].data)
            .expect("flow");
    assert!(
        !flow["type_params"]["metric_aggregation_params"]["agg_time_dimension"]
            .as_str()
            .expect("field")
            .contains("__")
    );
}
