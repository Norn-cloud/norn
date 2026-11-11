#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "local read acceptance scenarios"
)]

use norn_cell::export::Exporter;
use norn_cell::{CellStore, Model, SqliteCell, StoreOptions, engine::NativeEngine};
use std::collections::BTreeMap;
use norn_ir::{RuntimeValue, cell_id::CellId};
use norn_kernel::read_envelope::{Completeness, ReadRequirement, ReadSemantics};
use norn_kernel::{CellHost as _, FactKey, authz::CommitBinding};
use norn_testkit::proptest::prelude::*;

fn binding() -> CommitBinding {
    CommitBinding {
        action_id: "read".into(),
        resource_kind: "demo.Item".into(),
        resource_id: "item".into(),
        artifact_version: Some("1".into()),
        policy_id: "demo.policy".into(),
        policy_version: "1".into(),
        resource_version: "0".into(),
        authorization_epoch: "1".into(),
        evaluated_at_logical_time: "0".into(),
    }
}
fn binding_for(key: &FactKey) -> CommitBinding {
    CommitBinding {
        resource_kind: key.object_type.0.clone(),
        resource_id: key.key.clone(),
        ..binding()
    }
}
fn model() -> Model {
    Model::from_definitions(&norn_testkit::cell_host::law_object_types(), &[]).expect("model")
}
fn host() -> SqliteCell<(), (), RuntimeValue> {
    let cell = CellId::try_from("00000000000000000000000000000001".to_owned()).expect("id");
    let store =
        CellStore::from_engine_bound(NativeEngine::open_in_memory().expect("engine"), model(), &cell)
            .expect("store");
    SqliteCell::new(store, "demo/dev", |_| RuntimeValue::Null).expect("host")
}
fn host_over(store: CellStore) -> SqliteCell<(), (), RuntimeValue> {
    SqliteCell::new(store, "demo/dev", |_| RuntimeValue::Null).expect("host")
}
fn dump(host: &SqliteCell<(), (), RuntimeValue>) -> std::collections::BTreeMap<String, Vec<String>> {
    host.store().read().dump().expect("dump")
}

#[test]
fn cell_local_read_envelope_captures_local_frontier_and_existing_authority_pins() {
    let mut host = host();
    let key = FactKey::new("demo.Item", "item");
    let authority = binding();
    // Unknown types refuse rather than issue incomplete evidence.
    assert!(host.read_envelope(&key, &authority).is_err());
    let key = norn_testkit::cell_host::law_key(1);
    let authority = CommitBinding {
        resource_kind: key.object_type.0.clone(),
        resource_id: key.key.clone(),
        ..authority
    };
    let envelope = host
        .read_envelope(&key, &authority)
        .expect("local point read");
    assert_eq!(envelope.token.context.frontier.world, "main");
    assert_eq!(envelope.token.context.frontier.revision, 0);
    assert_eq!(
        envelope.token.context.authority_pin.authorization_epoch,
        authority.authorization_epoch
    );
    assert_eq!(envelope.token.context.definition_pin.version, 0);
    assert_eq!(envelope.token.completeness, Completeness::Complete);
    assert_eq!(envelope.token.semantics, ReadSemantics::Exact);
    envelope
        .token
        .validate(&envelope.token.context, ReadRequirement::Exact)
        .expect("matching context");
    let mut wrong_resource = authority;
    wrong_resource.resource_id = "other".into();
    assert!(host.read_envelope(&key, &wrong_resource).is_err());
    let wire = serde_json::to_value(&envelope).expect("wire");
    assert_eq!(wire["token"]["format"], "norn.cell.read/v1");
    assert!(wire["token"]["context"]["frontier"].is_object());
    host.commit(norn_kernel::CellCommit::empty())
        .expect("commit");
    let fresh = host
        .read_envelope(&key, &binding_for(&key))
        .expect("new snapshot");
    assert_eq!(fresh.token.context.frontier.revision, 1);
    assert!(
        envelope
            .token
            .validate(&fresh.token.context, ReadRequirement::Exact)
            .is_err()
    );
}

proptest! {
    #[test]
    fn cell_local_read_envelope_tokens_cannot_cross_any_context_pin(changed in 0usize..15, salt in any::<u64>()) {
        let host = host();
        let key = norn_testkit::cell_host::law_key(1);
        let authority = binding_for(&key);
        let envelope = host.read_envelope(&key, &authority).expect("read");
        let mut context = envelope.token.context.clone();
        let suffix = format!("-changed-{salt}");
        match changed {
            0 => context.frontier.cell = CellId::try_from("00000000000000000000000000000002".to_owned()).expect("other id"),
            1 => context.frontier.world.push_str(&suffix),
            2 => context.frontier.revision = context.frontier.revision.checked_add(1).expect("revision"),
            3 => context.definition_pin.version = context.definition_pin.version.checked_add(1).expect("definition version"),
            4 => context.definition_pin.model_hash.push_str(&suffix),
            5 => context.definition_pin.admission_receipt = Some(suffix),
            6 => context.authority_pin.action_id.push_str(&suffix),
            7 => context.authority_pin.resource_kind.push_str(&suffix),
            8 => context.authority_pin.resource_id.push_str(&suffix),
            9 => context.authority_pin.artifact_version = Some(suffix),
            10 => context.authority_pin.policy_id.push_str(&suffix),
            11 => context.authority_pin.policy_version.push_str(&suffix),
            12 => context.authority_pin.resource_version.push_str(&suffix),
            13 => context.authority_pin.authorization_epoch.push_str(&suffix),
            _ => context.authority_pin.evaluated_at_logical_time.push_str(&suffix),
        }
        prop_assert!(envelope.token.validate(&context, ReadRequirement::Exact).is_err());
        prop_assert!(envelope.token.validate(&context, ReadRequirement::AllowStale).is_err());
    }
}

