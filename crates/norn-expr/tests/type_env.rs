#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! The `norn.expr.env/1` type environment derived from an `EntityIR` v2
//! artifact set (T1-EXPR-2, #2519), its golden snapshot and content hash, and
//! the closed actor operand (#2419).

mod support;

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::actor::{ACTOR_KINDS, ACTOR_RECORD, ACTOR_SCOPE_RECORD};
use norn_expr::{
    EnvError, EnvOptions, FieldSource, RecordKind, TYPE_ENV_ID, TYPE_ENV_VERSION, Type, ZenTypeEnv,
};
use norn_ir::{
    InterfaceDef, InterfaceName, ObjectTypeName, PropertyType, PropertyTypeName,
    ResourceLimitProfile, TextFormat, TimezonePolicy, UnitRef, UnitSystem,
};
use support::{Model, fields, name};

/// Pinned content hash of the retail fixture environment (no `self`/`input`
/// bound). A change here means the derivation or the canonical form changed:
/// that is a new `TYPE_ENV_VERSION`, not an edit of the golden.
const RETAIL_ENV_SHA256: &str = "904280e5f3015c9f85d4557d326cef2783b0d84a4fe8304fc46b2a6e4b4250cd";

const GOLDEN: &str = "tests/golden/retail_env.json";

fn pretty(bytes: &[u8]) -> String {
    let value: serde_json::Value = serde_json::from_slice(bytes).expect("canonical JSON");
    let mut text = serde_json::to_string_pretty(&value).expect("pretty JSON");
    text.push('\n');
    text
}

#[test]
fn retail_env_matches_golden_snapshot() {
    let env = Model::retail().env();
    let actual = pretty(&env.canonical_json().expect("canonical"));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("NORN_BLESS").is_some() {
        std::fs::write(&path, &actual).expect("write golden");
    }
    let expected = std::fs::read_to_string(&path).expect("read golden");
    assert_eq!(actual, expected, "set NORN_BLESS=1 to regenerate {GOLDEN}");
}

#[test]
fn retail_env_hash_is_pinned() {
    let env = Model::retail().env();
    let reference = env.reference().expect("hash");
    assert_eq!(reference.id, TYPE_ENV_ID);
    assert_eq!(reference.version, TYPE_ENV_VERSION);
    assert_eq!(
        reference.sha256, RETAIL_ENV_SHA256,
        "environment bytes changed: bump TYPE_ENV_VERSION instead of editing the pin"
    );
}

#[test]
fn derivation_is_deterministic_and_order_independent() {
    let model = Model::retail();
    let mut reversed = Model::retail();
    reversed.object_types.reverse();
    reversed.property_types.reverse();
    assert_eq!(
        model.env().content_sha256().unwrap(),
        reversed.env().content_sha256().unwrap()
    );
}

#[test]
fn every_binding_and_option_changes_the_hash() {
    let model = Model::retail();
    let base = model.env();
    let base_hash = base.content_sha256().unwrap();
    let variants = [
        base.clone().bind_self(&name("retail.Sku")).unwrap(),
        base.bind_input(fields([("quantity", Type::Integer)]))
            .unwrap(),
        ZenTypeEnv::from_artifact_set(&model.definitions(), EnvOptions { max_link_depth: 2 })
            .unwrap(),
    ];
    for variant in variants {
        assert_ne!(variant.content_sha256().unwrap(), base_hash);
    }
    let mut changed = Model::retail();
    changed.object_types[0].properties[2].property_type = PropertyType::Decimal {
        scale: norn_ir::WholeNumber(3),
        unit: None,
        currency_source: None,
    };
    assert_ne!(changed.env().content_sha256().unwrap(), base_hash);
}

#[test]
fn link_depth_defaults_to_the_limit_profile_relation_depth() {
    let profile = ResourceLimitProfile::norn_limits_v1();
    assert_eq!(
        EnvOptions::default().max_link_depth,
        profile.slice.max_relation_depth
    );
    assert_eq!(
        EnvOptions::from_limit_profile(&profile),
        EnvOptions::default()
    );
}

fn field(env: &ZenTypeEnv, record: &str, member: &str) -> (Type, FieldSource) {
    let field = env
        .record(record)
        .unwrap_or_else(|| panic!("no record {record}"))
        .fields
        .get(member)
        .unwrap_or_else(|| panic!("no member {record}.{member}"));
    (field.ty.clone(), field.source.clone())
}

fn record(name: &str) -> Type {
    Type::Record(name.to_owned())
}

