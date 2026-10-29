#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! `MappingIR` checks (T8-MAP-1a, #2600) through the artifact-set checker.
//!
//! The set is a test slice of Norn Core (`tests/fixtures/mapping/model/`),
//! the Frappe `SourceSchemaIR` fixture as a `source_schema` artifact, and
//! the `ERPNext` mapping payload fixture
//! (`crates/norn-ir/tests/fixtures/mapping/`) as a `mapping` artifact bound
//! to that schema by hash. The set checks clean, and every invalid payload
//! fixture reports the code its file name starts with
//! (`<validation|model>.<detail>.<case>.json` → `mapping.<validation|model>.<detail>`).
//! The compiled mapping (T8-MAP-1b, #2601) yields actions that pass the
//! artifact-set check, coverage for every map, and, applied to `ERPNext`
//! documents, action invocations, link patches, and provenance.

use std::collections::BTreeSet;
use std::{fs, path::Path};

use norn_checker::lint::coverage::{MemberRef, MemberStatus};
use norn_checker::mapping::{
    CompiledMapping, MappingModel, SourceDocument, compile_mapping_artifact, input_fields,
    lint_observed_deviations, mapping_key,
};
use norn_checker::source_schema::decode_source_schema;
use norn_checker::{ArtifactRef, Diagnostic, EntityArtifactSetContext, check_artifact_set};
use norn_ir::mapping::SnapshotDeviationReason;
use norn_ir::provenance::{InstanceProvenance, MappingVersion, SourceRecordRef, UtcTimestamp};
use norn_ir::source_schema::SourceSchemaIr;
use norn_ir::{
    ArtifactEnvelope, DataQualityFinding, DataQualityFindingStatus, DataQualityLineage,
    DataQualityOutcome, DataQualityRuleRef, DataQualitySeverity, DataQualitySubject,
    DiagnosticSeverity, LinkId, ObjectTypeName, PropertyId, ResourceLimitProfile, RuntimeValue,
};
use serde_json::{Value, json};

const SOURCE_ID: &str = "erpnext.source_schema";
const MAPPING_ID: &str = "erpnext.norn_core";
const FASHION_SOURCE_ID: &str = "erpnext.fashion_source_schema";
const FASHION_MAPPING_ID: &str = "erpnext.fashion_item.mapping";

fn ir_fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../norn-ir/tests/fixtures")
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} should be JSON: {error}", path.display()))
}

fn frappe() -> Value {
    read_json(&ir_fixtures().join("source_schema/frappe.json"))
}

fn source() -> SourceSchemaIr {
    decode_source_schema(&frappe())
        .unwrap_or_else(|findings| panic!("the Frappe schema should check clean: {findings:#?}"))
}

fn envelope(artifact_id: &str, kind: &str, payload: &Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": kind,
        "namespace": "erpnext",
        "name": artifact_id,
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "import" },
        "payload": payload
    }))
    .expect("the envelope should decode leniently")
}

fn source_artifact() -> ArtifactEnvelope {
    envelope(SOURCE_ID, "source_schema", &frappe())
}

fn check_records_golden(records: &[norn_ir::mapping::MappedRecord], name: &str) -> RuntimeValue {
    let rendered = norn_ir::runtime::encode(records).expect("records encode exactly");
    let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mapping")
        .join(name);
    if std::env::var_os("NORN_CHECKER_UPDATE_GOLDENS").is_some() {
        let text = serde_json::to_string_pretty(records).expect("records serialize exactly");
        fs::write(&golden, format!("{text}\n")).expect("write records golden");
    }
    let expected = RuntimeValue::from_json_text(
        fs::read_to_string(golden).expect("records golden is readable"),
    )
    .expect("records golden decodes");
    assert_eq!(rendered, expected);
    rendered
}

fn fashion_source_payload() -> Value {
    read_json(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mapping/toy-variant.source-schema.json"),
    )
}

fn fashion_source_artifact(payload: &Value) -> ArtifactEnvelope {
    envelope(FASHION_SOURCE_ID, "source_schema", payload)
}

fn fashion_mapping_payload() -> Value {
    read_json(&ir_fixtures().join("mapping/fashion-size-color.mapping.json"))
}

fn fashion_mapping_artifact(mut payload: Value, source_payload: &Value) -> ArtifactEnvelope {
    let source = fashion_source_artifact(source_payload);
    let sha256 = ArtifactRef::of(&source)
        .expect("the Fashion source artifact should hash")
        .sha256;
    *payload
        .pointer_mut("/source/sha256")
        .expect("Fashion mapping fixture declares a source hash") = json!(sha256);
    let mut artifact = envelope(FASHION_MAPPING_ID, "mapping", &payload);
    artifact.dependencies = model_artifacts()
        .iter()
        .chain([&source])
        .map(|dependency| norn_ir::ArtifactDependency {
            artifact_id: dependency.artifact_id.clone(),
            kind: dependency.kind(),
            version: Some(dependency.version.clone()),
            required: true,
        })
        .collect();
    artifact
}

fn fashion_set_with(mapping: &ArtifactEnvelope, source_payload: &Value) -> Vec<ArtifactEnvelope> {
    let mut artifacts = model_artifacts();
    artifacts.push(fashion_source_artifact(source_payload));
    artifacts.push(mapping.clone());
    artifacts
}

fn fashion_mapping_findings(payload: Value, source_payload: &Value) -> Vec<Diagnostic> {
    let mapping = fashion_mapping_artifact(payload, source_payload);
    let artifacts = fashion_set_with(&mapping, source_payload);
    let registry = BTreeSet::new();
    check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    )
}

fn assert_fashion_mapping_finding(
    payload: Value,
    source_payload: &Value,
    expected_code: &str,
    message: &str,
) {
    let findings = fashion_mapping_findings(payload, source_payload);
    assert!(
        findings.iter().any(|finding| finding.code == expected_code),
        "{message}: {findings:#?}"
    );
}

fn compiled_fashion() -> CompiledMapping {
    let source_payload = fashion_source_payload();
    let mapping = fashion_mapping_artifact(fashion_mapping_payload(), &source_payload);
    compile_mapping_artifact(&mapping, &fashion_set_with(&mapping, &source_payload), &[])
        .unwrap_or_else(|findings| panic!("the Fashion mapping should compile: {findings:#?}"))
}

fn model_artifacts() -> Vec<ArtifactEnvelope> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mapping/model");
    let mut paths: Vec<_> = fs::read_dir(&dir)
        .expect("model fixtures should be listable")
        .map(|entry| entry.expect("entry should be readable").path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            serde_json::from_value(read_json(path))
                .unwrap_or_else(|error| panic!("{} should decode: {error}", path.display()))
        })
        .collect()
}

/// The mapping artifact of a payload fixture, bound to the source
/// artifact's real hash unless `keep_hash`.
fn mapping_artifact(mut payload: Value, keep_hash: bool) -> ArtifactEnvelope {
    if !keep_hash {
        let sha256 = ArtifactRef::of(&source_artifact())
            .expect("the source artifact should hash")
            .sha256;
        *payload
            .pointer_mut("/source/sha256")
            .expect("mapping fixtures declare a source hash") = json!(sha256);
    }
    let mut artifact = envelope(MAPPING_ID, "mapping", &payload);
    artifact.dependencies = model_artifacts()
        .iter()
        .chain([&source_artifact()])
        .map(|dependency| norn_ir::ArtifactDependency {
            artifact_id: dependency.artifact_id.clone(),
            kind: dependency.kind(),
            version: Some(dependency.version.clone()),
            required: true,
        })
        .collect();
    artifact
}

