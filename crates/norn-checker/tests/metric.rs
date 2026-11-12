#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test assertions and fixture mutation"
)]

use std::collections::BTreeSet;

use norn_checker::metric::codes;
use norn_checker::{EffectiveCatalog, check_metrics_and_questions};
use norn_ir::{ArtifactEnvelope, MetricDefinition, PropertyTypeName, QuestionIR};
use serde_json::{Value, json};

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    let mut value = value.clone();
    if value.get("type").is_some_and(|t| t == "simple")
        && let Some(time) = value.pointer("/x-norn/timeDimension")
    {
        let path: norn_ir::metric::DimensionPath =
            serde_json::from_value(time.clone()).expect("time");
        value["type_params"]["metric_aggregation_params"]["agg_time_dimension"] =
            json!(norn_checker::metric::field_name(&path));
    }
    serde_json::from_str(&value.to_string()).expect("typed fixture decodes")
}

const ONTOLOGY: &str = include_str!("../../../fixtures/ir/metrics/ontology.json");
const SALES: &str = include_str!("../../../fixtures/ir/metrics/net-sales.definition.json");
const CUSTOMERS: &str =
    include_str!("../../../fixtures/ir/metrics/distinct-customers.definition.json");
const SALES_QUESTION: &str =
    include_str!("../../../fixtures/ir/metrics/sales-by-store-day.question.json");
const CUSTOMER_QUESTION: &str =
    include_str!("../../../fixtures/ir/metrics/customers-by-region-week.question.json");

fn catalog(source: &str) -> EffectiveCatalog {
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(source).expect("ontology decodes");
    let registry = BTreeSet::from([PropertyTypeName("iso4217.currency_code".into())]);
    EffectiveCatalog::from_artifacts_with_registry(&artifacts, &registry)
}

fn check(metric: &Value, question: &Value) -> Vec<norn_checker::Diagnostic> {
    let metric: MetricDefinition = decode(metric);
    let question: QuestionIR = decode(question);
    check_metrics_and_questions(&[metric], &[question], &catalog(ONTOLOGY))
}

fn sales() -> (Value, Value) {
    (
        serde_json::from_str(SALES).expect("metric"),
        serde_json::from_str(SALES_QUESTION).expect("question"),
    )
}

fn assert_code(findings: &[norn_checker::Diagnostic], code: &str) {
    assert!(
        findings.iter().any(|d| d.code == code),
        "missing {code}: {findings:?}"
    );
    for finding in findings {
        assert!(
            finding.registration().is_some(),
            "{} is registered",
            finding.code
        );
        assert!(finding.is_error());
        if [codes::UNDECLARED_ADDITIVITY, codes::RATIO_AGGREGATION].contains(&finding.code.as_str())
        {
            assert_eq!(
                finding.remediation,
                Some(norn_checker::DiagnosticRemediation::FixDefinition)
            );
        }

        assert!(
            finding
                .span
                .as_ref()
                .expect("span")
                .pointer
                .starts_with('/')
        );
    }
}

