#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! `EntityIR` v2 links and invariants (T1-ONT-1d, #2514): wire round-trip of
//! the fixtures (1:1, 1:N, and N:M links, an inverse pair, link properties, a
//! required interface link, an extension link, an invariant), the
//! null-vs-omitted discipline, closed shapes, and the canonical cardinality
//! form.

use norn_ir::{
    InterfaceDef, JsonValue, Link, LinkCardinality, LinkId, Multiplicity, ObjectTypeDef,
    ObjectTypeExtension,
};

macro_rules! fixture {
    ($dir:literal, $name:literal) => {
        (
            $name,
            include_str!(concat!("../../../fixtures/ir/entity-v2/", $dir, "/", $name)),
        )
    };
}

const OBJECT_TYPES: [(&str, &str); 4] = [
    fixture!("object-types", "sales-order.object-type.json"),
    fixture!("object-types", "billing-profile.object-type.json"),
    fixture!("object-types", "project.object-type.json"),
    fixture!("object-types", "meta-table.object-type.json"),
];
const INTERFACES: [(&str, &str); 1] = [fixture!("interfaces", "order.interface.json")];
const EXTENSIONS: [(&str, &str); 1] = [fixture!(
    "extensions",
    "acme-customer-referrals.extension.json"
)];

fn canonical(source: &str) -> String {
    let value: JsonValue = serde_json::from_str(source).expect("fixture is JSON");
    serde_json::to_string(&value).expect("JSON encodes")
}

fn round_trips<T>(fixtures: &[(&str, &str)])
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    for (name, source) in fixtures {
        let def: T = serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("{name} should decode: {error}"));
        let encoded = serde_json::to_string(&def).expect("definition encodes");
        assert_eq!(canonical(&encoded), canonical(source), "{name} round-trip");
    }
}

fn decodes<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> bool {
    serde_json::from_str::<T>(&value.to_string()).is_ok()
}

fn object_type(index: usize) -> ObjectTypeDef {
    serde_json::from_str(OBJECT_TYPES[index].1).expect("decodes")
}

fn link<'a>(def: &'a ObjectTypeDef, id: &str) -> &'a Link {
    def.links
        .iter()
        .find(|link| link.id.as_str() == id)
        .expect("link exists")
}

#[test]
fn every_fixture_round_trips() {
    round_trips::<ObjectTypeDef>(&OBJECT_TYPES);
    round_trips::<InterfaceDef>(&INTERFACES);
    round_trips::<ObjectTypeExtension>(&EXTENSIONS);
}

#[test]
fn fixtures_cover_one_to_one_one_to_many_and_many_to_many() {
    use Multiplicity::{Many, One};
    let (order, profile, project, table) = (
        object_type(0),
        object_type(1),
        object_type(2),
        object_type(3),
    );

    // 1:N with an inverse pair: each order has one customer; a customer has
    // many orders through the inverse role.
    let customer = link(&order, "customer");
    assert_eq!(
        (customer.cardinality.to, customer.cardinality.from),
        (One, Many)
    );
    assert!(customer.cardinality.required);
    assert_eq!(customer.target.as_str(), "crm.Customer");
    assert_eq!(
        customer.inverse.as_ref().map(LinkId::as_str),
        Some("orders")
    );

    // 1:1.
    let account = link(&profile, "account");
    assert_eq!(
        (account.cardinality.to, account.cardinality.from),
        (One, One)
    );
    assert_eq!(
        account.inverse.as_ref().map(LinkId::as_str),
        Some("billing_profile")
    );

    // N:M with link properties.
    let members = link(&project, "members");
    assert_eq!(
        (members.cardinality.to, members.cardinality.from),
        (Many, Many)
    );
    let properties: Vec<&str> = members
        .properties
        .iter()
        .map(|property| property.id.as_str())
        .collect();
    assert_eq!(properties, ["role", "allocation"]);

    // The technical view (ruling 2026-09-25 section 18): backedBy and
    // derivedFrom are ordinary links.
    assert_eq!(link(&order, "backed_by").target.as_str(), "meta.Table");
    assert_eq!(link(&table, "derived_from").target.as_str(), "meta.Table");

    // Invariants.
    assert_eq!(order.invariants[0].id.as_str(), "non_negative_total");
    assert_eq!(order.invariants[0].expression.as_str(), "self.total >= 0");

    // A required link on an interface, and an extension link.
    let interface: InterfaceDef = serde_json::from_str(INTERFACES[0].1).expect("decodes");
    assert_eq!(interface.links[0].target.as_str(), "party.Party");
    assert!(interface.links[0].cardinality.required);
    let extension: ObjectTypeExtension = serde_json::from_str(EXTENSIONS[0].1).expect("decodes");
    assert_eq!(extension.links[0].id.as_str(), "acme__referred_by");
    assert_eq!(
        extension.links[0].inverse.as_ref().map(LinkId::as_str),
        Some("acme__referrals")
    );
}