fn check(mapping: ArtifactEnvelope) -> Vec<Diagnostic> {
    let mut artifacts = model_artifacts();
    artifacts.push(source_artifact());
    artifacts.push(mapping);
    check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
}

#[test]
fn model_and_source_check_clean_on_their_own() {
    let mut artifacts = model_artifacts();
    artifacts.push(source_artifact());
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(findings.is_empty(), "{findings:#?}");
    assert!(MappingModel::from_artifacts(&model_artifacts()).is_ok());
}

#[test]
fn erpnext_mapping_checks_clean_in_the_artifact_set() {
    let mapping = mapping_artifact(
        read_json(&ir_fixtures().join("mapping/erpnext.mapping.json")),
        false,
    );
    assert_eq!(mapping_key(&mapping), "erpnext.norn_core@0.1.0");
    let findings = check(mapping);
    assert!(
        findings.is_empty(),
        "the set should check clean: {findings:#?}"
    );
}

#[test]
fn input_record_types_follow_the_source_schema() {
    let fields = input_fields(&source(), "Customer").expect("Customer is declared");
    let rendered: Vec<String> = fields
        .iter()
        .map(|(name, ty)| format!("{name}: {ty:?}"))
        .collect();
    assert_eq!(fields.len(), 4, "{rendered:#?}");
    assert!(
        rendered
            .iter()
            .any(|field| field.starts_with("customer_type: Enum")),
        "a declared code set types its field as an enum: {rendered:#?}"
    );
    assert!(
        rendered
            .iter()
            .any(|field| field.starts_with("custom_loyalty_tier: Optional(Enum")),
        "a field the source does not require is nullable: {rendered:#?}"
    );
    assert!(input_fields(&source(), "Supplier").is_none());
}

#[test]
fn invalid_fixtures_report_their_code() {
    let mut cases = 0_u32;
    for entry in
        fs::read_dir(ir_fixtures().join("mapping/invalid")).expect("fixtures should be listable")
    {
        let path = entry.expect("entry should be readable").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("fixture names are UTF-8")
            .to_owned();
        let mut parts = name.split('.');
        let tier = parts.next().expect("a tier segment");
        let detail = parts.next().expect("a detail segment");
        let expected = format!("mapping.{tier}.{detail}");
        let mut mapping = mapping_artifact(read_json(&path), detail == "source_hash_mismatch");
        if detail == "invalid_key" {
            mapping.artifact_id = format!("{MAPPING_ID}@0");
        }
        let findings = check(mapping);
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == expected.as_str()),
            "{name} should report {expected}: {findings:#?}"
        );
        assert!(
            findings
                .iter()
                .all(|finding| finding.registration().is_some()),
            "{name}: every code is registered"
        );
        cases = cases.saturating_add(1);
    }
    assert!(cases >= 30, "every invalid fixture was exercised");
}

#[test]
fn mapping_targets_must_be_declared_dependencies() {
    let mut mapping = mapping_artifact(
        read_json(&ir_fixtures().join("mapping/erpnext.mapping.json")),
        false,
    );
    mapping
        .dependencies
        .retain(|dependency| dependency.artifact_id != "party.Customer.entity");
    let findings = check(mapping);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "mapping.model.undeclared_dependency"),
        "{findings:#?}"
    );
}

#[test]
fn composition_pack_can_contain_mapping_and_source_schema() {
    let mapping = mapping_artifact(
        read_json(&ir_fixtures().join("mapping/erpnext.mapping.json")),
        false,
    );
    let pack = envelope(
        "erpnext.mapping_pack",
        "business_pack",
        &json!({
            "packId": "erpnext.mapping_pack",
            "packVersion": "0.1.0",
            "artifacts": [
                {"artifactId": SOURCE_ID, "kind": "source_schema", "version": "0.1.0"},
                {"artifactId": MAPPING_ID, "kind": "mapping", "version": "0.1.0"}
            ]
        }),
    );
    let mut artifacts = model_artifacts();
    artifacts.extend([source_artifact(), mapping, pack]);
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(findings.is_empty(), "{findings:#?}");
}

// ---------------------------------------------------------------------------
// T8-MAP-1b (#2601): compile to actions + coverage, and apply
// ---------------------------------------------------------------------------

fn erpnext_mapping() -> ArtifactEnvelope {
    mapping_artifact(
        read_json(&ir_fixtures().join("mapping/erpnext.mapping.json")),
        false,
    )
}

fn set_with(mapping: &ArtifactEnvelope) -> Vec<ArtifactEnvelope> {
    let mut artifacts = model_artifacts();
    artifacts.push(source_artifact());
    artifacts.push(mapping.clone());
    artifacts
}

fn compiled() -> CompiledMapping {
    let mapping = erpnext_mapping();
    compile_mapping_artifact(&mapping, &set_with(&mapping), &[])
        .unwrap_or_else(|findings| panic!("the mapping should compile: {findings:#?}"))
}

fn compiled_with_marked_sales_order_link(marked_field: &str, expression: &str) -> CompiledMapping {
    let mut source_payload = frappe();
    let field = source_payload
        .get_mut("fields")
        .and_then(Value::as_array_mut)
        .expect("source fields are an array")
        .iter_mut()
        .find(|field| field["dataset"] == "Sales Order" && field["name"] == marked_field)
        .unwrap_or_else(|| panic!("Sales Order.{marked_field} exists"));
    field
        .as_object_mut()
        .expect("source field is an object")
        .insert(
            "markings".to_owned(),
            json!(["privacy.customer_association"]),
        );
    let source = envelope(SOURCE_ID, "source_schema", &source_payload);
    let marking_catalog = envelope(
        "core.markings.entity",
        "entity",
        &json!({
            "entityVersion": "2.0",
            "markings": [{
                "id": "privacy.customer_association",
                "label": "Customer association",
                "severity": "moderate",
                "category": "privacy.customer_association",
                "mask": { "kind": "redacted" }
            }]
        }),
    );
    let mut models = model_artifacts();
    models.push(marking_catalog);

    let mut mapping_payload = read_json(&ir_fixtures().join("mapping/erpnext.mapping.json"));
    *mapping_payload
        .pointer_mut("/maps/1/links/0/key")
        .expect("Sales Order link expression exists") = json!(expression);
    *mapping_payload
        .pointer_mut("/source/sha256")
        .expect("mapping source hash exists") = json!(
        ArtifactRef::of(&source)
            .expect("the marked source schema hashes")
            .sha256
    );
    let mut mapping = envelope(MAPPING_ID, "mapping", &mapping_payload);
    mapping.dependencies = models
        .iter()
        .chain([&source])
        .map(|dependency| norn_ir::ArtifactDependency {
            artifact_id: dependency.artifact_id.clone(),
            kind: dependency.kind(),
            version: Some(dependency.version.clone()),
            required: true,
        })
        .collect();

    let mut artifacts = models;
    artifacts.extend([source, mapping.clone()]);
    compile_mapping_artifact(&mapping, &artifacts, &[])
        .unwrap_or_else(|findings| panic!("the marked mapping should compile: {findings:#?}"))
}

#[test]
fn compiled_actions_pass_the_artifact_set_check() {
    let compiled = compiled();
    let ids: Vec<&str> = compiled
        .actions()
        .iter()
        .map(|action| action.action_id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "erpnext.norn_core.customer.observe",
            "erpnext.norn_core.sales_order.observe",
            "erpnext.norn_core.sales_order.status.submit",
            "erpnext.norn_core.sales_order.status.cancel",
        ]
    );
    let mut artifacts = model_artifacts();
    artifacts.extend_from_slice(compiled.action_artifacts());
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(
        findings.is_empty(),
        "compiled actions pass T2/T3: {findings:#?}"
    );
}

