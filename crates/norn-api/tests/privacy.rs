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

// #1485 S6: fixtures write through the kernel path, never the raw door.
#[path = "../../norn-cell/tests/support/governed.rs"]
mod governed;
use governed::GovernedCommit as _;
use std::collections::{BTreeMap, BTreeSet};

use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::object_query::ObjectQueryParams;
use norn_api::object_review::get_object_history;
use norn_api::object_routes::{ObjectAccess, get_object_query, get_queried_object};
use norn_api::privacy::{NornMaskMode, NornRevealRequest, reveal};
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, ObjectRecord};
use norn_checker::entity_scopes::ScopedView;
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
    host_with(org, mask, secret, &[])
}

/// [`host`], then each of `later` committed in turn, one commit each.
fn host_with(
    org: &str,
    mask: &Json,
    secret: Option<&[u8]>,
    later: &[Vec<FactWriteRow>],
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
        for (time, writes) in (3..).zip(later) {
            store
                .commit(&CommitBatch {
                    world: MAIN_WORLD.into(),
                    scope: physical(org).key(),
                    logical_time: time,
                    writes: writes.clone(),
                    ..CommitBatch::default()
                })
                .unwrap();
        }
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

/// Replace the retained canonical set of `dir`'s cell through another
/// connection, as an out-of-process editor would; the previous set.
fn swap_retained_set(dir: &tempfile::TempDir, edit: &dyn Fn(&mut Json)) -> String {
    use norn_cell::engine::{SqlEngine as _, SqlValue};
    let raw = NativeEngine::open(
        &dir.path().join("cell.sqlite"),
        norn_cell::StoreOptions::default(),
    )
    .unwrap();
    let held = raw
        .query_row(
            "SELECT \"canonical_set\" FROM \"_norn_definition_version\" WHERE \"version\" = 1",
            &[],
            |row| row.get::<String>(0),
        )
        .unwrap();
    let mut set: Json = serde_json::from_str(&held).unwrap();
    edit(&mut set);
    // An out-of-process editor; this engine writes the retained tables only
    // inside a definition-writes scope (S1, #1547).
    raw.definition_writes(|| {
        raw.execute(
            "UPDATE \"_norn_definition_version\" SET \"canonical_set\" = ?1 WHERE \"version\" = 1",
            &[SqlValue::Text(serde_json::to_string(&set).unwrap())],
        )
    })
    .unwrap();
    held
}

#[test]
fn a_marked_field_is_never_served_clear_from_an_empty_catalog() {
    // S2 (#1547): a retained set that transiently declares no marking yields
    // no declared marking. The catalog is keyed by the foreign-write
    // revision, so the repaired set is read again; and a cell that holds
    // labels its catalog does not resolve withholds those fields, so the
    // marked email is masked or withheld, never served in the clear, both
    // while the set is broken and after it is repaired at the same
    // definitions version.
    let (cell, dir) = host("org_a", &json!({"kind": "redacted"}), Some(&SECRET));
    let clear = V::from("ada@example.com");
    let read = || get_queried_object(&cell, &scope("org_a"), Some("viewer"), TYPE, "c1", &Mapping);
    let held = swap_retained_set(&dir, &|set| {
        set.as_array_mut()
            .unwrap()
            .retain(|artifact| artifact["artifactId"] != "shop.markings.entity");
    });
    let document = serde_json::to_value(read().expect("the read is served")).unwrap();
    assert!(document["values"].get("email").is_none(), "{document}");
    assert_ne!(document["values"]["email"], json!(clear.as_str()));
    assert_eq!(document["masked"]["email"]["mode"], "refuse");
    swap_retained_set(&dir, &|set| *set = serde_json::from_str(&held).unwrap());
    assert_eq!(email(&cell, "org_a", "viewer", "c1").0, V::from("•••"));
}

/// #1519 S6: a write that changed only a withheld field is not in the
/// history: its version, commit and time would tell that a hidden value
/// changed then. Here the second version changes only the refused email.
#[test]
fn a_history_omits_a_write_that_changed_only_a_withheld_field() {
    let (host, _directory) = host_with(
        "org_a",
        &json!({"kind": "refuse"}),
        Some(&SECRET),
        &[vec![customer("c1", "Ada", "ada@new.example.com")]],
    );
    let history =
        get_object_history(&host, &scope("org_a"), Some("viewer"), TYPE, "c1", &Mapping).unwrap();
    let listed: Vec<u64> = history
        .versions
        .iter()
        .map(|version| version.version)
        .collect();
    assert_eq!(listed, [1], "{history:#?}");
    assert_eq!(
        history.versions[0]
            .values
            .as_ref()
            .and_then(|values| values.get("name")),
        Some(&V::from("Ada"))
    );
    let document = serde_json::to_string(&history).unwrap();
    for withheld in ["example.com", "\"version\":2,", "\"logicalTime\":3,"] {
        assert!(
            !document.contains(withheld),
            "{withheld} leaked: {document}"
        );
    }
}

/// The documents Studio's privacy walk replays (`fixtures/privacy`), as
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
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/privacy");
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

/// A privacy cell's set: the three-systems catalog with the privacy pack
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

/// The privacy pack as a hosted cell is provisioned with it: the registry
/// packs with the canon.Party overlay, the markings catalog and the reveal
/// policy, admitted by `admit_kernel_set` and served under `serving_model`.
/// A member's read of `canon.Party.display_name` is the pseudonym, never the
/// value, so the catalog the model skips does reach the projection.
#[test]
fn a_provisioned_privacy_set_pseudonymizes_a_party_name() {
    use norn_ir::ArtifactEnvelope;
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
    let admitted = match norn_api::kernel_set::admit_kernel_set(&artifacts) {
        Ok(admitted) => admitted,
        Err(norn_api::kernel_set::KernelRefusal::Refused(found)) => panic!("refused: {found:#?}"),
        Err(norn_api::kernel_set::KernelRefusal::Invalid(detail)) => panic!("invalid: {detail}"),
    };
    let model = norn_api::kernel_set::serving_model(&artifacts).expect("the set is served");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let physical = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .install_definitions(
                model.clone(),
                norn_cell::InstalledDefinitionInput {
                    canonical_set: admitted.canonical_set,
                    registry: vec![],
                    receipt_sha256: admitted.receipt_sha256,
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
                logical_time: 2,
                writes: vec![FactWriteRow::Put {
                    object_type: PARTY.into(),
                    key: "p1".into(),
                    values: BTreeMap::from([
                        ("display_name".to_owned(), V::from("Ada Lovelace")),
                        ("kind".to_owned(), V::from("person")),
                    ]),
                }],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let cell = DevHost::from_store(store, physical, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy)
        .with_pseudonym_secret(Some(&SECRET));
    let scope = ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    };
    let record = get_queried_object(&cell, &scope, Some("member"), PARTY, "p1", &Mapping).unwrap();
    let document = serde_json::to_value(&record).unwrap();
    let name = document["values"]["display_name"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(!name.contains("Ada"), "the value never leaves: {document}");
    assert_eq!(
        document["masked"]["display_name"]["mode"], "pseudonym",
        "{document}"
    );
    assert_eq!(
        document["masked"]["display_name"]["markings"],
        json!(["pii.contact"])
    );
}

/// PRIVACY-4 on the provisioned privacy set: a question over `canon.Holding`
/// cannot group or filter by its owner's `display_name` (marked `pii.contact`
/// by the overlay), for any caller, since the answer's buckets would carry the
/// name unmasked; grouping by the owner's unmarked `kind` still admits. This is
/// the admission `POST /v1/questions:evaluate` and saved question definitions
/// both run (`norn_checker::metric::compute::admit`, over
/// `check_metrics_and_questions`).
#[test]
fn a_question_cannot_group_or_filter_by_a_marked_party_name() {
    use norn_ir::{ArtifactEnvelope, MetricDefinition, QuestionIR};
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
    for artifact in &mut artifacts {
        if artifact.artifact_id == "canon.Party.entity" {
            *artifact = read("registry/privacy/overlays/canon.Party.entity.json");
        }
    }
    artifacts.push(read(
        "registry/privacy/artifacts/privacy.markings.entity.json",
    ));
    let catalog = ScopedView::uniform(&artifacts, &BTreeSet::new()).into_catalog();
    let metric: MetricDefinition = serde_json::from_value(json!({
        "name": "demo.holdings",
        "label": "demo.holdings",
        "type": "simple",
        "type_params": {
            "expr": "resource_code",
            "metric_aggregation_params": {
                "semantic_model": "canon.Holding",
                "agg": "count",
                "agg_time_dimension": "observed_at"
            }
        },
        "x-norn": {
            "irVersion": "0.1",
            "additivity": { "kind": "additive" },
            "timeDimension": { "property": "observed_at" },
            "owner": "demo",
            "certified": false
        }
    }))
    .unwrap();
    let question = |group: Json, filter: Json| -> QuestionIR {
        serde_json::from_value(json!({
            "irVersion": "0.1",
            "name": "adhoc.holdings_by_owner",
            "objectSet": "canon.Holding",
            "aggregate": [{ "metric": "demo.holdings" }],
            "groupBy": group,
            "filter": filter,
            "window": { "start": "2026-01-01T00:00:00Z", "end": "2026-11-01T00:00:00Z" },
            "freshness": { "kind": "live", "maxAgeMs": 5000 }
        }))
        .unwrap()
    };
    let admit = |question: &QuestionIR| {
        norn_checker::metric::compute::admit(
            std::slice::from_ref(&metric),
            question,
            &catalog,
            10_000,
        )
    };
    let refused = |question: &QuestionIR| match admit(question) {
        Ok(_) => false,
        Err(findings) => findings
            .iter()
            .any(|finding| finding.code.as_str() == "question.validation.marked_field"),
    };
    let owner = |property: &str| json!({ "links": ["owner"], "property": property });

    // The unmarked baseline passes the checker's admission, where the marking
    // rule lives (the count itself is outside the exact engine's profile).
    let by_kind = question(json!([{ "path": owner("kind") }]), json!([]));
    let findings = norn_checker::check_metrics_and_questions(
        std::slice::from_ref(&metric),
        std::slice::from_ref(&by_kind),
        &catalog,
    );
    assert_eq!(findings, [], "{findings:?}");
    assert!(refused(&question(
        json!([{ "path": owner("display_name") }]),
        json!([])
    )));
    assert!(refused(&question(
        json!([{ "path": owner("kind") }]),
        json!([{ "path": owner("display_name"), "operator": "eq", "value": "Ada" }])
    )));
}

/// HOST-INSTALL-1: a pack applied over a provisioned privacy set keeps its
/// markings. The batches fixture pack is applied through `apply_definitions` (the
/// hosted route's function); the read path takes the marking catalog from
/// the retained canonical set of the new revision, so a member's read of
/// `canon.Party.display_name` is still the pseudonym.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one provisioned cell, one apply, one read"
)]
fn an_applied_pack_keeps_the_privacy_markings() {
    use norn_api::cell_definitions::{apply_definitions, plan_definitions};
    use norn_ir::ArtifactEnvelope;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = |path: &str| std::fs::read_to_string(root.join(path)).unwrap();
    let read = |path: &str| -> ArtifactEnvelope { serde_json::from_str(&text(path)).unwrap() };
    let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(&text(
        "fixtures/demo/explorer/three-systems.definitions.json",
    ))
    .unwrap();
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
    let Ok(admitted) = norn_api::kernel_set::admit_kernel_set(&artifacts) else {
        panic!("the privacy set is admitted");
    };
    let model = norn_api::kernel_set::serving_model(&artifacts).expect("the set is served");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let physical = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .install_definitions(
                model.clone(),
                norn_cell::InstalledDefinitionInput {
                    canonical_set: admitted.canonical_set,
                    registry: vec![],
                    receipt_sha256: admitted.receipt_sha256,
                    admission_receipt: Some(admitted.receipt),
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
                logical_time: 2,
                writes: vec![FactWriteRow::Put {
                    object_type: PARTY.into(),
                    key: "p1".into(),
                    values: BTreeMap::from([
                        ("display_name".to_owned(), V::from("Ada Lovelace")),
                        ("kind".to_owned(), V::from("person")),
                    ]),
                }],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let cell = DevHost::from_store(store, physical, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy)
        .with_pseudonym_secret(Some(&SECRET));
    let scope = ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    };
    let pack: Vec<Json> = [
        "lab.Batch.entity",
        "lab.batch.access.policy",
        "lab.batch.mass_conserved.law",
        "lab.batch.record.action",
    ]
    .iter()
    .map(|id| {
        serde_json::from_str(&text(&format!(
            "fixtures/packs/batches/artifacts/{id}.json"
        )))
        .unwrap()
    })
    .collect();
    let kinds = norn_api::definition_routes::DefinitionKinds::default();
    let plan = plan_definitions(
        &cell,
        &scope,
        Some("owner"),
        &kinds,
        &json!({ "artifacts": pack }).to_string(),
    )
    .unwrap();
    let applied = apply_definitions(
        &cell,
        &scope,
        Some("owner"),
        &kinds,
        &json!({ "artifacts": pack, "expectedVersion": plan.base_version }).to_string(),
        10,
    )
    .unwrap();
    assert_eq!(applied.definitions_version, plan.next_version);
    let record = get_queried_object(&cell, &scope, Some("member"), PARTY, "p1", &Mapping).unwrap();
    let document = serde_json::to_value(&record).unwrap();
    let name = document["values"]["display_name"]
        .as_str()
        .unwrap_or_default();
    assert!(!name.contains("Ada"), "the value never leaves: {document}");
    assert_eq!(
        document["masked"]["display_name"]["markings"],
        json!(["pii.contact"]),
        "{document}"
    );
}

// #1547 round 4 (S2, S3): a cell's labels, not its declarations, decide
// whether a read projects.

const ITEM: &str = "shop.Item";

/// A cell that declares no marking: a public item, and a `norn.RuleRun`,
/// whose detail the kernel labels `norn.unknown-lineage` as it commits.
fn undeclared_cell() -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let types: Vec<norn_ir::ObjectTypeDef> = [
        json!({"name": ITEM, "properties": [{"id": "name", "type": {"kind": "text"}}]}),
        json!({"name": "norn.RuleRun", "properties": [{"id": "detail", "type": {"kind": "text"}}]}),
    ]
    .into_iter()
    .map(|object| serde_json::from_value(object).unwrap())
    .collect();
    let model = norn_cell::Model::from_definitions(&types, &[]).unwrap();
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        let put = |object_type: &str, key: &str, property: &str, value: &str| FactWriteRow::Put {
            object_type: object_type.into(),
            key: key.into(),
            values: BTreeMap::from([(property.to_owned(), V::from(value))]),
        };
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: physical("org_a").key(),
                logical_time: 2,
                writes: vec![
                    put(ITEM, "i1", "name", "Lathe"),
                    put("norn.RuleRun", "r1", "detail", "rule detail"),
                ],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let host = DevHost::from_store(store, physical("org_a"), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy)
        .with_pseudonym_secret(Some(&SECRET));
    (host, directory)
}

#[test]
fn a_kernel_label_in_a_cell_without_markings_leaves_public_reads_served() {
    // S3 (#1547): a cell with no marking declarations still holds the
    // kernel's `norn.unknown-lineage` label on a RuleRun. A public object
    // reads as itself, and the labelled detail is withheld, not served and
    // not a refused read.
    let (cell, _dir) = undeclared_cell();
    let item = get_queried_object(&cell, &scope("org_a"), Some("viewer"), ITEM, "i1", &Mapping)
        .expect("a public object is served");
    assert_eq!(item.values.unwrap().get("name"), Some(&V::from("Lathe")));
    if let Ok(run) = get_queried_object(
        &cell,
        &scope("org_a"),
        Some("viewer"),
        "norn.RuleRun",
        "r1",
        &Mapping,
    ) {
        assert_eq!(run.values.unwrap().get("detail"), None);
    }
}

#[test]
fn a_declassified_history_is_never_served_clear_from_an_empty_catalog() {
    // S2 (#1547): every current label is declassified, so only the earlier
    // version holds one, and the definitions then declare no marking. The
    // history still projects that version: its email is withheld, never
    // served in the clear.
    let (cell, dir) = host_with(
        "org_a",
        &json!({"kind": "redacted"}),
        Some(&SECRET),
        &[vec![customer("c1", "Ada B.", "ada@new.example.com")]],
    );
    {
        use norn_cell::engine::SqlEngine as _;
        // As the declassification flow leaves the current labels.
        NativeEngine::open(
            &dir.path().join("cell.sqlite"),
            norn_cell::StoreOptions::default(),
        )
        .unwrap()
        .execute(
            "UPDATE \"_norn_marking_current\" SET \"markings\" = '[]'",
            &[],
        )
        .unwrap();
    }
    swap_retained_set(&dir, &|set| {
        set.as_array_mut()
            .unwrap()
            .retain(|artifact| artifact["artifactId"] != "shop.markings.entity");
    });
    let history =
        get_object_history(&cell, &scope("org_a"), Some("viewer"), TYPE, "c1", &Mapping).unwrap();
    let document = serde_json::to_string(&history).unwrap();
    assert!(
        !document.contains("ada@example.com"),
        "the earlier version's email leaked: {document}"
    );
    let first = history
        .versions
        .iter()
        .find(|version| version.version == 1)
        .expect("the first version is listed");
    assert_eq!(
        first.values.as_ref().and_then(|values| values.get("name")),
        Some(&V::from("Ada"))
    );
}

/// A native engine that runs `hook` once, just before the first statement
/// naming `marker` among its parameters: a commit by another connection at
/// that point of a request.
struct Hooked {
    inner: NativeEngine,
    marker: String,
    hook: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Hooked {
    fn fire(&self, params: &[norn_cell::engine::SqlValue]) {
        if params
            .iter()
            .any(|param| matches!(param, norn_cell::engine::SqlValue::Text(text) if *text == self.marker))
            && let Some(hook) = self.hook.lock().unwrap().take()
        {
            hook();
        }
    }
}

impl norn_cell::engine::SqlEngine for Hooked {
    fn execute(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<(), norn_cell::CellError> {
        self.fire(params);
        self.inner.execute(sql, params)
    }
    fn query(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<Vec<Vec<norn_cell::engine::SqlValue>>, norn_cell::CellError> {
        self.fire(params);
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), norn_cell::CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<norn_cell::CellError>>(
        &self,
        f: impl FnOnce() -> Result<T, X>,
    ) -> Result<T, X> {
        self.inner.transaction(f)
    }
    fn platform_table_globs(&self) -> &'static [&'static str] {
        self.inner.platform_table_globs()
    }
    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.inner.read_isolation()
    }
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, norn_cell::CellError> {
        self.inner.read_cache_revision()
    }
    fn admitted(&self) -> Result<(), norn_cell::CellError> {
        self.inner.admitted()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, norn_cell::CellError>,
    ) -> Result<T, norn_cell::CellError> {
        self.inner.definition_writes(f)
    }
}

/// Records every call an export hands the engine, and performs none.
#[derive(Default)]
struct Recording(std::sync::Mutex<Vec<String>>);
impl norn_api::analytics_routes::Transport for Recording {
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
        norn_cell::analytics::EngineCapabilities::DUCKDB
    }
    fn resumed(&self) -> Option<norn_api::analytics_routes::EngineAnswer> {
        None
    }
    fn call(
        &self,
        call: &norn_api::analytics_routes::EngineCall,
    ) -> Option<norn_api::analytics_routes::EngineAnswer> {
        self.0.lock().unwrap().push(call.op.clone());
        None
    }
}

#[test]
fn an_export_never_carries_a_row_committed_after_its_marking_decision() {
    // S3 (#1547): the cell holds no label when the export decides that
    // `norn.RuleRun`'s stored values may leave. Another connection then
    // commits a RuleRun, whose detail the kernel labels, before the export
    // chooses its frontier. The frontier would include that labelled row, so
    // the export decides again at the moved version and is refused: no
    // engine call is made.
    const RUN: &str = "norn.RuleRun";
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let types: Vec<norn_ir::ObjectTypeDef> = [
        json!({"name": ITEM, "properties": [{"id": "name", "type": {"kind": "text"}}]}),
        json!({"name": RUN, "properties": [{"id": "detail", "type": {"kind": "text"}}]}),
    ]
    .into_iter()
    .map(|object| serde_json::from_value(object).unwrap())
    .collect();
    let model = norn_cell::Model::from_definitions(&types, &[]).unwrap();
    let put = |object_type: &str, key: &str, property: &str, value: &str| FactWriteRow::Put {
        object_type: object_type.into(),
        key: key.into(),
        values: BTreeMap::from([(property.to_owned(), V::from(value))]),
    };
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: physical("org_a").key(),
                logical_time: 2,
                writes: vec![put(ITEM, "i1", "name", "Lathe")],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let export = |hooked: bool| {
        let foreign = (path.clone(), model.clone());
        let engine = Hooked {
            inner: NativeEngine::open(&path, norn_cell::StoreOptions::default()).unwrap(),
            // The source read's first query, between the decision and the
            // frontier: the type's committed dataset manifest, if any.
            marker: norn_cell::dataset_manifest::MANIFEST_EVENT.to_owned(),
            hook: std::sync::Mutex::new(hooked.then(|| -> Box<dyn FnOnce() + Send> {
                Box::new(move || {
                    let mut store =
                        CellStore::open(&foreign.0, foreign.1, norn_cell::StoreOptions::default())
                            .unwrap();
                    store
                        .commit(&CommitBatch {
                            world: MAIN_WORLD.into(),
                            scope: physical("org_a").key(),
                            logical_time: 3,
                            writes: vec![put(RUN, "r1", "detail", "secret rule detail")],
                            ..CommitBatch::default()
                        })
                        .unwrap();
                })
            })),
        };
        let store = CellStore::from_engine(engine, model.clone()).unwrap();
        let host = DevHost::from_store(store, physical("org_a"), |command| command.payload.clone())
            .unwrap()
            .with_authorizer(Policy);
        let transport = Recording::default();
        let outcome = norn_api::analytics_routes::export_objects(
            &host,
            &norn_api::analytics_routes::ExportQuery {
                org_id: Some("org_a".into()),
                environment_id: Some("dev".into()),
                columns: "detail".into(),
                format: Some("jsonl".into()),
            },
            Some("viewer"),
            RUN,
            &Mapping,
            &transport,
        );
        let calls = transport.0.into_inner().unwrap();
        (outcome, calls)
    };
    // Unlabelled at one version: the export reaches the engine.
    let (_outcome, calls) = export(false);
    assert_ne!(calls.len(), 0, "an unlabelled export reaches the engine");
    // The labelled row lands between the decision and the frontier.
    let (outcome, calls) = export(true);
    let refused = outcome.expect_err("the export is refused");
    assert_eq!(refused.status, 403, "{refused:?}");
    assert_eq!(calls.len(), 0, "the engine was asked: {calls:?}");
}

/// #1534 S7 (bounded within `MAX_HISTORY_SCAN = 1024`): hidden-only writes —
/// even more than `MAX_HISTORY_VERSIONS` (64) of them — collapse onto the
/// earliest version that introduced the visible state and never list a
/// hidden-only boundary write with its commit/time or flip `truncated` to
/// `true`.
#[test]
fn history_collapses_hidden_writes_across_the_version_cutoff() {
    // Pure trailing hidden writes (> 64, within MAX_HISTORY_SCAN): World 1 has
    // 70 email-only writes after v1; World 2 has v1 alone. Their full history
    // JSON is bit-exact identical.
    let hidden_burst: Vec<Vec<FactWriteRow>> = (1..=70)
        .map(|step| vec![customer("c1", "Ada", &format!("hidden-{step}@example.com"))])
        .collect();
    let (world_one, _dir_one) = host_with(
        "org_a",
        &json!({"kind": "refuse"}),
        Some(&SECRET),
        &hidden_burst,
    );
    let (world_two, _dir_two) = host_with("org_a", &json!({"kind": "refuse"}), Some(&SECRET), &[]);

    let hist_one = get_object_history(
        &world_one,
        &scope("org_a"),
        Some("viewer"),
        TYPE,
        "c1",
        &Mapping,
    )
    .unwrap();
    let hist_two = get_object_history(
        &world_two,
        &scope("org_a"),
        Some("viewer"),
        TYPE,
        "c1",
        &Mapping,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&hist_one).unwrap(),
        serde_json::to_string(&hist_two).unwrap(),
        "70 hidden-only writes crossing the 64-version boundary must collapse to v1"
    );
    assert!(!hist_one.truncated);
    assert_eq!(hist_one.versions.len(), 1);
    assert_eq!(hist_one.versions[0].version, 1);

    // Mid-history + trailing hidden writes: none of the 80 hidden-only writes
    // appear in history, and `truncated` stays false.
    let mut with_hidden = hidden_burst;
    with_hidden.push(vec![customer("c1", "Ada B.", "hidden-70@example.com")]);
    for step in 71..=80 {
        with_hidden.push(vec![customer(
            "c1",
            "Ada B.",
            &format!("hidden-{step}@example.com"),
        )]);
    }
    let (host, _dir) = host_with(
        "org_a",
        &json!({"kind": "refuse"}),
        Some(&SECRET),
        &with_hidden,
    );
    let history =
        get_object_history(&host, &scope("org_a"), Some("viewer"), TYPE, "c1", &Mapping).unwrap();
    assert_eq!(
        history
            .versions
            .iter()
            .map(|entry| entry.version)
            .collect::<Vec<_>>(),
        vec![72, 1]
    );
    assert!(!history.truncated);
    let document = serde_json::to_string(&history).unwrap();
    assert!(
        !document.contains("hidden-"),
        "hidden email leaked: {document}"
    );
}