#[test]
fn cell_local_read_envelope_stale_and_partial_keep_their_frontier_but_never_satisfy_exact() {
    let host = host();
    let key = norn_testkit::cell_host::law_key(1);
    let authority = binding_for(&key);
    let mut token = host.read_envelope(&key, &authority).expect("read").token;
    let context = token.context.clone();
    token.semantics = ReadSemantics::Stale;
    assert!(token.validate(&context, ReadRequirement::Exact).is_err());
    token
        .validate(&context, ReadRequirement::AllowStale)
        .expect("explicit stale");
    assert_eq!(token.context.frontier, context.frontier);
    token.semantics = ReadSemantics::Exact;
    token.completeness = Completeness::Partial;
    assert!(token.validate(&context, ReadRequirement::Exact).is_err());
}

/// Refused envelopes (unknown type, another resource kind or id, unqualified
/// legacy history) issue nothing and leave the complete store state unchanged.
#[test]
fn cell_local_read_envelope_refusals_leave_complete_state_unchanged() {
    use norn_cell::engine::SqlEngine as _;
    let host = host();
    let key = norn_testkit::cell_host::law_key(1);
    let before = dump(&host);
    let unknown = FactKey::new("demo.Item", "item");
    assert!(host.read_envelope(&unknown, &binding()).is_err());
    let mut other_id = binding_for(&key);
    other_id.resource_id.push_str("-other");
    assert!(host.read_envelope(&key, &other_id).is_err());
    let mut other_kind = binding_for(&key);
    other_kind.resource_kind.push_str("-other");
    assert!(host.read_envelope(&key, &other_kind).is_err());
    assert_eq!(dump(&host), before);

    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("legacy.sqlite");
    drop(CellStore::open(&path, model(), StoreOptions::default()).expect("empty store"));
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine
        .execute(
            "DELETE FROM _norn_meta WHERE key IN ('binding:cell_id', 'binding:cell_identity_format')",
            &[],
        )
        .expect("legacy fixture");
    let legacy = host_over(CellStore::from_engine(engine, model()).expect("legacy local open"));
    let before = dump(&legacy);
    assert!(legacy.read_envelope(&key, &binding_for(&key)).is_err());
    assert_eq!(dump(&legacy), before);
}

/// A token never crosses into a same-scope sibling cell, even at the same
/// revision under the same authority; restoring the cell from its export keeps
/// the same logical identity, so the restored cell reproduces the same context.
#[tokio::test]
async fn cell_local_read_envelope_tokens_do_not_cross_cells_and_survive_restore() {
    let dir = tempfile::tempdir().expect("directory");
    let source_path = dir.path().join("source.sqlite");
    let source_id = CellId::try_from("00000000000000000000000000000001".to_owned()).expect("id");
    let mut source = host_over(
        CellStore::open_bound(&source_path, model(), StoreOptions::default(), &source_id)
            .expect("source"),
    );
    let mut sibling = host_over(CellStore::open_in_memory(model()).expect("sibling"));
    let key = norn_testkit::cell_host::law_key(1);
    let authority = binding_for(&key);
    source.commit(norn_kernel::CellCommit::empty()).expect("commit");
    sibling.commit(norn_kernel::CellCommit::empty()).expect("commit");
    let token = source.read_envelope(&key, &authority).expect("read").token;
    let sibling_token = sibling.read_envelope(&key, &authority).expect("read").token;
    assert_eq!(token.context.frontier.revision, sibling_token.context.frontier.revision);
    assert_eq!(token.context.authority_pin, sibling_token.context.authority_pin);
    assert_ne!(token.context.frontier.cell, sibling_token.context.frontier.cell);
    for requirement in [ReadRequirement::Exact, ReadRequirement::AllowStale] {
        assert!(token.validate(&sibling_token.context, requirement).is_err());
        assert!(sibling_token.validate(&token.context, requirement).is_err());
    }

    let mut exporter = Exporter::local(&dir.path().join("export")).expect("exporter");
    let record = source.store().export_record(1).expect("read").expect("record");
    exporter.append(&record).await.expect("export");
    drop(source);
    let target = dir.path().join("restored.sqlite");
    let bindings = BTreeMap::from([
        ("scope".to_owned(), "demo/dev".to_owned()),
        ("cell_id".to_owned(), String::from(source_id.clone())),
        (
            "cell_identity_format".to_owned(),
            norn_cell::cell_identity::FORMAT.to_owned(),
        ),
    ]);
    CellStore::restore_from(
        &mut exporter,
        &target,
        model(),
        "demo/dev",
        &bindings,
        StoreOptions::default(),
    )
    .await
    .expect("restore");
    let restored =
        host_over(CellStore::open(&target, model(), StoreOptions::default()).expect("restored"));
    let restored_token = restored.read_envelope(&key, &authority).expect("read").token;
    assert_eq!(restored_token.context.frontier.cell, source_id);
    token
        .validate(&restored_token.context, ReadRequirement::Exact)
        .expect("restore keeps the same logical cell and frontier");
    let mut other_authority = authority.clone();
    other_authority.authorization_epoch.push_str("-next");
    let reauthorized = restored
        .read_envelope(&key, &other_authority)
        .expect("read")
        .token;
    assert!(token.validate(&reauthorized.context, ReadRequirement::AllowStale).is_err());
}
