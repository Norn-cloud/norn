//! PRIVACY-1/2: marked properties leave the server projected, and a reveal
//! is a governed action with a ledger receipt.
//!
//! A cell declares `pii.contact` as a `pseudonym` of `Customer` and keeps
//! customer emails under it. A viewer sees a stable pseudonym that never
//! equals the email, the same for equal emails (so filters, counts and joins
//! still hold), different in another cell; an auditor whose policy allows
//! `norn.reveal` sees the field as revealable and reveals it with a purpose,
//! which commits a receipt before the value is returned.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "privacy correspondence"
)]

use std::collections::{BTreeMap, BTreeSet};

use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::object_query::ObjectQueryParams;
use norn_api::object_routes::{ObjectAccess, get_object_query, get_queried_object};
use norn_api::privacy::{NornMaskMode, NornRevealRequest, reveal};
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, ObjectRecord};
use norn_ir::RuntimeValue as V;
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
};
use serde_json::{Value as Json, json};

const TYPE: &str = "shop.Customer";
const PARTY: &str = "canon.Party";
const SECRET: [u8; 32] = [42; 32];

fn physical(org: &str) -> CellScope {
    CellScope {
        org_id: org.into(),
        environment_id: "dev".into(),
    }
}
fn scope(org: &str) -> ScopeQuery {
    ScopeQuery {
        org_id: Some(org.into()),
        environment_id: Some("dev".into()),
    }
}

/// Everyone reads every row; only `auditor` may reveal `pii.contact`.
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, _: &ReadRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn reveal(&self, actor: &str, marking: &str) -> bool {
        actor == "auditor" && marking == "pii.contact"
    }
}

struct Mapping;
impl ObjectAccess for Mapping {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<PdpRequest, Refusal> {
        let key = ScopeKey {
            org: scope.org_id.clone(),
            environment: Some(scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        Ok(PdpRequest {
            principal: PrincipalInput {
                subject_id: actor.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
                attributes: BTreeMap::new(),
            },
            scope: Some(key.clone()),
            capability: format!("fixture.{capability:?}"),
            action_id: "norn.read".into(),
            resource: ResourceInput {
                object_type: row
                    .map_or(object_type, |row| row.object_type.as_str())
                    .into(),
                id: row.map_or(object_type, |row| row.id.as_str()).into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }
}

fn customer(key: &str, name: &str, email: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: TYPE.into(),
        key: key.into(),
        values: BTreeMap::from([
            ("name".to_owned(), V::from(name)),
            ("email".to_owned(), V::from(email)),
        ]),
    }
}

/// A cell at `org` whose `pii.contact` marking projects as `mask`.
fn host(
    org: &str,
    mask: &Json,
    secret: Option<&[u8]>,
) -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let object: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": TYPE,
        "properties": [
            {"id": "name", "type": {"kind": "text"}},
            {"id": "email", "type": {"kind": "text"}, "markings": ["pii.contact"]}
        ]
    }))
    .unwrap();
    let model = norn_cell::Model::from_definitions(std::slice::from_ref(&object), &[]).unwrap();
    let artifacts = json!([
        {"irVersion":"0.1","artifactId":"shop.Customer.entity","kind":"entity","namespace":"shop","name":"Customer","version":"0.1.0","lifecycle":"draft","scope":{"orgId":org,"environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","objectType":object}},
        {"irVersion":"0.1","artifactId":"shop.markings.entity","kind":"entity","namespace":"shop","name":"Markings","version":"0.1.0","lifecycle":"draft","scope":{"orgId":org,"environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","markings":[{"id":"pii.contact","label":"Contact","severity":"high","category":"pii.contact","mask":mask}]}}
    ]);
    let key = physical(org).key();
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .install_definitions(
                model.clone(),
                norn_cell::InstalledDefinitionInput {
                    canonical_set: serde_json::to_string(&artifacts).unwrap(),
                    registry: vec![],
                    receipt_sha256: "privacy".into(),
                    admission_receipt: None,
                    readmission: None,
                    diff: json!({}),
                    installed_at: 1,
                    layout: None,
                },
                Some(0),
                &key,
            )
            .unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: key,
                logical_time: 2,
                writes: vec![
                    customer("c1", "Ada", "ada@example.com"),
                    customer("c2", "Ada L.", "ada@example.com"),
                    customer("c3", "Grace", "grace@example.com"),
                ],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let host = DevHost::from_store(store, physical(org), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy)
        .with_pseudonym_secret(secret);
    (host, directory)
}

