//! HOST-IDENTITY, HOST-WIRE, HOST-EXACT and HOST-RECEIPT correspondence.
use norn_api::{defs::Admitted, install::defs_response};
use norn_checker::{
    EntityArtifactSetContext, check_artifact_set_with_receipt, diagnostics_to_json,
    prepare_artifact_source,
};
use norn_ir::{LayoutSidecar, ResourceLimitProfile, RuntimeValue};
use std::collections::BTreeSet;

fn checked(number: &str, reverse_keys: bool) -> Result<Admitted, Box<dyn std::error::Error>> {
    let text = format!(
        r#"{{"irVersion":"0.1","artifactId":"test.setting","kind":"setting","namespace":"test","name":"Setting","version":"0.1.0","lifecycle":"draft","scope":{{"orgId":"org_demo","environmentId":"dev"}},"dependencies":[],"provenance":{{"source":"authoring_sdk"}},"payload":{{"settingVersion":"0.1","settingKey":"test.value","valueType":"number","value":{number},"effectiveFrom":"2026-01-01T00:00:00Z","review":{{"state":"approved","reviewedBy":"reviewer","reviewedAt":"2026-01-01T00:00:00Z","reason":"synthetic"}},"scopeRequired":["orgId","environmentId"]}}}}"#
    );
    let mut source = RuntimeValue::from_json_text(&text)?;
    if reverse_keys {
        let reordered = text.replacen(
            "\"irVersion\":\"0.1\",\"artifactId\":\"test.setting\"",
            "\"artifactId\":\"test.setting\",\"irVersion\":\"0.1\"",
            1,
        );
        source = RuntimeValue::from_json_text(&reordered)?;
    }
    let prepared =
        prepare_artifact_source(source).map_err(|diagnostics| format!("{diagnostics:?}"))?;
    let artifacts: Vec<norn_ir::ArtifactEnvelope> = vec![prepared.artifact];
    let registry = BTreeSet::new();
    let result = norn_checker::receipt::check_artifact_sources_with_receipt(
        std::slice::from_ref(&prepared.source),
        &[],
        &registry,
        None,
        &ResourceLimitProfile::norn_limits_v1(),
        None,
    )?;
    assert!(result.receipt.admitted, "{:?}", result.diagnostics);
    Ok(Admitted {
        artifacts,
        canonical_set: String::from_utf8(
            RuntimeValue::Array(vec![prepared.source]).canonical_json()?,
        )?,
        registry: vec![],
        receipt: serde_json::from_slice(&result.receipt.canonical_json()?)?,
        receipt_sha256: result.receipt.content_sha256()?,
        diagnostics: diagnostics_to_json(&result.diagnostics),
        layout: LayoutSidecar::default(),
        layout_diagnostics: vec![],
    })
}