#[test]
fn properties_map_to_typed_fields() {
    let env = Model::retail().env();
    let sku = "retail.Sku";
    let property = |ty| (ty, FieldSource::Property { read_only: false });
    assert_eq!(field(&env, sku, "sku_code"), property(Type::text()));
    assert_eq!(
        field(&env, sku, "price"),
        property(Type::Decimal {
            scale: 2,
            unit: None
        })
    );
    // `ref` resolves; the property's own `nullable` makes it optional.
    assert_eq!(
        field(&env, sku, "weight"),
        property(
            Type::Decimal {
                scale: 3,
                unit: Some(UnitRef {
                    system: UnitSystem::Ucum,
                    code: "kg".to_owned()
                })
            }
            .optional()
        )
    );
    // Enums are literal unions, sorted.
    assert_eq!(
        field(&env, sku, "status"),
        property(Type::Enum {
            values: vec!["active".into(), "discontinued".into(), "draft".into()]
        })
    );
    assert_eq!(
        field(&env, sku, "launched_at"),
        property(Type::Timestamp {
            timezone: TimezonePolicy::Utc
        })
    );
    assert_eq!(
        field(&env, sku, "launch_date"),
        property(Type::Date.optional())
    );
    assert_eq!(field(&env, sku, "shelf_life"), property(Type::Duration));
    assert_eq!(field(&env, sku, "is_taxable"), property(Type::Bool));
    assert_eq!(field(&env, sku, "stock"), property(Type::Integer));
    assert_eq!(field(&env, sku, "attributes"), property(Type::Json));
    assert_eq!(
        field(&env, sku, "display_name"),
        (Type::text(), FieldSource::Property { read_only: true })
    );
    assert_eq!(
        field(&env, "crm.Customer", "email"),
        property(Type::Text {
            format: Some(TextFormat::Email)
        })
    );
    // Nullability anywhere on the `ref` chain (short_note → note) widens.
    assert_eq!(
        field(&env, "crm.Customer", "note"),
        property(Type::text().optional())
    );
}

#[test]
fn links_carry_cardinality_and_inverses() {
    let env = Model::retail().env();
    // to: one, required → the record itself.
    assert_eq!(
        field(&env, "commerce.SalesOrder", "customer"),
        (record("crm.Customer"), FieldSource::Link { required: true })
    );
    // to: many → a list.
    assert_eq!(
        field(&env, "commerce.SalesOrder", "lines"),
        (
            record("commerce.OrderLine").list(),
            FieldSource::Link { required: false }
        )
    );
    // Inverse roles take `cardinality.from` and are never required.
    assert_eq!(
        field(&env, "crm.Customer", "orders"),
        (
            record("commerce.SalesOrder").list(),
            FieldSource::Inverse {
                link: "commerce.SalesOrder.customer".into()
            }
        )
    );
    assert_eq!(
        field(&env, "commerce.OrderLine", "order"),
        (
            record("commerce.SalesOrder").optional(),
            FieldSource::Inverse {
                link: "commerce.SalesOrder.lines".into()
            }
        )
    );
    // A link to an interface: the inverse lands on the interface and on every
    // implementor, including subtypes of implementors.
    let order_lines = (
        record("commerce.OrderLine").list(),
        FieldSource::Inverse {
            link: "commerce.OrderLine.item".into(),
        },
    );
    for holder in ["commerce.Item", "retail.Sku", "retail.BundleSku"] {
        assert_eq!(field(&env, holder, "order_lines"), order_lines, "{holder}");
    }
    assert_eq!(
        field(&env, "commerce.OrderLine", "item"),
        (
            record("commerce.Item"),
            FieldSource::Link { required: true }
        )
    );
}

#[test]
fn inheritance_and_extensions_are_flattened() {
    let env = Model::retail().env();
    let bundle = env.record("retail.BundleSku").unwrap();
    let sku = env.record("retail.Sku").unwrap();
    assert_eq!(bundle.kind, RecordKind::Object);
    for member in sku.fields.keys() {
        assert!(bundle.fields.contains_key(member), "inherits {member}");
    }
    assert_eq!(
        field(&env, "retail.BundleSku", "components"),
        (
            record("retail.Sku").list(),
            FieldSource::Link { required: false }
        )
    );
    assert!(!sku.fields.contains_key("components"));
    assert_eq!(
        field(&env, "crm.Customer", "acme__loyalty_points").0,
        Type::Integer
    );
    assert_eq!(
        field(&env, "crm.Customer", "acme__referrer").0,
        record("crm.Customer").optional()
    );
    assert_eq!(
        field(&env, "crm.Customer", "acme__referrals").0,
        record("crm.Customer").list()
    );
    let item = env.record("commerce.Item").unwrap();
    assert_eq!(item.kind, RecordKind::Interface);
    assert_eq!(
        item.fields.keys().map(String::as_str).collect::<Vec<_>>(),
        ["name", "order_lines", "sku_code"]
    );
}