/// #1534 S6: consecutive readable versions with identical projected values are
/// NOT collapsed when their marking projection metadata changes.
#[test]
fn history_keeps_equal_value_versions_when_marking_changes() {
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
        {"irVersion":"0.1","artifactId":"shop.Customer.entity","kind":"entity","namespace":"shop","name":"Customer","version":"0.1.0","lifecycle":"draft","scope":{"orgId":"org_a","environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","objectType":object}},
        {"irVersion":"0.1","artifactId":"shop.markings.entity","kind":"entity","namespace":"shop","name":"Markings","version":"0.1.0","lifecycle":"draft","scope":{"orgId":"org_a","environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","markings":[
            {"id":"pii.contact","label":"Contact","severity":"high","category":"pii.contact","mask":{"kind":"redacted"}},
            {"id":"pii.secondary","label":"Secondary","severity":"high","category":"pii.secondary","mask":{"kind":"redacted"}}
        ]}}
    ]);
    let key = physical("org_a").key();
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .install_definitions(
                model.clone(),
                norn_cell::InstalledDefinitionInput {
                    canonical_set: serde_json::to_string(&artifacts).unwrap(),
                    registry: vec![],
                    receipt_sha256: "privacy-s6".into(),
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
                scope: key.clone(),
                logical_time: 2,
                writes: vec![customer("c1", "Ada", "ada@example.com")],
                ..CommitBatch::default()
            })
            .unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: key,
                logical_time: 3,
                writes: vec![customer("c1", "Ada", "ada@example.com")],
                events: vec![
                    V::from_json(json!({
                        "type": "observation.recorded",
                        "mapped": [{
                            "objectType": TYPE,
                            "key": "c1",
                            "propertyProvenance": {
                                "email": {"markings": ["pii.contact", "pii.secondary"]}
                            }
                        }]
                    }))
                    .unwrap(),
                ],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let host = DevHost::from_store(store, physical("org_a"), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy)
        .with_pseudonym_secret(Some(&SECRET));
    let history =
        get_object_history(&host, &scope("org_a"), Some("viewer"), TYPE, "c1", &Mapping).unwrap();
    assert_eq!(
        history
            .versions
            .iter()
            .map(|entry| entry.version)
            .collect::<Vec<_>>(),
        vec![2, 1],
        "marking metadata change must not be over-merged: {history:#?}"
    );
}