#[test]
fn domain_engine_targets_cannot_compile_to_generic_actions() {
    let mapping = erpnext_mapping();
    let mut artifacts = set_with(&mapping);
    let customer = artifacts
        .iter_mut()
        .find(|artifact| artifact.artifact_id == "party.Customer.entity")
        .expect("customer target exists");
    let mut value = serde_json::to_value(&*customer).expect("customer serializes");
    value["payload"]["mutationAuthority"] =
        json!({ "kind": "domain_engine", "domain": "accounting" });
    *customer = serde_json::from_value(value).expect("domain authority decodes");
    let findings = compile_mapping_artifact(&mapping, &artifacts, &[])
        .expect_err("generic observation actions cannot mutate domain-engine types");
    assert!(
        findings.iter().any(|finding| {
            finding.code == "mapping.model.unencodable_member"
                && finding
                    .message
                    .contains("artifact_set.action.target_not_writable")
        }),
        "{findings:#?}"
    );
}

#[test]
fn coverage_records_every_member_and_ignored_field() {
    let compiled = compiled();
    let coverage = compiled.coverage();
    assert_eq!(coverage.len(), 2);
    let order = coverage
        .iter()
        .find(|entry| entry.target.as_str() == "commerce.SalesOrder")
        .expect("the sales order coverage");
    assert_eq!(order.mapping_artifact_id, MAPPING_ID);
    assert_eq!(order.source.entity, "Sales Order");
    let counterparty = order
        .member(&MemberRef::Link(LinkId("counterparty".to_owned())))
        .expect("the link is covered");
    assert_eq!(
        counterparty.status,
        MemberStatus::Mapped {
            source_fields: vec!["name".to_owned(), "customer".to_owned()]
        }
    );
    assert!(order.accounts_for(&MemberRef::Property(PropertyId("status".to_owned()))));
    assert!(order.unaccounted_source_fields().is_empty(), "{order:#?}");
}

#[test]
fn an_uncovered_required_member_refuses_the_mapping() {
    let mut payload = read_json(&ir_fixtures().join("mapping/erpnext.mapping.json"));
    let properties = payload["maps"][1]["properties"]
        .as_array_mut()
        .expect("sales_order properties");
    properties.retain(|rule| rule["property"] != "order_number");
    let findings = check(mapping_artifact(payload, false));
    assert!(
        findings.iter().any(
            |finding| finding.code == "ontology.lint.unmapped_required_member"
                && finding.artifact_id.as_deref() == Some(MAPPING_ID)
        ),
        "{findings:#?}"
    );
}

fn document(dataset: &str, record: &Value) -> SourceDocument {
    SourceDocument::from_json(
        dataset,
        &serde_json::to_string(record).expect("test record serializes"),
        Some(UtcTimestamp("2026-09-30T09:00:00Z".to_owned())),
    )
    .expect("test source JSON parses exactly")
}

