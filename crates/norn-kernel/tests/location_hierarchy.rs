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
