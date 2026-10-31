//! Divergence witnesses establish identity through object keys or declared
//! canon keys, never by assumption (norn#79, norn#63 lane D).

use std::collections::BTreeMap;

use norn_checker::consistency::{
    CanonKey, ChaseBudget, Divergence, MappedObservation, WitnessIdentity, find_divergences,
};

fn observed(key: &str, values: &[(&str, &str)]) -> MappedObservation {
    MappedObservation {
        object_type: "shop.Order".to_owned(),
        key: key.to_owned(),
        values: values
            .iter()
            .map(|(property, value)| ((*property).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn by_number() -> CanonKey {
    CanonKey {
        type_name: "shop.Order".to_owned(),
        key: "by_number".to_owned(),
        properties: vec!["orderNumber".to_owned()],
    }
}

/// ERP `SO-1` and storefront `1001` are one order only through its number.
fn erp_and_storefront() -> Vec<MappedObservation> {
    vec![
        observed(
            "erp:SO-1",
            &[("orderNumber", "\"SO-1001\""), ("total", "114")],
        ),
        observed(
            "shopify:1001",
            &[("orderNumber", "\"SO-1001\""), ("total", "100")],
        ),
        observed(
            "shopify:1002",
            &[("orderNumber", "\"SO-1002\""), ("total", "50")],
        ),
    ]
}

#[test]
fn a_shared_canon_key_establishes_identity_and_the_divergence() {
    let found = find_divergences(
        &erp_and_storefront(),
        &[by_number()],
        ChaseBudget::default(),
    )
    .unwrap();
    assert_eq!(
        found,
        [Divergence {
            type_name: "shop.Order".to_owned(),
            property: "total".to_owned(),
            identity: WitnessIdentity::SharedKey("shop.Order".to_owned(), "by_number".to_owned()),
            left: (0, "114".to_owned()),
            right: (1, "100".to_owned()),
        }]
    );
}

#[test]
fn different_objects_are_never_assumed_to_be_one() {
    // Without the declared key nothing says SO-1 and 1001 are the same order.
    let found = find_divergences(&erp_and_storefront(), &[], ChaseBudget::default()).unwrap();
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn the_same_object_key_is_identity() {
    let observations = [
        observed("SO-9", &[("status", "\"invoiced\"")]),
        observed("SO-9", &[("status", "\"draft\"")]),
        observed("SO-9", &[("status", "\"cancelled\"")]),
        observed("SO-8", &[("status", "\"draft\"")]),
    ];
    let found = find_divergences(&observations, &[], ChaseBudget::default()).unwrap();
    let pairs: Vec<_> = found
        .iter()
        .map(|d| (d.identity.clone(), d.left.0, d.right.0))
        .collect();
    assert_eq!(
        pairs,
        [
            (WitnessIdentity::SameObjectKey, 0, 1),
            (WitnessIdentity::SameObjectKey, 0, 2)
        ]
    );
}

#[test]
fn agreeing_observations_are_clean() {
    let observations = [
        observed(
            "erp:SO-1",
            &[("orderNumber", "\"SO-1001\""), ("total", "114")],
        ),
        observed(
            "shopify:1001",
            &[("orderNumber", "\"SO-1001\""), ("total", "114")],
        ),
    ];
    assert!(
        find_divergences(&observations, &[by_number()], ChaseBudget::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_exhausted_budget_is_a_refusal() {
    let budget = ChaseBudget {
        max_steps: 2,
        ..ChaseBudget::default()
    };
    assert!(find_divergences(&erp_and_storefront(), &[by_number()], budget).is_err());
}