/// #1534 S6: consecutive readable versions with identical unmarked values are
/// NOT collapsed when their visible writer changes (`Other` -> `Action`).
#[test]
fn history_keeps_equal_value_versions_when_visible_writer_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let item: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": ITEM,
        "properties": [{"id": "name", "type": {"kind": "text"}}]
    }))
    .unwrap();
    let call: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": norn_kernel::ACTION_CALL_TYPE,
        "properties": [
            {"id": "actionId", "type": {"kind": "text"}},
            {"id": "actor", "type": {"kind": "text"}},
            {"id": "object", "type": {"kind": "json-ext"}}
        ]
    }))
    .unwrap();
    let model = norn_cell::Model::from_definitions(&[item, call], &[]).unwrap();
    let key = physical("org_a").key();
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        let put_item = || FactWriteRow::Put {
            object_type: ITEM.into(),
            key: "i1".into(),
            values: BTreeMap::from([("name".to_owned(), V::from("Lathe"))]),
        };
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: key.clone(),
                logical_time: 2,
                writes: vec![put_item()],
                ..CommitBatch::default()
            })
            .unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: key,
                logical_time: 3,
                writes: vec![
                    put_item(),
                    FactWriteRow::Put {
                        object_type: norn_kernel::ACTION_CALL_TYPE.into(),
                        key: "call-1".into(),
                        values: BTreeMap::from([
                            ("actionId".to_owned(), V::from("shop.item.confirm")),
                            ("actor".to_owned(), V::from("steward")),
                            (
                                "object".to_owned(),
                                V::from_json(json!({"objectType": ITEM, "key": "i1"})).unwrap(),
                            ),
                        ]),
                    },
                ],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let host = DevHost::from_store(store, physical("org_a"), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy);
    let history =
        get_object_history(&host, &scope("org_a"), Some("viewer"), ITEM, "i1", &Mapping).unwrap();
    assert_eq!(
        history
            .versions
            .iter()
            .map(|entry| entry.version)
            .collect::<Vec<_>>(),
        vec![2, 1],
        "visible writer change must not be over-merged: {history:#?}"
    );
    assert_eq!(
        serde_json::to_value(&history.versions[0].writer).unwrap()["kind"],
        json!("action")
    );
    assert_eq!(
        serde_json::to_value(&history.versions[1].writer).unwrap()["kind"],
        json!("other")
    );
}