fn pseudonym() -> Json {
    json!({"kind": "pseudonym", "subject": "Customer"})
}

fn email(host: &DevHost<NativeEngine>, org: &str, actor: &str, id: &str) -> (V, Json) {
    let record = get_queried_object(host, &scope(org), Some(actor), TYPE, id, &Mapping).unwrap();
    let document = serde_json::to_value(&record).unwrap();
    (
        record
            .values
            .unwrap()
            .get("email")
            .cloned()
            .unwrap_or(V::Null),
        document,
    )
}

#[test]
fn a_viewer_reads_stable_pseudonyms_that_keep_joins_and_counts() {
    let (cell, _dir) = host("org_a", &pseudonym(), Some(&SECRET));
    let (first, document) = email(&cell, "org_a", "viewer", "c1");
    let text = first.as_str().unwrap().to_owned();
    assert!(text.starts_with("Customer #"), "{text}");
    assert!(!text.contains("ada"), "the value never leaves: {text}");
    assert_eq!(document["masked"]["email"]["mode"], "pseudonym");
    assert_eq!(document["masked"]["email"]["revealable"], false);
    assert_eq!(
        document["masked"]["email"]["markings"],
        json!(["pii.contact"])
    );
    assert_eq!(
        document["values"]["name"], "Ada",
        "unmarked values are untouched"
    );
    // Stable across reads, equal for equal values, distinct for others.
    assert_eq!(email(&cell, "org_a", "viewer", "c1").0, first);
    assert_eq!(email(&cell, "org_a", "viewer", "c2").0, first);
    assert_ne!(email(&cell, "org_a", "viewer", "c3").0, first);
    // A grid query: filters and counts work on the pseudonym, never the value.
    let query = |filter: Option<Json>| {
        get_object_query(
            &cell,
            &scope("org_a"),
            Some("viewer"),
            TYPE,
            &ObjectQueryParams {
                filter: filter.map(|filter| filter.to_string()),
                ..ObjectQueryParams::default()
            },
            &Mapping,
        )
        .unwrap()
    };
    let joined = query(Some(
        json!({"all": [{"property": "email", "op": "eq", "value": text}]}),
    ));
    assert_eq!(joined.total.unwrap().count, 2);
    let raw = query(Some(
        json!({"all": [{"property": "email", "op": "eq", "value": "ada@example.com"}]}),
    ));
    assert_eq!(raw.total.unwrap().count, 0, "the stored value is no oracle");
    let all = query(None);
    let distinct: BTreeSet<String> = all
        .records
        .iter()
        .map(|record| {
            record.values.as_ref().unwrap()["email"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(distinct.len(), 2);
    // Another cell keys its pseudonyms differently.
    let (other, _other_dir) = host("org_b", &pseudonym(), Some(&SECRET));
    assert_ne!(email(&other, "org_b", "viewer", "c1").0, first);
}

#[test]
fn an_auditor_reveals_with_a_receipt_and_a_viewer_is_refused() {
    let (cell, _dir) = host("org_a", &pseudonym(), Some(&SECRET));
    let (projected, document) = email(&cell, "org_a", "auditor", "c1");
    assert_eq!(document["masked"]["email"]["revealable"], true);
    assert_ne!(
        projected,
        V::from("ada@example.com"),
        "revealable is not revealed"
    );
    let request = NornRevealRequest {
        property: "email".into(),
        purpose: Some("refund dispute #12".into()),
        idempotency_key: "k1".into(),
    };
    let revealed = reveal(
        &cell,
        &scope("org_a"),
        Some("auditor"),
        (TYPE, "c1"),
        &request,
        &Mapping,
    )
    .unwrap();
    assert_eq!(revealed.value, V::from("ada@example.com"));
    let receipt = &revealed.receipt;
    assert_eq!(
        (
            receipt.actor.as_str(),
            receipt.property.as_str(),
            receipt.purpose.as_str()
        ),
        ("auditor", "email", "refund dispute #12")
    );
    assert_eq!(receipt.markings, vec!["pii.contact".to_owned()]);
    assert!(!receipt.replayed);
    // A retry replays the same receipt; the key cannot be reused for another reveal.
    let again = reveal(
        &cell,
        &scope("org_a"),
        Some("auditor"),
        (TYPE, "c1"),
        &request,
        &Mapping,
    )
    .unwrap();
    assert!(again.receipt.replayed);
    assert_eq!(again.receipt.commit_seq, receipt.commit_seq);
    let other = reveal(
        &cell,
        &scope("org_a"),
        Some("auditor"),
        (TYPE, "c3"),
        &request,
        &Mapping,
    )
    .unwrap_err();
    assert_eq!(other.code, "reveal.idempotency_conflict");
    // A viewer may not reveal; an unmarked property is not revealable at all.
    let denied = reveal(
        &cell,
        &scope("org_a"),
        Some("viewer"),
        (TYPE, "c1"),
        &request,
        &Mapping,
    )
    .unwrap_err();
    assert_eq!(denied.status, http::StatusCode::FORBIDDEN);
    let unmarked = NornRevealRequest {
        property: "name".into(),
        ..request
    };
    let absent = reveal(
        &cell,
        &scope("org_a"),
        Some("auditor"),
        (TYPE, "c1"),
        &unmarked,
        &Mapping,
    )
    .unwrap_err();
    assert_eq!(absent.status, http::StatusCode::NOT_FOUND);
}

#[test]
fn every_mode_is_applied_on_the_server_and_an_unkeyed_pseudonym_redacts() {
    let (refused, _a) = host("org_a", &json!({"kind": "refuse"}), Some(&SECRET));
    let record = get_queried_object(
        &refused,
        &scope("org_a"),
        Some("viewer"),
        TYPE,
        "c1",
        &Mapping,
    )
    .unwrap();
    assert_eq!(record.withheld_properties, vec!["email".to_owned()]);
    assert!(!record.values.unwrap().contains_key("email"));
    assert_eq!(record.masked["email"].mode, NornMaskMode::Refuse);

    let (redacted, _b) = host("org_a", &json!({"kind": "redacted"}), Some(&SECRET));
    assert_eq!(email(&redacted, "org_a", "viewer", "c1").0, V::from("•••"));

    let (last, _c) = host(
        "org_a",
        &json!({"kind": "last-n", "count": 4}),
        Some(&SECRET),
    );
    assert_eq!(email(&last, "org_a", "viewer", "c1").0, V::from("•••.com"));

    let (unkeyed, _d) = host("org_a", &pseudonym(), None);
    let (value, document) = email(&unkeyed, "org_a", "viewer", "c1");
    assert_eq!(value, V::from("•••"));
    assert_eq!(document["masked"]["email"]["mode"], "redacted");
}

/// The documents Studio's privacy walk replays (`fixtures/demo/privacy`), as
/// this server writes them for a Party-shaped cell: a viewer's grid page, an
/// auditor's, and an auditor's reveal with its receipt. `NORN_BLESS=1`
/// rewrites them; review the diff.
const PARTY_ROWS: [(&str, &str, &str); 6] = [
    ("demo/party/0001", "Amira Haddad", "person"),
    ("demo/party/0002", "Nile Gold Traders", "organization"),
    ("demo/party/0003", "Amira Haddad", "person"),
    ("demo/party/0004", "Karim Mansour", "person"),
    ("demo/party/0005", "Delta Refinery", "organization"),
    ("demo/party/0006", "Salma Farouk", "person"),
];

/// The Party-shaped cell the retained documents are read from.
fn party_cell() -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let object: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": PARTY,
        "properties": [
            {"id": "display_name", "type": {"kind": "text"}, "markings": ["pii.contact"]},
            {"id": "kind", "type": {"kind": "enum", "values": [{"value": "person"}, {"value": "organization"}, {"value": "system"}]}}
        ]
    }))
    .unwrap();
    let model = norn_cell::Model::from_definitions(std::slice::from_ref(&object), &[]).unwrap();
    let artifacts = json!([
        {"irVersion":"0.1","artifactId":"canon.Party.entity","kind":"entity","namespace":"canon","name":"Party","version":"0.1.0","lifecycle":"draft","scope":{"orgId":"org_demo","environmentId":"analytics"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","objectType":object}},
        {"irVersion":"0.1","artifactId":"canon.markings.entity","kind":"entity","namespace":"canon","name":"Markings","version":"0.1.0","lifecycle":"draft","scope":{"orgId":"org_demo","environmentId":"analytics"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","markings":[{"id":"pii.contact","label":"Contact","severity":"high","category":"pii.contact","mask":pseudonym()}]}}
    ]);
    let physical = CellScope {
        org_id: "org_demo".into(),
        environment_id: "analytics".into(),
    };
    let rows = PARTY_ROWS;
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .install_definitions(
                model.clone(),
                norn_cell::InstalledDefinitionInput {
                    canonical_set: serde_json::to_string(&artifacts).unwrap(),
                    registry: vec![],
                    receipt_sha256: "privacy-demo".into(),
                    admission_receipt: None,
                    readmission: None,
                    diff: json!({}),
                    installed_at: 1,
                    layout: None,
                },
                Some(0),
                &physical.key(),
            )
            .unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: physical.key(),
                logical_time: 1_793_620_800_000,
                writes: rows
                    .iter()
                    .map(|(key, name, kind)| FactWriteRow::Put {
                        object_type: PARTY.into(),
                        key: (*key).into(),
                        values: BTreeMap::from([
                            ("display_name".to_owned(), V::from(*name)),
                            ("kind".to_owned(), V::from(*kind)),
                        ]),
                    })
                    .collect(),
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let cell = DevHost::from_store(store, physical, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy)
        .with_pseudonym_secret(Some(&SECRET));
    (cell, directory)
}

fn retained(name: &str, document: &dyn erased::Doc) {
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/demo/privacy");
    let mut text = document.text();
    text.push('\n');
    let file = root.join(name);
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&file, &text).unwrap();
    }
    let kept = std::fs::read_to_string(&file).expect("retained document; bless with NORN_BLESS=1");
    assert!(
        kept == text,
        "{name} differs from the cell's output; review it and rerun with NORN_BLESS=1"
    );
}

#[test]
fn retained_privacy_documents_are_the_servers_own() {
    let (cell, _dir) = party_cell();
    let rows = PARTY_ROWS;
    let scope = ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("analytics".into()),
    };
    let page = |actor: &str| {
        get_object_query(
            &cell,
            &scope,
            Some(actor),
            PARTY,
            &ObjectQueryParams::default(),
            &Mapping,
        )
        .unwrap()
    };
    let viewer = page("viewer");
    let auditor = page("auditor");
    for record in &viewer.records {
        let name = record.values.as_ref().unwrap()["display_name"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(name.starts_with("Customer #"), "{name}");
        assert!(rows.iter().all(|(_, raw, _)| !name.contains(raw)), "{name}");
    }
    // The auditor policy here reveals pii.contact; the walk's auditor role.
    let revealed = reveal(
        &cell,
        &scope,
        Some("auditor"),
        (PARTY, "demo/party/0001"),
        &NornRevealRequest {
            property: "display_name".into(),
            purpose: Some("KYC review for refund #4471".into()),
            idempotency_key: "demo-reveal-1".into(),
        },
        &Mapping,
    )
    .unwrap();
    assert_eq!(revealed.value, V::from("Amira Haddad"));
    retained("canon.Party.viewer.page.json", &viewer);
    retained("canon.Party.auditor.page.json", &auditor);
    retained("canon.Party.reveal.json", &revealed);
}

/// The guard's documents for the dashboard assistant's walk, as retained.
#[test]
fn retained_guard_documents_are_the_servers_own() {
    let (cell, _dir) = party_cell();
    let scope = ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("analytics".into()),
    };
    // PRIVACY-GUARD-1: the dashboard assistant's prompt, guarded, and the
    // model's reply restored for its author (who may reveal) and for a viewer.
    let asked = "Show weekly refunds for Amira Haddad: she was charged twice on card 4539 1488 0343 6467. Reach her at amira.haddad@fastmail.com or +20 100 555 0132.";
    let guarded = norn_api::privacy_guard::guard(
        &cell,
        &scope,
        Some("auditor"),
        &norn_api::privacy_guard::NornGuardRequest {
            text: asked.into(),
            context: vec![],
            purpose: Some("dashboard-assistant".into()),
            idempotency_key: "demo-guard-1".into(),
        },
        &Mapping,
    )
    .unwrap();
    for raw in ["Amira", "4539", "fastmail", "555"] {
        assert!(
            !guarded.safe_prompt.contains(raw),
            "{}",
            guarded.safe_prompt
        );
    }
    let reply = "Weekly refunds for [CUSTOMER_1]: the duplicate charge on [CARD_1] shows in the October 24 week. The catalog holds no contact fields, so [EMAIL_1] and [PHONE_1] are not used.";
    let restore_for = |actor: &str, text: &str, key: &str| {
        norn_api::privacy_guard::restore(
            &cell,
            &scope,
            Some(actor),
            guarded.guard_id,
            &norn_api::privacy_guard::NornRestoreRequest {
                text: text.into(),
                idempotency_key: key.into(),
            },
            &Mapping,
        )
        .unwrap()
    };
    let reply_auditor = restore_for("auditor", reply, "demo-reply-auditor");
    let reply_viewer = restore_for("viewer", reply, "demo-reply-viewer");
    let original = restore_for("auditor", &guarded.safe_prompt, "demo-original");
    assert!(reply_auditor.complete && !reply_viewer.complete);
    retained("guard.prompt.json", &guarded);
    retained("guard.reply.auditor.json", &reply_auditor);
    retained("guard.reply.viewer.json", &reply_viewer);
    retained("guard.original.auditor.json", &original);
}

