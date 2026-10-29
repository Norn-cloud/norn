#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "fixture-based integration test"
)]

//! Complete entitlement snapshots refuse excessive joins atomically.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::mapping::{SourceDocument, compile_mapping_artifact};
use norn_ir::ArtifactEnvelope;
use norn_ir::provenance::UtcTimestamp;
use serde_json::Value;

// Independent synthetic schema and mapping: no vendor pack or registry release.
fn envelope(id: &str, kind: &str, payload: Value) -> ArtifactEnvelope {
    let mut document = serde_json::json!({
        "irVersion":"0.1", "artifactId":id, "kind":kind, "namespace":"toy",
        "name":id, "version":"0.1.0", "lifecycle":"draft",
        "scope":{"orgId":"org_demo","environmentId":"dev"}, "dependencies":[],
        "provenance":{"source":"system"}
    });
    document["payload"] = payload;
    serde_json::from_value(document).expect("synthetic typed envelope")
}

fn compiled_mapping() -> norn_checker::mapping::CompiledMapping {
    compile_mapping_with_edit(|_| {}).expect("synthetic mapping compiles")
}

fn mapping_fixture() -> (Value, Vec<ArtifactEnvelope>) {
    let datasets = ["Permissions", "Assignments", "Principals", "Restrictions"];
    let fields: Vec<_> = [
        ("Permissions", &["name", "role", "modified", "read"][..]),
        ("Assignments", &["name", "role", "parent", "modified"][..]),
        ("Principals", &["name", "modified"][..]),
        ("Restrictions", &["name", "user", "modified"][..]),
    ].into_iter().flat_map(|(dataset, names)| names.iter().enumerate().map(move |(index, name)| {
        serde_json::json!({"dataset":dataset,"name":name,"ordinal":index,"nativeType":"text", "typeCandidate":{"type":"string","origin":"declared"},"required":true})
    })).collect();
    let source = envelope(
        "toy.permissions.source",
        "source_schema",
        serde_json::json!({
            "format":"norn.source-schema/1", "source":{"system":"Toy","instance":"synthetic"},
            "observedAt":"2026-10-01T00:00:00Z", "adapter":{"name":"toy","version":"0.1.0"},
            "rawMetadata":{"uri":"evidence://synthetic/permissions","sha256":"a".repeat(64)},
            "datasets":datasets.map(|id| serde_json::json!({"id":id,"nativeName":id,"kind":"table"})), "fields":fields
        }),
    );
    let core: ArtifactEnvelope = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.Entitlement.entity.json"
    ))
    .expect("core entitlement");
    let target = envelope(
        "toy.Target.entity",
        "entity",
        serde_json::json!({"entityVersion":"2.0","objectType":{"name":"toy.Target","implements":["toy.TargetInterface"],"properties":[{"id":"id","type":{"kind":"text"}}]}}),
    );
    let interface = envelope(
        "toy.Target.interface",
        "entity",
        serde_json::json!({"entityVersion":"2.0","interface":{"name":"toy.TargetInterface","properties":[{"id":"id","type":{"kind":"text"}}]}}),
    );
    let principal = envelope(
        "party.Person.entity",
        "entity",
        serde_json::json!({"entityVersion":"2.0","objectType":{"name":"party.Person","properties":[{"id":"id","type":{"kind":"text"}}]}}),
    );
    let payload = serde_json::json!({
        "format":"norn.mapping/1", "source":{"schemaId":source.artifact_id,"version":source.version,"sha256":norn_checker::ArtifactRef::of(&source).expect("source hash").sha256},
        "interfaces":["toy.Target.interface"], "maps":[{"id":"object","source":{"dataset":"Permissions"},"target":{"entity":"toy.Target.entity","interface":"toy.Target.interface"},"when":"false","subject":"input.name","sourceRecord":"input.name","properties":[{"property":"id","value":"input.name"}]}], "identityBindings":[{"sourceKey":"principal-source-001","objectType":"party.Person","key":"person-001","evidence":"synthetic explicit binding"}],
        "entitlements":[{"id":"permission_read","sourceAlias":"dp","source":{"dataset":"Permissions"},
            "joins":[{"alias":"role","dataset":"Assignments","mode":"inner","on":[{"left":{"alias":"dp","field":"role"},"right":"role"}],"recordId":"name","version":"modified"},{"alias":"restriction","dataset":"Restrictions","mode":"anti","on":[{"left":{"alias":"role","field":"parent"},"right":"user"}]}],
            "requiredSnapshot":datasets,"targetType":"toy.Target", "grantee":{"kind":"principal","objectType":"party.Person","key":{"alias":"role","field":"parent"}},
            "eligibility":["input.dp_read == '1'"],"actions":[{"action":"toy.read","when":"true"}],"condition":{"kind":"none"},
            "sourceRecord":"name","sourceVersion":"modified","coverage":{"status":"exact","reason":"independent synthetic bounded-join test"},"status":"shadow"}]
    });
    (payload, vec![source, target, interface, principal, core])
}