#[test]
fn erpnext_goldens_resolve_required_parent_time_and_currency_conversion() {
    let metrics =
        [SALES, CUSTOMERS].map(|s| serde_json::from_str::<MetricDefinition>(s).expect("metric"));
    let questions = [SALES_QUESTION, CUSTOMER_QUESTION]
        .map(|s| serde_json::from_str::<QuestionIR>(s).expect("question"));
    let findings = check_metrics_and_questions(&metrics, &questions, &catalog(ONTOLOGY));
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn every_scalar_rejection_has_a_stable_code_and_pointer() {
    let cases = [
        ("/name", json!("Sales"), codes::INVALID_NAME),
        ("/x-norn/owner", json!(" "), codes::INVALID_OWNER),
        (
            "/type_params/metric_aggregation_params/semantic_model",
            json!("erpnext.Missing"),
            codes::UNKNOWN_OBJECT,
        ),
        ("/type_params/expr", json!("absent"), codes::UNKNOWN_PATH),
        ("/type_params/expr", json!("name"), codes::INVALID_MEASURE),
        (
            "/x-norn/timeDimension/property",
            json!("docstatus"),
            codes::INVALID_TIME,
        ),
        (
            "/x-norn/timeDimension/links",
            json!(["absent"]),
            codes::UNKNOWN_PATH,
        ),
        (
            "/x-norn/defaultFilters/0/value",
            json!(12),
            codes::INVALID_FILTER,
        ),
        (
            "/x-norn/defaultFilters/0/operator",
            json!("gt"),
            codes::INVALID_FILTER,
        ),
        (
            "/x-norn/currency/property/property",
            json!("name"),
            codes::INVALID_CURRENCY,
        ),
        (
            "/x-norn/currency/conversion/target",
            json!("sar"),
            codes::INVALID_CONVERSION,
        ),
        (
            "/x-norn/currency/conversion/rate/property",
            json!("name"),
            codes::INVALID_CONVERSION,
        ),
        (
            "/x-norn/additivity",
            json!({"kind":"semi_additive","over":"time","aggregate":"sum"}),
            codes::INVALID_ADDITIVITY,
        ),
    ];
    for (pointer, replacement, code) in cases {
        let (mut metric, question) = sales();
        *metric.pointer_mut(pointer).expect("fixture pointer") = replacement;
        assert_code(&check(&metric, &question), code);
    }
}

#[test]
fn question_references_windows_freshness_and_selections_are_checked() {
    let cases = [
        ("/name", json!("Bad"), codes::QUESTION_NAME),
        ("/aggregate", json!([]), codes::EMPTY_METRICS),
        (
            "/aggregate/0/metric",
            json!("erpnext.absent"),
            codes::UNKNOWN_METRIC,
        ),
        (
            "/groupBy/0/path/links",
            json!(["parent_sales_invoice", "absent"]),
            codes::UNKNOWN_PATH,
        ),
        (
            "/groupBy/0/path/property",
            json!("absent"),
            codes::UNKNOWN_PATH,
        ),
        (
            "/window/end",
            json!("2026-09-01T00:00:00Z"),
            codes::INVALID_WINDOW,
        ),
        (
            "/window/start",
            json!("2026-02-30T00:00:00Z"),
            codes::INVALID_WINDOW,
        ),
        (
            "/window/start",
            json!("2026-10-01T00:00:00+00:00"),
            codes::INVALID_WINDOW,
        ),
        (
            "/freshness",
            json!({"kind":"delayed","maxAgeMs":0}),
            codes::INVALID_FRESHNESS,
        ),
        (
            "/freshness",
            json!({"kind":"as_of","at":"2026-02-30T00:00:00Z"}),
            codes::INVALID_FRESHNESS,
        ),
        (
            "/compare",
            json!({"kind":"plan","question":"erpnext.absent"}),
            codes::UNKNOWN_COMPARISON,
        ),
    ];
    for (pointer, replacement, code) in cases {
        let (metric, mut question) = sales();
        *question.pointer_mut(pointer).expect("pointer") = replacement;
        assert_code(&check(&metric, &question), code);
    }
    let (metric, mut question) = sales();
    question["groupBy"][0]["grain"] = json!("day");
    assert_code(&check(&metric, &question), codes::INVALID_DIMENSION);
    question["groupBy"][0]
        .as_object_mut()
        .expect("object")
        .remove("grain");
    question["filter"] =
        json!([{"path":{"property":"net_amount"},"operator":"eq","value":"12.50"}]);
    assert_code(&check(&metric, &question), codes::INVALID_FILTER);
    question["filter"][0]["value"] = json!(12.5);
    assert!(check(&metric, &question).is_empty());
}

#[test]
fn fan_out_is_checked_at_every_hop_in_both_directions() {
    for links in [
        json!(["parent_sales_invoice", "items"]),
        json!(["parent_sales_invoice", "customer", "stores"]),
    ] {
        let (mut metric, mut question) = sales();
        question["groupBy"] = json!([{"path":{"links":links,"property":"name"}}]);
        assert_code(&check(&metric, &question), codes::FAN_OUT);
        metric["x-norn"]["safeAggregation"] = json!("symmetric");
        assert!(check(&metric, &question).is_empty());
    }
    // From a customer, the inverse role of an invoice's customer link is many.
    let mut ontology: Value = serde_json::from_str(ONTOLOGY).expect("ontology");
    ontology[1]["payload"]["objectType"]["links"][0]["inverse"] = json!("invoices");
    let mut metric: Value = serde_json::from_str(CUSTOMERS).expect("metric");
    metric["type_params"]["metric_aggregation_params"]["semantic_model"] =
        json!("erpnext.Customer");
    metric["type_params"]["expr"] = json!("name");
    metric["x-norn"]["timeDimension"] = json!({"links":["invoices"],"property":"posting_date"});
    metric["x-norn"]["safeAggregation"] = json!("symmetric");
    let metric: MetricDefinition = decode(&metric);
    assert_code(
        &check_metrics_and_questions(&[metric], &[], &catalog(&ontology.to_string())),
        codes::INVALID_TIME,
    );
}

#[test]
fn sums_reject_mixed_currencies_without_conversion_and_infer_singletons() {
    let (mut metric, question) = sales();
    metric["x-norn"]["currency"]
        .as_object_mut()
        .expect("currency")
        .remove("conversion");
    assert_code(&check(&metric, &question), codes::MIXED_CURRENCY);
    metric["x-norn"]["currency"]["property"]["property"] = json!("fx_currency");
    assert_code(&check(&metric, &question), codes::MIXED_CURRENCY);
    metric["x-norn"]["currency"]["property"]["property"] = json!("base_currency");
    assert_code(&check(&metric, &question), codes::CURRENCY_SOURCE_MISMATCH);
}

#[test]
fn rollups_enforce_semi_additive_time_and_non_additive_dimensions() {
    let (mut metric, mut question) = sales();
    metric["x-norn"]["additivity"] =
        json!({"kind":"semi_additive","over":"time","aggregate":"last"});
    question["aggregate"][0]["reaggregate"] =
        json!({"aggregate":"sum","across":[],"overTime":true});
    assert_code(&check(&metric, &question), codes::SEMI_ADDITIVE_TIME);
    question["aggregate"][0]["reaggregate"]["aggregate"] = json!("last");
    assert_code(&check(&metric, &question), codes::SNAPSHOT_AGGREGATION);
    question["aggregate"][0]["reaggregate"] = json!({"aggregate":"sum","across":[metric["x-norn"]["timeDimension"].clone()],"overTime":false});
    assert_code(&check(&metric, &question), codes::SEMI_ADDITIVE_TIME);
    question["aggregate"][0]["reaggregate"] = json!({"aggregate":"sum","across":[question["groupBy"][0]["path"].clone()],"overTime":false});
    assert_code(&check(&metric, &question), codes::SNAPSHOT_AGGREGATION);
    metric["x-norn"]["additivity"] = json!({"kind":"non_additive"});
    assert_code(&check(&metric, &question), codes::NON_ADDITIVE_ROLLUP);
    metric["x-norn"]["additivity"] = json!({"kind":"additive"});
    question["aggregate"][0]["reaggregate"]["across"] = json!([{"property":"absent"}]);
    assert_code(&check(&metric, &question), codes::INVALID_ROLLUP);
}

fn ratio() -> Value {
    json!({"name":"erpnext.ratio","label":"Ratio","type":"ratio","type_params":{"numerator":"erpnext.net_sales","denominator":"erpnext.other"},"x-norn":{"irVersion":"0.1","additivity":{"kind":"non_additive"},"timeDimension":{"links":["parent_sales_invoice"],"property":"posting_date"},"owner":"finance","certified":false}})
}

#[test]
fn ratios_check_units_currencies_object_grain_references_cycles_and_additivity() {
    let (sales, _) = sales();
    let mut other = sales.clone();
    other["name"] = json!("erpnext.other");
    let base_ratio = ratio();
    let run = |a: &Value, b: &Value, c: &Value| {
        let metrics: Vec<MetricDefinition> = [a, b, c].iter().map(|v| decode(v)).collect();
        check_metrics_and_questions(&metrics, &[], &catalog(ONTOLOGY))
    };
    assert!(run(&sales, &other, &base_ratio).is_empty());
    let mut average = other.clone();
    average["type_params"]["metric_aggregation_params"]["agg"] = json!("average");
    average["x-norn"]["additivity"] = json!({"kind":"non_additive"});
    assert_code(
        &run(&sales, &average, &base_ratio),
        codes::RATIO_AGGREGATION,
    );

    other["x-norn"]["currency"] = json!({"property":{"links":["parent_sales_invoice"],"property":"currency"},"conversion":{"target":"USD","rate":{"property":"conversion_rate"}}});
    assert_code(&run(&sales, &other, &base_ratio), codes::UNIT_MISMATCH);
    let mut kg = sales;
    kg["x-norn"]
        .as_object_mut()
        .expect("extension")
        .remove("currency");
    kg["type_params"]["expr"] = json!("weight");
    let mut liters = kg.clone();
    liters["name"] = json!("erpnext.other");
    liters["type_params"]["expr"] = json!("volume");
    assert_code(&run(&kg, &liters, &base_ratio), codes::UNIT_MISMATCH);
    liters["type_params"]["expr"] = json!("weight");
    assert!(run(&kg, &liters, &base_ratio).is_empty());
    let mut bad = base_ratio.clone();
    bad["type_params"]["numerator"] = json!("erpnext.absent");
    assert_code(&run(&kg, &liters, &bad), codes::UNKNOWN_METRIC);
    bad["type_params"]["numerator"] = json!("erpnext.ratio");
    assert_code(&run(&kg, &liters, &bad), codes::METRIC_CYCLE);
    bad = base_ratio.clone();
    bad["x-norn"]["additivity"] = json!({"kind":"additive"});
    assert_code(&run(&kg, &liters, &bad), codes::INVALID_ADDITIVITY);
    liters["x-norn"]["timeDimension"] = json!({"property":"name"});
    assert_code(&run(&kg, &liters, &base_ratio), codes::GRAIN_MISMATCH);
}

#[test]
fn aggregate_algebra_rejects_additive_averages_min_max_and_distinct_counts() {
    for aggregate in ["average", "min", "max", "count_distinct"] {
        let (mut metric, question) = sales();
        metric["type_params"]["metric_aggregation_params"]["agg"] = json!(aggregate);
        assert_code(&check(&metric, &question), codes::INVALID_ADDITIVITY);
        metric["x-norn"]["additivity"] = json!({"kind":"non_additive"});
        if aggregate == "count_distinct" {
            metric["x-norn"]
                .as_object_mut()
                .expect("extension")
                .remove("currency");
        }
        assert!(check(&metric, &question).is_empty());
    }
    let (mut metric, question) = sales();
    metric["x-norn"]
        .as_object_mut()
        .expect("extension")
        .remove("currency");
    metric["type_params"]["metric_aggregation_params"]["agg"] = json!("count");
    metric["type_params"]
        .as_object_mut()
        .expect("params")
        .remove("expr");
    assert!(check(&metric, &question).is_empty());
    metric["type_params"]["metric_aggregation_params"]["agg"] = json!("sum");
    assert_code(&check(&metric, &question), codes::INVALID_MEASURE);
}

#[test]
fn duplicates_comparison_cycles_and_depth_limits_are_rejected() {
    let (metric, question) = sales();
    let metric: MetricDefinition = decode(&metric);
    let question: QuestionIR = decode(&question);
    let cat = catalog(ONTOLOGY);
    assert_code(
        &check_metrics_and_questions(&[metric.clone(), metric.clone()], &[], &cat),
        codes::DUPLICATE_METRIC,
    );
    assert_code(
        &check_metrics_and_questions(
            std::slice::from_ref(&metric),
            &[question.clone(), question.clone()],
            &cat,
        ),
        codes::DUPLICATE_QUESTION,
    );
    let mut q = question.clone();
    q.metrics.push(q.metrics[0].clone());
    assert_code(
        &check_metrics_and_questions(std::slice::from_ref(&metric), &[q], &cat),
        codes::DUPLICATE_SELECTION,
    );
    let mut q = question.clone();
    q.dimensions.push(q.dimensions[0].clone());
    assert_code(
        &check_metrics_and_questions(std::slice::from_ref(&metric), &[q], &cat),
        codes::DUPLICATE_SELECTION,
    );
    let mut q = question;
    q.comparison = Some(norn_ir::metric::QuestionComparison::Forecast {
        question: q.name.clone(),
        mapping: vec![],
    });
    assert_code(
        &check_metrics_and_questions(std::slice::from_ref(&metric), &[q], &cat),
        codes::COMPARISON_CYCLE,
    );
    let mut metric = metric;
    metric.norn.time_dimension.as_mut().expect("time").links =
        vec![norn_ir::LinkId("items".into()); 65];
    metric
        .type_params
        .metric_aggregation_params
        .as_mut()
        .expect("agg")
        .agg_time_dimension =
        norn_checker::metric::field_name(metric.norn.time_dimension.as_ref().expect("time"));
    assert_code(
        &check_metrics_and_questions(&[metric], &[], &cat),
        codes::COMPLEXITY_LIMIT,
    );
}

#[test]
fn timestamp_windows_compare_instants_including_fractional_seconds() {
    let (metric, mut question) = sales();
    question["window"] = json!({"start":"2026-10-01T00:00:00Z","end":"2026-10-01T00:00:00.1Z"});
    assert!(check(&metric, &question).is_empty());
    question["window"] =
        json!({"start":"2026-10-01T00:00:00.10Z","end":"2026-10-01T00:00:00.100Z"});
    assert_code(&check(&metric, &question), codes::INVALID_WINDOW);
}

#[test]
fn effective_shared_units_and_inherited_links_are_used() {
    let mut ontology: Value = serde_json::from_str(ONTOLOGY).expect("ontology");
    let mut types = ontology[0].clone();
    types["artifactId"] = json!("erpnext.property_types.entity.v2");
    types["payload"] = json!({"entityVersion":"2.0","propertyTypes":[{"name":"erpnext.mass","type":{"kind":"decimal","additivity":{"kind":"additive"},"scale":3,"unit":{"system":"ucum","code":"kg"}}}]});
    ontology.as_array_mut().expect("array").push(types);
    // Locate the physical weight property by id, rather than relying on order.
    for property in ontology[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
    {
        if property["id"] == "weight" {
            property["type"] = json!({"kind":"ref","name":"erpnext.mass"});
        }
    }
    let mut inherited = ontology[0].clone();
    inherited["artifactId"] = json!("erpnext.SpecialInvoiceItem.entity.v2");
    inherited["payload"] = json!({"entityVersion":"2.0","objectType":{"name":"erpnext.SpecialInvoiceItem","extends":"erpnext.SalesInvoiceItem"}});
    ontology.as_array_mut().expect("array").push(inherited);
    let (mut metric, mut question) = sales();
    metric["x-norn"]
        .as_object_mut()
        .expect("extension")
        .remove("currency");
    metric["type_params"]["metric_aggregation_params"]["semantic_model"] =
        json!("erpnext.SpecialInvoiceItem");
    question["objectSet"] = json!("erpnext.SpecialInvoiceItem");
    metric["type_params"]["expr"] = json!("weight");
    question["groupBy"] =
        json!([{"path":{"links":["parent_sales_invoice","store"],"property":"name"}}]);
    let metric: MetricDefinition = decode(&metric);
    let question: QuestionIR = decode(&question);
    let findings =
        check_metrics_and_questions(&[metric], &[question], &catalog(&ontology.to_string()));
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn conversion_and_currency_paths_cannot_fan_out_or_carry_quantity_units() {
    let (mut metric, question) = sales();
    metric["x-norn"]["currency"]["conversion"]["rate"]["property"] = json!("weight");
    assert_code(&check(&metric, &question), codes::INVALID_CONVERSION);
    metric["x-norn"]["currency"]["conversion"]["rate"] =
        json!({"links":["parent_sales_invoice","items"],"property":"conversion_rate"});
    assert_code(&check(&metric, &question), codes::INVALID_CONVERSION);
    metric["x-norn"]["currency"]["property"]["links"] =
        json!(["parent_sales_invoice", "items", "parent_sales_invoice"]);
    assert_code(&check(&metric, &question), codes::INVALID_CURRENCY);
}

#[test]
fn comparisons_check_named_questions_and_compatible_selections() {
    let (metric, question) = sales();
    let metric: MetricDefinition = decode(&metric);
    let mut actual: QuestionIR = decode(&question);
    let mut planned = actual.clone();
    planned.name = "erpnext.plan".into();
    planned.comparison = None;
    actual.comparison = Some(norn_ir::metric::QuestionComparison::Plan {
        question: planned.name.clone(),
        mapping: vec![],
    });
    let cat = catalog(ONTOLOGY);
    assert!(
        check_metrics_and_questions(
            std::slice::from_ref(&metric),
            &[actual.clone(), planned.clone()],
            &cat
        )
        .is_empty()
    );
    planned.dimensions.clear();
    assert_code(
        &check_metrics_and_questions(
            std::slice::from_ref(&metric),
            &[actual.clone(), planned.clone()],
            &cat,
        ),
        codes::UNKNOWN_COMPARISON,
    );
    planned.dimensions = actual.dimensions.clone();
    planned.comparison = Some(norn_ir::metric::QuestionComparison::Forecast {
        question: actual.name.clone(),
        mapping: vec![],
    });
    assert_code(
        &check_metrics_and_questions(&[metric], &[actual, planned], &cat),
        codes::COMPARISON_CYCLE,
    );
}

#[test]
fn filters_validate_literal_types_without_stored_value_bounds() {
    let (mut metric, mut question) = sales();
    metric["x-norn"]["defaultFilters"][0]["path"]["property"] = json!("absent");
    assert_code(&check(&metric, &question), codes::UNKNOWN_PATH);
    let (metric, _) = sales();
    question["filter"] = json!([{"path":{"links":["parent_sales_invoice","absent"],"property":"name"},"operator":"eq","value":"x"}]);
    assert_code(&check(&metric, &question), codes::UNKNOWN_PATH);
    question["filter"] = json!([{"path":{"property":"idx"},"operator":"gt","value":0}]);
    assert!(check(&metric, &question).is_empty());
    question["filter"][0]["value"] = json!(1);
    assert!(check(&metric, &question).is_empty());
    question["filter"] = json!([{"path":{"property":"qty"},"operator":"gt","value":null}]);
    assert_code(&check(&metric, &question), codes::INVALID_FILTER);
    question["filter"][0]["operator"] = json!("eq");
    assert!(check(&metric, &question).is_empty());
    // A to-many filter is existential, and does not require symmetric aggregation.
    question["filter"] = json!([{"path":{"links":["parent_sales_invoice","items"],"property":"name"},"operator":"eq","value":"item-1"}]);
    assert!(check(&metric, &question).is_empty());
}

#[test]
fn ratio_dependency_depth_is_bounded_and_every_operand_is_checked() {
    let (metric, _) = sales();
    let base: MetricDefinition = decode(&metric);
    let mut metrics = vec![base.clone()];
    for index in 0_u32..65 {
        let mut ratio = ratio();
        ratio["name"] = json!(format!("erpnext.ratio_{index}"));
        ratio["type_params"]["numerator"] = json!(if index == 64 {
            base.name.clone()
        } else {
            format!("erpnext.ratio_{}", index.saturating_add(1))
        });
        ratio["type_params"]["denominator"] = json!(base.name.clone());
        metrics.push(decode::<MetricDefinition>(&ratio));
    }
    assert_code(
        &check_metrics_and_questions(&metrics, &[], &catalog(ONTOLOGY)),
        codes::COMPLEXITY_LIMIT,
    );
    // No short-circuit around the denominator when the numerator is absent.
    let mut bad = ratio();
    bad["type_params"]["numerator"] = json!("erpnext.missing_a");
    bad["type_params"]["denominator"] = json!("erpnext.missing_b");
    let findings =
        check_metrics_and_questions(&[decode::<MetricDefinition>(&bad)], &[], &catalog(ONTOLOGY));
    assert_eq!(
        findings
            .iter()
            .filter(|d| d.code == codes::UNKNOWN_METRIC)
            .count(),
        2
    );
}

#[test]
fn non_additive_distinct_counts_group_underlying_objects_but_never_roll_up() {
    let mut metric: Value = serde_json::from_str(CUSTOMERS).expect("metric");
    let mut question: Value = serde_json::from_str(CUSTOMER_QUESTION).expect("question");
    assert!(check(&metric, &question).is_empty());
    question["aggregate"][0]["reaggregate"] = json!({"aggregate":"sum","across":[question["groupBy"][0]["path"].clone()],"overTime":false});
    assert_code(&check(&metric, &question), codes::NON_ADDITIVE_ROLLUP);
    metric["x-norn"]["additivity"] = json!({"kind":"additive"});
    assert_code(&check(&metric, &question), codes::INVALID_ADDITIVITY);
}

#[test]
fn ratio_questions_resolve_every_operand_dimension_and_reject_rollups() {
    let (first, mut question) = sales();
    let mut second = first.clone();
    second["name"] = json!("erpnext.other");
    let mut ratio = ratio();
    question["aggregate"][0]["metric"] = ratio["name"].clone();
    question["groupBy"] =
        json!([{"path":{"links":["parent_sales_invoice","customer","stores"],"property":"name"}}]);
    let run = |ratio: &Value, question: &Value| {
        let metrics = [
            decode::<MetricDefinition>(&first),
            decode::<MetricDefinition>(&second),
            decode::<MetricDefinition>(ratio),
        ];
        check_metrics_and_questions(
            &metrics,
            &[decode::<QuestionIR>(question)],
            &catalog(ONTOLOGY),
        )
    };
    assert_code(&run(&ratio, &question), codes::FAN_OUT);
    ratio["x-norn"]["safeAggregation"] = json!("symmetric");
    assert!(run(&ratio, &question).is_empty());
    question["aggregate"][0]["reaggregate"] = json!({"aggregate":"sum","across":[question["groupBy"][0]["path"].clone()],"overTime":false});
    assert_code(&run(&ratio, &question), codes::NON_ADDITIVE_ROLLUP);
}

#[test]
fn symmetric_fan_out_buckets_cannot_be_collapsed_as_scalar_rollups() {
    for links in [
        json!(["parent_sales_invoice", "customer", "stores"]),
        json!(["parent_sales_invoice", "items"]),
    ] {
        let (mut metric, mut question) = sales();
        metric["x-norn"]["safeAggregation"] = json!("symmetric");
        let axis = json!({"links":links,"property":"name"});
        question["groupBy"] = json!([{"path":axis}]);
        assert!(check(&metric, &question).is_empty());
        question["aggregate"][0]["reaggregate"] =
            json!({"aggregate":"sum","across":[axis],"overTime":false});
        let findings = check(&metric, &question);
        assert_code(&findings, codes::FAN_OUT_ROLLUP);
        assert!(findings.iter().any(|d| d.code == codes::FAN_OUT_ROLLUP
            && d.span.as_ref().expect("span").pointer
                == "/questions/0/aggregate/0/reaggregate/across/0"));
    }
}

#[test]
fn snapshot_questions_refuse_implicit_time_aggregation_and_unordered_mixed_reductions() {
    let (mut metric, question) = sales();
    metric["x-norn"]["additivity"] =
        json!({"kind":"semi_additive","over":"time","aggregate":"last"});
    // 10 then 20 must not silently become 30 at day/month/window grain.
    for grain in ["day", "month", "year"] {
        let mut q = question.clone();
        q["groupBy"][1]["grain"] = json!(grain);
        assert_code(&check(&metric, &q), codes::SNAPSHOT_AGGREGATION);
    }
    let mut q = question;
    q["groupBy"] = json!([]);
    assert_code(&check(&metric, &q), codes::SNAPSHOT_AGGREGATION);
    let (_, mut q) = sales();
    // Staggered store snapshots need last-per-store THEN sum, not one last.
    q["aggregate"][0]["reaggregate"] =
        json!({"aggregate":"last","across":[q["groupBy"][0]["path"]],"overTime":true});
    assert_code(&check(&metric, &q), codes::MIXED_AXIS_ROLLUP);
    q["aggregate"][0]["reaggregate"]["overTime"] = json!(false);
    q["aggregate"][0]["reaggregate"]["across"] =
        json!([q["groupBy"][0]["path"], metric["x-norn"]["timeDimension"]]);
    assert_code(&check(&metric, &q), codes::MIXED_AXIS_ROLLUP);
}

#[test]
fn mandatory_paths_reject_optional_forward_and_unproven_inverse_existence() {
    let (metric, question) = sales();
    for inverse in [false, true] {
        let mut ontology: Value = serde_json::from_str(ONTOLOGY).expect("ontology");
        if inverse {
            ontology[0]["payload"]["objectType"]["links"][0]["id"] = json!("declared_parent");
            ontology[1]["payload"]["objectType"]["links"][1]["inverse"] =
                json!("parent_sales_invoice");
        } else {
            ontology[0]["payload"]["objectType"]["links"][0]["cardinality"]["required"] =
                json!(false);
        }
        let findings = check_metrics_and_questions(
            &[decode::<MetricDefinition>(&metric)],
            &[decode::<QuestionIR>(&question)],
            &catalog(&ontology.to_string()),
        );
        assert_code(&findings, codes::PARTIAL_PATH);
        for pointer in [
            "/metrics/0/x-norn/timeDimension",
            "/metrics/0/x-norn/currency/property",
        ] {
            assert!(findings.iter().any(|d| d.code == codes::PARTIAL_PATH
                && d.span.as_ref().expect("span").pointer == pointer));
        }
        // A non-null terminal rate cannot repair a missing FX link.
        ontology[1]["payload"]["objectType"]["properties"]
            .as_array_mut()
            .expect("props")
            .push(json!({"id":"fx_rate","type":{"kind":"decimal","scale":8}}));
        let mut metric = metric.clone();
        metric["x-norn"]["currency"]["conversion"]["rate"] =
            json!({"links":["parent_sales_invoice"],"property":"fx_rate"});
        let findings = check_metrics_and_questions(
            &[decode::<MetricDefinition>(&metric)],
            &[],
            &catalog(&ontology.to_string()),
        );
        assert!(findings.iter().any(|d| d.code == codes::PARTIAL_PATH
            && d.span.as_ref().expect("span").pointer
                == "/metrics/0/x-norn/currency/conversion/rate"));
    }
    // Nullable group-by paths remain allowed, and eq null tests absence.
    let mut q = question;
    q["filter"] = json!([{"path":{"links":["parent_sales_invoice","store"],"property":"name"},"operator":"eq","value":null}]);
    assert!(check(&metric, &q).is_empty());
    q["filter"][0]["path"] = json!({"property":"name"});
    assert_code(&check(&metric, &q), codes::INVALID_FILTER);
}

#[test]
fn monetary_type_derives_currency_and_rejects_omission_and_rebinding() {
    let (mut metric, question) = sales();
    metric["x-norn"]
        .as_object_mut()
        .expect("extension")
        .remove("currency");
    assert_code(&check(&metric, &question), codes::MIXED_CURRENCY);
    let (mut metric, _) = sales();
    metric["x-norn"]["currency"] =
        json!({"property":{"links":["parent_sales_invoice"],"property":"base_currency"}});
    assert_code(&check(&metric, &question), codes::CURRENCY_SOURCE_MISMATCH);
    // A genuinely fixed-currency monetary type can infer its singleton.
    let mut ontology: Value = serde_json::from_str(ONTOLOGY).expect("ontology");
    for property in ontology[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("props")
    {
        if property["id"] == "net_amount" {
            property["type"]["currencySource"]["property"] = json!("base_currency");
        }
    }
    metric["x-norn"]
        .as_object_mut()
        .expect("extension")
        .remove("currency");
    assert!(
        check_metrics_and_questions(
            &[decode::<MetricDefinition>(&metric)],
            &[],
            &catalog(&ontology.to_string())
        )
        .is_empty()
    );
    // Non-monetary quantity types cannot acquire currency through an assertion.
    metric["type_params"]["expr"] = json!("weight");
    metric["x-norn"]["currency"] =
        json!({"property":{"links":["parent_sales_invoice"],"property":"base_currency"}});
    assert_code(&check(&metric, &question), codes::CURRENCY_SOURCE_MISMATCH);
}

#[test]
fn comparison_mapping_aligns_distinct_metrics_and_requires_compatible_periods_and_outputs() {
    let (metric, question) = sales();
    let mut planned_metric = metric.clone();
    planned_metric["name"] = json!("erpnext.planned_sales");
    let mut planned = question.clone();
    planned["name"] = json!("erpnext.plan");
    planned.as_object_mut().expect("question").remove("compare");
    planned["aggregate"][0]["metric"] = planned_metric["name"].clone();
    let mut actual = question;
    actual["compare"] = json!({"kind":"plan","question":"erpnext.plan","mapping":[{"metric":"erpnext.net_sales","referenceMetric":"erpnext.planned_sales"}]});
    let run = |planned_metric: &Value, planned: &Value, actual: &Value| {
        check_metrics_and_questions(
            &[
                decode::<MetricDefinition>(&metric),
                decode::<MetricDefinition>(planned_metric),
            ],
            &[decode::<QuestionIR>(actual), decode::<QuestionIR>(planned)],
            &catalog(ONTOLOGY),
        )
    };
    assert!(run(&planned_metric, &planned, &actual).is_empty());
    planned["window"]["start"] = json!("2026-08-01T00:00:00Z");
    assert_code(
        &run(&planned_metric, &planned, &actual),
        codes::UNKNOWN_COMPARISON,
    );
    planned["window"] = actual["window"].clone();
    planned_metric["x-norn"]["currency"]["conversion"]["target"] = json!("USD");
    assert_code(
        &run(&planned_metric, &planned, &actual),
        codes::UNKNOWN_COMPARISON,
    );
    planned_metric["x-norn"]["currency"]["conversion"]["target"] = json!("SAR");
    actual["compare"]["mapping"] = json!([]);
    assert_code(
        &run(&planned_metric, &planned, &actual),
        codes::UNKNOWN_COMPARISON,
    );
}

#[test]
fn cached_dependency_depth_is_independent_of_input_order() {
    let (mut metric, _) = sales();
    metric["x-norn"]
        .as_object_mut()
        .expect("extension")
        .remove("currency");
    metric["type_params"] = json!({"metric_aggregation_params":{"semantic_model":"erpnext.SalesInvoiceItem","agg":"count","agg_time_dimension":"norn_path_20_parent_usales_uinvoice_12_posting_udate_value"}});
    let mut metrics = vec![decode::<MetricDefinition>(&metric)];
    for index in 0_u32..64 {
        let mut r = ratio();
        r["name"] = json!(format!("erpnext.ratio_{index}"));
        r["type_params"]["numerator"] = if index == 0 {
            metric["name"].clone()
        } else {
            json!(format!("erpnext.ratio_{}", index.saturating_sub(1)))
        };
        r["type_params"]["denominator"] = metric["name"].clone();
        metrics.push(decode::<MetricDefinition>(&r));
    }
    assert!(check_metrics_and_questions(&metrics[..64], &[], &catalog(ONTOLOGY)).is_empty());
    assert_code(
        &check_metrics_and_questions(&metrics, &[], &catalog(ONTOLOGY)),
        codes::COMPLEXITY_LIMIT,
    );
    metrics.reverse();
    assert_code(
        &check_metrics_and_questions(&metrics, &[], &catalog(ONTOLOGY)),
        codes::COMPLEXITY_LIMIT,
    );
}

#[test]
fn shared_and_inherited_monetary_types_keep_the_authoritative_currency_source() {
    let mut ontology: Value = serde_json::from_str(ONTOLOGY).expect("ontology");
    let mut types = ontology[0].clone();
    types["artifactId"] = json!("erpnext.money_types.entity.v2");
    types["payload"] = json!({"entityVersion":"2.0","propertyTypes":[{"name":"erpnext.money","type":{"kind":"decimal","additivity":{"kind":"additive"},"scale":2,"currencySource":{"links":["parent_sales_invoice"],"property":"currency"}}}]});
    ontology.as_array_mut().expect("array").push(types);
    for property in ontology[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
    {
        if property["id"] == "net_amount" {
            property["type"] = json!({"kind":"ref","name":"erpnext.money"});
        }
    }
    let mut inherited = ontology[0].clone();
    inherited["artifactId"] = json!("erpnext.SpecialInvoiceItem.entity.v2");
    inherited["payload"] = json!({"entityVersion":"2.0","objectType":{"name":"erpnext.SpecialInvoiceItem","extends":"erpnext.SalesInvoiceItem"}});
    ontology.as_array_mut().expect("array").push(inherited);
    let (mut metric, mut question) = sales();
    metric["type_params"]["metric_aggregation_params"]["semantic_model"] =
        json!("erpnext.SpecialInvoiceItem");
    question["objectSet"] = json!("erpnext.SpecialInvoiceItem");
    let cat = catalog(&ontology.to_string());
    assert!(
        check_metrics_and_questions(
            &[decode::<MetricDefinition>(&metric)],
            &[decode::<QuestionIR>(&question)],
            &cat
        )
        .is_empty()
    );
    metric["x-norn"]
        .as_object_mut()
        .expect("extension")
        .remove("currency");
    assert_code(
        &check_metrics_and_questions(&[decode::<MetricDefinition>(&metric)], &[], &cat),
        codes::MIXED_CURRENCY,
    );
}

fn keyed_compute_fixture() -> (Vec<MetricDefinition>, QuestionIR, EffectiveCatalog) {
    let (mut measure, mut question) = sales();
    measure["type_params"]["expr"] = json!("weight");
    measure["type_params"]["metric_aggregation_params"]["agg_time_dimension"] =
        json!("observed_at");
    measure["x-norn"]["timeDimension"] = json!({"property":"observed_at"});
    measure["x-norn"]["defaultFilters"] = json!([]);
    measure["x-norn"]
        .as_object_mut()
        .expect("policy")
        .remove("currency");
    question
        .as_object_mut()
        .expect("question")
        .remove("compare");
    question["groupBy"] = json!([{"path":{"property":"observed_at"},"grain":"week"}]);
    let mut ontology: Value = serde_json::from_str(ONTOLOGY).expect("ontology");
    ontology[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({"id":"observed_at","type":{"kind":"timestamp","timezone":"utc"}}));
    (
        vec![decode(&measure)],
        decode(&question),
        catalog(&ontology.to_string()),
    )
}

#[test]
fn property_additivity_declared_sum_preserves_typed_identity_and_freshness() {
    use norn_checker::metric::compute::admit;
    let (definitions, question, catalog) = keyed_compute_fixture();
    let plan = admit(&definitions, &question, &catalog, 100).expect("keyed sum");
    assert_eq!(plan.grain(), &question.object_set);
    assert_eq!(plan.question(), &question);
    assert_eq!(plan.max_items(), 100);
    let [sum] = plan.sums() else {
        panic!("one measure")
    };
    assert_eq!(sum.property.id.as_str(), "weight");
    assert_eq!(sum.time_property.as_str(), "observed_at");
    assert!(
        matches!(&sum.property.kind, norn_checker::EffectiveKind::Base(
        norn_ir::PropertyType::Decimal { unit: Some(unit), .. }
    ) if unit.code == "kg")
    );
}

#[test]
fn query_grain_refuses_expansion_implicit_keys_and_unsupported_semantics() {
    use norn_checker::metric::compute::{AMBIGUOUS_GRAIN, UNSUPPORTED_ENGINE, admit};
    let (definitions, question, catalog) = keyed_compute_fixture();
    let mut expanded = question.clone();
    expanded.dimensions[0].path.links = vec![
        norn_ir::LinkId("parent_sales_invoice".into()),
        norn_ir::LinkId("items".into()),
    ];
    expanded.dimensions[0].path.property = norn_ir::PropertyId("name".into());
    expanded.dimensions[0].grain = None;
    assert_code(
        &admit(&definitions, &expanded, &catalog, 100).expect_err("fanout"),
        codes::FAN_OUT,
    );
    assert_code(
        &admit(&definitions, &expanded, &catalog, 100).expect_err("grain"),
        AMBIGUOUS_GRAIN,
    );
    let mut symmetric = definitions.clone();
    symmetric[0].norn.safe_aggregation = Some(norn_ir::metric::SafeAggregation::Symmetric);
    assert_code(
        &admit(&symmetric, &expanded, &catalog, 100).expect_err("unqualified traversal"),
        UNSUPPORTED_ENGINE,
    );
    let mut duplicate = question.clone();
    duplicate.metrics.push(duplicate.metrics[0].clone());
    assert!(admit(&definitions, &duplicate, &catalog, 100).is_err());
    assert_code(
        &admit(&definitions, &question, &catalog, 0).expect_err("zero bound"),
        UNSUPPORTED_ENGINE,
    );
    let mut multiple = question;
    let mut other = definitions[0].clone();
    other.name = "erpnext.other".into();
    multiple.metrics.push(norn_ir::metric::QuestionMetric {
        metric: other.name.clone(),
        inline: None,
        reaggregate: None,
    });
    let mut definitions = definitions;
    definitions.push(other);
    assert_code(
        &admit(&definitions, &multiple, &catalog, 100).expect_err("multiple outputs"),
        UNSUPPORTED_ENGINE,
    );
}

/// QUERY-FANOUT: one Holding whose `tags` link is many-to-many.
fn holding_fanout_fixture() -> (Vec<MetricDefinition>, QuestionIR, EffectiveCatalog) {
    let mut ontology: Vec<Value> = serde_json::from_str(ONTOLOGY).expect("ontology");
    let template = ontology.swap_remove(0);
    let artifact = |object_type: Value| {
        let name = object_type["name"].clone();
        let mut artifact = template.clone();
        artifact["artifactId"] = json!(format!("{}.entity.v2", name.as_str().expect("name")));
        artifact["namespace"] = json!("example");
        artifact["name"] = name;
        artifact["provenance"]["sourcePath"] = json!("synthetic:query-fanout");
        artifact["payload"]["objectType"] = object_type;
        artifact
    };
    let holding = json!({"name":"example.Holding","properties":[
        {"id":"value","type":{"kind":"decimal","scale":3,"unit":{"system":"ucum","code":"g"},"additivity":{"kind":"additive"}}},
        {"id":"observed_at","type":{"kind":"timestamp","timezone":"utc"}}
    ],"links":[{"id":"tags","target":"example.Tag","cardinality":{"to":"many","from":"many","required":false}}]});
    let tag = json!({"name":"example.Tag","properties":[{"id":"label","type":{"kind":"text"}}]});
    let catalog = catalog(&json!([artifact(holding), artifact(tag)]).to_string());
    let (mut measure, mut question) = sales();
    measure["name"] = json!("example.holding_value");
    measure["type_params"]["expr"] = json!("value");
    measure["type_params"]["metric_aggregation_params"]["semantic_model"] =
        json!("example.Holding");
    measure["x-norn"]["timeDimension"] = json!({"property":"observed_at"});
    measure["x-norn"]["defaultFilters"] = json!([]);
    measure["x-norn"]
        .as_object_mut()
        .expect("policy")
        .remove("currency");
    question
        .as_object_mut()
        .expect("question")
        .remove("compare");
    question["name"] = json!("example.holding_total");
    question["objectSet"] = json!("example.Holding");
    question["aggregate"] = json!([{"metric":"example.holding_value"}]);
    question["groupBy"] = json!([]);
    (vec![decode(&measure)], decode(&question), catalog)
}

/// QUERY-GRAIN-1 (QUERY-FANOUT): one holding worth 80 linked to two tags totals 80
/// at holding grain, in grams; grouping by the expanded tag grain refuses.
#[test]
fn query_grain_fanout_holding_80_with_two_tags_totals_80_and_expanded_grain_refuses() {
    use norn_checker::metric::compute::{AMBIGUOUS_GRAIN, admit, contribution::project};
    let (definitions, question, catalog) = holding_fanout_fixture();
    let plan = admit(&definitions, &question, &catalog, 100).expect("holding grain");
    assert_eq!(plan.grain().as_str(), "example.Holding");
    let [sum] = plan.sums() else {
        panic!("one measure")
    };
    assert!(sum.filters.is_empty(), "no link traversal at root grain");
    assert!(
        matches!(&sum.property.kind, norn_checker::EffectiveKind::Base(
        norn_ir::PropertyType::Decimal { unit: Some(unit), .. }
    ) if unit.code == "g")
    );
    // The plan projects each root identity once; its two tags are not part of the row.
    let holding = std::collections::BTreeMap::from([
        ("value".into(), norn_ir::RuntimeValue::from(80_i64)),
        (
            "observed_at".into(),
            norn_ir::RuntimeValue::from("2026-10-23T00:00:00Z"),
        ),
    ]);
    let contribution = project(&plan, &holding)
        .expect("project")
        .expect("in window");
    assert_eq!(contribution.bucket, norn_ir::RuntimeValue::Null);
    assert_eq!(contribution.amounts, vec![Some(80_000_000)], "80 at 1e-6");

    let mut expanded = question;
    expanded.dimensions = vec![decode(
        &json!({"path":{"links":["tags"],"property":"label"}}),
    )];
    let findings = admit(&definitions, &expanded, &catalog, 100).expect_err("expanded grain");
    assert_code(&findings, codes::FAN_OUT);
    assert_code(&findings, AMBIGUOUS_GRAIN);
}

proptest::proptest! {
    #[test]
    fn compute_deterministic_ratio_lowering_ignores_definition_order(reverse in proptest::bool::ANY, rotation in 0usize..3, filter_key in 0u32..32, max_items in 1u32..=1000) {
        use norn_checker::metric::compute::admit;
        let (mut definitions, mut question, catalog) = keyed_compute_fixture();
        let filter = norn_ir::metric::MetricFilter {
            path: norn_ir::metric::DimensionPath { links: vec![], property: norn_ir::PropertyId("name".into()) },
            operator: norn_ir::metric::FilterOperator::Eq,
            value: norn_ir::RuntimeValue::from(format!("row-{filter_key}")),
        };
        definitions[0].norn.default_filters = vec![filter.clone()];
        let mut denominator = definitions[0].clone();
        denominator.norn.default_filters.clear();
        denominator.name = "erpnext.other".into();
        let mut ratio: MetricDefinition = decode(&ratio());
        ratio.norn.time_dimension = definitions[0].norn.time_dimension.clone();
        question.metrics[0].metric.clone_from(&ratio.name);
        definitions.extend([denominator, ratio]);
        let expected = admit(&definitions, &question, &catalog, max_items).expect("ratio");
        if reverse { definitions.reverse(); }
        definitions.rotate_left(rotation);
        let actual = admit(&definitions, &question, &catalog, max_items).expect("reordered ratio");
        proptest::prop_assert_eq!(actual.sums().iter().map(|s| &s.name).collect::<Vec<_>>(), expected.sums().iter().map(|s| &s.name).collect::<Vec<_>>());
        proptest::prop_assert_eq!(actual.sums()[0].filters.as_slice(), &[filter]);
        proptest::prop_assert!(actual.sums()[1].filters.is_empty());
        proptest::prop_assert_eq!(actual.grain(), expected.grain());
        proptest::prop_assert_eq!(actual.max_items(), max_items);
    }

    #[test]
    fn query_freshness_shape_never_admits_a_zero_age(age in proptest::prop_oneof![proptest::strategy::Just(0u64), 1u64..=10000], delayed in proptest::bool::ANY) {
        let (definitions, mut question, catalog) = keyed_compute_fixture();
        question.freshness = if delayed {
            norn_ir::metric::FreshnessPolicy::Delayed { max_age_ms: norn_ir::WholeNumber(age) }
        } else {
            norn_ir::metric::FreshnessPolicy::Live { max_age_ms: norn_ir::WholeNumber(age) }
        };
        proptest::prop_assert_eq!(norn_checker::metric::compute::admit(&definitions, &question, &catalog, 100).is_ok(), age > 0);
    }
}

proptest::proptest! {
    #[test]
    fn query_projection_uses_iso_weeks_exact_values_and_half_open_windows(day in 19u8..=25, amount in 0u32..=1_000_000) {
        use norn_checker::metric::compute::{admit, contribution::project};
        let (definitions, mut question, catalog) = keyed_compute_fixture();
        question.time_window.start = norn_ir::UtcTimestamp("2026-10-19T00:00:00Z".into());
        question.time_window.end = norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into());
        let plan = admit(&definitions, &question, &catalog, 100).expect("plan");
        let mut row = std::collections::BTreeMap::from([
            ("weight".into(), norn_ir::RuntimeValue::from(i64::from(amount))),
            ("observed_at".into(), norn_ir::RuntimeValue::from(format!("2026-10-{day}T12:00:00Z"))),
        ]);
        let actual = project(&plan, &row).expect("project").expect("included");
        proptest::prop_assert_eq!(actual.bucket, norn_ir::RuntimeValue::from("2026-10-19"));
        proptest::prop_assert_eq!(actual.amounts, vec![Some(i64::from(amount).checked_mul(1_000_000).expect("coefficient"))]);
        let mut invalid = row.clone();
        invalid.insert("weight".into(), norn_ir::RuntimeValue::from_json_text("0.0000001").expect("decimal"));
        proptest::prop_assert!(project(&plan, &invalid).is_err());
        invalid.remove("weight");
        proptest::prop_assert!(project(&plan, &invalid).is_err());
        row.insert("observed_at".into(), norn_ir::RuntimeValue::from("2026-10-26T00:00:00Z"));
        proptest::prop_assert!(project(&plan, &row).expect("boundary").is_none());
    }
}

#[test]
fn query_ordered_predicates_refuse_before_reading_any_population() {
    use norn_checker::metric::compute::admit;
    let (mut definitions, mut question, catalog) = keyed_compute_fixture();
    let filter = decode(&json!({"path":{"property":"weight"},"operator":"gt","value":0}));
    question.filters = vec![filter];
    assert!(admit(&definitions, &question, &catalog, 100).is_err());
    definitions[0].norn.default_filters = std::mem::take(&mut question.filters);
    assert!(admit(&definitions, &question, &catalog, 100).is_err());
}

/// METRIC-ADDITIVITY-1: declaration, rather than unit or metric metadata, authorizes aggregation.
#[test]
fn invoice_totals_require_their_authoritative_currency() {
    let (mut metric, _) = sales();
    metric["type_params"]["expr"] = json!("grand_total");
    metric["type_params"]["metric_aggregation_params"]["semantic_model"] =
        json!("erpnext.SalesInvoice");
    metric["x-norn"]["timeDimension"] = json!({"property":"posting_date"});
    metric["x-norn"]["defaultFilters"] = json!([]);
    metric["x-norn"]
        .as_object_mut()
        .expect("policy")
        .remove("currency");
    let metric: MetricDefinition = decode(&metric);
    assert_code(
        &check_metrics_and_questions(&[metric], &[], &catalog(ONTOLOGY)),
        codes::MIXED_CURRENCY,
    );
}

#[test]
fn property_additivity_refuses_every_undeclared_numeric_aggregate() {
    let (metric, question) = sales();
    for aggregate in ["sum", "average", "min", "max", "count", "count_distinct"] {
        let mut ontology: Value = serde_json::from_str(ONTOLOGY).expect("ontology");
        let properties = ontology[0]["payload"]["objectType"]["properties"]
            .as_array_mut()
            .expect("properties");
        let amount = properties
            .iter_mut()
            .find(|p| p["id"] == "net_amount")
            .expect("amount");
        amount["type"]
            .as_object_mut()
            .expect("type")
            .remove("additivity");
        let mut measure = metric.clone();
        measure["type_params"]["metric_aggregation_params"]["agg"] = json!(aggregate);
        let findings = check_metrics_and_questions(
            &[decode(&measure)],
            &[decode(&question)],
            &catalog(&ontology.to_string()),
        );
        assert_code(&findings, codes::UNDECLARED_ADDITIVITY);
        let refusal = findings
            .iter()
            .find(|d| d.code == codes::UNDECLARED_ADDITIVITY)
            .expect("refusal");
        assert_eq!(
            refusal.remediation,
            Some(norn_checker::DiagnosticRemediation::FixDefinition)
        );
    }
}

#[test]
fn ratios_prices_and_balances_cannot_be_laundered_through_additive_metrics() {
    let (metric, question) = sales();
    for (class, expected) in [
        (
            json!({"kind":"ratio", "numerator":"weight", "denominator":"volume"}),
            codes::RATIO_AGGREGATION,
        ),
        (json!({"kind":"non_additive"}), codes::INVALID_ADDITIVITY),
        (json!({"kind":"semi_additive"}), codes::SEMI_ADDITIVE_TIME),
    ] {
        let mut ontology: Value = serde_json::from_str(ONTOLOGY).expect("ontology");
        let properties = ontology[0]["payload"]["objectType"]["properties"]
            .as_array_mut()
            .expect("properties");
        properties
            .iter_mut()
            .find(|p| p["id"] == "net_amount")
            .expect("amount")["type"]["additivity"] = class;
        assert_code(
            &check_metrics_and_questions(
                &[decode(&metric)],
                &[decode(&question)],
                &catalog(&ontology.to_string()),
            ),
            expected,
        );
    }
}

#[test]
fn numeric_additivity_cannot_be_inferred_from_units_or_scale() {
    for scale in 0u64..=18 {
        for additive in [false, true] {
            let mut ontology: Value = serde_json::from_str(ONTOLOGY).expect("ontology");
            let properties = ontology[0]["payload"]["objectType"]["properties"]
                .as_array_mut()
                .expect("properties");
            let amount = properties
                .iter_mut()
                .find(|p| p["id"] == "weight")
                .expect("weight");
            amount["type"]["scale"] = json!(scale);
            if !additive {
                amount["type"]
                    .as_object_mut()
                    .expect("type")
                    .remove("additivity");
            }
            let (mut metric, question) = sales();
            metric["type_params"]["expr"] = json!("weight");
            metric["x-norn"]
                .as_object_mut()
                .expect("extension")
                .remove("currency");
            let findings = check_metrics_and_questions(
                &[decode(&metric)],
                &[decode(&question)],
                &catalog(&ontology.to_string()),
            );
            assert_eq!(
                findings
                    .iter()
                    .any(|d| d.code == codes::UNDECLARED_ADDITIVITY),
                !additive,
                "scale {scale} additive {additive}"
            );
        }
    }
}

fn question_wording() -> norn_checker::metric::question_display::QuestionWording {
    use norn_checker::metric::question_display::{QuestionLabels, QuestionWording};
    use norn_ir::deontic::TextDirection;
    QuestionWording(std::collections::BTreeMap::from([
        (
            "en".into(),
            QuestionLabels {
                direction: TextDirection::Ltr,
                population: "Population".into(),
                measure: "Measure".into(),
                by: "By".into(),
                filters: "Where".into(),
                effective_time: "Effective time".into(),
                knowledge_time: "Knowledge policy".into(),
            },
        ),
        (
            "ar".into(),
            QuestionLabels {
                direction: TextDirection::Rtl,
                population: "مجتمع البيانات".into(),
                measure: "المقياس".into(),
                by: "التجميع حسب".into(),
                filters: "الشروط".into(),
                effective_time: "وقت السريان".into(),
                knowledge_time: "سياسة المعرفة".into(),
            },
        ),
    ]))
}

proptest::proptest! {
    #[test]
    fn question_locale_identity(age in 1u64..=10000, pinned in proptest::bool::ANY) {
        use norn_checker::metric::{compute::admit, question_display::render_question};
        let (definitions,mut question,catalog) = keyed_compute_fixture();
        question.freshness = if pinned { norn_ir::metric::FreshnessPolicy::AsOf { at:norn_ir::UtcTimestamp("2026-11-02T00:00:00Z".to_owned()) } } else { norn_ir::metric::FreshnessPolicy::Live { max_age_ms:norn_ir::WholeNumber(age) } };
        let plan = admit(&definitions,&question,&catalog,100).expect("plan");
        let wording = question_wording();
        let en = render_question(&plan,&wording,"EN").expect("English");
        let ar = render_question(&plan,&wording,"ar").expect("Arabic");
        proptest::prop_assert_eq!(&en.question,&question);
        proptest::prop_assert_eq!(&ar.question,&question);
        proptest::prop_assert_eq!(&en.question_digest,&ar.question_digest);
        proptest::prop_assert_eq!(&en.binding_digest,&ar.binding_digest);
        proptest::prop_assert_eq!(en.slots.iter().map(|s|(&s.id,&s.text)).collect::<Vec<_>>(),ar.slots.iter().map(|s|(&s.id,&s.text)).collect::<Vec<_>>());
        proptest::prop_assert_ne!(en.direction,ar.direction);
        proptest::prop_assert!(en.slots[1].text.contains("kg"));
        proptest::prop_assert!(en.slots[4].text.contains(&question.time_window.start.0));
        let policy = if pinned { "as_of" } else { "live" };
        proptest::prop_assert!(en.slots[5].text.contains(policy));
    }
}

#[test]
fn question_locale_identity_refuses_unknown_or_incomplete_wording_and_retains_denominator() {
    use norn_checker::metric::{compute::admit, question_display::render_question};
    let (mut definitions, mut question, catalog) = keyed_compute_fixture();
    let mut denominator = definitions[0].clone();
    denominator.name = "erpnext.other".into();
    let mut rate: MetricDefinition = decode(&ratio());
    rate.norn.time_dimension = definitions[0].norn.time_dimension.clone();
    question.metrics[0].metric.clone_from(&rate.name);
    definitions.extend([denominator, rate]);
    let plan = admit(&definitions, &question, &catalog, 100).expect("ratio plan");
    let mut wording = question_wording();
    let en = render_question(&plan, &wording, "en").expect("English");
    assert_eq!(en.slots[1].text.matches("Σ(").count(), 2);
    assert_eq!(en.slots[1].text.matches("kg").count(), 2);
    assert!(en.slots[1].text.contains(" / "));
    assert_eq!(
        render_question(&plan, &wording, "fr").expect_err("no fallback"),
        "law.wording.unknown_locale"
    );
    wording.0.get_mut("ar").expect("Arabic").measure.clear();
    assert_eq!(
        render_question(&plan, &wording, "en").expect_err("all translations complete"),
        "law.wording.incomplete_locale"
    );
}
