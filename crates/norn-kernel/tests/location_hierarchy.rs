#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test fixtures"
)]

//! Registry-declared hierarchy roles obey final-graph and cell-scope admission.

use norn_ir::ObjectTypeDef;
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, Command, FactKey, FactMutation, FactVersion, FactWrite, LinkKey,
    LinkMutation, LinkWrite,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::Value;

type Cell = MemoryCell<String, JsonObject, JsonObject>;

fn cell() -> Cell {
    let types = [
        include_str!("../../../registry/core/artifacts/location.Region.entity.json"),
        include_str!("../../../registry/core/artifacts/location.Space.entity.json"),
        include_str!("../../../registry/core/artifacts/identity.BusinessUnit.entity.json"),
    ]
    .into_iter()
    .map(|raw| {
        let envelope: Value = serde_json::from_str(raw).expect("registry JSON");
        let mut ty: ObjectTypeDef =
            serde_json::from_value(envelope["payload"]["objectType"].clone())
                .expect("registry type");
        // Isolate topology here. Full scalar/interface admission is exercised
        // by norn-ontology's registry_tree and location_hierarchy suites.
        ty.properties.clear();
        ty.identity.clear();
        ty.implements.clear();
        ty.links.retain(|link| link.id.as_str() == "parent");
        ty
    })
    .collect();
    MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone()).with_types(types)
}

fn create(ty: &str, id: &str) -> FactWrite {
    FactWrite {
        key: FactKey::new(ty, id),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    }
}

fn edge(ty: &str, source: &str, target: &str) -> LinkWrite {
    LinkWrite {
        key: LinkKey::new(FactKey::new(ty, source), "parent", FactKey::new(ty, target)),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    }
}

#[test]
fn hierarchy_cycles_refuse_atomically_including_same_commit_and_existing_paths() {
    for ty in ["location.Region", "location.Space", "identity.BusinessUnit"] {
        for edges in [vec![("a", "a")], vec![("a", "b"), ("b", "a")]] {
            let mut cell = cell();
            let refusal = cell
                .commit(CellCommit {
                    writes: vec![create(ty, "a"), create(ty, "b")],
                    links: edges.into_iter().map(|(a, b)| edge(ty, a, b)).collect(),
                    ..CellCommit::empty()
                })
                .expect_err("a final-graph cycle refuses");
            assert!(refusal.to_string().contains("cycle"));
            assert!(cell.ledger().is_empty());
            assert!(!cell.load(&FactKey::new(ty, "a")).expect("load").exists());
        }
        let mut cell = cell();
        cell.commit(CellCommit {
            writes: vec![create(ty, "a"), create(ty, "b"), create(ty, "c")],
            links: vec![edge(ty, "a", "b"), edge(ty, "b", "c")],
            ..CellCommit::empty()
        })
        .expect("an acyclic chain admits");
        let before = cell.ledger().len();
        let refusal = cell
            .commit(CellCommit {
                links: vec![edge(ty, "c", "a")],
                ..CellCommit::empty()
            })
            .expect_err("a back edge across committed ancestors refuses");
        assert!(refusal.to_string().contains("cycle"));
        assert_eq!(cell.ledger().len(), before);
        // Reparenting checks the final graph, so removing a->b while adding
        // c->a does not retain a false cycle from the old snapshot.
        let mut removed = edge(ty, "a", "b");
        removed.expected = cell.load_link(&removed.key).expect("link").version;
        removed.mutation = LinkMutation::Remove;
        cell.commit(CellCommit {
            links: vec![removed, edge(ty, "c", "a")],
            ..CellCommit::empty()
        })
        .expect("final acyclic reparenting admits");
    }
}

#[test]
fn hierarchy_remote_cell_target_cannot_be_linked_and_missing_parents_admit() {
    let ty = "location.Region";
    let mut local = cell();
    let mut remote = cell();
    local
        .commit(CellCommit {
            writes: vec![create(ty, "local")],
            ..CellCommit::empty()
        })
        .expect("missing parent is allowed");
    remote
        .commit(CellCommit {
            writes: vec![create(ty, "remote")],
            ..CellCommit::empty()
        })
        .expect("remote scope has its own parent");
    let before = local.ledger().len();
    let refusal = local
        .commit(CellCommit {
            links: vec![edge(ty, "local", "remote")],
            ..CellCommit::empty()
        })
        .expect_err("another cell's target is not a local endpoint");
    assert_eq!(refusal.code(), "commit.dangling_link");
    assert_eq!(local.ledger().len(), before);
}

#[test]
fn hierarchy_traversal_budget_refuses_before_mutation() {
    let ty = "location.Region";
    let mut cell = cell();
    // Build a long, valid chain in independent commits; the final insertion
    // traverses more than the cumulative 1,000-record budget.
    cell.commit(CellCommit {
        writes: (0..=1001).map(|i| create(ty, &i.to_string())).collect(),
        ..CellCommit::empty()
    })
    .expect("objects admit");
    for i in 0..1000 {
        cell.commit(CellCommit {
            links: vec![edge(ty, &i.to_string(), &(i + 1).to_string())],
            ..CellCommit::empty()
        })
        .expect("each forward append has a short traversal");
    }
    let before = cell.ledger().len();
    let refusal = cell
        .commit(CellCommit {
            links: vec![edge(ty, "1001", "0")],
            ..CellCommit::empty()
        })
        .expect_err("long traversal fails closed");
    assert!(refusal.to_string().contains("budget"));
    assert_eq!(refusal.code(), "commit.traversal_limit_exceeded");
    assert_eq!(refusal.category(), norn_kernel::RefusalCategory::Limit);
    assert_eq!(cell.ledger().len(), before);
}