fn compile_mapping_with_edit(
    edit: impl FnOnce(&mut Value),
) -> Result<norn_checker::mapping::CompiledMapping, Vec<norn_checker::Diagnostic>> {
    let (mut payload, mut artifacts) = mapping_fixture();
    edit(&mut payload);
    let mapping = envelope("toy.permissions.mapping", "mapping", payload);
    artifacts.push(mapping.clone());
    compile_mapping_artifact(&mapping, &artifacts, &[])
}

fn small_permission_snapshot() -> (Vec<SourceDocument>, BTreeSet<String>) {
    let documents = [
        ("Permissions", serde_json::json!({"name":"DP-READ","role":"reader","modified":"1","read":"1"})),
        ("Assignments", serde_json::json!({"name":"HR-001","role":"reader","parent":"principal-source-001","modified":"1"})),
        ("Principals", serde_json::json!({"name":"principal-source-001","modified":"1"})),
    ].map(|(dataset, record)| SourceDocument::from_json(dataset, &record.to_string(), None).expect("synthetic source row"));
    (
        documents.to_vec(),
        ["Permissions", "Assignments", "Principals", "Restrictions"]
            .map(str::to_owned)
            .into_iter()
            .collect(),
    )
}

fn owned_entitlement_keys(
    records: &[norn_ir::mapping::MappedRecord],
) -> BTreeMap<String, BTreeSet<String>> {
    let mut owned = BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| record.object_type.as_str() == "core.Entitlement")
    {
        owned
            .entry(record.map.clone())
            .or_insert_with(BTreeSet::new)
            .insert(record.key.clone());
    }
    owned
}

fn set_source_field(document: &mut SourceDocument, field: &str, value: Value) {
    let mut record = source_record_json(&document.record);
    record[field] = value;
    document.record =
        norn_canonical::parse(serde_json::to_string(&record).expect("source record re-encodes"))
            .expect("edited source record remains canonical JSON");
}

fn source_record_json(record: &norn_canonical::JsonValue) -> Value {
    let bytes = norn_canonical::serialize(record).expect("source record serializes canonically");
    serde_json::from_slice(&bytes).expect("canonical source record is JSON")
}

#[test]
fn high_fanout_permission_snapshot_is_refused_without_partial_output() {
    let compiled = compiled_mapping();
    let (small_documents, complete) = small_permission_snapshot();
    let recorded_at = UtcTimestamp("2026-10-02T00:00:00Z".to_owned());
    let previous = compiled
        .apply_complete_snapshot(&small_documents, &complete, &BTreeMap::new(), &recorded_at)
        .expect("small complete snapshot remains supported");
    let previously_owned = owned_entitlement_keys(&previous.records);
    assert!(!previously_owned.is_empty());

    let role_row = small_documents
        .iter()
        .find(|document| document.dataset == "Assignments")
        .expect("small snapshot has one role assignment")
        .clone();
    let mut high_fanout = small_documents;
    high_fanout.extend((0..4_100).map(|_| role_row.clone()));

    let result =
        compiled.apply_complete_snapshot(&high_fanout, &complete, &previously_owned, &recorded_at);
    let diagnostics = result.expect_err("high-fanout joins exceed the bounded row budget");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].code.as_str(),
        "mapping.runtime.evaluation_failed"
    );
    assert!(diagnostics[0].message.contains("intermediate-row budget"));
    // The pure API returns only Err here, so callers receive no partial object
    // upserts and no reconciliation deletes from the attempted snapshot.
}

#[test]
fn unrelated_documents_do_not_consume_the_entitlement_join_comparison_budget() {
    let compiled = compiled_mapping();
    let (small_documents, complete) = small_permission_snapshot();
    let noise = SourceDocument::from_json("Unrelated", "null", None)
        .expect("unrelated canonical noise document parses");
    let mut documents = small_documents;
    documents.extend(std::iter::repeat_n(noise, 4_096));

    compiled
        .apply_complete_snapshot(
            &documents,
            &complete,
            &BTreeMap::new(),
            &UtcTimestamp("2026-10-02T00:00:00Z".to_owned()),
        )
        .expect("unrelated input rows are indexed without being scanned for join matches");
}