#[test]
fn only_actor_is_bound_until_a_site_binds_self_and_input() {
    let env = Model::retail().env();
    assert_eq!(
        env.variables()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["actor"]
    );
    let env = env
        .bind_self(&name("retail.Sku"))
        .unwrap()
        .bind_input(fields([
            ("quantity", Type::Integer),
            ("customer", record("crm.Customer")),
        ]))
        .unwrap();
    assert_eq!(
        env.variables()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["actor", "input", "self"]
    );
    assert!(matches!(
        env.clone().bind_self(&name("retail.Sku")),
        Err(EnvError::AlreadyBound { .. })
    ));
    assert!(matches!(
        env.bind_input(BTreeMap::new()),
        Err(EnvError::AlreadyBound { .. })
    ));
}

#[test]
fn expression_sites_can_bind_explicit_typed_operands() {
    let env = ZenTypeEnv::input_only(BTreeMap::new(), EnvOptions::default())
        .unwrap()
        .bind_operand("slice", record("norn:ActionSlice"));
    assert!(matches!(env, Err(EnvError::Unresolved { name, .. }) if name == "norn:ActionSlice"));

    let records = BTreeMap::from([
        (
            "norn:ActionSlice".to_owned(),
            fields([("existing", record("retail.Sku"))]),
        ),
        ("retail.Sku".to_owned(), fields([("code", Type::text())])),
    ]);
    let env = ZenTypeEnv::input_only_with_records(
        records,
        fields([("skuId", Type::text())]),
        EnvOptions::default(),
    )
    .unwrap()
    .bind_operand("slice", record("norn:ActionSlice"))
    .unwrap()
    .bind_operand("skuId", Type::text())
    .unwrap();
    assert_eq!(
        env.variables().get("slice"),
        Some(&record("norn:ActionSlice"))
    );
    assert_eq!(env.variables().get("skuId"), Some(&Type::text()));
    assert!(matches!(
        env.clone().bind_operand("slice", Type::Bool),
        Err(EnvError::AlreadyBound { .. })
    ));
    assert!(matches!(
        env.bind_operand("missing", record("norn:Missing")),
        Err(EnvError::Unresolved { name, .. }) if name == "norn:Missing"
    ));
}

#[test]
fn interface_invariant_binds_self_to_the_interface_contract() {
    let env = Model::retail()
        .env()
        .bind_interface_self(&InterfaceName("commerce.Item".to_owned()))
        .unwrap();
    assert_eq!(env.variables().get("self"), Some(&record("commerce.Item")));
    assert_eq!(field(&env, "commerce.Item", "sku_code").0, Type::text());
    assert!(matches!(
        env.bind_interface_self(&InterfaceName("commerce.Item".to_owned())),
        Err(EnvError::AlreadyBound { .. })
    ));
    assert!(matches!(
        Model::retail()
            .env()
            .bind_interface_self(&InterfaceName("retail.Sku".to_owned())),
        Err(EnvError::Unresolved { .. })
    ));
}

#[test]
fn supplied_registry_refs_are_opaque_without_blocking_other_records() {
    let interfaces: [InterfaceDef; 1] = [serde_json::from_value(serde_json::json!({
        "name": "test.CurrencyBearing",
        "properties": [{ "id": "currency", "type": { "kind": "ref", "name": "iso4217.currency_code" } }]
    }))
    .unwrap()];
    let definitions = norn_expr::EntityDefinitions {
        interfaces: &interfaces,
        ..norn_expr::EntityDefinitions::default()
    };
    let registry = BTreeSet::from([PropertyTypeName("iso4217.currency_code".to_owned())]);
    let env =
        ZenTypeEnv::from_artifact_set_with_registry(&definitions, &registry, EnvOptions::default())
            .unwrap();
    assert_eq!(
        field(&env, "test.CurrencyBearing", "currency").0,
        Type::Json
    );
    assert!(matches!(
        ZenTypeEnv::from_artifact_set(&definitions, EnvOptions::default()),
        Err(errors) if matches!(errors.first(), Some(EnvError::Unresolved { name, .. }) if name == "iso4217.currency_code")
    ));
    let checking_env = ZenTypeEnv::from_artifact_set_for_checking(
        &definitions,
        &BTreeSet::new(),
        EnvOptions::default(),
    )
    .unwrap();
    assert_eq!(
        field(&checking_env, "test.CurrencyBearing", "currency").0,
        Type::Json
    );
}

#[test]
fn binding_rejects_unknown_or_non_object_types() {
    let env = Model::retail().env();
    for bad in ["retail.Nope", "commerce.Item", ACTOR_RECORD] {
        assert!(
            matches!(
                env.clone().bind_self(&ObjectTypeName(bad.to_owned())),
                Err(EnvError::Unresolved { .. })
            ),
            "{bad}"
        );
    }
    assert!(matches!(
        env.bind_input(fields([("x", record("retail.Nope"))])),
        Err(EnvError::Unresolved { .. })
    ));
}

