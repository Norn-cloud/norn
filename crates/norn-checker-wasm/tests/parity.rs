#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! The checker ABI natively (T1-CHK-4, #2527).
//!
//! Runs every case of `tests/parity/cases.json` through the ABI and checks it
//! against the checker called directly. With `NORN_CHECKER_WASM_PARITY_OUT`
//! set, each case's output is also written to `<dir>/<name>.out`; the CI job
//! `checker-wasm` then runs `tests/parity/parity.mjs` against the built npm
//! package and requires byte-identical output for every case.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_checker::{
    ArtifactRef, EntityArtifactSetContext, check_artifact_set_with_flow_source_map,
    decode_artifact_json, diagnostics_to_json,
};
use norn_checker_wasm::{
    canonical_runtime_value_json, check_artifact_set_json, check_expression_json,
    evaluate_expression_json, function_contract_json, lifecycle_diagrams_json, version_json,
};
use norn_expr::{EvaluationBudget, Expression, Value as ExprValue};
use norn_ir::ResourceLimitProfile;
use serde_json::Value;

const OUT_ENV: &str = "NORN_CHECKER_WASM_PARITY_OUT";

/// `norn-checker`'s pinned hash of the retail registry receipt
/// (`tests/check_receipt.rs`): the ABI issues the same receipt.
const RETAIL_RECEIPT_SHA256: &str =
    "c4f1873c8393a5148cc6b039bcb4e01355345f45e4d921ee55d0d0c8766b803e";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cases() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/cases.json");
    let text = std::fs::read_to_string(&path).expect("read cases.json");
    let manifest: Value = serde_json::from_str(&text).expect("cases.json parses");
    manifest["cases"].as_array().expect("cases array").clone()
}

