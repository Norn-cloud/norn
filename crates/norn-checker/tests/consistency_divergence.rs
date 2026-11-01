//! Divergence witnesses establish identity through object keys or declared
//! canon keys, never by assumption (norn#79, norn#63 lane D).

use std::collections::BTreeMap;

use norn_checker::consistency::{
    CanonKey, ChaseBudget, ChaseError, Divergence, MappedObservation, WitnessIdentity,
    find_divergences,
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

#[test]
fn identity_through_a_chain_of_records_fails_closed() {
    // Sol R1 S1: A and C share no key; only B links them (email, then
    // phone), so no pairwise witness exists.
    let observations = [
        observed("a", &[("email", "\"e\""), ("total", "1")]),
        observed("b", &[("email", "\"e\""), ("phone", "\"p\"")]),
        observed("c", &[("phone", "\"p\""), ("total", "2")]),
    ];
    let keys = [
        CanonKey {
            type_name: "shop.Order".to_owned(),
            key: "by_email".to_owned(),
            properties: vec!["email".to_owned()],
        },
        CanonKey {
            type_name: "shop.Order".to_owned(),
            key: "by_phone".to_owned(),
            properties: vec!["phone".to_owned()],
        },
    ];
    assert!(matches!(
        find_divergences(&observations, &keys, ChaseBudget::default()),
        Err(ChaseError::Internal(_))
    ));
}

#[test]
fn repeated_values_are_attributed_to_their_own_records() {
    // Sol R1 S2: `[1, 2, 2]` on one object is (0, 1) then (0, 2).
    let observations = [
        observed("SO-7", &[("total", "1")]),
        observed("SO-7", &[("total", "2")]),
        observed("SO-7", &[("total", "2")]),
    ];
    let found = find_divergences(&observations, &[], ChaseBudget::default()).unwrap();
    let pairs: Vec<_> = found.iter().map(|d| (d.left.0, d.right.0)).collect();
    assert_eq!(pairs, [(0, 1), (0, 2)]);
}

#[test]
fn an_identical_fact_of_a_third_record_does_not_block_a_valid_pair() {
    // Sol R2 S3: observation 0 duplicates observation 1's key fact; 1 and 2
    // still establish the shared key on their own.
    let observations = [
        observed("a", &[("email", "\"e\"")]),
        observed("a", &[("email", "\"e\""), ("total", "1")]),
        observed("b", &[("email", "\"e\""), ("total", "2")]),
    ];
    let keys = [CanonKey {
        type_name: "shop.Order".to_owned(),
        key: "by_email".to_owned(),
        properties: vec!["email".to_owned()],
    }];
    let found = find_divergences(&observations, &keys, ChaseBudget::default()).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        (found[0].left.0, found[0].right.0, found[0].identity.clone()),
        (
            1,
            2,
            WitnessIdentity::SharedKey("shop.Order".to_owned(), "by_email".to_owned())
        )
    );
}

#[test]
fn the_pair_does_not_depend_on_observation_order() {
    // CodeRabbit on #108: two records share the fact `total(a, 1)`, but only
    // one of them carries the email that links it to `c`.
    let keys = [CanonKey {
        type_name: "shop.Order".to_owned(),
        key: "by_email".to_owned(),
        properties: vec!["email".to_owned()],
    }];
    let without_email = observed("a", &[("total", "1")]);
    let with_email = observed("a", &[("email", "\"e\""), ("total", "1")]);
    let linked = observed("c", &[("email", "\"e\""), ("total", "2")]);
    for (observations, expected) in [
        (
            [without_email.clone(), with_email.clone(), linked.clone()],
            (1, 2),
        ),
        ([with_email, without_email, linked], (0, 2)),
    ] {
        let found = find_divergences(&observations, &keys, ChaseBudget::default()).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].left.0, found[0].right.0), expected);
        assert_eq!(
            found[0].identity,
            WitnessIdentity::SharedKey("shop.Order".to_owned(), "by_email".to_owned())
        );
    }
}
