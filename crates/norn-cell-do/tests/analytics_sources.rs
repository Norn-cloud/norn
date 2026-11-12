//! D102 / HOST-READ-5: the sources pack's own types, written by the cell's
//! connection reports (observations through the report mapping, run after
//! run), profile on the engine exactly as the cell profiles them: a property
//! named like an engine alias (`label`) is just a column.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    missing_docs,
    reason = "engine correspondence over reported connections"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_api::sources::{UploadRun, report_upload};
use norn_cell::analytics::engine::{Engine, Settings};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, InstalledDefinitionInput, Model};
use norn_cell_do::olap::InProcess;
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::server;
use norn_checker::{
    EntityArtifactSetContext,
    receipt::{ReceiptSources, check_artifact_set_with_receipt},
};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
use serde_json::{Value, json};

const MEMBER: Option<&str> = Some("Bearer fixture:viewer@demo");
const SCOPE: &str = "orgId=org_demo&environmentId=dev";
const AT_MS: u64 = 1_794_484_800_000;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn artifact(path: &str) -> ArtifactEnvelope {
    serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect(path)).expect(path)
}

/// The three-systems catalogue with the sources pack, installed.
fn host() -> (DevHost<NativeEngine>, FixturePrincipals) {
    let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(
        &std::fs::read_to_string(
            root().join("fixtures/demo/explorer/three-systems.definitions.json"),
        )
        .expect("retained catalog"),
    )
    .expect("artifacts");
    for name in [
        "source.Synced.interface",
        "source.Connection.entity",
        "source.SyncRun.entity",
        "norn.connect.source",
        "norn.connect.mapping",
    ] {
        artifacts.push(artifact(&format!(
            "registry/packs/sources/artifacts/{name}.json"
        )));
    }
    let (base, candidate): (Vec<_>, Vec<_>) = artifacts
        .iter()
        .cloned()
        .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
    let values = |set: &[ArtifactEnvelope]| -> Vec<RuntimeValue> {
        set.iter()
            .map(|a| {
                RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("value")
            })
            .collect()
    };
    let sources = ReceiptSources::new(&values(&candidate), &values(&base)).expect("sources");
    let checked = check_artifact_set_with_receipt(
        &sources,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("checked");
    assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
    let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot = serde_json::from_value(
            serde_json::to_value(artifact.payload.to_json_value()).expect("payload"),
        )
        .expect("entity root");
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            EntityRoot::V2Markings(_) | EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                panic!("{} is outside this profile", artifact.artifact_id)
            }
        }
    }
    let model = Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .expect("model")
        .with_governing_laws(&artifacts)
        .expect("governing laws");
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(
            RuntimeValue::from_json(serde_json::to_value(&artifacts).expect("set"))
                .expect("value")
                .canonical_json()
                .expect("canonical"),
        )
        .expect("UTF-8"),
        registry: vec![],
        receipt_sha256: checked.receipt.content_sha256().expect("receipt digest"),
        admission_receipt: Some(
            String::from_utf8(checked.receipt.canonical_json().expect("receipt")).expect("UTF-8"),
        ),
        readmission: None,
        diff: json!({}),
        installed_at: 1,
        layout: None,
    };
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store
        .install_definitions(model, input, Some(0), &scope.key())
        .expect("installed");
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |_| BTreeMap::new())
        .expect("host")
        .with_authorizer(policy);
    (host, principals)
}

fn profile_of(response: &server::Response) -> Value {
    assert_eq!(response.status, 200, "{}", response.body);
    let mut document: Value = serde_json::from_str(&response.body).unwrap();
    let object = document.as_object_mut().unwrap();
    object.remove("engine");
    object.remove("scanLimit");
    document
}

#[test]
fn reported_connections_and_runs_profile_on_the_engine_as_in_the_cell() {
    let (host, principals) = host();
    for (run, (read, landed, quarantined)) in [(4u64, 3u64, 1u64), (9, 9, 0), (2, 0, 2)]
        .into_iter()
        .enumerate()
    {
        let started_ms = AT_MS + 60_000 * u64::try_from(run).unwrap();
        for id in ["upload.items", "upload.sales"] {
            report_upload(
                &host,
                &UploadRun {
                    id,
                    label: "items.csv",
                    uri: "s3://uploads/demo/items.csv",
                    sha256: &"ab".repeat(32),
                    mapping: "",
                    actor: "viewer@demo",
                    started_ms,
                    ended_ms: started_ms + 5,
                    counts: (read, landed, quarantined),
                    reasons: BTreeMap::from([("bulk.type".to_owned(), quarantined)]),
                },
            );
        }
    }
    assert!(
        host.host_object("source.Connection", "upload.items")
            .expect("read")
            .is_some(),
        "the report landed"
    );
    let dir = tempfile::tempdir().unwrap();
    let transport = InProcess(
        Engine::new(Settings {
            root: dir.path().to_path_buf(),
            memory_limit: "256MB".into(),
            threads: 1,
        })
        .unwrap(),
    );
    for name in ["source.Connection", "source.SyncRun"] {
        let path = format!("/v1/objects/{name}:profile");
        let request = server::Request {
            method: "GET",
            path: &path,
            query: SCOPE,
            authorization: MEMBER,
            now: 0,
            body: "",
        };
        let exact = server::handle(&host, &principals, &request);
        let engine = server::analytics(&host, &principals, &transport, None, &request)
            .expect("an engine route");
        assert_eq!(profile_of(&engine), profile_of(&exact), "{name}");
    }
    // A property named as a row's own column, or one named twice, is refused
    // before the engine is asked.
    for columns in ["id", "version", "label,label", "nope"] {
        let path = "/v1/objects/source.Connection:export";
        let query = format!("{SCOPE}&columns={columns}&format=jsonl");
        let request = server::Request {
            method: "GET",
            path,
            query: &query,
            authorization: MEMBER,
            now: 0,
            body: "",
        };
        let refused = server::analytics(&host, &principals, &transport, None, &request)
            .expect("an engine route");
        assert_eq!(refused.status, 400, "{columns}: {}", refused.body);
    }
}