#[test]
fn applying_the_mapping_yields_actions_links_and_provenance() {
    let documents = [
        document(
            "Customer",
            &json!({
                "doctype": "Customer",
                "name": "CUST-0001",
                "customer_name": "  Acme Trading ",
                "customer_type": "Company",
                "custom_loyalty_tier": ""
            }),
        ),
        document(
            "Sales Order",
            &json!({
                "doctype": "Sales Order",
                "name": "SAL-ORD-0001",
                "customer": "CUST-0001",
                "transaction_date": "2026-09-29",
                "grand_total": 1250.5,
                "docstatus": 1,
                "items": [{ "name": "row-1", "parent": "SAL-ORD-0001", "item_code": "SKU-1", "qty": 2 }]
            }),
        ),
    ];
    let recorded_at = UtcTimestamp("2026-10-01T00:00:00Z".to_owned());
    let records = compiled()
        .apply(&documents, &recorded_at)
        .unwrap_or_else(|findings| panic!("the documents should map: {findings:#?}"));
    check_records_golden(&records, "compiled-records.json");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end mapping fixture exercises both member and identity provenance propagation"
)]
fn mapped_provenance_inherits_member_and_identity_source_fields() {
    let mut source_payload = frappe();
    let field = source_payload["fields"]
        .as_array_mut()
        .expect("source fields are an array")
        .iter_mut()
        .find(|field| field["dataset"] == "Customer" && field["name"] == "customer_name")
        .expect("Customer.customer_name exists");
    field["markings"] = json!(["pii.contact"]);
    let key_field = source_payload["fields"]
        .as_array_mut()
        .expect("source fields are an array")
        .iter_mut()
        .find(|field| field["dataset"] == "Customer" && field["name"] == "name")
        .expect("Customer.name exists");
    key_field["markings"] = json!(["pii.contact"]);
    let lifecycle_field = source_payload["fields"]
        .as_array_mut()
        .expect("source fields are an array")
        .iter_mut()
        .find(|field| field["dataset"] == "Sales Order" && field["name"] == "docstatus")
        .expect("Sales Order.docstatus exists");
    lifecycle_field["markings"] = json!(["pii.contact"]);
    let source = envelope(SOURCE_ID, "source_schema", &source_payload);

    let marking_catalog = envelope(
        "core.markings.entity",
        "entity",
        &json!({
            "entityVersion": "2.0",
            "markings": [{
                "id": "pii.contact",
                "label": "Personal contact information",
                "severity": "moderate",
                "category": "pii.contact",
                "mask": {"kind": "redacted"}
            }]
        }),
    );
    let mut models = model_artifacts();
    models.push(marking_catalog);

    let mut mapping_payload = read_json(&ir_fixtures().join("mapping/erpnext.mapping.json"));
    let customer_map = &mut mapping_payload["maps"][0];
    customer_map["subject"] = json!("input.customer_name");
    customer_map["sourceRecord"] = json!("input.name");
    customer_map["properties"]
        .as_array_mut()
        .expect("customer properties are an array")
        .iter_mut()
        .find(|property| property["property"] == "customer_code")
        .expect("customer code is mapped")["value"] = json!("'constant'");
    mapping_payload["source"]["sha256"] = json!(
        ArtifactRef::of(&source)
            .expect("the marked source schema hashes")
            .sha256
    );
    let mut mapping = envelope(MAPPING_ID, "mapping", &mapping_payload);
    mapping.dependencies = models
        .iter()
        .chain([&source])
        .map(|dependency| norn_ir::ArtifactDependency {
            artifact_id: dependency.artifact_id.clone(),
            kind: dependency.kind(),
            version: Some(dependency.version.clone()),
            required: true,
        })
        .collect();

    let mut artifacts = models;
    artifacts.push(source);
    artifacts.push(mapping.clone());
    let context = EntityArtifactSetContext {
        registry: &BTreeSet::new(),
        base: &[],
        tenant: None,
    };
    let findings = check_artifact_set(&artifacts, &context);
    assert!(
        findings.is_empty(),
        "the marked mapping set is valid: {findings:#?}"
    );

    let compiled = compile_mapping_artifact(&mapping, &artifacts, &[])
        .expect("marked source fields compile with the admitted mapping");
    let records = compiled
        .apply(
            &[
                document(
                    "Customer",
                    &json!({
                        "doctype": "Customer",
                        "name": "CUST-0001",
                        "customer_name": "Acme Trading",
                        "customer_type": "Company",
                        "custom_loyalty_tier": ""
                    }),
                ),
                document(
                    "Sales Order",
                    &json!({
                        "doctype": "Sales Order",
                        "name": "SAL-ORD-0001",
                        "customer": "CUST-0001",
                        "transaction_date": "2026-09-29",
                        "grand_total": 1250.5,
                        "docstatus": 1,
                        "items": [{
                            "name": "row-1",
                            "parent": "SAL-ORD-0001",
                            "item_code": "SKU-1",
                            "qty": 2
                        }]
                    }),
                ),
            ],
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect("customer source data maps");
    let customer = records
        .iter()
        .find(|record| record.map == "customer")
        .expect("the customer map emits a record");
    let display_name = customer
        .property_provenance
        .get(&norn_ir::PropertyId("display_name".to_owned()))
        .expect("display name property provenance is emitted");
    assert_eq!(display_name.source_fields, ["name", "customer_name"]);
    assert_eq!(
        display_name.markings,
        [norn_ir::MarkingId("pii.contact".to_owned())]
    );
    assert_eq!(customer.provenance.source_fields, ["name", "customer_name"]);
    assert_eq!(
        customer.provenance.markings,
        [norn_ir::MarkingId("pii.contact".to_owned())],
        "subject and source-record reads label the record identity"
    );
    let constant_code = customer
        .property_provenance
        .get(&norn_ir::PropertyId("customer_code".to_owned()))
        .expect("customer code property provenance is emitted");
    assert_eq!(constant_code.source_fields, ["name", "customer_name"]);
    assert_eq!(
        constant_code.markings,
        [norn_ir::MarkingId("pii.contact".to_owned())],
        "identity-source markings protect constant output properties too"
    );

    let sales_order = records
        .iter()
        .find(|record| record.map == "sales_order")
        .expect("the sales order map emits a record");
    let lifecycle = &sales_order.lifecycle_snapshots[0];
    assert_eq!(lifecycle.property.as_str(), "status");
    assert_eq!(lifecycle.provenance.source_fields, ["name", "docstatus"]);
    assert_eq!(
        lifecycle.provenance.markings,
        [norn_ir::MarkingId("pii.contact".to_owned())],
        "lifecycle snapshots retain labels from the source fields they read"
    );
    assert_eq!(
        sales_order
            .property_provenance
            .get(&lifecycle.property)
            .expect("lifecycle property provenance is emitted"),
        &lifecycle.provenance,
        "lifecycle snapshots and their property provenance carry identical metadata"
    );
}

#[test]
fn classified_link_rule_refuses_string_and_null_results_before_evaluation() {
    let compiled = compiled_with_marked_sales_order_link(
        "customer",
        "input.customer == 'CLEAR' ? null : input.customer",
    );
    let findings = compiled
        .apply(
            &[
                document(
                    "Sales Order",
                    &json!({
                        "doctype": "Sales Order",
                        "name": "SO-STRING",
                        "customer": "C-1",
                        "transaction_date": "2026-09-29",
                        "grand_total": 1.0,
                        "docstatus": 0,
                        "items": []
                    }),
                ),
                document(
                    "Sales Order",
                    &json!({
                        "doctype": "Sales Order",
                        "name": "SO-NULL",
                        "customer": "CLEAR",
                        "transaction_date": "2026-09-29",
                        "grand_total": 1.0,
                        "docstatus": 0,
                        "items": []
                    }),
                ),
            ],
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect_err("classified link results are not admitted");

    assert_eq!(findings.len(), 2, "both string and null rows are refused");
    assert!(findings.iter().all(|finding| {
        finding.code == "mapping.runtime.evaluation_failed"
            && finding.message.contains("classified association")
            && finding
                .message
                .contains("unsupported until durable edge classification")
    }));
}

#[test]
fn classified_link_identity_source_is_refused_before_evaluation() {
    let compiled = compiled_with_marked_sales_order_link("name", "input.customer");
    let findings = compiled
        .apply(
            &[document(
                "Sales Order",
                &json!({
                    "doctype": "Sales Order",
                    "name": "SO-IDENTITY",
                    "customer": "C-1",
                    "transaction_date": "2026-09-29",
                    "grand_total": 1.0,
                    "docstatus": 0,
                    "items": []
                }),
            )],
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect_err("identity classifications taint association rules");

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].code, "mapping.runtime.evaluation_failed");
    assert!(findings[0].message.contains("classified association"));
}

fn mark_source_field(payload: &mut Value, dataset: &str, name: &str) {
    payload
        .get_mut("fields")
        .and_then(Value::as_array_mut)
        .expect("source fields are declared")
        .iter_mut()
        .find(|field| {
            field.get("dataset") == Some(&json!(dataset)) && field.get("name") == Some(&json!(name))
        })
        .expect("the classified source field exists")
        .as_object_mut()
        .expect("source fields are objects")
        .insert("markings".to_owned(), json!(["privacy.source"]));
}

fn compiled_marked_mapping(
    source_payload: &Value,
    mut payload: Value,
    source_id: &str,
    mapping_id: &str,
) -> CompiledMapping {
    let source = envelope(source_id, "source_schema", source_payload);
    *payload
        .pointer_mut("/source/sha256")
        .expect("source hash is declared") = json!(
        ArtifactRef::of(&source)
            .expect("marked source hashes")
            .sha256
    );
    let mut artifacts = model_artifacts();
    artifacts.push(envelope(
        "core.markings.entity",
        "entity",
        &json!({
            "entityVersion": "2.0",
            "markings": [{
                "id": "privacy.source", "label": "Private source",
                "severity": "moderate", "category": "privacy.source",
                "mask": { "kind": "redacted" }
            }]
        }),
    ));
    artifacts.push(source);
    let mut mapping = envelope(mapping_id, "mapping", &payload);
    mapping.dependencies = artifacts
        .iter()
        .map(|artifact| norn_ir::ArtifactDependency {
            artifact_id: artifact.artifact_id.clone(),
            kind: artifact.kind(),
            version: Some(artifact.version.clone()),
            required: true,
        })
        .collect();
    artifacts.push(mapping.clone());
    compile_mapping_artifact(&mapping, &artifacts, &[])
        .unwrap_or_else(|findings| panic!("the marked mapping is admitted: {findings:#?}"))
}

fn fashion_marking_document() -> SourceDocument {
    document(
        "Item",
        &json!({
            "name": "STYLE-M", "item_code": "STYLE-M", "item_name": "Private Style",
            "variant_of": "STYLE", "attributes": [{
                "name": "ATTR-M", "parent": "STYLE-M", "attribute": "Size", "attribute_value": "M"
            }]
        }),
    )
}

#[test]
fn nested_property_markings_include_guard_collection_predicate_and_value() {
    let recorded_at = UtcTimestamp("2026-10-01T00:00:00Z".to_owned());
    for (dataset, field, constant) in [
        ("Item", "variant_of", false),
        ("Item", "attributes", false),
        ("Item Variant Attribute", "attribute", true),
        ("Item Variant Attribute", "attribute_value", false),
    ] {
        let mut source_payload = fashion_source_payload();
        mark_source_field(&mut source_payload, dataset, field);
        let mut payload = fashion_mapping_payload();
        if constant {
            for map in payload
                .get_mut("maps")
                .and_then(Value::as_array_mut)
                .expect("maps exist")
            {
                *map.pointer_mut("/properties/2/value")
                    .expect("size value exists") = json!("'constant'");
            }
        }
        let compiled = compiled_marked_mapping(
            &source_payload,
            payload,
            FASHION_SOURCE_ID,
            FASHION_MAPPING_ID,
        );
        let records = compiled
            .apply(&[fashion_marking_document()], &recorded_at)
            .expect("one variant maps");
        let record = records.first().expect("variant record exists");
        assert_eq!(records.len(), 1);
        assert_eq!(
            record.provenance.source_fields,
            ["name", "item_code", "variant_of"]
        );
        let expected_identity = if field == "variant_of" {
            vec![norn_ir::MarkingId("privacy.source".to_owned())]
        } else {
            Vec::new()
        };
        assert_eq!(
            record.provenance.markings, expected_identity,
            "{dataset}.{field}"
        );
        let size = record
            .property_provenance
            .get(&PropertyId("size".to_owned()))
            .expect("size provenance exists");
        let mut expected_fields = vec![
            "name",
            "item_code",
            "variant_of",
            "attributes",
            "attributes.attribute",
        ];
        if !constant {
            expected_fields.push("attributes.attribute_value");
        }
        assert_eq!(size.source_fields, expected_fields, "{dataset}.{field}");
        assert_eq!(
            size.markings,
            [norn_ir::MarkingId("privacy.source".to_owned())],
            "{dataset}.{field}"
        );
        assert_eq!(
            record
                .invocations
                .first()
                .expect("invocation exists")
                .input
                .get("size"),
            Some(&RuntimeValue::String(
                if constant { "constant" } else { "M" }.to_owned()
            ))
        );
        if field == "variant_of" {
            assert!(
                record
                    .property_provenance
                    .values()
                    .all(|provenance| provenance.markings == record.provenance.markings)
            );
            let deletes = compiled
                .apply_delete(&fashion_marking_document(), &recorded_at)
                .expect("complete guarded delete maps");
            assert_eq!(deletes.len(), 1);
            assert_eq!(
                deletes.first().expect("delete exists").provenance,
                InstanceProvenance {
                    transform: Some("fashion_item.delete".to_owned()),
                    ..record.provenance.clone()
                }
            );
        }
    }
}

#[test]
fn classified_guard_refuses_constant_and_null_associations() {
    let mut source_payload = frappe();
    mark_source_field(&mut source_payload, "Sales Order", "customer");
    for expression in ["'C-1'", "null"] {
        let mut payload = read_json(&ir_fixtures().join("mapping/erpnext.mapping.json"));
        payload
            .pointer_mut("/maps/1")
            .expect("order map exists")
            .as_object_mut()
            .expect("map is an object")
            .insert("when".to_owned(), json!("input.customer != ''"));
        *payload
            .pointer_mut("/maps/1/links/0/key")
            .expect("link key exists") = json!(expression);
        let compiled = compiled_marked_mapping(&source_payload, payload, SOURCE_ID, MAPPING_ID);
        let findings = compiled.apply(&[document("Sales Order", &json!({
            "doctype": "Sales Order", "name": "SO-PRIVATE", "customer": "C-1",
            "transaction_date": "2026-09-29", "grand_total": 1, "docstatus": 0, "items": []
        }))], &UtcTimestamp("2026-10-01T00:00:00Z".to_owned())).expect_err("guard classification protects associations");
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings.first().expect("refusal exists").code,
            "mapping.runtime.evaluation_failed"
        );
        assert!(
            findings
                .first()
                .expect("refusal exists")
                .message
                .contains("classified association")
        );
    }
}

