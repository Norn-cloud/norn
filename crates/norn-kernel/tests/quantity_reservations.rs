#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "pure admission assertions"
)]
//! Exact arithmetic and conservative resolution at the shared kernel boundary.
#[path = "../../norn-cell/tests/quantity_support/mod.rs"]
mod quantity_support;
use norn_ir::quantity_ledger::{EvaluatedLedgerLeg, QuantityOperation};
use norn_kernel::quantity::{
    QuantityBalance, QuantityError, QuantityKey, QuantityPosting, QuantityState,
    prepare_quantity as raw_prepare_quantity, quantity_legs,
};
use quantity_support::{define, pack, post, request, reserve};

fn state() -> QuantityState {
    QuantityState {
        version: 2,
        pack: Some(pack()),
        unrestricted_accounts: vec!["issuance".into()],
        balances: [(
            QuantityKey {
                account: "lot".into(),
                unit: "item".into(),
                layer: "physical".into(),
            },
            QuantityBalance {
                version: 1,
                posted: "100".into(),
                held: "0".into(),
            },
        )]
        .into_iter()
        .collect(),
        ..QuantityState::default()
    }
}
#[test]
fn pinned_pack_and_revision_bound_are_checked_before_mutation() {
    prepare_quantity(&define(), &QuantityState::default(), 0).expect("define checked pack");
    let mut stale = post(1, "p", "ship", 1);
    stale.balance_versions = vec![
        norn_ir::quantity_ledger::QuantityBalanceVersion {
            account: "lot".into(),
            unit: "item".into(),
            layer: "physical".into(),
            version: 0,
        },
        norn_ir::quantity_ledger::QuantityBalanceVersion {
            account: "transit".into(),
            unit: "item".into(),
            layer: "physical".into(),
            version: 0,
        },
    ];
    assert_eq!(
        raw_prepare_quantity(&stale, &state(), 0).expect_err("stale balance"),
        QuantityError::StaleVersion {
            expected: 0,
            current: 1
        }
    );
    let mut current = state();
    current.version = u64::MAX;
    assert_eq!(
        prepare_quantity(&post(u64::MAX, "p", "ship", 1), &current, 0)
            .expect_err("revision overflow"),
        QuantityError::Overflow
    );
}
#[test]
fn min_negation_and_exact_head_overflow_are_typed_refusals() {
    let mut current = state();
    current.original = Some(QuantityPosting {
        reverses: None,
        legs: vec![EvaluatedLedgerLeg {
            leg_id: "min".into(),
            account_id: "lot".into(),
            unit_id: "item".into(),
            layer: "physical".into(),
            unit_precision: 0,
            amount_unscaled: i128::MIN.to_string(),
        }],
    });
    assert_eq!(
        quantity_legs(
            &request(
                2,
                QuantityOperation::Reverse {
                    original_id: "p".into(),
                    posting_id: "r".into()
                }
            ),
            &current
        )
        .expect_err("MIN negation"),
        QuantityError::Overflow
    );
    current.balances.insert(
        QuantityKey {
            account: "transit".into(),
            unit: "item".into(),
            layer: "physical".into(),
        },
        QuantityBalance {
            version: 1,
            posted: i128::MAX.to_string(),
            held: "0".into(),
        },
    );
    assert_eq!(
        prepare_quantity(&post(2, "p", "ship", 1), &current, 0).expect_err("exact overflow"),
        QuantityError::Overflow
    );
}
#[test]
fn complete_pinned_vector_is_required_and_deadline_is_exclusive() {
    let mut current = state();
    let planned = prepare_quantity(&reserve(2, "r", 30, 10), &current, 0).expect("reserve");
    current.version = planned.version;
    current.balances.extend(planned.balances);
    current.reservation = planned.reservation.map(|(_, r)| r);
    let consume = request(
        3,
        QuantityOperation::PostReservation {
            reservation_id: "r".into(),
            posting_id: "p".into(),
            resolution: norn_ir::quantity_ledger::QuantityResolution::All,
        },
    );
    assert_eq!(
        prepare_quantity(&consume, &current, 10).expect_err("exclusive deadline"),
        QuantityError::Expired
    );
    assert!(
        current
            .reservation
            .as_mut()
            .expect("reservation")
            .legs
            .pop()
            .is_some()
    );
    assert_eq!(
        prepare_quantity(&consume, &current, 9).expect_err("partial vector"),
        QuantityError::InvalidContract
    );
}