#[test]
fn optional_members_may_be_omitted_but_not_null() {
    let cardinality = serde_json::json!({ "to": "one", "from": "many" });
    let link = |member: (&str, serde_json::Value)| {
        let mut link = serde_json::json!({ "id": "customer", "target": "crm.Customer",
            "cardinality": cardinality });
        link[member.0] = member.1;
        serde_json::json!({ "name": "commerce.SalesOrder", "links": [link] })
    };
    for member in [
        ("inverse", serde_json::Value::Null),
        ("description", serde_json::Value::Null),
        ("properties", serde_json::Value::Null),
        (
            "cardinality",
            serde_json::json!({ "to": "one", "from": "many", "required": null }),
        ),
    ] {
        let value = link(member);
        assert!(!decodes::<ObjectTypeDef>(&value), "{value} is rejected");
    }
    for value in [
        serde_json::json!({ "name": "commerce.SalesOrder", "links": null }),
        serde_json::json!({ "name": "commerce.SalesOrder", "invariants": null }),
        serde_json::json!({ "name": "commerce.SalesOrder",
            "invariants": [{ "id": "x", "expression": "true", "message": "m", "description": null }] }),
    ] {
        assert!(!decodes::<ObjectTypeDef>(&value), "{value} is rejected");
    }
    assert!(!decodes::<InterfaceDef>(
        &serde_json::json!({ "name": "commerce.Order", "links": null })
    ));
    assert!(!decodes::<ObjectTypeExtension>(
        &serde_json::json!({ "namespace": "acme", "target": "crm.Customer", "links": null })
    ));
    // Every optional member omitted.
    assert!(decodes::<ObjectTypeDef>(
        &serde_json::json!({ "name": "commerce.SalesOrder",
        "links": [{ "id": "customer", "target": "crm.Customer", "cardinality": cardinality }] })
    ));
}

#[test]
fn closed_shapes_reject_unknown_and_missing_members() {
    let with_link = |link: serde_json::Value| serde_json::json!({ "name": "commerce.SalesOrder", "links": [link] });
    for link in [
        // Both ends are declared; there is no ambient default.
        serde_json::json!({ "id": "customer", "target": "crm.Customer", "cardinality": { "to": "one" } }),
        serde_json::json!({ "id": "customer", "target": "crm.Customer" }),
        serde_json::json!({ "id": "customer", "cardinality": { "to": "one", "from": "many" } }),
        serde_json::json!({ "id": "customer", "target": "crm.Customer",
            "cardinality": { "to": "single", "from": "many" } }),
        serde_json::json!({ "id": "customer", "target": "crm.Customer",
            "cardinality": { "to": "one", "from": "many", "min": 1 } }),
        // One target, never a list; the source is the declaring type.
        serde_json::json!({ "id": "customer", "target": ["crm.Customer"],
            "cardinality": { "to": "one", "from": "many" } }),
        serde_json::json!({ "id": "customer", "from": "commerce.SalesOrder", "target": "crm.Customer",
            "cardinality": { "to": "one", "from": "many" } }),
    ] {
        let value = with_link(link);
        assert!(!decodes::<ObjectTypeDef>(&value), "{value} is rejected");
    }
    // An interface requirement names no inverse and no link properties.
    for member in [
        ("inverse", serde_json::json!("orders")),
        ("properties", serde_json::json!([])),
    ] {
        let mut requirement = serde_json::json!({ "id": "customer", "target": "party.Party",
            "cardinality": { "to": "one", "from": "many" } });
        requirement[member.0] = member.1;
        let value = serde_json::json!({ "name": "commerce.Order", "links": [requirement] });
        assert!(!decodes::<InterfaceDef>(&value), "{value} is rejected");
    }
    // An invariant needs an expression and a message.
    for invariant in [
        serde_json::json!({ "id": "x", "expression": "true" }),
        serde_json::json!({ "id": "x", "message": "m" }),
        serde_json::json!({ "id": "x", "expression": "true", "message": "m", "severity": "warn" }),
    ] {
        let value = serde_json::json!({ "name": "commerce.SalesOrder", "invariants": [invariant] });
        assert!(!decodes::<ObjectTypeDef>(&value), "{value} is rejected");
    }
    // An extension adds no invariants.
    assert!(!decodes::<ObjectTypeExtension>(&serde_json::json!({
        "namespace": "acme", "target": "crm.Customer",
        "invariants": [{ "id": "acme__x", "expression": "true", "message": "m" }]
    })));
}

#[test]
fn the_canonical_form_omits_an_optional_link() {
    let cardinality = LinkCardinality {
        to: Multiplicity::One,
        from: Multiplicity::Many,
        required: false,
    };
    assert_eq!(
        serde_json::to_value(cardinality).expect("encodes"),
        serde_json::json!({ "to": "one", "from": "many" })
    );
    let required = LinkCardinality {
        required: true,
        ..cardinality
    };
    assert_eq!(
        serde_json::to_value(required).expect("encodes"),
        serde_json::json!({ "to": "one", "from": "many", "required": true })
    );
    let decoded: LinkCardinality = serde_json::from_value(
        serde_json::json!({ "to": "one", "from": "many", "required": false }),
    )
    .expect("decodes");
    assert_eq!(decoded, cardinality);
}