mod erased {
    /// A document retained as pretty JSON.
    pub(crate) trait Doc {
        fn text(&self) -> String;
    }
    impl<T: serde::Serialize> Doc for T {
        fn text(&self) -> String {
            serde_json::to_string_pretty(self).expect("document JSON")
        }
    }
}

#[test]
fn the_guard_tokenizes_marked_fields_and_detected_identifiers_and_restores_through_the_pdp() {
    use norn_api::privacy_guard::{NornGuardRequest, NornRestoreRequest, guard, restore};
    let (cell, _dir) = host("org_a", &pseudonym(), Some(&SECRET));
    let text = "Refund the duplicate charge for ada@example.com on card 4539 1488 0343 6467; call +1 415 555 0132.";
    let guarded = guard(
        &cell,
        &scope("org_a"),
        Some("auditor"),
        &NornGuardRequest {
            text: text.into(),
            context: vec![norn_api::privacy_guard::NornGuardRef {
                object_type: TYPE.into(),
                id: "c3".into(),
            }],
            purpose: Some("test".into()),
            idempotency_key: "g1".into(),
        },
        &Mapping,
    )
    .unwrap();
    let safe = &guarded.safe_prompt;
    for raw in ["ada@example.com", "4539", "415 555", "grace@example.com"] {
        assert!(!safe.contains(raw), "{raw} reached the model: {safe}");
    }
    assert!(
        safe.contains("[CUSTOMER_1]") && safe.contains("[CUSTOMER_2]"),
        "{safe}"
    );
    assert!(
        safe.contains("[CARD_1]") && safe.contains("[PHONE_1]"),
        "{safe}"
    );
    assert!(
        safe.contains("- name: Grace"),
        "unmarked context stays: {safe}"
    );
    let document = serde_json::to_string(&guarded).unwrap();
    assert!(
        !document.contains("example.com") && !document.contains("4539"),
        "{document}"
    );
    // The reply restores for the auditor (a logged reveal) and the author's own literals.
    let reply = "Refunded [CARD_1] for [CUSTOMER_2]; we will call [PHONE_1].";
    let restore_as = |actor: &str, key: &str| {
        restore(
            &cell,
            &scope("org_a"),
            Some(actor),
            guarded.guard_id,
            &NornRestoreRequest {
                text: reply.into(),
                idempotency_key: key.into(),
            },
            &Mapping,
        )
        .unwrap()
    };
    let auditor = restore_as("auditor", "r1");
    let joined: String = auditor.segments.iter().map(|s| s.text.as_str()).collect();
    assert!(
        joined.contains("ada@example.com") && joined.contains("4539 1488 0343 6467"),
        "{joined}"
    );
    assert!(auditor.complete);
    assert_eq!(
        auditor.reveals.len(),
        1,
        "one logged reveal for the one field"
    );
    // A viewer reads the pseudonym and keeps the placeholders it did not type.
    let viewer = restore_as("viewer", "r2");
    let joined: String = viewer.segments.iter().map(|s| s.text.as_str()).collect();
    assert!(
        joined.contains("Customer #") && joined.contains("[CARD_1]"),
        "{joined}"
    );
    assert!(!joined.contains("ada@example.com"), "{joined}");
    assert!(viewer.reveals.is_empty() && !viewer.complete);
}