#[test]
fn nested_map_inherits_classified_parent_collection_without_changing_child_field_names() {
    let mut source_payload = fashion_source_payload();
    mark_source_field(&mut source_payload, "Item", "attributes");
    let mut payload = fashion_mapping_payload();
    let maps = payload
        .get_mut("maps")
        .and_then(Value::as_array_mut)
        .expect("maps exist");
    maps.truncate(1);
    let map = maps
        .first_mut()
        .expect("child map exists")
        .as_object_mut()
        .expect("map is an object");
    map.remove("when");
    map.insert("source".to_owned(), json!({"dataset": "Item Variant Attribute", "nestedIn": {"dataset": "Item", "field": "attributes"}}));
    map.insert("subject".to_owned(), json!("input.name"));
    map.insert(
        "properties".to_owned(),
        json!([
            {"property": "item_code", "value": "input.name"},
            {"property": "display_name", "value": "input.attribute"},
            {"property": "size", "value": "input.attribute_value"},
            {"property": "color", "value": "null"}
        ]),
    );
    let compiled = compiled_marked_mapping(
        &source_payload,
        payload,
        FASHION_SOURCE_ID,
        FASHION_MAPPING_ID,
    );
    let records = compiled
        .apply(
            &[fashion_marking_document()],
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect("child maps");
    assert_eq!(records.len(), 1);
    let record = records.first().expect("child record exists");
    assert_eq!(record.key, "ATTR-M");
    assert_eq!(record.provenance.source_fields, ["name"]);
    assert_eq!(
        record.provenance.markings,
        [norn_ir::MarkingId("privacy.source".to_owned())]
    );
    assert!(
        record
            .property_provenance
            .values()
            .all(|provenance| provenance.markings == record.provenance.markings)
    );
    assert_eq!(
        record
            .property_provenance
            .get(&PropertyId("size".to_owned()))
            .expect("child size provenance exists")
            .source_fields,
        ["name", "attribute_value"]
    );
}

#[test]
fn fashion_size_color_pivot_filters_templates_and_preserves_field_provenance() {
    let compiled = compiled_fashion();
    let documents = [
        document(
            "Item",
            &json!({
                "name": "ONE-DRS-001",
                "item_code": "ONE-DRS-001",
                "item_name": "One Dress",
                "variant_of": null,
                "attributes": []
            }),
        ),
        document(
            "Item",
            &json!({
                "name": "ONE-DRS-001-S-BLK",
                "item_code": "ONE-DRS-001-S-BLK",
                "item_name": "  ONE-DRS-001-S-BLK  ",
                "variant_of": "ONE-DRS-001",
                "attributes": [
                    {
                        "name": "ONE-DRS-001-S-BLK-Size",
                        "parent": "ONE-DRS-001-S-BLK",
                        "attribute": "Size",
                        "attribute_value": "S"
                    },
                    {
                        "name": "ONE-DRS-001-S-BLK-Color",
                        "parent": "ONE-DRS-001-S-BLK",
                        "attribute": "Color",
                        "attribute_value": "Black"
                    }
                ]
            }),
        ),
    ];
    let recorded_at = UtcTimestamp("2026-10-01T00:00:00Z".to_owned());
    let records = compiled
        .apply(&documents, &recorded_at)
        .unwrap_or_else(|findings| panic!("Fashion rows should map: {findings:#?}"));
    assert_eq!(records.len(), 2, "opposite guards select one map per row");
    let template_record = records.first().expect("template guard output exists");
    let variant_record = records.get(1).expect("variant guard output exists");
    assert_eq!(template_record.key, "ONE-DRS-001");
    assert_eq!(variant_record.key, "ONE-DRS-001-S-BLK");
    assert_eq!(
        template_record
            .property_provenance
            .get(&PropertyId("size".to_owned()))
            .expect("template size provenance exists")
            .transform
            .as_deref(),
        Some("fashion_item_template.size")
    );
    assert_eq!(
        variant_record
            .property_provenance
            .get(&PropertyId("color".to_owned()))
            .expect("variant color provenance exists")
            .transform
            .as_deref(),
        Some("fashion_item.color")
    );
    let rendered = check_records_golden(&records, "fashion-size-color-records.json");
    assert_eq!(
        rendered,
        norn_ir::runtime::encode(
            &compiled
                .apply(&documents, &recorded_at)
                .expect("replay has identical result")
        )
        .expect("replay encodes")
    );

    let coverage = compiled
        .coverage()
        .first()
        .expect("the Fashion mapping coverage exists");
    let size = coverage
        .member(&MemberRef::Property(PropertyId("size".to_owned())))
        .expect("size is covered");
    let MemberStatus::Mapped { source_fields } = &size.status else {
        panic!("size is mapped");
    };
    for field in [
        "variant_of",
        "attributes",
        "attributes.attribute",
        "attributes.attribute_value",
    ] {
        assert!(
            source_fields.iter().any(|read| read == field),
            "{source_fields:#?}"
        );
    }
}

#[test]
fn nested_pivot_absence_clears_nullable_fields_and_duplicates_refuse() {
    let compiled = compiled_fashion();
    let recorded_at = UtcTimestamp("2026-10-01T00:00:00Z".to_owned());
    let absent = document(
        "Item",
        &json!({
            "name": "ONE-DRS-001-M-RED",
            "item_code": "ONE-DRS-001-M-RED",
            "item_name": "One Dress M Red",
            "variant_of": "ONE-DRS-001",
            "attributes": [
                {
                    "name": "ONE-DRS-001-M-RED-Color",
                    "parent": "ONE-DRS-001-M-RED",
                    "attribute": "Color",
                    "attribute_value": "Red"
                }
            ]
        }),
    );
    let output = compiled
        .apply(&[absent], &recorded_at)
        .expect("a missing size is an explicit nullable clear");
    let encoded = norn_ir::runtime::encode(&output).expect("records encode");
    assert_eq!(
        encoded.pointer("/0/invocations/0/input/size"),
        Some(&RuntimeValue::Null)
    );
    assert_eq!(
        encoded.pointer("/0/invocations/0/input/color"),
        Some(&RuntimeValue::String("Red".to_owned()))
    );
    assert!(encoded.pointer("/0/propertyProvenance/size").is_some());

    let duplicate = document(
        "Item",
        &json!({
            "name": "ONE-DRS-001-M-BLK",
            "item_code": "ONE-DRS-001-M-BLK",
            "item_name": "One Dress M Black",
            "variant_of": "ONE-DRS-001",
            "attributes": [
                {
                    "name": "size-1",
                    "parent": "ONE-DRS-001-M-BLK",
                    "attribute": "Size",
                    "attribute_value": "M"
                },
                {
                    "name": "size-2",
                    "parent": "ONE-DRS-001-M-BLK",
                    "attribute": "Size",
                    "attribute_value": "L"
                }
            ]
        }),
    );
    let findings = compiled
        .apply(&[duplicate], &recorded_at)
        .expect_err("duplicate Size rows must not silently choose the first");
    assert!(findings.iter().any(|finding| {
        finding.code == "mapping.runtime.evaluation_failed"
            && finding.message.contains("matched multiple rows")
    }));
}

#[test]
fn nested_pivot_refuses_a_collection_over_the_slice_row_limit() {
    let compiled = compiled_fashion();
    let max_rows = ResourceLimitProfile::norn_limits_v1().slice.max_records;
    let row_count = usize::try_from(u64::from(max_rows) + 1)
        .expect("the configured row limit fits the test host");
    let document = document(
        "Item",
        &json!({
            "name": "ONE-DRS-001-S-BLK",
            "item_code": "ONE-DRS-001-S-BLK",
            "item_name": "One Dress S Black",
            "variant_of": "ONE-DRS-001",
            "attributes": vec![Value::Null; row_count]
        }),
    );
    let findings = compiled
        .apply(
            &[document],
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect_err("oversized child collections refuse before row evaluation");
    assert!(findings.iter().any(|finding| {
        finding.code == "mapping.runtime.invalid_record"
            && finding.message.contains(&format!("{row_count} rows"))
            && finding
                .message
                .contains(&format!("slice limit is {max_rows}"))
    }));
}

#[test]
fn guarded_maps_skip_nullable_subjects_and_guarded_deletes_refuse_partial_records() {
    let source_payload = fashion_source_payload();
    let mut payload = fashion_mapping_payload();
    let variant_map = payload["maps"][1].clone();
    payload["maps"] = json!([variant_map]);
    payload["maps"][0]["subject"] = json!("input.variant_of != null ? input.item_code : null");
    let mapping = fashion_mapping_artifact(payload, &source_payload);
    let artifacts = fashion_set_with(&mapping, &source_payload);
    let compiled = compile_mapping_artifact(&mapping, &artifacts, &[]).unwrap_or_else(|findings| {
        panic!("the guarded nullable subject type checks: {findings:#?}")
    });
    let template = document(
        "Item",
        &json!({
            "name": "ONE-DRS-001",
            "item_code": "ONE-DRS-001",
            "item_name": "One Dress",
            "variant_of": null,
            "attributes": []
        }),
    );
    assert!(
        compiled
            .apply(
                std::slice::from_ref(&template),
                &UtcTimestamp("2026-10-01T00:00:00Z".to_owned())
            )
            .expect("the false guard skips before the nullable subject")
            .is_empty()
    );
    assert!(
        compiled
            .apply_delete(&template, &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()))
            .expect("a complete template delete evaluates its false guard")
            .is_empty(),
        "a guarded delete cannot tombstone a record from the opposite branch"
    );
    let variant_delete = document(
        "Item",
        &json!({
            "name": "ONE-DRS-001-S-BLK",
            "item_code": "ONE-DRS-001-S-BLK",
            "item_name": "One Dress S Black",
            "variant_of": "ONE-DRS-001",
            "attributes": []
        }),
    );
    let deletes = compiled
        .apply_delete(
            &variant_delete,
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect("a complete variant delete selects its branch");
    assert_eq!(deletes.len(), 1);
    assert_eq!(
        deletes.first().expect("variant delete exists").key,
        "ONE-DRS-001-S-BLK"
    );
    let partial_delete = document("Item", &json!({ "name": "ONE-DRS-001" }));
    let findings = compiled
        .apply_delete(
            &partial_delete,
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect_err("an identity-only delete cannot select a guarded branch");
    assert!(findings.iter().any(|finding| {
        finding.code == "mapping.runtime.invalid_record"
            && finding
                .message
                .contains("guarded maps require a complete record")
    }));
}

#[test]
fn guard_literal_text_is_not_a_field_read_and_bracket_fields_are_supported() {
    let source_payload = fashion_source_payload();
    let mut payload = fashion_mapping_payload();
    let variant_map = payload["maps"][1].clone();
    payload["maps"] = json!([variant_map]);
    payload["maps"][0]["when"] =
        json!("input['variant_of'] != null ? true : 'input.not_a_field' != 'input.not_a_field'");
    let mapping = fashion_mapping_artifact(payload, &source_payload);
    let compiled =
        compile_mapping_artifact(&mapping, &fashion_set_with(&mapping, &source_payload), &[])
            .unwrap_or_else(|findings| panic!("literal and bracket guard checks: {findings:#?}"));
    let records = compiled
        .apply(
            &[
                document(
                    "Item",
                    &json!({
                        "name": "ONE-DRS-001",
                        "item_code": "ONE-DRS-001",
                        "item_name": "One Dress",
                        "variant_of": null,
                        "attributes": []
                    }),
                ),
                document(
                    "Item",
                    &json!({
                        "name": "ONE-DRS-001-S-BLK",
                        "item_code": "ONE-DRS-001-S-BLK",
                        "item_name": "One Dress S Black",
                        "variant_of": "ONE-DRS-001",
                        "attributes": [
                            { "name": "size", "parent": "ONE-DRS-001-S-BLK", "attribute": "Size", "attribute_value": "S" },
                            { "name": "color", "parent": "ONE-DRS-001-S-BLK", "attribute": "Color", "attribute_value": "Black" }
                        ]
                    }),
                ),
            ],
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect("both branch outcomes evaluate");
    assert_eq!(records.len(), 1);
    let size = compiled.coverage()[0]
        .member(&MemberRef::Property(PropertyId("size".to_owned())))
        .expect("size coverage exists");
    let MemberStatus::Mapped { source_fields } = &size.status else {
        panic!("size is mapped");
    };
    assert!(source_fields.iter().any(|field| field == "variant_of"));
    assert!(!source_fields.iter().any(|field| field == "not_a_field"));
}

#[test]
fn nested_pivot_expression_and_target_type_errors_are_reported() {
    let source_payload = fashion_source_payload();
    let mut invalid = fashion_mapping_payload();
    invalid["maps"][0]["properties"][2]["nested"]["where"] = json!("input.attribute");
    assert_fashion_mapping_finding(
        invalid,
        &source_payload,
        "mapping.model.expression_type",
        "a text predicate is rejected",
    );

    let mut invalid = fashion_mapping_payload();
    invalid["maps"][0]["properties"][2]["nested"]["where"] =
        json!("input.attribute_name == 'Size'");
    assert_fashion_mapping_finding(
        invalid,
        &source_payload,
        "mapping.model.expression_type",
        "an undeclared child field is rejected",
    );

    let mut invalid = fashion_mapping_payload();
    invalid["maps"][0]["when"] = json!("input.variant_of");
    assert_fashion_mapping_finding(
        invalid,
        &source_payload,
        "mapping.model.expression_type",
        "a non-boolean map guard is rejected",
    );

    let mut invalid = fashion_mapping_payload();
    invalid["maps"][0]["properties"][2]["value"] = json!("1");
    assert_fashion_mapping_finding(
        invalid,
        &source_payload,
        "mapping.model.expression_type",
        "a pivot value with the wrong target type is rejected",
    );
}

#[test]
fn nested_pivot_relation_errors_are_reported() {
    let source_payload = fashion_source_payload();
    let mut invalid = fashion_mapping_payload();
    invalid["maps"][0]["properties"][2]["nested"]["dataset"] = json!("Item");
    assert_fashion_mapping_finding(
        invalid,
        &source_payload,
        "mapping.model.invalid_nesting",
        "the child must match a declared parent_child relation",
    );

    let mut invalid = fashion_mapping_payload();
    invalid["maps"][0]["properties"][2]["nested"]["field"] = json!("item_name");
    assert_fashion_mapping_finding(
        invalid,
        &source_payload,
        "mapping.model.invalid_nesting",
        "a scalar field cannot stand in for the declared child collection",
    );

    let mut non_json_source = source_payload.clone();
    non_json_source["fields"][4]["typeCandidate"]["type"] = json!("string");
    assert_fashion_mapping_finding(
        fashion_mapping_payload(),
        &non_json_source,
        "mapping.model.invalid_nesting",
        "the parent collection field must have JSON type",
    );

    for (normalized_type, message) in [
        ("binary", "a binary parent collection field is rejected"),
        ("unknown", "an unknown parent collection field is rejected"),
    ] {
        let mut non_json_source = source_payload.clone();
        non_json_source["fields"][4]["typeCandidate"]["type"] = json!(normalized_type);
        assert_fashion_mapping_finding(
            fashion_mapping_payload(),
            &non_json_source,
            "mapping.model.invalid_nesting",
            message,
        );
    }
}

#[test]
fn a_nested_pivot_without_a_match_refuses_a_required_target_property() {
    let source_payload = fashion_source_payload();
    let mapping = fashion_mapping_artifact(fashion_mapping_payload(), &source_payload);
    let mut artifacts = fashion_set_with(&mapping, &source_payload);
    let style = artifacts
        .iter_mut()
        .find(|artifact| artifact.artifact_id == "fashion.Style.entity")
        .expect("Fashion target exists");
    let mut style_value = serde_json::to_value(&*style).expect("Style serializes");
    let properties = style_value["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("Style properties are an array");
    properties
        .iter_mut()
        .find(|property| property["id"] == "size")
        .expect("size is declared")
        .as_object_mut()
        .expect("size property is an object")
        .remove("constraints");
    *style = serde_json::from_value(style_value).expect("required Style decodes");
    let compiled = compile_mapping_artifact(&mapping, &artifacts, &[])
        .unwrap_or_else(|findings| panic!("required target compiles: {findings:#?}"));
    let document = document(
        "Item",
        &json!({
            "name": "ONE-DRS-001-M-RED",
            "item_code": "ONE-DRS-001-M-RED",
            "item_name": "One Dress M Red",
            "variant_of": "ONE-DRS-001",
            "attributes": [
                {
                    "name": "color",
                    "parent": "ONE-DRS-001-M-RED",
                    "attribute": "Color",
                    "attribute_value": "Red"
                }
            ]
        }),
    );
    let findings = compiled
        .apply(
            &[document],
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect_err("an absent required pivot value refuses");
    assert!(findings.iter().any(|finding| {
        finding.code == "mapping.runtime.evaluation_failed"
            && finding.message.contains("no matching child row")
    }));
}

#[test]
fn lifecycle_state_expression_emits_snapshot_without_firing_transition_actions() {
    let mut payload = read_json(&ir_fixtures().join("mapping/erpnext.mapping.json"));
    let rule = &mut payload["maps"][1]["lifecycles"][0];
    rule.as_object_mut()
        .expect("lifecycle rule is an object")
        .remove("transition");
    rule["state"] = json!("input.docstatus == 2 ? 'cancelled' : null");
    let mapping = mapping_artifact(payload, false);
    let artifacts = set_with(&mapping);
    let compiled = compile_mapping_artifact(&mapping, &artifacts, &[])
        .expect("an admitted lifecycle enum state compiles");
    let records = compiled
        .apply(
            &[document(
                "Sales Order",
                &json!({
                    "name": "SAL-ORD-0002",
                    "customer": "CUST-0001",
                    "transaction_date": "2026-09-29",
                    "grand_total": 10,
                    "docstatus": 2,
                    "items": []
                }),
            )],
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect("the source record maps");
    let record = records
        .iter()
        .find(|record| record.map == "sales_order")
        .expect("the sales order map emits a record");
    assert_eq!(record.invocations.len(), 1);
    assert_eq!(
        record.invocations[0].action_id,
        "erpnext.norn_core.sales_order.observe"
    );
    assert_eq!(record.lifecycle_snapshots.len(), 1);
    assert_eq!(record.lifecycle_snapshots[0].property.as_str(), "status");
    assert_eq!(record.lifecycle_snapshots[0].state, "cancelled");
    assert_eq!(
        record.lifecycle_snapshots[0]
            .provenance
            .transform
            .as_deref(),
        Some("sales_order.status")
    );
    assert!(
        compiled
            .actions()
            .iter()
            .any(|action| { action.action_id == "erpnext.norn_core.sales_order.status.cancel" })
    );
}

#[test]
fn lifecycle_rule_requires_exactly_one_of_state_or_transition() {
    let mut both = read_json(&ir_fixtures().join("mapping/erpnext.mapping.json"));
    both["maps"][1]["lifecycles"][0]["state"] = json!("'submitted'");
    let findings = check(mapping_artifact(both, false));
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "mapping.validation.lifecycle_rule_shape"),
        "both forms are invalid: {findings:#?}"
    );

    let mut neither = read_json(&ir_fixtures().join("mapping/erpnext.mapping.json"));
    neither["maps"][1]["lifecycles"][0]
        .as_object_mut()
        .expect("lifecycle rule is an object")
        .remove("transition");
    let findings = check(mapping_artifact(neither, false));
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "mapping.validation.lifecycle_rule_shape"),
        "neither form is invalid: {findings:#?}"
    );
}

#[test]
fn lifecycle_state_expression_is_checked_against_the_lifecycle_enum() {
    let mut payload = read_json(&ir_fixtures().join("mapping/erpnext.mapping.json"));
    let rule = &mut payload["maps"][1]["lifecycles"][0];
    rule.as_object_mut()
        .expect("lifecycle rule is an object")
        .remove("transition");
    rule["state"] = json!("input.docstatus == 1 ? 'not_a_state' : null");
    let findings = check(mapping_artifact(payload, false));
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "mapping.model.expression_type"),
        "an undeclared lifecycle state is rejected: {findings:#?}"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the regression checks both lifecycle deviation outcomes and full source lineage"
)]
fn observed_lifecycle_deviations_are_advisory_and_preserve_lineage_context() {
    let provenance = |record: &str| InstanceProvenance {
        source: SourceRecordRef::new("ERPNext", record),
        mapping_version: MappingVersion("erpnext.norn_core@0.1.0".to_owned()),
        transform: Some("sales_order.status".to_owned()),
        observed_at: Some(UtcTimestamp("2026-09-30T09:00:00Z".to_owned())),
        recorded_at: UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        source_fields: Vec::new(),
        markings: Vec::new(),
        upstream: Vec::new(),
    };
    let observed_finding = |key: &str,
                            from: Option<&str>,
                            state: &str,
                            reason: SnapshotDeviationReason,
                            source_record: &str| {
        DataQualityFinding::v2(
            DataQualityRuleRef::Builtin {
                id: "norn.lifecycle.observed_out_of_band".to_owned(),
                version: "1".to_owned(),
            },
            vec![DataQualitySubject::new(
                ObjectTypeName("commerce.SalesOrder".to_owned()),
                key,
            )],
            DataQualitySeverity::Warning,
            "The observed lifecycle state has no unique shortest declared path.",
            DataQualityOutcome::ObservedOutOfBand {
                property: PropertyId("status".to_owned()),
                from: from.map(str::to_owned),
                state: state.to_owned(),
                reason,
            },
            vec![DataQualityLineage::new(
                provenance(source_record),
                vec![format!("observation-{key}")],
            )],
            UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
            UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
            DataQualityFindingStatus::Open,
        )
        .expect("the observed lifecycle finding has a stable identity")
    };
    let findings = [
        observed_finding(
            "SO-9",
            Some("submitted"),
            "draft",
            SnapshotDeviationReason::NoDeclaredPath,
            "Sales Order/SO-9",
        ),
        observed_finding(
            "SO-10",
            None,
            "cancelled",
            SnapshotDeviationReason::AmbiguousShortestPath,
            "Sales Order/SO-10",
        ),
        DataQualityFinding::v2(
            DataQualityRuleRef::Builtin {
                id: "norn.invariant.check".to_owned(),
                version: "1".to_owned(),
            },
            vec![DataQualitySubject::new(
                ObjectTypeName("commerce.SalesOrder".to_owned()),
                "SO-11",
            )],
            DataQualitySeverity::Warning,
            "An unrelated invariant was not satisfied.",
            DataQualityOutcome::NotSatisfied,
            vec![DataQualityLineage::new(
                provenance("Sales Order/SO-11"),
                vec!["observation-SO-11".to_owned()],
            )],
            UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
            UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
            DataQualityFindingStatus::Open,
        )
        .expect("the invariant finding has a stable identity"),
    ];
    let findings = lint_observed_deviations(&findings);
    assert_eq!(findings.len(), 2);
    assert!(findings.iter().all(|finding| {
        finding.code == "mapping.runtime.observed_out_of_band"
            && finding.severity == DiagnosticSeverity::Warning
            && finding
                .registration()
                .is_some_and(|entry| entry.severity == DiagnosticSeverity::Warning)
    }));
    assert!(findings[0].message.contains("ERPNext/Sales Order/SO-9"));
    assert!(
        findings[0]
            .message
            .contains("mappingVersion=erpnext.norn_core@0.1.0")
    );
    assert!(
        findings[0]
            .message
            .contains("observedAt=2026-09-30T09:00:00Z")
    );
    assert!(findings[0].message.contains("submitted -> draft"));
    assert!(findings[0].message.contains("reason=no_declared_path"));
    assert!(findings[1].message.contains("<initial> -> cancelled"));
    assert!(
        findings[1]
            .message
            .contains("reason=ambiguous_shortest_path")
    );
}

#[test]
fn a_nonconforming_record_is_a_typed_failure() {
    let documents = [document(
        "Customer",
        &json!({ "name": "CUST-0002", "customer_name": "Beta", "customer_type": "Cooperative" }),
    )];
    let findings = compiled()
        .apply(&documents, &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()))
        .expect_err("an undeclared Select value does not conform");
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "mapping.runtime.invalid_record"),
        "{findings:#?}"
    );
}