#[test]
fn valid_pack_at_all_cardinality_and_identifier_limits_passes() {
    use norn_ir::quantity_ledger::{
        LedgerAccountIr, LedgerLayerIr, LedgerParameterIr, LedgerUnitIr, LinearTermIr,
    };
    use norn_kernel::quantity::{
        MAX_QUANTITY_IDENTIFIER_BYTES, MAX_QUANTITY_INTEGER_BYTES, MAX_QUANTITY_LEGS,
        MAX_QUANTITY_PACK_MEMBERS, check_quantity_pack_bounds,
    };
    let id = |prefix: &str, index: usize| {
        let prefix = format!("{prefix}{index}.");
        format!(
            "{prefix}{}",
            "x".repeat(MAX_QUANTITY_IDENTIFIER_BYTES - prefix.len())
        )
    };
    let mut bounded = pack();
    bounded.pack_id = id("pack", 0);
    bounded.ontology_artifact.artifact_id = id("ontology", 0);
    bounded.ontology_artifact.artifact_version = id("version", 0);
    bounded.units = (0..MAX_QUANTITY_PACK_MEMBERS)
        .map(|i| LedgerUnitIr {
            unit_id: id("unit", i),
            unit_term_id: id("term", i),
            quantity_kind_term_id: id("kind", i),
            precision: 0,
        })
        .collect();
    bounded.accounts = (0..128)
        .map(|i| LedgerAccountIr {
            funding: norn_ir::quantity_ledger::LedgerFundingPolicy::Gross,
            account_id: id("account", i),
            allowed_unit_ids: bounded.units.iter().map(|u| u.unit_id.clone()).collect(),
        })
        .collect();
    bounded.layers = (0..MAX_QUANTITY_PACK_MEMBERS)
        .map(|i| LedgerLayerIr {
            layer_id: id("layer", i),
        })
        .collect();
    let mut template = bounded.templates[0].clone();
    template.parameters = (0..MAX_QUANTITY_PACK_MEMBERS)
        .map(|i| LedgerParameterIr {
            parameter_id: id("parameter", i),
            scale: 0,
            min_unscaled: i128::MIN.to_string(),
            max_unscaled: i128::MAX.to_string(),
        })
        .collect();
    assert_eq!(
        template.parameters[0].min_unscaled.len(),
        MAX_QUANTITY_INTEGER_BYTES
    );
    let mut leg = template.legs[0].clone();
    leg.account_id = bounded.accounts[0].account_id.clone();
    leg.unit_id = bounded.units[0].unit_id.clone();
    leg.layer = bounded.layers[0].layer_id.clone();
    leg.amount.terms = template
        .parameters
        .iter()
        .map(|p| LinearTermIr {
            parameter_id: p.parameter_id.clone(),
            coefficient_unscaled: "0".into(),
            coefficient_scale: 0,
        })
        .collect();
    template.legs = (0..MAX_QUANTITY_LEGS)
        .map(|i| {
            let mut next = leg.clone();
            next.leg_id = id("leg", i);
            next
        })
        .collect();
    bounded.templates = (0..MAX_QUANTITY_PACK_MEMBERS)
        .map(|i| {
            let mut next = template.clone();
            next.template_id = id("template", i);
            next
        })
        .collect();
    check_quantity_pack_bounds(&bounded).expect("all independent limits are inclusive");

    let mut excessive = bounded.clone();
    excessive.accounts[0].allowed_unit_ids.push("extra".into());
    assert_eq!(
        check_quantity_pack_bounds(&excessive),
        Err(QuantityError::InvalidContract)
    );
    excessive = bounded.clone();
    excessive.templates[0].legs[0]
        .amount
        .terms
        .push(leg.amount.terms[0].clone());
    assert_eq!(
        check_quantity_pack_bounds(&excessive),
        Err(QuantityError::InvalidContract)
    );
    bounded.pack_id.push('x');
    assert_eq!(
        check_quantity_pack_bounds(&bounded),
        Err(QuantityError::InvalidContract)
    );
}

#[test]
fn identifier_limit_counts_utf8_bytes_in_otherwise_valid_packs() {
    use norn_kernel::quantity::{MAX_QUANTITY_IDENTIFIER_BYTES, check_quantity_pack_bounds};
    let mut candidate = pack();
    candidate.ontology_artifact.artifact_id = "é".repeat(MAX_QUANTITY_IDENTIFIER_BYTES / 2);
    check_quantity_pack_bounds(&candidate).expect("128 UTF-8 bytes are admitted");
    candidate.ontology_artifact.artifact_id.push('x');
    assert_eq!(
        check_quantity_pack_bounds(&candidate),
        Err(QuantityError::InvalidContract)
    );
}

fn prepare_quantity(
    request: &norn_ir::quantity_ledger::QuantityRequest,
    state: &QuantityState,
    now: u64,
) -> Result<norn_kernel::quantity::QuantityPlan, QuantityError> {
    let mut request = request.clone();
    let keys = norn_kernel::quantity::quantity_balance_keys(&request, state)?;
    request.balance_versions = keys
        .into_iter()
        .map(|key| norn_ir::quantity_ledger::QuantityBalanceVersion {
            version: state
                .balances
                .get(&key)
                .map_or(0, |balance| balance.version),
            account: key.account,
            unit: key.unit,
            layer: key.layer,
        })
        .collect();
    raw_prepare_quantity(&request, state, now)
}