#[test]
fn host_exact_literals_survive_definition_projection() {
    for number in [
        "9007199254740993",
        "0.1234567890123456789012345678",
        "79228162514264337593543950335",
        "0.0000000000000000000000000001",
    ] {
        let admitted = checked(number, false).unwrap();
        let response = defs_response(&admitted, 7, 3).unwrap();
        assert_eq!(
            response.artifacts.first().unwrap().get(),
            admitted
                .canonical_set
                .trim_start_matches('[')
                .trim_end_matches(']')
        );
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains(&format!(r#""value":{number}"#)), "{json}");
        assert_eq!(response.receipt, admitted.receipt);
        assert_eq!(response.receipt_sha256, admitted.receipt_sha256);
    }
    assert!(RuntimeValue::from_json_text("0.00000000000000000000000000001").is_err());
    assert!(RuntimeValue::from_json_text("79228162514264337593543950336").is_err());
}

#[test]
fn host_receipt_keeps_checker_identity_and_binds_semantic_changes() {
    let first = checked("25", false).unwrap();
    let reordered = checked("25", true).unwrap();
    let changed = checked("26", false).unwrap();
    assert_eq!(first.receipt_sha256, reordered.receipt_sha256);
    assert_ne!(first.receipt_sha256, changed.receipt_sha256);
    let mut malformed = first.clone();
    malformed.receipt = serde_json::json!({});
    assert_eq!(
        defs_response(&malformed, 1, 0).unwrap_err().code,
        "host.error"
    );
    let mut malformed = first;
    malformed.canonical_set = "[".into();
    assert_eq!(
        defs_response(&malformed, 1, 0).unwrap_err().code,
        "host.error"
    );
}

#[test]
fn host_definition_wire_keeps_empty_collections_and_revision_names() {
    let mut admitted = checked("25", false).unwrap();
    admitted.canonical_set = "[]".into();
    admitted.artifacts.clear();
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let checked = check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&[], &[]).unwrap(),
        &context,
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .unwrap();
    assert!(checked.receipt.admitted);
    admitted.receipt = serde_json::from_slice(&checked.receipt.canonical_json().unwrap()).unwrap();
    admitted.receipt_sha256 = checked.receipt.content_sha256().unwrap();
    admitted.diagnostics = diagnostics_to_json(&checked.diagnostics);
    admitted.layout_diagnostics.clear();
    let response = defs_response(&admitted, 2, 9).unwrap();
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(wire["admitted"], true);
    assert_eq!(wire["version"], 2);
    assert_eq!(wire["layoutRevision"], 9);
    for key in ["registry", "baseArtifactIds", "artifacts"] {
        assert_eq!(wire[key], serde_json::json!([]));
    }
    assert!(wire.get("flowSourceMap").is_none());
    assert!(wire.get("receiptSha256").is_some());
    let _: norn_ir::LayoutSidecar = response.layout;
}

#[test]
fn host_admission_flag_requires_an_explicit_true_receipt() {
    for receipt in [
        serde_json::json!({"artifactSet": {"base": []}}),
        serde_json::json!({"admitted": false, "artifactSet": {"base": []}}),
        serde_json::json!({"admitted": null, "artifactSet": {"base": []}}),
        serde_json::json!({"admitted": "true", "artifactSet": {"base": []}}),
    ] {
        let mut admitted = checked("25", false).unwrap();
        admitted.receipt = receipt;
        let refusal = defs_response(&admitted, 1, 0).unwrap_err();
        assert_eq!(refusal.code, "host.error");
        assert_eq!(refusal.status, 500);
        assert_eq!(
            refusal.explanation,
            norn_api::refusal::Refusal::UNPROVEN_DETAIL
        );
    }
}

#[test]
fn host_receipt_exact_integer_and_decimal_conformance() {
    for (left, right) in [
        ("9007199254740992", "9007199254740993"),
        (
            "0.1234567890123456789012345678",
            "0.1234567890123456789012345679",
        ),
    ] {
        let first = checked(left, false).unwrap();
        let second = checked(right, false).unwrap();
        assert_ne!(first.canonical_set, second.canonical_set);
        let first = defs_response(&first, 1, 0).unwrap();
        let second = defs_response(&second, 2, 0).unwrap();
        assert_ne!(first.receipt_sha256, second.receipt_sha256);
    }
}

#[test]
fn host_receipt_projects_the_checker_flow_source_map() {
    let mut admitted = checked("25", false).unwrap();
    let sources = RuntimeValue::from_json_text(&admitted.canonical_set).unwrap();
    let map = norn_ir::FlowSourceMap {
        format: norn_ir::FlowSourceMapFormat::V1,
        flows: vec![],
    };
    let checked = norn_checker::receipt::check_artifact_sources_with_receipt(
        sources.as_array().unwrap(),
        &[],
        &BTreeSet::new(),
        None,
        &ResourceLimitProfile::norn_limits_v1(),
        Some(&map),
    )
    .unwrap();
    assert!(checked.receipt.admitted);
    admitted.receipt = serde_json::from_slice(&checked.receipt.canonical_json().unwrap()).unwrap();
    admitted.receipt_sha256 = checked.receipt.content_sha256().unwrap();
    let response = defs_response(&admitted, 1, 0).unwrap();
    assert_eq!(response.flow_source_map.as_ref(), Some(&map));
    let wire = serde_json::to_value(response).unwrap();
    assert_eq!(wire["flowSourceMap"], admitted.receipt["flowSourceMap"]);
}