#[test]
fn required_owner_checks_final_graph_and_inherited_concrete_contract() {
    let envelope: Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/party.Brand.entity.json"
    ))
    .expect("Brand");
    let mut brand: ObjectTypeDef =
        serde_json::from_value(envelope["payload"]["objectType"].clone()).expect("type");
    brand.properties.clear();
    brand.identity.clear();
    brand.implements.clear();
    let organization = serde_json::from_value(serde_json::json!({"name":"party.Organization"}))
        .expect("owner type");
    let subtype =
        serde_json::from_value(serde_json::json!({"name":"acme.Brand", "extends":"party.Brand"}))
            .expect("subtype");
    let mut cell: Cell = MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(vec![brand, organization, subtype]);
    let refusal = cell
        .commit(CellCommit {
            writes: vec![create("acme.Brand", "brand")],
            ..CellCommit::empty()
        })
        .expect_err("ownerless inherited Brand refuses");
    assert!(refusal.to_string().contains("required link `legal_entity`"));
    assert!(cell.ledger().is_empty());
    let owner = |id| LinkWrite {
        // A base-type spelling still uses the concrete subtype's contract.
        key: LinkKey::new(
            FactKey::new("party.Brand", "brand"),
            "legal_entity",
            FactKey::new("party.Organization", id),
        ),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![
            create("acme.Brand", "brand"),
            create("party.Organization", "one"),
            create("party.Organization", "two"),
        ],
        links: vec![owner("one")],
        ..CellCommit::empty()
    })
    .expect("atomic owned create admits");
    let before = cell.ledger().len();
    let mut remove = owner("one");
    remove.expected = cell.load_link(&remove.key).expect("owner").version;
    remove.mutation = LinkMutation::Remove;
    let refusal = cell
        .commit(CellCommit {
            links: vec![remove.clone()],
            ..CellCommit::empty()
        })
        .expect_err("last owner removal refuses");
    assert!(refusal.to_string().contains("required link `legal_entity`"));
    assert_eq!(cell.ledger().len(), before);
    assert!(
        cell.load_link(&remove.key)
            .expect("unchanged owner")
            .exists()
    );
    cell.commit(CellCommit {
        links: vec![remove, owner("two")],
        ..CellCommit::empty()
    })
    .expect("atomic replacement admits");
}

#[test]
fn hierarchy_high_fan_in_ancestor_uses_only_outgoing_role() {
    let ty = "location.Region";
    let mut cell = cell();
    cell.commit(CellCommit {
        writes: (0..1100)
            .map(|i| create(ty, &format!("child-{i}")))
            .chain([
                create(ty, "ancestor"),
                create(ty, "middle"),
                create(ty, "leaf"),
            ])
            .collect(),
        ..CellCommit::empty()
    })
    .expect("objects");
    // Separate commits keep the per-turn topology proof within its budget.
    for i in 0..1100 {
        cell.commit(CellCommit {
            links: vec![edge(ty, &format!("child-{i}"), "ancestor")],
            ..CellCommit::empty()
        })
        .expect("fan-in");
    }
    cell.commit(CellCommit {
        links: vec![edge(ty, "middle", "ancestor")],
        ..CellCommit::empty()
    })
    .expect("middle");
    cell.commit(CellCommit {
        links: vec![edge(ty, "leaf", "middle")],
        ..CellCommit::empty()
    })
    .expect("ancestor's unrelated incoming children consume no outgoing budget");
}

#[test]
fn hierarchy_same_commit_target_keys_spend_the_traversal_byte_budget() {
    let ty = "location.Region";
    let keys: Vec<_> = (0..80)
        .map(|i| format!("{i:04}{}", "x".repeat(4092)))
        .collect();
    let mut cell = cell();
    let refusal = cell
        .commit(CellCommit {
            writes: keys.iter().map(|id| create(ty, id)).collect(),
            links: keys
                .windows(2)
                .map(|pair| edge(ty, &pair[0], &pair[1]))
                .collect(),
            ..CellCommit::empty()
        })
        .expect_err("new-node adjacency also spends the byte budget");
    assert!(refusal.to_string().contains("byte budget"));
    assert_eq!(refusal.code(), "commit.traversal_limit_exceeded");
    assert_eq!(refusal.category(), norn_kernel::RefusalCategory::Limit);
    assert!(cell.ledger().is_empty());
}