#[test]
fn unmatched_join_keys_do_not_consume_comparison_budget_or_change_snapshot_output() {
    let compiled = compiled_mapping();
    let (small_documents, complete) = small_permission_snapshot();
    let primary = small_documents
        .iter()
        .find(|document| document.dataset == "Permissions")
        .expect("small snapshot has one permission row")
        .clone();
    let recorded_at = UtcTimestamp("2026-10-02T00:00:00Z".to_owned());
    let expected = compiled
        .apply_complete_snapshot(&small_documents, &complete, &BTreeMap::new(), &recorded_at)
        .expect("small complete snapshot maps");
    let mut documents = small_documents;
    let unmatched_permission = SourceDocument::from_json(
        "Restrictions",
        r#"{"name":"UP-UNMATCHED","user":"another-user","allow":"Customer","for_value":"CUST-OTHER","apply_to_all_doctypes":0,"is_default":0,"applicable_for":"Sales Order","modified":"2026-10-01T00:00:00Z"}"#,
        None,
    )
    .expect("unmatched permission row conforms to the fixture shape");
    documents.extend(std::iter::repeat_n(unmatched_permission, 4_000));
    for index in 0..25 {
        let mut extra = primary.clone();
        set_source_field(
            &mut extra,
            "name",
            serde_json::json!(format!("DP-BUDGET-{index}")),
        );
        set_source_field(&mut extra, "read", serde_json::json!("0"));
        documents.push(extra);
    }

    let actual = compiled
        .apply_complete_snapshot(&documents, &complete, &BTreeMap::new(), &recorded_at)
        .expect("same-dataset but key-unmatched candidates are not join work");
    assert_eq!(actual, expected);
}

#[test]
fn shared_join_key_index_entry_limit_is_enforced_separately() {
    let compiled = compile_mapping_with_edit(|payload| {
        let map = payload["entitlements"]
            .as_array_mut()
            .expect("permission maps are declared")
            .iter_mut()
            .find(|map| map["id"] == "permission_read")
            .expect("sales order map exists");
        map["joins"]
            .as_array_mut()
            .expect("joins are declared")
            .push(serde_json::json!({
                "alias": "permission_by_name",
                "dataset": "Restrictions",
                "mode": "inner",
                "cardinality": "atMostOne",
                "on": [{"left": {"alias": "dp", "field": "name"}, "right": "name"}],
                "recordId": "name",
                "version": "modified"
            }));
    })
    .expect("extra bounded join index compiles");
    let (mut documents, complete) = small_permission_snapshot();
    let permission = SourceDocument::from_json(
        "Restrictions",
        r#"{"name":"UP-INDEX","user":"another-user","allow":"Customer","for_value":"CUST-OTHER","apply_to_all_doctypes":0,"is_default":0,"applicable_for":"Sales Order","modified":"2026-10-01T00:00:00Z"}"#,
        None,
    )
    .expect("permission row parses");
    documents.extend(std::iter::repeat_n(permission, 50_001));

    let diagnostics = compiled
        .apply_complete_snapshot(
            &documents,
            &complete,
            &BTreeMap::new(),
            &UtcTimestamp("2026-10-02T00:00:00Z".to_owned()),
        )
        .expect_err("the distinct per-dataset key indexes share one entry cap");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].code.as_str(),
        "mapping.runtime.evaluation_failed"
    );
    assert!(
        diagnostics[0]
            .message
            .contains("join-key index entry budget")
    );
}

#[test]
fn total_input_document_limit_is_separate_from_join_comparison_budget() {
    let compiled = compiled_mapping();
    let (small_documents, complete) = small_permission_snapshot();
    let noise = SourceDocument::from_json("Unrelated", "null", None)
        .expect("unrelated canonical noise document parses");
    let mut documents = small_documents;
    documents.extend(std::iter::repeat_n(noise, 100_001));

    let diagnostics = compiled
        .apply_complete_snapshot(
            &documents,
            &complete,
            &BTreeMap::new(),
            &UtcTimestamp("2026-10-02T00:00:00Z".to_owned()),
        )
        .expect_err("the total document index has its own input bound");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].code.as_str(),
        "mapping.runtime.evaluation_failed"
    );
    assert!(diagnostics[0].message.contains("input document budget"));
}
