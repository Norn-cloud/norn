#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "local read acceptance scenarios"
)]

use norn_cell::{CellStore, Model, SqliteCell, engine::NativeEngine};
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
fn host() -> SqliteCell<(), (), RuntimeValue> {
    let cell = CellId::try_from("00000000000000000000000000000001".to_owned()).expect("id");
    let store = CellStore::from_engine_bound(
        NativeEngine::open_in_memory().expect("engine"),
        Model::from_definitions(&norn_testkit::cell_host::law_object_types(), &[]).expect("model"),
        &cell,
    )
    .expect("store");
    SqliteCell::new(store, "demo/dev", |_| RuntimeValue::Null).expect("host")
}

#[test]
fn native_point_read_captures_local_frontier_and_existing_authority_pins() {
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
    fn tokens_cannot_cross_any_context_pin(changed in 0usize..15, salt in any::<u64>()) {
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
fn stale_and_partial_keep_their_frontier_but_never_satisfy_exact() {
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
