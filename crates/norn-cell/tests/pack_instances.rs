#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    missing_docs,
    reason = "COMPLETE-UNITS correspondence"
)]
//! COMPLETE-UNITS: one generic pack, exact USD/g substitutions and durable receipts.
use norn_cell::{CellStore, InstalledDefinitionInput, Model, SqliteCell, StoreOptions};
use norn_ir::definition_catalog::{Member, MemberRef};
use norn_ir::mapping::InterpretationEndpoint;
use norn_ir::{
    ActionRoot, ArtifactEnvelope, ArtifactKind, ArtifactPayload, CompositionPackRoot, InterfaceDef,
    ObjectTypeDef, PackArtifactKind, PackArtifactRef, PackInstance, PackTypeUnitBinding, Property,
    PropertyConstraints, PropertyId, PropertyType, ResourceLimitProfile, RuntimeValue,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial,
    ActionOutcome, CellHost, DecisionTurn, FactEvent, LogicalTime, TypeCatalog, World,
    commit_action,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};
type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
struct Policy;
impl ActionAuthorizer for Policy {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, _: &norn_kernel::ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> { Ok(()) }
    fn authorize_read_type(&self, _: &norn_kernel::ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> { Ok(()) }
}
fn pin(a: &ArtifactEnvelope) -> InterpretationEndpoint {
    InterpretationEndpoint {
        artifact_id: a.artifact_id.clone(),
        version: a.version.clone(),
        sha256: norn_checker::ArtifactRef::of(a)
            .expect("semantic pin")
            .sha256,
    }
}
fn payload<T: serde::de::DeserializeOwned>(a: &ArtifactEnvelope) -> T {
    serde_json::from_value(serde_json::to_value(a.payload.to_json_value()).expect("wire"))
        .expect("typed payload")
}
fn set_pack(a: &mut ArtifactEnvelope, root: CompositionPackRoot) {
    a.payload = ArtifactPayload::from_kind_json(
        &ArtifactKind::BusinessPack,
        serde_json::from_value(serde_json::to_value(root).expect("typed pack")).expect("wire"),
    )
    .expect("payload");
}
#[allow(clippy::disallowed_methods, reason = "real typed fixture compiler")]
fn artifacts() -> Vec<ArtifactEnvelope> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let compiled = Command::new("bun").current_dir(root)
        .args("tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/pack-instances --org org_demo --env dev".split_whitespace()).output().expect("compiler");
    assert!(
        compiled.status.success(),
        "{} {}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut all: Vec<ArtifactEnvelope> =
        serde_json::from_slice(&compiled.stdout).expect("compiler IR");
    let generic = all
        .iter()
        .find(|a| a.artifact_id == "demo.GenericLedger.interface")
        .expect("generic")
        .clone();
    let mut pack = generic.clone();
    pack.artifact_id = "demo.ledger.pack".into();
    pack.dependencies.clear();
    set_pack(
        &mut pack,
        CompositionPackRoot {
            pack_id: "demo.ledger.pack".into(),
            pack_version: "1.0.0".into(),
            pack_kind: None,
            provides: None,
            type_parameters: [
                ("Ledger", "demo.GenericLedger"),
                ("Account", "demo.GenericAccount"),
            ]
            .into_iter()
            .map(|(slot, name)| {
                (
                    slot.into(),
                    MemberRef {
                        artifact_id: format!("{name}.interface"),
                        version: "1.0.0".into(),
                        member: Member::Type {
                            type_name: name.into(),
                        },
                    },
                )
            })
            .collect(),
            artifacts: [
                ("demo.GenericLedger.interface", PackArtifactKind::Entity),
                ("demo.GenericAccount.interface", PackArtifactKind::Entity),
                ("demo.ledger.balance.law", PackArtifactKind::Law),
            ]
            .into_iter()
            .map(|(id, kind)| PackArtifactRef {
                artifact_id: id.into(),
                version: "1.0.0".into(),
                kind,
                additional: BTreeMap::new(),
            })
            .collect(),
            instances: None,
            extensions: None,
        },
    );
    let instances = [("ledger-usd", "Usd"), ("ledger-fine", "Fine")]
        .into_iter()
        .map(|(id, prefix)| {
            let bindings = [("Ledger", "Transfer"), ("Account", "Account")]
                .into_iter()
                .map(|(slot, suffix)| {
                    let target = all
                        .iter()
                        .find(|a| a.artifact_id == format!("demo.{prefix}{suffix}.entity"))
                        .expect("concrete target");
                    let root: norn_ir::EntityRootV2 = payload(target);
                    (
                        slot.into(),
                        PackTypeUnitBinding {
                            target: pin(target),
                            type_name: root.object_type.name,
                            decimals: root
                                .object_type
                                .properties
                                .into_iter()
                                .filter(|p| matches!(p.id.as_str(), "debit" | "credit"))
                                .map(|p| (p.id.0, p.property_type))
                                .collect(),
                        },
                    )
                })
                .collect();
            PackInstance {
                instance_id: id.into(),
                scope: generic.scope.clone(),
                pack: pin(&pack),
                imports: BTreeMap::new(),
                instances: vec![],
                type_unit_bindings: bindings,
            }
        })
        .collect();
    let mut app = generic.clone();
    app.artifact_id = "demo.ledgers.app".into();
    app.dependencies.clear();
    set_pack(
        &mut app,
        CompositionPackRoot {
            pack_id: "demo.ledgers.app".into(),
            pack_version: "1.0.0".into(),
            pack_kind: None,
            provides: None,
            type_parameters: BTreeMap::new(),
            artifacts: vec![PackArtifactRef {
                artifact_id: pack.artifact_id.clone(),
                version: pack.version.clone(),
                kind: PackArtifactKind::BusinessPack,
                additional: BTreeMap::new(),
            }],
            instances: Some(instances),
            extensions: None,
        },
    );
    all.extend([pack, app]);
    let findings = norn_checker::check_artifact_set(&all, &context());
    assert!(findings.is_empty(), "valid baseline: {findings:?}");
    all
}
fn context() -> norn_checker::EntityArtifactSetContext<'static> {
    static REGISTRY: std::sync::LazyLock<BTreeSet<norn_ir::PropertyTypeName>> =
        std::sync::LazyLock::new(BTreeSet::new);
    norn_checker::EntityArtifactSetContext {
        registry: &REGISTRY,
        base: &[],
        tenant: None,
    }
}
fn definitions(
    all: &[ArtifactEnvelope],
) -> (Vec<ObjectTypeDef>, Vec<InterfaceDef>, Vec<ActionRoot>) {
    let mut types = Vec::<ObjectTypeDef>::new();
    let mut interfaces = Vec::new();
    let mut actions = Vec::new();
    for a in all {
        let wire = serde_json::to_value(a.payload.to_json_value()).expect("payload");
        if let Some(t) = wire.get("objectType") {
            types.push(
                serde_json::from_value(serde_json::to_value(t).expect("wire")).expect("type"),
            );
        }
        if let Some(i) = wire.get("interface") {
            interfaces.push(
                serde_json::from_value(serde_json::to_value(i).expect("wire")).expect("interface"),
            );
        }
        if a.kind() == ArtifactKind::Action {
            actions.push(payload(a));
        }
    }
    let mut receipt: ObjectTypeDef = types.first().expect("receipt template").clone();
    receipt.name = norn_ir::ObjectTypeName(ACTION_CALL_TYPE.into());
    receipt.implements.clear();
    receipt.invariants.clear();
    receipt.identity.clear();
    receipt.links.clear();
    receipt.properties = "actionId actionArtifactId actionVersion effectKind object input effectiveInput derivedValues preconditionOutcomes parameterReads actor turn transition from to rowOperations".split_whitespace().map(|name| Property {
        id: PropertyId(name.into()), description: None, property_type: PropertyType::JsonExt { schema: None }, constraints: PropertyConstraints { nullable: true, ..Default::default() }, default: None, markings: vec![], computed: None,
    }).collect();
    types.push(receipt);
    (types, interfaces, actions)
}
fn values(text: &str) -> JsonObject {
    RuntimeValue::from_json_text(text)
        .expect("exact")
        .as_object()
        .expect("object")
        .clone()
}
fn call<'a>(a: &'a ActionRoot, id: &str, input: JsonObject) -> ActionCall<'a> {
    ActionCall {
        action: a,
        action_artifact_id: &a.action_id,
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: "ledger:clerk".into(),
        input,
        idempotency_key: id.into(),
        object_type: None,
        turn: DecisionTurn {
            id: id.into(),
            now: LogicalTime::from_millis(1),
            world: World::Main,
            consumes: None,
        },
    }
}
#[test]
fn complete_units_two_pinned_instances_commit_and_reopen_without_currency_leakage() {
    let all = artifacts();
    let findings = norn_checker::check_artifact_set(&all, &context());
    assert!(findings.is_empty(), "{findings:?}");
    let instances = norn_checker::verification::effective_law_instances(
        &all,
        &context(),
        norn_graph::Budget {
            max_nodes: 4096,
            max_edges: 16384,
            max_steps: 65536,
        },
    )
    .expect("B2 law instances");
    let currency: Vec<_> = instances
        .iter()
        .filter(|i| i.law.artifact_id == "demo.ledger.usd.law")
        .collect();
    assert_eq!(currency.len(), 2);
    assert!(
        currency
            .iter()
            .all(|i| i.subject.artifact_id.starts_with("demo.Usd"))
    );
    let original_currency = all
        .iter()
        .find(|a| a.artifact_id == "demo.ledger.usd.law")
        .expect("original law");
    assert!(
        currency
            .iter()
            .all(|i| i.law_digest == pin(original_currency).sha256)
    );
    let owner = all
        .iter()
        .find(|a| a.artifact_id == "demo.ledgers.app")
        .expect("instances");
    let declarations: CompositionPackRoot = payload(owner);
    let nodes = declarations.instances.expect("two declarations");
    assert_eq!(nodes.len(), 2);
    assert_ne!(nodes[0].instance_id, nodes[1].instance_id);
    assert_eq!(nodes[0].pack, nodes[1].pack);
    assert_eq!(
        instances
            .iter()
            .filter(|i| i.law.artifact_id == "demo.ledger.balance.law")
            .count(),
        2
    );
    let (types, interfaces, actions) = definitions(&all);
    let model =
        Model::from_definitions_with_interfaces(&types, &[], &interfaces).expect("native model");
    let catalog = TypeCatalog::new(types.iter().filter(|t| t.name.as_str() != ACTION_CALL_TYPE))
        .with_interfaces(&interfaces);
    let raw = all
        .iter()
        .map(|a| norn_ir::runtime::encode(a).expect("source"))
        .collect::<Vec<_>>();
    let sources = norn_checker::ReceiptSources::new(&raw, &[]).expect("exact sources");
    let checked = norn_checker::check_artifact_set_with_receipt(
        &sources,
        &context(),
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("shared admission");
    assert!(checked.receipt.admitted);
    let receipt =
        String::from_utf8(checked.receipt.canonical_json().expect("receipt")).expect("UTF8");
    let canonical_set = serde_json::to_string(&all).expect("retained typed set");
    let folder = tempfile::tempdir().expect("database");
    let path = folder.path().join("instances.sqlite");
    let open =
        || CellStore::open(&path, model.clone(), StoreOptions::default()).expect("real cell");
    let mut store = open();
    let installed = store
        .install_definitions(
            model.clone(),
            InstalledDefinitionInput {
                canonical_set: canonical_set.clone(),
                registry: vec![],
                receipt_sha256: norn_checker::admission_receipt_sha256(&receipt).expect("digest"),
                admission_receipt: Some(receipt),
                readmission: None,
                diff: serde_json::json!({}),
                installed_at: 1,
                layout: None,
            },
            Some(0),
            "org_demo/dev",
        )
        .expect("atomic definition installation");
    let mut cell: Cell =
        SqliteCell::new(store, "org_demo/dev", |command: &norn_kernel::Command<JsonObject>| command.payload.clone()).expect("host");
    for (action_id, id, input) in [
        (
            "demo.ledger.open_usd",
            "cash",
            r#"{"id":"cash","ledger_id":"ledger-usd","currency":"USD"}"#,
        ),
        (
            "demo.ledger.open_usd",
            "sales",
            r#"{"id":"sales","ledger_id":"ledger-usd","currency":"USD"}"#,
        ),
        (
            "demo.ledger.open_fine",
            "vault",
            r#"{"id":"vault","ledger_id":"ledger-fine"}"#,
        ),
        (
            "demo.ledger.open_fine",
            "wip",
            r#"{"id":"wip","ledger_id":"ledger-fine"}"#,
        ),
    ] {
        let action = actions
            .iter()
            .find(|a| a.action_id == action_id)
            .expect("account action");
        commit_action(
            &mut cell,
            &catalog,
            &call(action, id, values(input)),
            &Policy,
        )
        .expect("governed account creation");
    }
    let requests = [
        (
            "demo.ledger.post_usd",
            "usd",
            r#"{"id":"usd","ledger_id":"ledger-usd","from_account":"cash","to_account":"sales","currency":"USD","debit":25,"credit":25}"#,
        ),
        (
            "demo.ledger.post_fine",
            "fine",
            r#"{"id":"fine","ledger_id":"ledger-fine","from_account":"vault","to_account":"wip","debit":100,"credit":100}"#,
        ),
    ];
    let mut originals = Vec::new();
    for (action_id, id, input) in requests {
        let action = actions
            .iter()
            .find(|a| a.action_id == action_id)
            .expect("governed action");
        let ActionOutcome::Committed { receipt, record } = commit_action(
            &mut cell,
            &catalog,
            &call(action, id, values(input)),
            &Policy,
        )
        .expect("ordinary evaluator and atomic commit") else {
            panic!("new transfer")
        };
        let durable = cell
            .store()
            .read()
            .receipt_at(receipt.seq.0)
            .expect("receipt lookup")
            .expect("durable receipt");
        assert_eq!(durable.body.definitions_version, installed.version);
        let posted = cell
            .load(&record.object)
            .expect("posted")
            .values
            .expect("values");
        assert_eq!(posted.get("debit"), posted.get("credit"));
        originals.push((action_id, id, input, record, durable));
    }
    let usd = actions
        .iter()
        .find(|a| a.action_id == "demo.ledger.post_usd")
        .expect("USD action");
    for (key, input) in [
        (
            "eur",
            r#"{"id":"eur","ledger_id":"ledger-usd","from_account":"cash","to_account":"sales","currency":"EUR","debit":25,"credit":25}"#,
        ),
        (
            "unbalanced",
            r#"{"id":"unbalanced","ledger_id":"ledger-usd","from_account":"cash","to_account":"sales","currency":"USD","debit":25,"credit":100}"#,
        ),
        (
            "wrong-instance",
            r#"{"id":"wrong-instance","ledger_id":"ledger-fine","from_account":"cash","to_account":"sales","currency":"USD","debit":25,"credit":25}"#,
        ),
    ] {
        let before = cell.store().read().dump().expect("before");
        assert!(
            commit_action(&mut cell, &catalog, &call(usd, key, values(input)), &Policy).is_err()
        );
        assert_eq!(before, cell.store().read().dump().expect("atomic refusal"));
    }
    drop(cell);
    let mut cell: Cell =
        SqliteCell::new(open(), "org_demo/dev", |command: &norn_kernel::Command<JsonObject>| command.payload.clone()).expect("reopen");
    assert_eq!(
        cell.store()
            .read()
            .installed_definitions(Some(installed.version))
            .expect("retained version")
            .expect("definition set")
            .canonical_set,
        canonical_set
    );
    for (action_id, id, input, original, durable) in originals {
        assert_eq!(
            cell.store()
                .read()
                .receipt_at(durable.body.commit_seq)
                .expect("durable receipt"),
            Some(durable)
        );
        let action = actions
            .iter()
            .find(|a| a.action_id == action_id)
            .expect("action");
        assert!(
            matches!(commit_action(&mut cell, &catalog, &call(action, id, values(input)), &Policy).expect("replay"), ActionOutcome::Replayed { record } if record == original)
        );
    }
}
fn corrupted(mut all: Vec<ArtifactEnvelope>, case: u8) -> Vec<ArtifactEnvelope> {
    let owner = all
        .iter_mut()
        .find(|a| a.artifact_id == "demo.ledgers.app")
        .expect("declarations");
    let mut root: CompositionPackRoot = payload(owner);
    let usd = root
        .instances
        .as_mut()
        .expect("instances")
        .first_mut()
        .expect("USD");
    match case {
        0 => {
            usd.type_unit_bindings.clear();
        }
        1 => {
            usd.type_unit_bindings
                .get_mut("Ledger")
                .expect("slot")
                .target
                .sha256 = "0".repeat(64);
        }
        2 => {
            usd.type_unit_bindings
                .get_mut("Ledger")
                .expect("slot")
                .decimals
                .remove("credit");
        }
        3 => {
            usd.type_unit_bindings
                .get_mut("Ledger")
                .expect("slot")
                .type_name = norn_ir::ObjectTypeName("demo.FineTransfer".into());
        }
        _ => {
            usd.type_unit_bindings
                .get_mut("Ledger")
                .expect("slot")
                .decimals
                .insert("credit".into(), PropertyType::Integer {});
        }
    }
    set_pack(owner, root);
    all
}
#[test]
fn incomplete_stale_or_mixed_instance_bindings_refuse_before_install() {
    let source = artifacts();
    for case in 0..5 {
        let findings =
            norn_checker::check_artifact_set(&corrupted(source.clone(), case), &context());
        let expected = match case {
            1 => "core.composition.unresolved_pin",
            4 => "core.values.unit_mismatch",
            _ => "core.composition.mapping_gap",
        };
        assert!(
            findings.iter().any(|d| d.code.as_str() == expected),
            "case {case}: {findings:?}"
        );
    }
}
norn_testkit::proptest::proptest! {
    #[test]
    fn generated_stale_target_pins_never_bind_a_pack_instance(hash in "[0-9a-f]{64}") {
        static SOURCE: std::sync::LazyLock<Vec<ArtifactEnvelope>> = std::sync::LazyLock::new(artifacts);
        let mut all = SOURCE.clone();
        let owner = all.iter_mut().find(|a| a.artifact_id == "demo.ledgers.app").expect("owner");
        let mut root: CompositionPackRoot = payload(owner);
        let pin = &mut root.instances.as_mut().expect("instances")[0].type_unit_bindings.get_mut("Ledger").expect("slot").target;
        norn_testkit::proptest::prop_assume!(pin.sha256 != hash);
        pin.sha256 = hash; set_pack(owner, root);
        let findings = norn_checker::check_artifact_set(&all, &context());
        norn_testkit::proptest::prop_assert!(findings.iter().any(|d| d.code.as_str() == "core.composition.unresolved_pin"));
    }
}

#[test]
fn pack_law_substitution_follows_only_the_selected_instance() {
    let base = artifacts();
    let mut law = base
        .iter()
        .find(|a| a.artifact_id == "demo.ledger.balance.law")
        .expect("generic law")
        .clone();
    law.artifact_id = "demo.ledger.scoped.law".into();
    let mut root: norn_ir::LawIR = payload(&law);
    root.id = "demo.ledger.scoped".into();
    root.scope.root = norn_ir::LawRoot::Pack("ledger-usd".into());
    law.payload = ArtifactPayload::from_kind_json(
        &ArtifactKind::Law,
        serde_json::from_value(serde_json::to_value(root).expect("typed law")).expect("tree"),
    )
    .expect("payload");
    let instances = norn_checker::verification::effective_law_instances(
        &[law],
        &norn_checker::EntityArtifactSetContext {
            registry: context().registry,
            base: &base,
            tenant: None,
        },
        norn_graph::Budget {
            max_nodes: 4096,
            max_edges: 16384,
            max_steps: 65536,
        },
    )
    .expect("checked instance scope");
    let scoped: Vec<_> = instances
        .iter()
        .filter(|i| i.law.artifact_id == "demo.ledger.scoped.law")
        .collect();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].subject.artifact_id, "demo.UsdTransfer.entity");
    assert!(
        scoped[0]
            .instance_paths
            .iter()
            .all(|p| p.last().expect("binding path").instance_id == "ledger-usd")
    );
}