/// The `pitch` cell's set: the three-systems catalog with the privacy pack
/// (its marking catalog and reveal policy) and its `canon.Party` overlay in
/// place of the canon artifact. It admits as one set, and the canon pack
/// alone stays unmarked.
#[test]
fn the_privacy_pack_admits_over_the_three_systems_set() {
    use norn_checker::{
        EntityArtifactSetContext,
        receipt::{ReceiptSources, check_artifact_set_with_receipt},
    };
    use norn_ir::{ArtifactEnvelope, ResourceLimitProfile};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &str| -> ArtifactEnvelope {
        serde_json::from_str(&std::fs::read_to_string(root.join(path)).unwrap()).unwrap()
    };
    let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("fixtures/demo/explorer/three-systems.definitions.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let canon = artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == "canon.Party.entity")
        .unwrap();
    assert!(
        !serde_json::to_string(canon)
            .unwrap()
            .contains("pii.contact")
    );
    for artifact in &mut artifacts {
        if artifact.artifact_id == "canon.Party.entity" {
            *artifact = read("registry/privacy/overlays/canon.Party.entity.json");
        }
    }
    artifacts.push(read(
        "registry/privacy/artifacts/privacy.markings.entity.json",
    ));
    artifacts.push(read(
        "registry/privacy/artifacts/privacy.reveal.policy.json",
    ));
    let (base, candidate): (Vec<_>, Vec<_>) = artifacts
        .iter()
        .cloned()
        .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
    let values = |set: &[ArtifactEnvelope]| -> Vec<V> {
        set.iter()
            .map(|a| V::from_json(serde_json::to_value(a).unwrap()).unwrap())
            .collect()
    };
    let sources = ReceiptSources::new(&values(&candidate), &values(&base)).unwrap();
    let checked = check_artifact_set_with_receipt(
        &sources,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .unwrap();
    assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
}
