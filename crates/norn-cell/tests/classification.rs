#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "GRAPH-CLASS-1 scenarios"
)]
use norn_cell::classification::{ClassificationError, evaluate};
use norn_cell::reactive::{Dependency, VisibleItem};
use norn_cell::{
    CellStore, CommitBatch, FactWriteRow, InstalledDefinitionInput, MAIN_WORLD, Model, StoreOptions,
};
use norn_checker::{EntityArtifactSetContext, classification::Classifier};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile, RuntimeValue};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn fixture(max_records: u32) -> (Model, InstalledDefinitionInput, Classifier) {
    let definition = json!({"name":"test.Scrap", "properties":[
        {"id":"fine_g", "type":{"kind":"integer"}},
        {"id":"note", "type":{"kind":"text"}, "constraints":{"nullable":true}}
    ]});
    let obligation =
        json!({"name":"test.Approval", "properties":[{"id":"state", "type":{"kind":"text"}}]});
    let model = Model::from_definitions(
        &[
            serde_json::from_value(definition.clone()).expect("base"),
            serde_json::from_value(obligation.clone()).expect("obligation"),
        ],
        &[],
    )
    .expect("model");
    let base: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion":"0.1", "artifactId":"test.Scrap.entity", "version":"1.0.0", "kind":"entity",
        "namespace":"test", "name":"Scrap", "lifecycle":"draft", "scope":{}, "dependencies":[],
        "provenance":{"source":"system"}, "payload":{"entityVersion":"2.0", "objectType":definition}
    }))
    .expect("artifact");
    let mut approval = serde_json::to_value(&base).expect("approval envelope");
    approval["artifactId"] = json!("test.Approval.entity");
    approval["name"] = json!("Approval");
    approval["payload"]["objectType"] = obligation;
    let mut bounds = ResourceLimitProfile::norn_limits_v1();
    bounds.slice.max_records = max_records;
    let class: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion":"0.1", "artifactId":"test.HighValue", "version":"1.0.0", "kind":"defined_concept",
        "namespace":"test", "name":"HighValue", "lifecycle":"draft", "scope":{},
        "provenance":{"source":"system"}, "dependencies":[{"artifactId":"test.Scrap.entity", "version":"1.0.0", "kind":"entity", "required":true}],
        "payload":{"id":"test.HighValue", "version":"1.0.0", "base":{"artifactId":"test.Scrap.entity", "version":"1.0.0",
            "member":{"kind":"type", "typeName":"test.Scrap"}}, "predicate":"self.fine_g > 100 and self.note == null", "dependencies":[], "bounds":bounds}
    })).expect("classification");
    let classifier = Classifier::prepare(
        &class,
        &EntityArtifactSetContext {
            base: std::slice::from_ref(&base),
            registry: &BTreeSet::new(),
            tenant: None,
        },
    )
    .expect("admission");
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(
            RuntimeValue::from_json(json!([base, class, approval]))
                .expect("set")
                .canonical_json()
                .expect("canonical"),
        )
        .expect("utf8"),
        registry: vec![],
        receipt_sha256: "test-admission".into(),
        admission_receipt: None,
        readmission: None,
        diff: json!({}),
        installed_at: 1,
        layout: None,
    };
    (model, input, classifier)
}
fn put(amount: i64) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.into(),
        scope: "test".into(),
        logical_time: 2,
        writes: vec![FactWriteRow::Put {
            object_type: "test.Scrap".into(),
            key: "lot-7".into(),
            values: BTreeMap::from([
                (
                    "fine_g".into(),
                    RuntimeValue::from_json(json!(amount)).expect("integer"),
                ),
                ("note".into(), RuntimeValue::Null),
            ]),
        }],
        ..CommitBatch::default()
    }
}
#[test]
fn graph_classify_changes_membership_without_events_or_obligation_changes() {
    let (model, input, class) = fixture(10);
    let mut store = CellStore::open_in_memory(model.clone()).expect("cell");
    store
        .install_definitions(model, input, Some(0), "test")
        .expect("install");
    let empty =
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Visible).expect("empty");
    assert!(empty.members.is_empty());
    assert!(
        empty
            .dependencies
            .entries
            .iter()
            .any(|d| matches!(d, Dependency::Range { .. }))
    );
    let mut activated = put(101);
    activated.writes.push(FactWriteRow::Put {
        object_type: "test.Approval".into(),
        key: "approval-1".into(),
        values: BTreeMap::from([("state".into(), RuntimeValue::String("activated".into()))]),
    });
    store.commit(&activated).expect("activate");
    let head = store.verified_head().expect("head");
    let read =
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Visible).expect("member");
    assert_eq!(read.members, BTreeSet::from(["lot-7".into()]));
    assert_eq!(store.verified_head().expect("head"), head);
    store.commit(&put(99)).expect("permitted reconciliation");
    let head = store.verified_head().expect("head");
    assert!(
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Visible)
            .expect("exit")
            .members
            .is_empty()
    );
    assert_eq!(
        store.verified_head().expect("no classification effects"),
        head
    );
    assert_eq!(
        store
            .read()
            .object(MAIN_WORLD, "test.Approval", "approval-1")
            .expect("read")
            .expect("durable")
            .values["state"],
        RuntimeValue::String("activated".into())
    );
}
#[test]
fn graph_classify_refuses_uninstalled_and_exhausted_reads_and_captures_hidden_rows() {
    let (model, input, class) = fixture(10);
    let mut store = CellStore::open_in_memory(model.clone()).expect("cell");
    assert!(matches!(
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Visible),
        Err(ClassificationError::Incomplete)
    ));
    store
        .install_definitions(model, input, Some(0), "test")
        .expect("install");
    store.commit(&put(101)).expect("base");
    assert!(matches!(
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| {
            VisibleItem::Exhausted
        }),
        Err(ClassificationError::Incomplete)
    ));
    let hidden = evaluate(&store, &class, MAIN_WORLD, &mut |_, _| VisibleItem::Hidden)
        .expect("authorized subset");
    assert!(hidden.members.is_empty());
    assert!(
        hidden
            .dependencies
            .entries
            .iter()
            .any(|d| matches!(d, Dependency::Object { key, .. } if key == "lot-7"))
    );
    let (_, _, foreign) = fixture(9);
    assert!(matches!(
        evaluate(&store, &foreign, MAIN_WORLD, &mut |_, _| {
            VisibleItem::Visible
        }),
        Err(ClassificationError::Incomplete)
    ));
}
#[test]
fn graph_classify_refuses_a_commit_during_visibility() {
    let (model, input, class) = fixture(10);
    let dir = tempfile::tempdir().expect("temporary cell");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("cell");
    store
        .install_definitions(model.clone(), input, Some(0), "test")
        .expect("install");
    store.commit(&put(101)).expect("base");
    let mut writer =
        CellStore::open(&path, model, StoreOptions::default()).expect("concurrent writer");
    assert!(matches!(
        evaluate(&store, &class, MAIN_WORLD, &mut |_, _| {
            writer.commit(&put(99)).expect("concurrent reconciliation");
            VisibleItem::Visible
        }),
        Err(ClassificationError::Incomplete)
    ));
}