#[test]
fn source_decimal_ingress_preserves_digits_beyond_binary64() {
    let document = SourceDocument::from_json(
        "Sales Order",
        r#"{"name":"SO-EXACT","customer":"CUST-0001","transaction_date":"2026-09-29","grand_total":9007199254740993.25,"docstatus":0,"items":[]}"#,
        None,
    )
    .expect("exact source JSON parses");
    let records = compiled()
        .apply(
            &[document],
            &UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        )
        .expect("the decimal is exactly representable by the expression engine");
    let rendered = norn_ir::runtime::encode(&records).expect("records encode");
    let total = rendered
        .pointer("/0/invocations/0/input/total_amount")
        .expect("total_amount");
    assert_eq!(
        total,
        &RuntimeValue::from_json_text("9007199254740993.25").expect("decimal")
    );
    // Serialized as the exact number token, not a string.
    assert!(
        serde_json::to_string(total)
            .expect("serializes")
            .starts_with("9007199254740993.25")
    );
}

/// #3013: a lifecycle rule's `onDelete` must name a terminal state of its
/// lifecycle.
#[test]
fn on_delete_must_name_a_terminal_state() {
    let mut payload = read_json(&ir_fixtures().join("mapping/erpnext.mapping.json"));
    let lifecycle = payload["maps"][1]["lifecycles"][0]
        .as_object_mut()
        .expect("sales_order lifecycle rule");
    lifecycle.insert("onDelete".to_owned(), json!("submitted"));
    let findings = check(mapping_artifact(payload.clone(), false));
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "mapping.model.invalid_on_delete"),
        "{findings:#?}"
    );
    payload["maps"][1]["lifecycles"][0]["onDelete"] = json!("cancelled");
    let findings = check(mapping_artifact(payload, false));
    assert!(
        !findings
            .iter()
            .any(|finding| finding.code == "mapping.model.invalid_on_delete"),
        "{findings:#?}"
    );
}