/// #2419: the actor is exactly the ratified identity tuple plus derived
/// read-only facts; nothing else.
#[test]
fn actor_operand_is_the_closed_identity_tuple() {
    let env = Model::retail().env();
    assert_eq!(env.variables().get("actor"), Some(&record(ACTOR_RECORD)));
    let actor = env.record(ACTOR_RECORD).unwrap();
    assert_eq!(actor.kind, RecordKind::Builtin);
    let expected = fields([
        ("actorKind", Type::enumeration(ACTOR_KINDS)),
        ("agentSessionId", Type::text().optional()),
        ("authMethod", Type::text()),
        ("capability", Type::text()),
        ("roles", Type::text().list()),
        ("scope", record(ACTOR_SCOPE_RECORD)),
        ("subjectId", Type::text()),
    ]);
    let actual: BTreeMap<String, Type> = actor
        .fields
        .iter()
        .map(|(name, field)| (name.clone(), field.ty.clone()))
        .collect();
    assert_eq!(actual, expected);
    let scope: Vec<&str> = env
        .record(ACTOR_SCOPE_RECORD)
        .unwrap()
        .fields
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        scope,
        [
            "businessUnitId",
            "environmentId",
            "orgId",
            "tenantId",
            "workspaceId"
        ]
    );
    assert_eq!(ACTOR_KINDS, ["agent", "service", "system", "user"]);
}

fn derive(model: &Model) -> Vec<EnvError> {
    ZenTypeEnv::from_artifact_set(&model.definitions(), EnvOptions::default())
        .expect_err("derivation must fail closed")
}

#[test]
fn derivation_fails_closed() {
    let mut model = Model::retail();
    model.object_types[1].extends = Some(name("retail.Missing"));
    assert!(derive(&model).contains(&EnvError::Unresolved {
        site: "retail.BundleSku.extends".into(),
        name: "retail.Missing".into()
    }));

    let mut model = Model::retail();
    model.object_types[0].extends = Some(name("retail.BundleSku"));
    assert!(
        derive(&model)
            .iter()
            .any(|e| matches!(e, EnvError::Cycle { .. }))
    );

    let mut model = Model::retail();
    model.property_types[2].property_type = PropertyType::Ref {
        name: norn_ir::PropertyTypeName("commerce.short_note".into()),
    };
    assert!(
        derive(&model)
            .iter()
            .any(|e| matches!(e, EnvError::Cycle { .. }))
    );

    let mut model = Model::retail();
    model.object_types[3].links[0].target = name("crm.Missing");
    assert!(derive(&model).contains(&EnvError::Unresolved {
        site: "commerce.SalesOrder.customer".into(),
        name: "crm.Missing".into()
    }));

    let mut model = Model::retail();
    let duplicate = model.object_types[2].clone();
    model.object_types.push(duplicate);
    assert!(derive(&model).contains(&EnvError::DuplicateName {
        name: "crm.Customer".into()
    }));

    let mut model = Model::retail();
    model.extensions[0].target = name("crm.Missing");
    assert!(
        derive(&model)
            .iter()
            .any(|e| matches!(e, EnvError::Unresolved { name, .. } if name == "crm.Missing"))
    );

    // An inverse role that collides with a member of the target.
    let mut model = Model::retail();
    model.object_types[3].links[0].inverse = Some(norn_ir::LinkId("email".into()));
    assert!(derive(&model).contains(&EnvError::MemberCollision {
        record: "crm.Customer".into(),
        member: "email".into()
    }));
}

#[test]
fn structured_property_members_have_closed_expression_types() {
    let model: Model = serde_json::from_value(serde_json::json!({
        "propertyTypes":[], "interfaces":[], "extensions":[],
        "objectTypes":[{"name":"test.Point","properties":[{"id":"point","type":{"kind":"record","members":[
            {"id":"latitude","type":{"kind":"decimal","scale":6}},
            {"id":"longitude","type":{"kind":"decimal","scale":6}}
        ]}}]}]
    })).expect("model");
    let env = model.env();
    let point = &env.record("test.Point").expect("object").fields["point"].ty;
    let Type::Record(name) = point else {
        panic!("expected record, got {point:?}")
    };
    let members = &env.record(name).expect("value record").fields;
    assert_eq!(members.len(), 2);
    assert_eq!(
        members["latitude"].ty,
        Type::Decimal {
            scale: 6,
            unit: None
        }
    );
    assert!(!members.contains_key("unknown"));
}