fn catalog_access(
    capabilities: &[&str],
) -> (norn_kernel::authz::Pdp, norn_kernel::authz::AccessRequest) {
    use norn_kernel::authz::{
        AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, PrincipalInput,
        ResourceInput, ScopeKey,
    };
    let types = norn_cell::catalog::resource_types().expect("virtual resources");
    let projection = norn_projections::project_cedar_schema(&types, &[], &[]);
    let host = norn_projections::project_host_access_schema(&types);
    let schema = AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
        .expect("schema");
    let access = serde_json::from_value(json!({"accessVersion":"0.1", "policyId":"test.catalog", "rules":
        (["Type", "Property"].iter().map(|name| json!({"ruleId":format!("read_{name}"),
            "actions":["norn.read"], "resourceType":format!("norn.catalog.{name}"), "scope":{"dimensions":["org"]}})).collect::<Vec<_>>())
    })).expect("AccessIR");
    let scope = ScopeKey {
        org: "test".into(),
        tenant: None,
        workspace: None,
        environment: None,
        business_unit: None,
    };
    let pdp = norn_kernel::authz::Pdp::new(
        norn_kernel::authz::compile(&schema, &access).expect("admitted AccessIR"),
        &[CapabilityGrant {
            subject_id: "reader".into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(scope.clone()),
            capabilities: capabilities.iter().map(|value| (*value).into()).collect(),
            binding: None,
        }],
    )
    .expect("PDP");
    let request = AccessRequest {
        principal: PrincipalInput {
            subject_id: "reader".into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles: vec![],
            attributes: BTreeMap::new(),
        },
        scope: Some(scope.clone()),
        capability: "norn.read".into(),
        action_id: "norn.read".into(),
        resource: ResourceInput {
            object_type: "unrelated.Type".into(),
            id: "unrelated-object".into(),
            scope,
            attributes: BTreeMap::new(),
        },
        context: BTreeMap::new(),
        binding: None,
        delegation: Delegation::Direct,
    };
    (pdp, request)
}
#[test]
fn graph_catalog_reads_exact_member_ids_through_slice_and_kernel_pdp() {
    use norn_cell::catalog::CatalogSnapshot;
    use norn_cell::reactive::{ReactiveLimits, SliceQuery};
    use norn_ir::definition_catalog::{Member, MemberRef};
    let (model, input, _) = fixture(10);
    let mut store = CellStore::open_in_memory(model.clone()).expect("cell");
    store
        .install_definitions(model, input, Some(0), "test")
        .expect("install");
    let snapshot = CatalogSnapshot::read_active(&store, &BTreeSet::new()).expect("catalog");
    let head = store.verified_head().expect("head");
    let limits = ReactiveLimits::from_profile(&ResourceLimitProfile::norn_limits_v1());
    let mut query = SliceQuery {
        object_type: "norn.catalog.Type".into(),
        filter: BTreeMap::from([("role".into(), RuntimeValue::String("thing".into()))]),
        limit: 10,
        binding: None,
    };
    let (pdp, request) = catalog_access(&["norn.read"]);
    let scope = request.scope.as_ref().expect("scope");
    let result = snapshot
        .read(&query, &limits, &pdp, &request, scope)
        .expect("ordinary read");
    assert_eq!(result.items.len(), 2);
    assert_eq!(result.commit_seq, head.commits);
    let member: MemberRef = serde_json::from_str(&result.items[1].key).expect("exact MemberRef");
    assert_eq!(
        member,
        MemberRef {
            artifact_id: "test.Scrap.entity".into(),
            version: "1.0.0".into(),
            member: Member::Type {
                type_name: "test.Scrap".into()
            }
        }
    );
    query.object_type = "norn.catalog.Property".into();
    query.filter.clear();
    assert_eq!(
        snapshot
            .read(&query, &limits, &pdp, &request, scope)
            .expect("properties")
            .items
            .len(),
        3
    );
    let (denied, _) = catalog_access(&["norn.admin"]);
    assert!(
        snapshot
            .read(&query, &limits, &denied, &request, scope)
            .expect("capability denial")
            .items
            .is_empty()
    );
    let mut wrong = request.clone();
    wrong.action_id = "norn.write".into();
    assert!(snapshot.read(&query, &limits, &pdp, &wrong, scope).is_err());
    let mut truncated = limits;
    truncated.max_canonical_bytes = 1;
    assert!(
        !snapshot
            .read(&query, &truncated, &pdp, &request, scope)
            .expect("byte bound")
            .complete
    );
    truncated = limits;
    for (scan, limit, complete) in [(1, 1, true), (0, 1, false), (1, 2, false)] {
        truncated.max_scan = scan;
        query.limit = limit;
        assert_eq!(
            snapshot
                .read(&query, &truncated, &pdp, &request, scope)
                .expect("scan bound")
                .complete,
            complete
        );
    }
    let mut forbidden = put(0);
    forbidden.writes = vec![FactWriteRow::Put {
        object_type: query.object_type.clone(),
        key: result.items[0].key.clone(),
        values: result.items[0].values.clone(),
    }];
    assert!(store.commit(&forbidden).is_err());
    assert_eq!(store.verified_head().expect("read emits nothing"), head);
}