#[test]
fn unrelated_same_name_relations_do_not_form_a_commit_cycle() {
    let types = serde_json::from_value::<Vec<ObjectTypeDef>>(serde_json::json!([
        {"name":"test.A", "links":[{"id":"parent","target":"test.B","acyclic":true,"cardinality":{"to":"one","from":"many"}}]},
        {"name":"test.B", "links":[{"id":"parent","target":"test.A","acyclic":true,"cardinality":{"to":"one","from":"many"}}]}
    ])).expect("independent relations");
    let mut cell: Cell =
        MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone()).with_types(types);
    let mut ab = edge("test.A", "a", "b");
    ab.key.target.object_type = norn_ir::ObjectTypeName("test.B".to_owned());
    let mut ba = edge("test.B", "b", "a");
    ba.key.target.object_type = norn_ir::ObjectTypeName("test.A".to_owned());
    cell.commit(CellCommit {
        writes: vec![create("test.A", "a"), create("test.B", "b")],
        links: vec![ab, ba],
        ..CellCommit::empty()
    })
    .expect("each declaring relation has one edge and no cycle");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one atomic authority matrix checks both directions and unchanged reads"
)]
fn observed_link_authority_seals_changed_and_unchanged_sources_and_both_ownership_directions() {
    use norn_ir::provenance::{InstanceProvenance, MappingVersion, SourceRecordRef, UtcTimestamp};
    use norn_kernel::snapshot_observe::{
        ObservationEvent, ObservedSnapshot, ObservedSourceOwner, SOURCE_OWNERSHIP_TYPE,
        prepare_observed_snapshot, tombstone_key,
    };
    use norn_kernel::{DecisionTurn, LogicalTime, TypeCatalog, World, load_slice};
    let types: Vec<ObjectTypeDef> = serde_json::from_value(serde_json::json!([
        {"name":"party.Brand", "properties":[{"id":"name","type":{"kind":"text"}}],
         "links":[{"id":"legal_entity","target":"party.Organization","cardinality":{"to":"one","from":"many","required":true}}]},
        {"name":"party.Organization"},
        {"name":SOURCE_OWNERSHIP_TYPE, "properties":[{"id":"owner","type":{"kind":"json-ext"}}]},
        {"name":"norn.PendingLinks", "properties":[{"id":"links","type":{"kind":"json-ext"}}]}
    ])).expect("types");
    let catalog = TypeCatalog::new(types.iter());
    let mut cell: MemoryCell<ObservationEvent, JsonObject, JsonObject> =
        MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone())
            .with_types(types.clone());
    let source = FactKey::new("party.Brand", "brand");
    let marker = FactKey::new(SOURCE_OWNERSHIP_TYPE, "party.Brand/brand");
    let one = FactKey::new("party.Organization", "one");
    let two = FactKey::new("party.Organization", "two");
    let provenance = InstanceProvenance {
        source_fields: Vec::new(),
        markings: Vec::new(),
        source: SourceRecordRef::new("erpnext", "Brand/brand"),
        mapping_version: MappingVersion("erpnext.brand@1".to_owned()),
        transform: None,
        observed_at: None,
        recorded_at: UtcTimestamp("2026-10-14T00:00:00Z".to_owned()),
        upstream: Vec::new(),
    };
    let turn = DecisionTurn {
        id: "observe/brand".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };
    let values = JsonObject::from([("name".to_owned(), "Original".into())]);
    let prepare = |cell: &MemoryCell<ObservationEvent, JsonObject, JsonObject>,
                   values: &JsonObject,
                   provenance: &InstanceProvenance| {
        let slice = load_slice(cell, [source.clone(), tombstone_key(&source)]).expect("slice");
        prepare_observed_snapshot(
            &catalog,
            &slice,
            &turn,
            ObservedSnapshot {
                key: &source,
                values,
                lifecycles: &[],
                provenance,
            },
        )
        .expect("snapshot")
        .into_parts()
    };
    let link = |target: FactKey| LinkWrite {
        key: LinkKey::new(source.clone(), "legal_entity", target),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    let (mut commit, mut authorization) = prepare(&cell, &values, &provenance);
    commit.writes.extend([
        create("party.Organization", "one"),
        create("party.Organization", "two"),
    ]);
    commit.writes.push(FactWrite {
        key: marker.clone(),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::from([(
                "owner".to_owned(),
                norn_ir::runtime::encode(&ObservedSourceOwner::from_provenance(&provenance))
                    .expect("owner"),
            )]),
        },
    });
    commit.links.push(link(one.clone()));
    authorization
        .seal_link_intent(&catalog, &source, "legal_entity", Some(&one), &commit.links)
        .expect("mapped intent");
    let mut no_marker = commit.clone();
    no_marker.writes.retain(|write| write.key != marker);
    assert!(
        matches!(
            cell.commit_observed(no_marker, authorization.clone()),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ),
        "an observed edge must durably retain its authority witness"
    );
    cell.commit_observed(commit, authorization)
        .expect("mapped source and owner");
    let mut remove = link(one.clone());
    remove.expected = cell.load_link(&remove.key).expect("link").version;
    remove.mutation = LinkMutation::Remove;
    let before = cell.ledger().len();
    for name in ["Original", "Changed"] {
        let values = JsonObject::from([("name".to_owned(), name.into())]);
        let (mut commit, authority) = prepare(&cell, &values, &provenance);
        commit.links.push(remove.clone());
        assert!(matches!(
            cell.commit_observed(commit, authority),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ));
        assert_eq!(cell.ledger().len(), before);
        assert!(cell.load_link(&remove.key).expect("owner remains").exists());
    }
    // Ownership cannot be erased first to launder a later authored edge.
    assert!(matches!(
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: marker.clone(),
                expected: cell.load(&marker).expect("marker").version,
                mutation: FactMutation::Delete
            }],
            ..CellCommit::empty()
        }),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    // Even an atomic authored replacement is forbidden on a source-owned role.
    assert!(matches!(
        cell.commit(norn_kernel::CellCommit {
            links: vec![remove.clone(), link(two.clone())],
            ..CellCommit::empty()
        }),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    // A different source or mapping cannot borrow the observed owner's scope.
    for foreign_mapping in [false, true] {
        let mut foreign = provenance.clone();
        if foreign_mapping {
            foreign.mapping_version = MappingVersion("foreign.brand@1".to_owned());
        } else {
            foreign.source.system = "foreign-source".to_owned();
        }
        let (mut commit, mut authority) = prepare(&cell, &values, &foreign);
        commit.links.extend([remove.clone(), link(two.clone())]);
        authority
            .seal_link_intent(&catalog, &source, "legal_entity", Some(&two), &commit.links)
            .expect("sealed foreign intent");
        assert!(matches!(
            cell.commit_observed(commit, authority),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ));
        assert_eq!(cell.ledger().len(), before);
    }
    // Legitimate mapped replacement retains the exact removal/addition proof.
    let (mut commit, mut authority) = prepare(&cell, &values, &provenance);
    commit.links.extend([remove.clone(), link(two.clone())]);
    authority
        .seal_link_intent(&catalog, &source, "legal_entity", Some(&two), &commit.links)
        .expect("replacement intent");
    let mut altered = commit.clone();
    altered.links[1].mutation = LinkMutation::Remove;
    assert!(matches!(
        cell.commit_observed(altered, authority.clone()),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    cell.commit_observed(commit, authority)
        .expect("mapped replacement");

    let missing = FactKey::new("party.Organization", "missing");
    let pending_key = FactKey::new("norn.PendingLinks", "party.Organization/missing");
    let pending_entry = norn_ir::runtime_object! {
        "status" => "PENDING", "required" => true,
        "sourceType" => source.object_type.as_str(), "sourceKey" => source.key.as_str(),
        "link" => "legal_entity", "targetType" => missing.object_type.as_str(), "targetKey" => missing.key.as_str(),
        "provenance" => norn_ir::runtime::encode(&provenance).expect("provenance"),
    };
    let pending_write = FactWrite {
        key: pending_key.clone(),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::from([(
                "links".to_owned(),
                norn_ir::RuntimeValue::Array(vec![pending_entry]),
            )]),
        },
    };
    assert!(
        matches!(
            cell.commit(CellCommit {
                writes: vec![pending_write.clone()],
                ..CellCommit::empty()
            }),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ),
        "authored pending JSON cannot create a source-owned resolution intent"
    );
    let mut foreign = provenance.clone();
    foreign.source.system = "foreign-source".to_owned();
    let (mut forged, mut authority) = prepare(&cell, &values, &foreign);
    let mut foreign_write = pending_write.clone();
    let FactMutation::Put { values: payload } = &mut foreign_write.mutation else {
        panic!("pending put")
    };
    let norn_ir::RuntimeValue::Array(entries) = payload.get_mut("links").expect("links") else {
        panic!("pending array")
    };
    let norn_ir::RuntimeValue::Object(fields) = entries.first_mut().expect("entry") else {
        panic!("pending object")
    };
    fields.insert(
        "provenance".to_owned(),
        norn_ir::runtime::encode(&foreign).expect("foreign provenance"),
    );
    forged.writes.push(foreign_write);
    let original = load_slice(&cell, [pending_key.clone()]).expect("original pending state");
    authority
        .seal_pending_link_changes(&original, &forged.writes)
        .expect("seals its own foreign claim");
    assert!(
        matches!(
            cell.commit_observed(forged, authority),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ),
        "retained ownership also seals pending changes before target resolution"
    );
    let (mut unsealed, authority) = prepare(&cell, &values, &provenance);
    unsealed.writes.push(pending_write.clone());
    assert!(
        matches!(
            cell.commit_observed(unsealed, authority),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ),
        "appended pending JSON must also carry exact opaque authority"
    );
    let (mut commit, mut authority) = prepare(&cell, &values, &provenance);
    let mut removal = link(two.clone());
    removal.expected = cell.load_link(&removal.key).expect("current owner").version;
    removal.mutation = LinkMutation::Remove;
    commit.links.push(removal);
    commit.writes.push(pending_write);
    let before = load_slice(&cell, [pending_key.clone()]).expect("pending original");
    authority
        .seal_link_intent(
            &catalog,
            &source,
            "legal_entity",
            Some(&missing),
            &commit.links,
        )
        .expect("mapped missing replacement");
    authority
        .seal_pending_link_changes(&before, &commit.writes)
        .expect("mapped pending evidence");
    cell.commit_observed(commit, authority)
        .expect("required mapped role may wait for its target");
    let (mut commit, mut authority) = prepare(&cell, &values, &provenance);
    commit.writes.push(create("party.Organization", "missing"));
    commit.writes.push(FactWrite {
        key: pending_key.clone(),
        expected: cell.load(&pending_key).expect("pending version").version,
        mutation: FactMutation::Delete,
    });
    commit.links.push(link(missing.clone()));
    let before = load_slice(&cell, [pending_key.clone()]).expect("pending original");
    authority
        .seal_link_intent(
            &catalog,
            &source,
            "legal_entity",
            Some(&missing),
            &commit.links,
        )
        .expect("resolution intent");
    authority
        .seal_pending_link_changes(&before, &commit.writes)
        .expect("resolution consumes pending evidence");
    cell.commit_observed(commit, authority)
        .expect("target publication, link and pending deletion are atomic");
    assert!(!cell.load(&pending_key).expect("pending consumed").exists());
    assert!(
        cell.load_link(&link(missing).key)
            .expect("resolved link")
            .exists()
    );

    let mut authored: MemoryCell<ObservationEvent, JsonObject, JsonObject> =
        MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone())
            .with_types(types.clone());
    authored
        .commit(CellCommit {
            writes: vec![
                FactWrite {
                    key: source.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: values.clone(),
                    },
                },
                create("party.Organization", "one"),
                create("party.Organization", "two"),
            ],
            links: vec![link(one)],
            ..CellCommit::empty()
        })
        .expect("authored source");
    let adoption = FactWrite {
        key: marker.clone(),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::from([(
                "owner".to_owned(),
                norn_ir::runtime::encode(&ObservedSourceOwner::from_provenance(&provenance))
                    .expect("owner"),
            )]),
        },
    };
    let before = authored.ledger().len();
    // Marker-only and scalar-changing observation scopes cannot adopt an
    // independently authored source. Nor can an authored marker write.
    for name in ["Original", "Foreign changed"] {
        let observed_values = JsonObject::from([("name".to_owned(), name.into())]);
        let (mut commit, authority) = prepare(&authored, &observed_values, &provenance);
        commit.writes.push(adoption.clone());
        assert!(matches!(
            authored.commit_observed(commit, authority),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ));
        assert_eq!(authored.ledger().len(), before);
        assert!(!authored.load(&marker).expect("no adoption").exists());
    }
    assert!(matches!(
        authored.commit(CellCommit {
            writes: vec![adoption.clone()],
            ..CellCommit::empty()
        }),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    // A forged marker on a never-materialized object also refuses.
    let mut absent: MemoryCell<ObservationEvent, JsonObject, JsonObject> =
        MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone())
            .with_types(types.clone());
    let (mut commit, authority) = prepare(&absent, &values, &provenance);
    commit.writes.retain(|write| write.key != source);
    commit.reads.push(norn_kernel::FactExpectation {
        key: source.clone(),
        expected: FactVersion::ABSENT,
    });
    commit.writes.push(adoption);
    assert!(matches!(
        absent.commit_observed(commit, authority),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    let mut removal = remove;
    removal.expected = authored
        .load_link(&removal.key)
        .expect("authored edge")
        .version;
    let (mut commit, mut authority) = prepare(&authored, &values, &provenance);
    commit.links.extend([removal.clone(), link(two.clone())]);
    authority
        .seal_link_intent(&catalog, &source, "legal_entity", Some(&two), &commit.links)
        .expect("mapped intent");
    assert!(matches!(
        authored.commit_observed(commit, authority),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    authored
        .commit(CellCommit {
            links: vec![removal, link(two)],
            ..CellCommit::empty()
        })
        .expect("authored replacement remains authorized");
}

type ClaimCell = MemoryCell<norn_kernel::ObservationEvent, JsonObject, JsonObject>;
/// Canonical endpoints and the registry claim type, including an inherited alias.
fn claim_fixture() -> (
    Vec<ObjectTypeDef>,
    ClaimCell,
    norn_ir::mapping::ObservedLinkClaim,
) {
    let mut types: Vec<ObjectTypeDef> = serde_json::from_value(serde_json::json!([
        {"name":"test.Source", "links":[{"id":"target","target":"test.Target","cardinality":{"to":"one","from":"many"}}]},
        {"name":"test.Target"}
    ])).expect("types");
    let declaration: Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/norn.ObservedLinkClaim.entity.json"
    ))
    .expect("claim declaration");
    types.push(
        serde_json::from_value(declaration["payload"]["objectType"].clone()).expect("claim type"),
    );
    types.push(
        serde_json::from_value(
            serde_json::json!({"name":"test.ClaimAlias", "extends":"norn.ObservedLinkClaim"}),
        )
        .expect("claim subtype"),
    );
    let mut cell = ClaimCell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(types.clone());
    cell.commit(CellCommit {
        writes: vec![
            create("test.Source", "one"),
            create("test.Target", "two"),
            create("test.Target", "other"),
        ],
        ..CellCommit::empty()
    })
    .expect("initial objects");
    let provenance = serde_json::from_value(serde_json::json!({
        "source":{"system":"synthetic-a","record":"rows/one"},
        "mappingVersion":"synthetic.mapping@1", "transform":"row.target", "recordedAt":"2026-11-03T00:00:00Z"
    })).expect("provenance");
    let claim = norn_ir::mapping::ObservedLinkClaim {
        observation_id: "bundle:first".into(),
        source_scope: "synthetic".into(),
        source_type: norn_ir::ObjectTypeName("test.Source".into()),
        source_key: "one".into(),
        role: norn_ir::LinkId("target".into()),
        target: Some(norn_ir::mapping::ObservedLinkTarget {
            object_type: norn_ir::ObjectTypeName("test.Target".into()),
            key: "two".into(),
        }),
        provenance,
    };
    (types, cell, claim)
}
/// Bind the immutable claim snapshot or read to its exact proposed edge.
fn claimed_commit(
    catalog: &norn_kernel::TypeCatalog<'_>,
    cell: &ClaimCell,
    claim: &norn_ir::mapping::ObservedLinkClaim,
) -> (
    CellCommit<norn_kernel::ObservationEvent, JsonObject, JsonObject>,
    norn_kernel::ObservationAuthorization,
) {
    claimed_commit_with(catalog, cell, claim, JsonObject::new())
}
/// [`claimed_commit`] proposing explicit link property values for a new edge.
fn claimed_commit_with(
    catalog: &norn_kernel::TypeCatalog<'_>,
    cell: &ClaimCell,
    claim: &norn_ir::mapping::ObservedLinkClaim,
    values: JsonObject,
) -> (
    CellCommit<norn_kernel::ObservationEvent, JsonObject, JsonObject>,
    norn_kernel::ObservationAuthorization,
) {
    let source = FactKey::new(claim.source_type.as_str(), &claim.source_key);
    let read = norn_kernel::FactExpectation {
        key: source.clone(),
        expected: cell.load(&source).expect("source").version,
    };
    let target = claim.target.as_ref().expect("explicit target fixture");
    let key = LinkKey::new(
        source,
        claim.role.as_str(),
        FactKey::new(target.object_type.as_str(), &target.key),
    );
    let state = cell.load_link(&key).expect("link state");
    let links = if state.values.is_none() {
        vec![LinkWrite {
            key,
            expected: state.version,
            mutation: LinkMutation::Add { values },
        }]
    } else {
        vec![]
    };
    let mut commit = CellCommit {
        reads: vec![read.clone()],
        links,
        ..CellCommit::empty()
    };
    let mut authority = norn_kernel::ObservationAuthorization::empty();
    assert!(
        authority
            .seal_observed_link_claim(catalog, claim, read.clone(), &commit)
            .is_err(),
        "unprepared claim bytes confer no authority"
    );
    let key = FactKey::new(
        norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
        claim.id().expect("claim id"),
    );
    let slice = norn_kernel::load_slice(cell, [key]).expect("claim slice");
    let turn = norn_kernel::DecisionTurn {
        id: claim.observation_id.clone(),
        now: norn_kernel::LogicalTime::EPOCH,
        world: norn_kernel::World::Main,
        consumes: None,
    };
    norn_kernel::snapshot_observe::prepare_observed_link_claim_snapshot(
        catalog, &slice, &turn, claim,
    )
    .expect("claim snapshot")
    .append_to(&mut commit, &mut authority)
    .expect("same atomic turn");
    authority
        .seal_observed_link_claim(catalog, claim, read, &commit)
        .expect("exact claim authority");
    (commit, authority)
}
#[test]
fn obs_link_convergence_seals_exact_claims_without_mapping_namespace_ownership() {
    let (types, mut cell, claim) = claim_fixture();
    let catalog = norn_kernel::TypeCatalog::new(types.iter());
    let (commit, authority) = claimed_commit(&catalog, &cell, &claim);
    assert!(
        matches!(
            cell.commit(commit.clone()),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ),
        "authored claim writes refuse"
    );
    let mut forged = commit.clone();
    forged.links[0].key.target = FactKey::new("test.Target", "other");
    assert!(matches!(
        cell.commit_observed(forged, authority.clone()),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    let key = FactKey::new(
        norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
        claim.id().expect("id"),
    );
    assert!(cell.load(&key).expect("absent claim").values.is_none());
    cell.commit_observed(commit, authority)
        .expect("one convergent edge plus claim");
    assert_eq!(
        cell.load(&key).expect("claim").values,
        Some(claim.values().expect("projection"))
    );
    let (commit, authority) = claimed_commit(&catalog, &cell, &claim);
    assert!(
        commit.writes.is_empty(),
        "retained claim supplies read-bound evidence"
    );
    cell.commit_observed(commit, authority)
        .expect("retained immutable claim");
    let mut second = claim;
    second.provenance.source.system = "synthetic-b".into();
    second.observation_id = "bundle:second".into();
    let (commit, authority) = claimed_commit(&catalog, &cell, &second);
    assert!(
        commit.links.is_empty(),
        "corroboration adds provenance, not another edge"
    );
    cell.commit_observed(commit, authority)
        .expect("second source");
    assert!(
        cell.load(&FactKey::new(
            norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
            second.id().expect("id")
        ))
        .expect("second provenance")
        .values
        .is_some()
    );
}
#[test]
fn obs_link_wrong_endpoint_type_refuses_claim_and_edge_atomically() {
    let (types, mut cell, mut claim) = claim_fixture();
    let target = claim.target.as_mut().expect("target");
    target.object_type = norn_ir::ObjectTypeName("test.Source".into());
    target.key = "one".into();
    let catalog = norn_kernel::TypeCatalog::new(types.iter());
    let (commit, authority) = claimed_commit(&catalog, &cell, &claim);
    assert!(matches!(
        cell.commit_observed(commit, authority),
        Err(norn_kernel::CommitError::LinkTarget { .. })
    ));
    assert!(
        cell.load(&FactKey::new(
            norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
            claim.id().expect("id")
        ))
        .expect("absent claim")
        .values
        .is_none()
    );
}
#[test]
fn obs_link_missing_endpoint_refuses_claim_and_edge_atomically() {
    let (types, mut cell, mut claim) = claim_fixture();
    claim.target.as_mut().expect("target").key = "missing".into();
    let catalog = norn_kernel::TypeCatalog::new(types.iter());
    let (commit, authority) = claimed_commit(&catalog, &cell, &claim);
    assert!(matches!(
        cell.commit_observed(commit, authority),
        Err(norn_kernel::CommitError::DanglingLink { .. })
    ));
    assert!(
        cell.load(&FactKey::new(
            norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
            claim.id().expect("id")
        ))
        .expect("absent claim")
        .values
        .is_none()
    );
}

#[test]
fn obs_link_authored_subtype_claims_refuse_at_identity_root() {
    let (_, mut cell, claim) = claim_fixture();
    let forged = CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new("test.ClaimAlias", "forged"),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: claim.values().expect("claim fields"),
            },
        }],
        ..CellCommit::empty()
    };
    assert!(matches!(
        cell.commit(forged),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
}

/// Generic observations confer no content-addressed claim authority.
fn generic_claim_attempt(
    catalog: &norn_kernel::TypeCatalog<'_>,
    cell: &ClaimCell,
    key: &FactKey,
    claim: &norn_ir::mapping::ObservedLinkClaim,
) -> (
    CellCommit<norn_kernel::ObservationEvent, JsonObject, JsonObject>,
    norn_kernel::ObservationAuthorization,
) {
    let slice = norn_kernel::load_slice(cell, [key.clone()]).expect("claim state");
    let turn = norn_kernel::DecisionTurn {
        id: claim.observation_id.clone(),
        now: norn_kernel::LogicalTime::EPOCH,
        world: norn_kernel::World::Main,
        consumes: None,
    };
    norn_kernel::snapshot_observe::prepare_observed_snapshot(
        catalog,
        &slice,
        &turn,
        norn_kernel::snapshot_observe::ObservedSnapshot {
            key,
            values: &claim.values().expect("values"),
            lifecycles: &[],
            provenance: &claim.provenance,
        },
    )
    .expect("generic snapshot")
    .into_parts()
}

#[test]
fn obs_link_generic_claim_creation_and_existing_claim_rewrite_refuse() {
    let (types, mut cell, claim) = claim_fixture();
    let catalog = norn_kernel::TypeCatalog::new(types.iter());
    let actual = claim.id().expect("id");
    for identity in ["forged-identity".to_owned(), actual.clone()] {
        let key = FactKey::new(norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE, identity);
        let (commit, authority) = generic_claim_attempt(&catalog, &cell, &key, &claim);
        assert!(matches!(
            cell.commit_observed(commit, authority),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ));
        assert!(cell.load(&key).expect("unchanged").values.is_none());
    }
    let (commit, authority) = claimed_commit(&catalog, &cell, &claim);
    cell.commit_observed(commit, authority)
        .expect("sealed claim");
    let key = FactKey::new(norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE, actual);
    let original = cell.load(&key).expect("original");
    let mut changed = claim;
    changed.observation_id = "different-evidence".into();
    let (commit, authority) = generic_claim_attempt(&catalog, &cell, &key, &changed);
    assert!(matches!(
        cell.commit_observed(commit, authority),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    assert_eq!(cell.load(&key).expect("retained"), original);
}

#[test]
fn obs_link_extension_defaults_seal_and_redeliver_but_changed_defaults_require_migration() {
    let (types, cell, claim) = claim_fixture();
    let extension: norn_ir::ObjectTypeExtension = serde_json::from_value(serde_json::json!({
        "namespace":"demo", "target":"norn.ObservedLinkClaim",
        "properties":[{"id":"demo__note", "type":{"kind":"text"}, "default":"reviewed"}]
    }))
    .expect("extension");
    let diagnostics = norn_checker::check_entity_model(
        &norn_checker::EntityModel {
            interfaces: &[],
            object_types: &[],
            extensions: std::slice::from_ref(&extension),
        },
        &norn_checker::EntityContext {
            catalog: &[],
            registry: &std::collections::BTreeSet::new(),
            tenant: None,
            base: norn_checker::EntityModel {
                interfaces: &[],
                object_types: &types,
                extensions: &[],
            },
        },
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let catalog = norn_kernel::TypeCatalog::new(&types).with_extensions([&extension]);
    let mut resolved = types.clone();
    resolved
        .iter_mut()
        .find(|t| t.name.as_str() == norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE)
        .expect("claim type")
        .properties
        .extend(extension.properties.clone());
    let mut cell = cell.with_types(resolved);
    let (commit, authority) = claimed_commit(&catalog, &cell, &claim);
    cell.commit_observed(commit, authority)
        .expect("extended claim");
    let key = FactKey::new(
        norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
        claim.id().expect("id"),
    );
    let retained = cell.load(&key).expect("retained");
    assert_eq!(
        retained.values.as_ref().expect("values")["demo__note"],
        norn_ir::RuntimeValue::from("reviewed")
    );
    let (commit, authority) = claimed_commit(&catalog, &cell, &claim);
    assert!(commit.writes.is_empty());
    cell.commit_observed(commit, authority)
        .expect("retained extended claim");
    let mut evolved = extension;
    evolved.properties[0].default =
        Some(serde_json::from_value(serde_json::json!("changed")).expect("default"));
    let evolved_catalog = norn_kernel::TypeCatalog::new(&types).with_extensions([&evolved]);
    let slice = norn_kernel::load_slice(&cell, [key.clone()]).expect("slice");
    let turn = norn_kernel::DecisionTurn {
        id: claim.observation_id.clone(),
        now: norn_kernel::LogicalTime::EPOCH,
        world: norn_kernel::World::Main,
        consumes: None,
    };
    assert!(
        norn_kernel::snapshot_observe::prepare_observed_link_claim_snapshot(
            &evolved_catalog,
            &slice,
            &turn,
            &claim
        )
        .is_err()
    );
    assert_eq!(cell.load(&key).expect("unchanged"), retained);
}

/// Load immutable evidence into the same turn as a relationship replacement.
fn append_retained_claim(
    catalog: &norn_kernel::TypeCatalog<'_>,
    cell: &ClaimCell,
    claim: &norn_ir::mapping::ObservedLinkClaim,
    commit: &mut CellCommit<norn_kernel::ObservationEvent, JsonObject, JsonObject>,
    authority: &mut norn_kernel::ObservationAuthorization,
) {
    let key = FactKey::new(
        norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
        claim.id().expect("id"),
    );
    let slice = norn_kernel::load_slice(cell, [key]).expect("slice");
    let turn = norn_kernel::DecisionTurn {
        id: claim.observation_id.clone(),
        now: norn_kernel::LogicalTime::EPOCH,
        world: norn_kernel::World::Main,
        consumes: None,
    };
    norn_kernel::snapshot_observe::prepare_observed_link_claim_snapshot(
        catalog, &slice, &turn, claim,
    )
    .expect("claim evidence")
    .append_to(commit, authority)
    .expect("same turn");
}

#[test]
fn obs_link_supersession_requires_prepared_successor_and_retains_old_evidence() {
    for multiplicity in [norn_ir::Multiplicity::One, norn_ir::Multiplicity::Many] {
        let (mut types, cell, old) = claim_fixture();
        types[0].links[0].cardinality.to = multiplicity;
        let mut cell = cell.with_types(types.clone());
        let catalog = norn_kernel::TypeCatalog::new(&types);
        let (commit, authority) = claimed_commit(&catalog, &cell, &old);
        cell.commit_observed(commit, authority)
            .expect("original claim");
        let source = FactKey::new(old.source_type.as_str(), &old.source_key);
        let key = LinkKey::new(
            source.clone(),
            old.role.as_str(),
            FactKey::new("test.Target", "two"),
        );
        let removal = LinkWrite {
            key: key.clone(),
            expected: cell.load_link(&key).expect("link").version,
            mutation: LinkMutation::Remove,
        };
        let (mut commit, authority) = claimed_commit(&catalog, &cell, &old);
        commit.links.push(removal.clone());
        assert!(matches!(
            cell.commit_observed(commit, authority),
            Err(norn_kernel::CommitError::ObservationAuthorization)
        ));
        let mut successor = old.clone();
        successor.observation_id = "bundle:successor".into();
        successor.target.as_mut().expect("target").key = "other".into();
        let (mut commit, mut authority) = claimed_commit(&catalog, &cell, &successor);
        commit.links.push(removal);
        append_retained_claim(&catalog, &cell, &old, &mut commit, &mut authority);
        let read = norn_kernel::FactExpectation {
            key: source.clone(),
            expected: cell.load(&source).expect("source").version,
        };
        authority
            .seal_observed_link_claim(&catalog, &old, read, &commit)
            .expect("backed withdrawal");
        cell.commit_observed(commit, authority)
            .expect("replacement");
        assert!(cell.load_link(&key).expect("old link").values.is_none());
        let replacement = LinkKey::new(
            source,
            old.role.as_str(),
            FactKey::new("test.Target", "other"),
        );
        assert!(
            cell.load_link(&replacement)
                .expect("new link")
                .values
                .is_some()
        );
        let old_key = FactKey::new(
            norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
            old.id().expect("id"),
        );
        assert_eq!(
            cell.load(&old_key).expect("evidence retained").values,
            Some(old.values().expect("values"))
        );
    }
}

#[test]
fn obs_link_clear_requires_old_claim_backing_and_preserves_authored_edges() {
    for backed in [false, true] {
        let (types, mut cell, old) = claim_fixture();
        let catalog = norn_kernel::TypeCatalog::new(&types);
        let source = FactKey::new(old.source_type.as_str(), &old.source_key);
        let key = LinkKey::new(
            source.clone(),
            old.role.as_str(),
            FactKey::new("test.Target", "two"),
        );
        if backed {
            let (commit, authority) = claimed_commit(&catalog, &cell, &old);
            cell.commit_observed(commit, authority).expect("old claim");
        } else {
            cell.commit(CellCommit {
                links: vec![LinkWrite {
                    key: key.clone(),
                    expected: norn_kernel::FactVersion(0),
                    mutation: LinkMutation::Add {
                        values: JsonObject::new(),
                    },
                }],
                ..CellCommit::empty()
            })
            .expect("authored edge");
        }
        let mut clear = old.clone();
        clear.observation_id = "bundle:clear".into();
        clear.target = None;
        let read = norn_kernel::FactExpectation {
            key: source.clone(),
            expected: cell.load(&source).expect("source").version,
        };
        let mut commit = CellCommit {
            reads: vec![read.clone()],
            links: vec![LinkWrite {
                key: key.clone(),
                expected: cell.load_link(&key).expect("link").version,
                mutation: LinkMutation::Remove,
            }],
            ..CellCommit::empty()
        };
        let mut authority = norn_kernel::ObservationAuthorization::empty();
        append_retained_claim(&catalog, &cell, &clear, &mut commit, &mut authority);
        if backed {
            append_retained_claim(&catalog, &cell, &old, &mut commit, &mut authority);
            authority
                .seal_observed_link_claim(&catalog, &old, read.clone(), &commit)
                .expect("old and successor evidence");
        }
        authority
            .seal_observed_link_claim(&catalog, &clear, read, &commit)
            .expect("clear evidence");
        let result = cell.commit_observed(commit, authority);
        if backed {
            result.expect("backed clear");
        } else {
            assert!(matches!(
                result,
                Err(norn_kernel::CommitError::ObservationAuthorization)
            ));
        }
        assert_eq!(cell.load_link(&key).expect("edge").values.is_none(), backed);
    }
}

#[test]
fn obs_link_foreign_capture_namespace_cannot_authorize_a_withdrawal() {
    let (types, mut cell, old) = claim_fixture();
    let catalog = norn_kernel::TypeCatalog::new(&types);
    let (commit, authority) = claimed_commit(&catalog, &cell, &old);
    cell.commit_observed(commit, authority).expect("original");
    let mut foreign = old.clone();
    foreign.source_scope = "another connector".into();
    foreign.observation_id = "foreign observation".into();
    foreign.target.as_mut().expect("target").key = "other".into();
    let (mut commit, mut authority) = claimed_commit(&catalog, &cell, &foreign);
    let source = FactKey::new(old.source_type.as_str(), &old.source_key);
    let key = LinkKey::new(
        source.clone(),
        old.role.as_str(),
        FactKey::new("test.Target", "two"),
    );
    commit.links.push(LinkWrite {
        key: key.clone(),
        expected: cell.load_link(&key).expect("old link").version,
        mutation: LinkMutation::Remove,
    });
    append_retained_claim(&catalog, &cell, &old, &mut commit, &mut authority);
    let read = norn_kernel::FactExpectation {
        key: source.clone(),
        expected: cell.load(&source).expect("source").version,
    };
    authority
        .seal_observed_link_claim(&catalog, &old, read, &commit)
        .expect("checked evidence");
    assert!(matches!(
        cell.commit_observed(commit, authority),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    assert!(cell.load_link(&key).expect("unchanged").values.is_some());
}

/// Commit `old`, then its replacement `successor`, through the backed withdrawal.
fn replace_claim(
    catalog: &norn_kernel::TypeCatalog<'_>,
    cell: &mut ClaimCell,
    old: &norn_ir::mapping::ObservedLinkClaim,
    successor: &norn_ir::mapping::ObservedLinkClaim,
) {
    let source = FactKey::new(old.source_type.as_str(), &old.source_key);
    let key = LinkKey::new(
        source.clone(),
        old.role.as_str(),
        FactKey::new("test.Target", "two"),
    );
    let removal = LinkWrite {
        key: key.clone(),
        expected: cell.load_link(&key).expect("link").version,
        mutation: LinkMutation::Remove,
    };
    let (mut commit, mut authority) = claimed_commit(catalog, cell, successor);
    commit.links.push(removal);
    append_retained_claim(catalog, cell, old, &mut commit, &mut authority);
    let read = norn_kernel::FactExpectation {
        key: source.clone(),
        expected: cell.load(&source).expect("source").version,
    };
    authority
        .seal_observed_link_claim(catalog, old, read, &commit)
        .expect("backed withdrawal");
    cell.commit_observed(commit, authority)
        .expect("replacement");
}

#[test]
fn obs_link_stale_claim_cannot_reverse_its_retained_replacement() {
    let (mut types, cell, old) = claim_fixture();
    types[0].links[0].cardinality.to = norn_ir::Multiplicity::Many;
    let mut cell = cell.with_types(types.clone());
    let catalog = norn_kernel::TypeCatalog::new(&types);
    let (commit, authority) = claimed_commit(&catalog, &cell, &old);
    cell.commit_observed(commit, authority).expect("original");
    let mut successor = old.clone();
    successor.observation_id = "bundle:successor".into();
    successor.target.as_mut().expect("target").key = "other".into();
    replace_claim(&catalog, &mut cell, &old, &successor);
    let source = FactKey::new(old.source_type.as_str(), &old.source_key);
    let stale = LinkKey::new(
        source.clone(),
        old.role.as_str(),
        FactKey::new("test.Target", "two"),
    );
    let current = LinkKey::new(
        source.clone(),
        old.role.as_str(),
        FactKey::new("test.Target", "other"),
    );
    let read = norn_kernel::FactExpectation {
        key: source.clone(),
        expected: cell.load(&source).expect("source").version,
    };
    let mut commit = CellCommit {
        reads: vec![read.clone()],
        links: vec![
            LinkWrite {
                key: current.clone(),
                expected: cell.load_link(&current).expect("current").version,
                mutation: LinkMutation::Remove,
            },
            LinkWrite {
                key: stale.clone(),
                expected: cell.load_link(&stale).expect("stale").version,
                mutation: LinkMutation::Add {
                    values: JsonObject::new(),
                },
            },
        ],
        ..CellCommit::empty()
    };
    let mut authority = norn_kernel::ObservationAuthorization::empty();
    append_retained_claim(&catalog, &cell, &old, &mut commit, &mut authority);
    append_retained_claim(&catalog, &cell, &successor, &mut commit, &mut authority);
    for claim in [&old, &successor] {
        authority
            .seal_observed_link_claim(&catalog, claim, read.clone(), &commit)
            .expect("retained evidence");
    }
    assert!(matches!(
        cell.commit_observed(commit, authority),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    assert!(cell.load_link(&stale).expect("stale").values.is_none());
    assert!(cell.load_link(&current).expect("current").values.is_some());
}

#[test]
fn obs_link_withdrawal_refuses_while_another_connector_supports_the_edge() {
    let (mut types, cell, old) = claim_fixture();
    types[0].links[0].cardinality.to = norn_ir::Multiplicity::Many;
    let mut cell = cell.with_types(types.clone());
    let catalog = norn_kernel::TypeCatalog::new(&types);
    let (commit, authority) = claimed_commit(&catalog, &cell, &old);
    cell.commit_observed(commit, authority).expect("original");
    let mut corroboration = old.clone();
    corroboration.source_scope = "another connector".into();
    corroboration.observation_id = "bundle:corroboration".into();
    let (commit, authority) = claimed_commit(&catalog, &cell, &corroboration);
    cell.commit_observed(commit, authority)
        .expect("corroboration");
    let mut successor = old.clone();
    successor.observation_id = "bundle:successor".into();
    successor.target.as_mut().expect("target").key = "other".into();
    let source = FactKey::new(old.source_type.as_str(), &old.source_key);
    let key = LinkKey::new(
        source.clone(),
        old.role.as_str(),
        FactKey::new("test.Target", "two"),
    );
    let (mut commit, mut authority) = claimed_commit(&catalog, &cell, &successor);
    commit.links.push(LinkWrite {
        key: key.clone(),
        expected: cell.load_link(&key).expect("link").version,
        mutation: LinkMutation::Remove,
    });
    append_retained_claim(&catalog, &cell, &old, &mut commit, &mut authority);
    append_retained_claim(&catalog, &cell, &corroboration, &mut commit, &mut authority);
    let read = norn_kernel::FactExpectation {
        key: source.clone(),
        expected: cell.load(&source).expect("source").version,
    };
    for claim in [&old, &corroboration] {
        authority
            .seal_observed_link_claim(&catalog, claim, read.clone(), &commit)
            .expect("retained evidence");
    }
    assert!(matches!(
        cell.commit_observed(commit, authority),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    assert!(cell.load_link(&key).expect("supported").values.is_some());
}

#[test]
fn obs_link_convergent_add_refuses_non_default_link_values() {
    let (mut types, cell, claim) = claim_fixture();
    types[0].links[0].properties = serde_json::from_value(serde_json::json!([
        {"id":"note", "type":{"kind":"text"}, "default":"plain"}
    ]))
    .expect("link property");
    let mut cell = cell.with_types(types.clone());
    let catalog = norn_kernel::TypeCatalog::new(&types);
    let note =
        |value: &str| JsonObject::from([("note".into(), norn_ir::RuntimeValue::from(value))]);
    let (commit, authority) = claimed_commit_with(&catalog, &cell, &claim, note("forged"));
    assert!(matches!(
        cell.commit_observed(commit, authority),
        Err(norn_kernel::CommitError::ObservationAuthorization)
    ));
    let (commit, authority) = claimed_commit_with(&catalog, &cell, &claim, note("plain"));
    cell.commit_observed(commit, authority)
        .expect("default link values");
}