fn read(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// Add the legacy inline editor metadata used by the parity regression.
fn inline_layout(mut artifact: Value) -> Value {
    let graph = artifact
        .pointer_mut("/payload/graph")
        .and_then(Value::as_object_mut)
        .expect("inline-layout fixture has a graph");
    graph.insert(
        "viewport".to_owned(),
        serde_json::json!({ "x": 12.5, "y": -4, "zoom": 1.25 }),
    );
    graph["nodes"]
        .as_array_mut()
        .and_then(|nodes| nodes.first_mut())
        .and_then(Value::as_object_mut)
        .expect("inline-layout fixture has a node")
        .insert(
            "description".to_owned(),
            Value::String("editor-only node description".to_owned()),
        );
    graph["edges"]
        .as_array_mut()
        .and_then(|edges| edges.first_mut())
        .and_then(Value::as_object_mut)
        .expect("inline-layout fixture has an edge")
        .insert(
            "targetHandle".to_owned(),
            Value::String("editor-target".to_owned()),
        );
    artifact
}

fn array_text(case: &Value, key: &str) -> String {
    let inline_layout_on = case["inlineLayoutOn"].as_str() == Some(key);
    let files: Vec<String> = case[key]
        .as_array()
        .map(|paths| {
            paths
                .iter()
                .map(|path| {
                    let source = read(path.as_str().expect("path string"));
                    if inline_layout_on {
                        let artifact: Value = serde_json::from_str(&source).expect("fixture JSON");
                        serde_json::to_string(&inline_layout(artifact)).expect("inline JSON")
                    } else {
                        source
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    format!("[{}]", files.join(","))
}

fn artifacts_text(case: &Value) -> String {
    case["artifactsText"]
        .as_str()
        .map_or_else(|| array_text(case, "artifacts"), str::to_owned)
}

fn context_text(case: &Value) -> String {
    if let Some(text) = case["contextText"].as_str() {
        return text.to_owned();
    }
    let mut members = vec![format!("\"base\":{}", array_text(case, "base"))];
    if let Some(extra) = case["context"].as_object() {
        members.extend(
            extra
                .iter()
                .map(|(key, value)| format!("{}:{value}", Value::from(key.as_str()))),
        );
    }
    format!("{{{}}}", members.join(","))
}

fn evaluation_input_text(case: &Value) -> String {
    if let Some(input) = case["inputJson"].as_str() {
        return input.to_owned();
    }
    if let Some(bytes) = case["inputJsonBytes"].as_u64() {
        let prefix = "{\"context\":{\"padding\":\"";
        let suffix = "\"}}";
        let bytes = usize::try_from(bytes).expect("input fixture size fits usize");
        let padding_bytes = bytes
            .checked_sub(prefix.len())
            .and_then(|remaining| remaining.checked_sub(suffix.len()))
            .expect("input fixture size fits JSON envelope");
        return format!("{prefix}{}{suffix}", "a".repeat(padding_bytes));
    }
    let Some(count) = case["inputSelectionCount"].as_u64() else {
        return "{}".to_owned();
    };
    let count = usize::try_from(count).expect("selection fixture count fits usize");
    let records = std::iter::repeat_n("{\"qty\":1}", count)
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"selection\":[{records}]}}")
}

fn run(case: &Value) -> String {
    let result = match case["call"].as_str().expect("call") {
        "checkArtifactSet" => check_artifact_set_json(&artifacts_text(case), &context_text(case)),
        "lifecycleDiagrams" => lifecycle_diagrams_json(&artifacts_text(case)),
        "checkExpression" => check_expression_json(
            case["expr"].as_str().expect("expr"),
            &artifacts_text(case),
            case["expectedType"].as_str().expect("expectedType"),
            case["selfType"].as_str(),
        ),
        "evaluateExpression" => Ok(evaluate_expression_json(
            case["expr"].as_str().expect("expr"),
            &evaluation_input_text(case),
            case["profile"].as_str(),
        )),
        "canonicalRuntimeValue" => {
            canonical_runtime_value_json(case["value"].as_str().expect("value"))
        }
        "functionContract" => function_contract_json(
            &read(case["manifest"].as_str().expect("manifest")),
            case["invocation"].as_str().map(read).as_deref(),
            case["receipt"].as_str().map(read).as_deref(),
        ),
        "version" => Ok(version_json()),
        other => panic!("unknown call {other}"),
    };
    result.unwrap_or_else(|error| format!("error: {error}"))
}

fn outputs() -> Vec<(String, Value, String)> {
    cases()
        .into_iter()
        .map(|case| {
            let name = case["name"].as_str().expect("name").to_owned();
            let output = run(&case);
            (name, case, output)
        })
        .collect()
}

fn output_of(name: &str) -> String {
    outputs()
        .into_iter()
        .find(|(case, _, _)| case == name)
        .map_or_else(|| panic!("no case {name}"), |(_, _, output)| output)
}

fn json_of(output: &str) -> Value {
    serde_json::from_str(output).unwrap_or_else(|error| panic!("{output}: {error}"))
}

#[test]
fn writes_native_outputs_for_the_wasm_parity_run() {
    let outputs = outputs();
    let names: BTreeSet<&str> = outputs.iter().map(|(name, _, _)| name.as_str()).collect();
    assert_eq!(names.len(), outputs.len(), "case names are unique");
    if let Some(dir) = std::env::var_os(OUT_ENV) {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("output directory");
        for (name, _, output) in &outputs {
            std::fs::write(dir.join(format!("{name}.out")), output).expect("write output");
        }
    }
}

#[test]
fn check_matches_the_checker_called_directly() {
    for (name, case, output) in outputs() {
        if case["call"] != "checkArtifactSet" || case.get("artifactsText").is_some() {
            continue;
        }
        let decode = |key: &str| -> Vec<_> {
            serde_json::from_str::<Vec<Value>>(&array_text(&case, key))
                .expect("fixture array parses")
                .iter()
                .map(|value| decode_artifact_json(value).expect("fixture decodes"))
                .collect()
        };
        let base = decode("base");
        let namespace = norn_ir::ExtensionNamespace(
            case["context"]["tenant"]["namespace"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        );
        let tenant = case["context"]["tenant"]["tenantId"]
            .as_str()
            .map(|tenant_id| norn_checker::TenantLayer {
                tenant_id,
                namespace: &namespace,
            });
        let registry = BTreeSet::new();
        let flow_source_map = case["context"]["flowSourceMap"].as_object().map(|_| {
            serde_json::from_value(case["context"]["flowSourceMap"].clone())
                .expect("flow source map decodes")
        });
        let direct = check_artifact_set_with_flow_source_map(
            &decode("artifacts"),
            &EntityArtifactSetContext {
                registry: &registry,
                base: &base,
                tenant,
            },
            &ResourceLimitProfile::norn_limits_v1(),
            flow_source_map.as_ref(),
        )
        .expect("receipt");
        let abi = json_of(&output);
        assert_eq!(
            abi["diagnostics"].to_string(),
            diagnostics_to_json(&direct.diagnostics).to_string(),
            "{name}: diagnostics"
        );
        assert_eq!(
            abi["receipt"].to_string(),
            String::from_utf8(direct.receipt.canonical_json().expect("canonical")).expect("utf-8"),
            "{name}: receipt"
        );
        assert_eq!(abi["admitted"], direct.receipt.admitted, "{name}: admitted");
    }
}

#[test]
fn clean_set_issues_the_pinned_receipt() {
    let output = json_of(&output_of("retail-registry-clean"));
    assert_eq!(output["format"], "norn-checker-abi/check@1");
    assert_eq!(output["admitted"], true);
    assert_eq!(
        output["diagnostics"]["format"],
        "norn-checker/diagnostics@1"
    );
    assert_eq!(output["diagnostics"]["diagnostics"], serde_json::json!([]));
    assert_eq!(output["receipt"]["format"], "norn-checker/receipt@2");
    assert_eq!(output["receiptSha256"], RETAIL_RECEIPT_SHA256);
}

#[test]
fn invalid_sets_report_findings_not_errors() {
    let output = json_of(&output_of("retail-with-unknown-supertype"));
    assert_eq!(output["admitted"], false);
    assert!(
        !output["diagnostics"]["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .is_empty()
    );
    assert_eq!(output["receipt"]["admitted"], false);

    let retired = json_of(&output_of("retired-kind-envelope"));
    assert_eq!(retired["admitted"], false);
    assert_eq!(retired["receipt"], Value::Null);
    assert_eq!(
        retired["diagnostics"]["diagnostics"][0]["artifactId"],
        "legacy.process"
    );
}

#[test]
fn tenant_layer_checks_against_its_base() {
    let output = json_of(&output_of("tenant-extension-on-base"));
    assert_eq!(output["admitted"], true, "{output}");
    assert_eq!(
        output["receipt"]["artifactSet"]["base"]
            .as_array()
            .map(Vec::len),
        Some(4)
    );
}

#[test]
fn inline_editor_layout_is_admitted_in_candidate_and_base_with_semantic_hashes() {
    let candidate = json_of(&output_of("inline-layout-rule-candidate"));
    let base = json_of(&output_of("inline-layout-rule-base"));
    let expression = json_of(&output_of("inline-layout-rule-expression-environment"));
    assert_eq!(candidate["admitted"], true, "{candidate}");
    assert_eq!(base["admitted"], true, "{base}");
    assert_eq!(expression["ok"], true, "{expression}");

    let plain: Value = serde_json::from_str(&read(
        "fixtures/rule-ir-v2/valid/reorder-priority.rule.json",
    ))
    .expect("valid RuleIR fixture");
    let plain = decode_artifact_json(&plain).expect("fixture decodes");
    let expected = ArtifactRef::of(&plain).expect("semantic artifact hash");
    assert_eq!(
        candidate["receipt"]["artifactSet"]["artifacts"][0]["sha256"].as_str(),
        Some(expected.sha256.as_str())
    );
    assert_eq!(
        base["receipt"]["artifactSet"]["base"][0]["sha256"].as_str(),
        Some(expected.sha256.as_str())
    );
}

#[test]
fn inline_layout_base_is_resolved_as_a_referenced_sub_decision() {
    let candidate = json_of(&output_of(
        "inline-layout-referenced-sub-decision-candidate",
    ));
    let base = json_of(&output_of("inline-layout-referenced-sub-decision-base"));
    assert_eq!(candidate["admitted"], true, "{candidate}");
    assert_eq!(base["admitted"], true, "{base}");
    let plain: Value = serde_json::from_str(&read("fixtures/rule-ir-v2/sets/chain-4.rule.json"))
        .expect("sub-decision fixture");
    let plain = decode_artifact_json(&plain).expect("fixture decodes");
    let expected = ArtifactRef::of(&plain).expect("semantic artifact hash");
    assert_eq!(
        candidate["receipt"]["artifactSet"]["artifacts"][1]["artifactId"].as_str(),
        Some(expected.artifact_id.as_str())
    );
    assert_eq!(
        candidate["receipt"]["artifactSet"]["artifacts"][1]["sha256"].as_str(),
        Some(expected.sha256.as_str())
    );
    assert_eq!(
        base["receipt"]["artifactSet"]["base"][0]["sha256"].as_str(),
        Some(expected.sha256.as_str())
    );
}

#[test]
fn invalid_input_is_an_error() {
    assert!(output_of("artifacts-not-json").starts_with("error: invalid artifacts: "));
    assert!(output_of("context-unknown-member").starts_with("error: invalid context: "));
    let outputs = outputs();
    for case in ["context-array", "context-string", "context-number", "context-boolean"] {
        let output = outputs
            .iter()
            .find(|(name, _, _)| name == case)
            .map(|(_, _, output)| output)
            .unwrap_or_else(|| panic!("no case {case}"));
        assert!(output.starts_with("error: invalid context: "), "{case}");
    }
    for case in [
        "context-tenant-array",
        "context-tenant-string",
        "context-tenant-number",
        "context-tenant-boolean",
    ] {
        let output = outputs
            .iter()
            .find(|(name, _, _)| name == case)
            .map(|(_, _, output)| output)
            .unwrap_or_else(|| panic!("no case {case}"));
        assert!(output.starts_with("error: invalid context.tenant: "), "{case}");
    }
    assert!(output_of("expression-bad-expected-type").starts_with("error: invalid expectedType: "));
}

#[test]
fn parity_conditions_match_the_widget_fixture() {
    let fixture: Value = serde_json::from_str(&read("fixtures/ir/ux-orders-customer.widget.json"))
        .expect("widget fixture JSON");
    let corpus = cases();
    for (name, pointer) in [
        (
            "evaluation-widget-visible-when-record",
            "/payload/ux/fields/0/visibleWhen/expression",
        ),
        (
            "evaluation-widget-visible-when-context",
            "/payload/ux/fields/3/visibleWhen/expression",
        ),
        (
            "evaluation-widget-enabled-when-record",
            "/payload/ux/actionPlacements/0/enabledWhen/expression",
        ),
    ] {
        let case = corpus
            .iter()
            .find(|case| case["name"] == name)
            .expect("parity case");
        assert_eq!(
            &case["expr"],
            fixture.pointer(pointer).expect("fixture condition")
        );
    }
}

#[test]
fn widget_conditions_evaluate_over_their_explicit_inputs() {
    for name in [
        "evaluation-widget-visible-when-record",
        "evaluation-widget-visible-when-context",
        "evaluation-widget-enabled-when-record",
        "evaluation-all-widget-input-slots",
        "evaluation-omitted-slot",
    ] {
        let output = json_of(&output_of(name));
        assert_eq!(output["format"], "norn-checker-abi/evaluation@1");
        assert_eq!(output["profile"], "norn-zen/6");
        assert_eq!(output["ok"], true, "{name}: {output}");
        assert_eq!(output["value"], true, "{name}: {output}");
    }
}

#[test]
fn evaluation_matches_norn_expr_and_preserves_decimal_tokens() {
    let case = cases()
        .into_iter()
        .find(|case| case["name"] == "evaluation-exact-decimals")
        .expect("exact decimal parity case");
    let input = norn_ir::RuntimeValue::from_json_text(evaluation_input_text(&case))
        .expect("exact runtime input");
    let input = ExprValue::try_from(&input).expect("expression input value");
    let context = ExprValue::object([("input", input)]);
    let direct = Expression::compile(case["expr"].as_str().expect("expr"))
        .expect("compile")
        .evaluate(&context, EvaluationBudget::default())
        .expect("evaluate");
    assert_eq!(
        direct.value(),
        &ExprValue::Number(
            "9007199254740993.3"
                .parse()
                .expect("exact expected decimal")
        )
    );

    let abi = output_of("evaluation-exact-decimals");
    assert!(abi.contains("\"value\":9007199254740993.3"), "{abi}");

    let mapping = output_of("evaluation-exact-action-mapping");
    assert!(
        mapping.contains(
            "\"value\":{\"customer\":\"C-42\",\"quantities\":[1.5],\"total\":9007199254740993.3}"
        ),
        "{mapping}"
    );
}

#[test]
fn evaluation_returns_typed_refusals_and_stops_at_its_fuel_limit() {
    for (name, code) in [
        ("evaluation-inexact-literal", "expr.construct.unsupported"),
        ("evaluation-compile-error", "expr.syntax.invalid"),
        ("evaluation-runtime-error", "expr.evaluation.failed"),
        ("evaluation-invalid-input-slot", "expr.input.invalid"),
        ("evaluation-invalid-json", "expr.input.invalid"),
        ("evaluation-input-over-limit", "expr.input.invalid"),
        ("evaluation-invalid-selection-item", "expr.input.invalid"),
        ("evaluation-selection-over-limit", "expr.input.invalid"),
        ("evaluation-invalid-profile", "expr.profile.invalid"),
    ] {
        let output = json_of(&output_of(name));
        assert_eq!(output["format"], "norn-checker-abi/evaluation@1");
        assert_eq!(output["ok"], false, "{name}: {output}");
        assert_eq!(output["error"]["code"], code, "{name}: {output}");
    }

    let budget = json_of(&output_of("evaluation-budget-exhausted"));
    assert_eq!(budget["ok"], false, "{budget}");
    assert_eq!(budget["error"]["code"], "expr.budget.exhausted", "{budget}");
    let limit = EvaluationBudget::default().units();
    assert_eq!(budget["error"]["limit"], limit, "{budget}");
    let units = budget["unitsUsed"].as_u64().expect("meter usage");
    assert!(units > 0 && units <= limit, "{budget}");
    assert!(
        budget["error"]["used"].as_u64().expect("attempted charge") > limit,
        "{budget}"
    );
}

#[test]
fn lifecycle_diagrams_match_the_projection() {
    let output = json_of(&output_of("purchase-order-lifecycles"));
    let diagrams = output.as_array().expect("array");
    assert_eq!(diagrams.len(), 2);
    assert_eq!(diagrams[0]["type"], "commerce.PurchaseOrder");
    assert_eq!(
        diagrams[0]["artifactId"],
        "commerce.PurchaseOrder.entity.v2"
    );
    assert_eq!(diagrams[0]["property"], "status");
    // The golden of norn-projections' tests/lifecycle_diagram.rs.
    assert_eq!(
        diagrams[0]["mermaid"],
        "stateDiagram-v2
    state \"draft\" as s0
    state \"submitted\" as s1
    state \"approved\" as s2
    state \"rejected\" as s3
    state \"received\" as s4
    state \"cancelled\" as s5
    [*] --> s0
    s0 --> s1 : submit
    s1 --> s2 : approve
    s1 --> s3 : reject
    s3 --> s0 : revise
    s2 --> s4 : receive
    s0 --> s5 : cancel
    s1 --> s5 : cancel
    s2 --> s5 : cancel
    s4 --> [*]
    s5 --> [*]
"
    );
    assert_eq!(diagrams[1]["property"], "payment_status");
}

#[test]
fn expressions_check_in_the_set_environment() {
    let ok = json_of(&output_of("expression-ok"));
    assert_eq!(ok["format"], "norn-checker-abi/expression@1");
    assert_eq!(ok["profile"], "norn-zen/6");
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["inferred"], "bool");

    for name in [
        "expression-temporal-season-window",
        "expression-temporal-equivalent-offsets",
        "expression-temporal-arithmetic",
    ] {
        let output = json_of(&output_of(name));
        assert_eq!(output["ok"], true, "{name}: {output}");
        assert_eq!(output["inferred"], "bool", "{name}: {output}");
    }

    for name in [
        "expression-unknown-member",
        "expression-result-mismatch",
        "expression-syntax",
        "expression-temporal-mixed-kinds",
        "expression-temporal-invalid-calendar",
    ] {
        let output = json_of(&output_of(name));
        assert_eq!(output["ok"], false, "{name}: {output}");
        let first = &output["diagnostics"][0];
        assert!(
            first["code"]
                .as_str()
                .is_some_and(|code| code.starts_with("expr.")),
            "{name}: {output}"
        );
    }
}

#[test]
fn version_names_the_formats() {
    let version = json_of(&output_of("version"));
    assert_eq!(version["abi"], "norn-checker-abi/1");
    assert_eq!(version["diagnosticsFormat"], "norn-checker/diagnostics@1");
    assert_eq!(version["receiptFormat"], "norn-checker/receipt@2");
    assert_eq!(version["exprProfile"], "norn-zen/6");
}

#[test]
fn runtime_values_match_the_pinned_golden() {
    // The norn-ir golden document (#2770): same canonical bytes and hash.
    let output = json_of(&output_of("runtime-value-golden"));
    let golden = read("crates/norn-ir/tests/golden/runtime-decimals.json");
    let golden: Value = serde_json::from_str(&golden).expect("golden");
    assert_eq!(output["canonical"], golden["hashed"]["norn1Canonical"]);
    assert_eq!(output["sha256"], golden["hashed"]["norn1Sha256"]);
    // Out of range is an input error, never a rounding.
    assert!(output_of("runtime-value-out-of-range").starts_with("error: "));
}

#[test]
fn function_contracts_match_the_pinned_golden() {
    // The norn-ir golden identities (T1-FN-1, #2776): the same digests
    // natively here and, through the parity run, on wasm32.
    let golden: Value =
        serde_json::from_str(&read("fixtures/function/golden.json")).expect("golden");
    for profile in ["wasm", "container", "effectful"] {
        let output = json_of(&output_of(&format!("function-{profile}")));
        let pinned = &golden[profile];
        assert_eq!(output["admitted"], true, "{profile}: {output}");
        assert_eq!(
            output["manifest"]["digest"], pinned["manifest"],
            "{profile}"
        );
        assert_eq!(
            output["manifest"]["executionProfile"], pinned["executionProfile"],
            "{profile}"
        );
        assert_eq!(
            output["invocation"]["digest"], pinned["invocation"],
            "{profile}"
        );
        assert_eq!(
            output["invocation"]["inputIdentity"], pinned["inputIdentity"],
            "{profile}"
        );
        assert_eq!(
            output["invocation"]["dependencyToken"], pinned["dependencyToken"],
            "{profile}"
        );
        assert_eq!(
            output["invocation"]["actionKey"], pinned["actionKey"],
            "{profile}"
        );
        assert_eq!(output["receipt"]["digest"], pinned["receipt"], "{profile}");
        assert_eq!(
            output["receipt"]["resultIdentity"], pinned["resultIdentity"],
            "{profile}"
        );
    }
}

#[test]
fn function_contract_findings_are_located_by_record() {
    let codes = |name: &str| -> Vec<(String, String)> {
        let output = json_of(&output_of(name));
        assert_eq!(output["admitted"], false, "{name}: {output}");
        output["diagnostics"]["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .map(|finding| {
                (
                    finding["code"].as_str().expect("code").to_owned(),
                    finding["span"]["pointer"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    };
    assert!(
        codes("function-bare-uri-code").contains(&(
            "function.manifest.mutable_identity".to_owned(),
            "/manifest/implementation/digest".to_owned()
        )),
        "a bare URI is not a code identity"
    );
    assert!(
        codes("function-secret-input").contains(&(
            "function.invocation.secret_input".to_owned(),
            "/invocation/inputs/apiKey".to_owned()
        )),
        "a secret is not a semantic input"
    );
    assert!(
        codes("function-credential-locator").contains(&(
            "function.manifest.embedded_secret".to_owned(),
            "/manifest/implementation/locator".to_owned()
        )),
        "a locator never carries a credential"
    );
    assert!(
        output_of("function-receipt-without-invocation").starts_with("error: invalid receiptJson"),
        "a receipt is never admitted without its invocation"
    );
    assert!(
        codes("function-receipt-wrong-dispatch")
            .iter()
            .any(|(code, _)| code == "function.invocation.manifest_mismatch"),
        "the effectful command does not name the wasm manifest"
    );
}

#[test]
fn widget_parity_corpus_preserves_keys_selections_and_display_values() {
    for case in cases() {
        if let Some(expected) = case["expectedValue"].as_str() {
            let name = case["name"].as_str().expect("case name");
            let output = output_of(name);
            assert!(
                output.contains(&format!("\"value\":{expected}")),
                "{name}: {output}"
            );
        }
        if let Some(expected) = case["expectedError"].as_str() {
            let name = case["name"].as_str().expect("case name");
            let output = json_of(&output_of(name));
            assert_eq!(output["ok"], false, "{name}");
            assert_eq!(output["error"]["code"], expected, "{name}");
        }
    }
}
