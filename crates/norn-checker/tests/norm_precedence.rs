//! Finite consistency laws, including every three-norm Boolean grounding.
use norn_checker::norm_precedence::{
    GroundedNorm, NormUnknown, PriorityReason, check_norm_precedence,
};
use norn_ir::deontic::DeonticModality::{self, Obligation, Permission, Prohibition};
use std::collections::BTreeSet;

fn norm(index: u32, modality: DeonticModality) -> GroundedNorm {
    GroundedNorm {
        id: format!("rule.{index}"),
        modality,
        authority: (format!("source.{index}"), "1".into()),
        authority_level: Some(index),
        enacted: 0,
        applicable: BTreeSet::from([0]),
        excepted: BTreeSet::new(),
    }
}

#[test]
fn every_strictly_ranked_three_norm_grounding_has_exactly_the_expected_survivors()
-> Result<(), String> {
    let grades = [Permission, Prohibition, Obligation];
    for a in grades {
        for b in grades {
            for c in grades {
                for applicable in 0_u8..8 {
                    for exceptions in 0_u8..8 {
                        let mut norms = [norm(3, a), norm(2, b), norm(1, c)];
                        for (n, bit) in norms.iter_mut().zip([1_u8, 2, 4]) {
                            if applicable & bit == 0 {
                                n.applicable.clear();
                            }
                            if exceptions & bit != 0 {
                                n.excepted.insert(0);
                            }
                        }
                        let active: Vec<_> = norms
                            .iter()
                            .enumerate()
                            .filter(|(_, n)| n.applicable.contains(&0) && !n.excepted.contains(&0))
                            .collect();
                        let expected: BTreeSet<_> =
                            active.first().map_or_else(BTreeSet::new, |(_, highest)| {
                                active
                                    .iter()
                                    .filter(|(_, n)| {
                                        (n.modality == Prohibition)
                                            == (highest.modality == Prohibition)
                                    })
                                    .map(|(i, _)| *i)
                                    .collect()
                            });
                        let results =
                            check_norm_precedence(&norms, 1).map_err(|e| format!("{e:?}"))?;
                        let result = results.first().ok_or("missing context")?;
                        assert_eq!(
                            result.survivors.iter().copied().collect::<BTreeSet<_>>(),
                            expected
                        );
                        assert!(result.conflicts.is_empty());
                        assert!(
                            result
                                .defeats
                                .iter()
                                .all(|d| expected.contains(&d.winner)
                                    && !expected.contains(&d.loser))
                        );
                        assert_eq!(
                            result.obligations.iter().copied().collect::<BTreeSet<_>>(),
                            active
                                .iter()
                                .filter(|(i, n)| expected.contains(i) && n.modality == Obligation)
                                .map(|(i, _)| *i)
                                .collect()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn posterior_precedes_specialis_and_specialis_uses_pre_exception_domains() -> Result<(), String> {
    let mut broad = norm(1, Prohibition);
    broad.applicable.insert(1);
    let mut narrow = norm(2, Permission);
    narrow.authority = broad.authority.clone();
    narrow.authority_level = broad.authority_level;
    let result = check_norm_precedence(&[broad, narrow], 2).map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        result
            .first()
            .and_then(|c| c.defeats.first())
            .ok_or("missing defeat")?
            .reason,
        PriorityReason::Specialis
    );
    let mut broad = norm(1, Prohibition);
    broad.enacted = 1;
    broad.applicable.insert(1);
    broad.excepted.insert(1);
    let mut narrow = norm(2, Permission);
    narrow.authority = broad.authority.clone();
    narrow.authority_level = broad.authority_level;
    let result = check_norm_precedence(&[broad, narrow], 2).map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        result
            .first()
            .and_then(|c| c.defeats.first())
            .ok_or("missing defeat")?
            .reason,
        PriorityReason::Posterior
    );
    assert!(result.get(1).ok_or("missing context")?.survivors.is_empty());
    Ok(())
}

#[test]
fn incomparable_authorities_keep_conflicts_and_missing_selection_is_unknown() -> Result<(), String>
{
    let high = norm(1, Prohibition);
    let mut other = norm(2, Obligation);
    other.authority_level = high.authority_level;
    other.enacted = 100;
    let mut norms = [high, other];
    let result = check_norm_precedence(&norms, 1).map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        result.first().ok_or("missing context")?.conflicts,
        vec![(0, 1)]
    );
    assert_eq!(
        result.first().ok_or("missing context")?.obligations,
        vec![1]
    );
    norms.first_mut().ok_or("missing norm")?.authority_level = None;
    assert_eq!(
        check_norm_precedence(&norms, 1),
        Err(NormUnknown::MissingAuthority)
    );
    Ok(())
}

#[test]
fn invalid_models_and_other_source_versions_never_silently_resolve() -> Result<(), String> {
    let mut norms = [norm(1, Permission), norm(2, Prohibition)];
    norms.get_mut(1).ok_or("missing norm")?.authority = ("source.1".into(), "2".into());
    norms.get_mut(1).ok_or("missing norm")?.authority_level = Some(1);
    assert_eq!(
        check_norm_precedence(&norms, 1)
            .map_err(|e| format!("{e:?}"))?
            .first()
            .ok_or("missing context")?
            .conflicts,
        vec![(0, 1)]
    );
    assert_eq!(check_norm_precedence(&norms, 0), Err(NormUnknown::Limit));
    assert_eq!(check_norm_precedence(&norms, 257), Err(NormUnknown::Limit));
    norms
        .first_mut()
        .ok_or("missing norm")?
        .applicable
        .insert(1);
    assert_eq!(check_norm_precedence(&norms, 1), Err(NormUnknown::Invalid));
    Ok(())
}

fn declared(
    norms: &[GroundedNorm],
    layers: &[norn_checker::norm_precedence::ConstitutionLayer],
    edges: &[(usize, usize)],
) -> norn_checker::norm_precedence::DeclaredOrderReport {
    let origins: Vec<_> = norms
        .iter()
        .map(|n| norn_checker::consistency::RuleOrigin {
            artifact_id: Some("synthetic.rules".into()),
            pointer: format!("/rules/{}", n.id),
        })
        .collect();
    norn_checker::norm_precedence::diagnose_declared_order(norms, &origins, layers, edges, 2, 1000)
        .unwrap()
}

#[test]
fn declared_priority_deletion_refuses_and_layers_never_silently_defeat() {
    use norn_checker::norm_precedence::ConstitutionLayer::{Company, Legal, Religious};
    let norms = [norm(2, Obligation), norm(1, Prohibition)];
    assert!(
        declared(&norms, &[Company; 2], &[(0, 1)])
            .diagnosis
            .cores
            .is_empty()
    );
    let refused = declared(&norms, &[Company; 2], &[]);
    assert_eq!(
        refused.diagnosis.cores,
        BTreeSet::from([BTreeSet::from([0, 1])])
    );
    assert!(
        refused
            .diagnostics
            .iter()
            .any(|d| d.code.as_str() == "consistency.normative.legacy_resolution")
    );
    assert!(
        refused
            .diagnostics
            .iter()
            .all(|d| d.artifact_id.as_deref() == Some("synthetic.rules") && d.span.is_some())
    );
    assert!(
        !declared(&norms, &[Religious, Company], &[(0, 1)])
            .diagnosis
            .cores
            .is_empty()
    );
    let mut inactive = [norm(0, Prohibition), norm(1, Permission)];
    inactive[1].excepted.insert(0);
    assert!(
        !declared(&inactive, &[Religious, Company], &[(1, 0)])
            .diagnosis
            .cores
            .is_empty()
    );
    assert!(
        declared(
            &[
                norm(0, Obligation),
                norm(1, Obligation),
                norm(2, Permission)
            ],
            &[Religious, Legal, Company],
            &[]
        )
        .diagnosis
        .cores
        .is_empty()
    );
}

#[test]
fn exact_cycle_cores_include_same_modality_rules_and_ignore_unrelated_rules() {
    use norn_checker::norm_precedence::ConstitutionLayer::Company;
    let norms = [
        norm(0, Obligation),
        norm(1, Obligation),
        norm(2, Obligation),
        norm(3, Obligation),
    ];
    let result = declared(&norms, &[Company; 4], &[(0, 1), (1, 2), (2, 0)]);
    assert_eq!(
        result.diagnosis.cores,
        BTreeSet::from([BTreeSet::from([0, 1, 2])])
    );
    assert!(
        declared(&norms, &[Company; 4], &[(0, 1), (1, 2)])
            .diagnosis
            .cores
            .is_empty()
    );
    assert_eq!(
        declared(&norms, &[Company; 4], &[(0, 0)]).diagnosis.cores,
        BTreeSet::from([BTreeSet::from([0])])
    );
}

proptest::proptest! {
    #[test]
    fn declared_order_is_independent_of_catalog_permutation(swap in proptest::bool::ANY, reverse in proptest::bool::ANY) {
        use norn_checker::norm_precedence::ConstitutionLayer::Company;
        let mut norms = [norm(0, Obligation), norm(1, Prohibition)];
        let mut edge = if reverse { (1, 0) } else { (0, 1) };
        if swap { norms.swap(0, 1); edge = (1 - edge.0, 1 - edge.1); }
        proptest::prop_assert!(declared(&norms, &[Company; 2], &[edge]).diagnosis.cores.is_empty());
        proptest::prop_assert_eq!(declared(&norms, &[Company; 2], &[]).diagnosis.cores.len(), 1);
    }
}

#[test]
fn malformed_binding_and_budget_exhaustion_remain_unknown() {
    use norn_checker::consistency::RuleOrigin;
    use norn_checker::norm_precedence::{ConstitutionLayer::Company, diagnose_declared_order};
    let norms = [norm(0, Obligation), norm(1, Prohibition)];
    let origins = [
        RuleOrigin {
            artifact_id: Some("rules".into()),
            pointer: "/0".into(),
        },
        RuleOrigin {
            artifact_id: Some("rules".into()),
            pointer: "/1".into(),
        },
    ];
    for (edges, budget) in [
        (vec![(0, 2)], 100),
        (vec![(0, 1), (0, 1)], 100),
        (vec![], 0),
    ] {
        assert!(
            diagnose_declared_order(&norms, &origins, &[Company; 2], &edges, 2, budget).is_err()
        );
    }
    assert!(
        diagnose_declared_order(
            &norms,
            &[origins[0].clone(), origins[0].clone()],
            &[Company; 2],
            &[],
            2,
            100
        )
        .is_err()
    );
}

#[test]
fn explicit_transitivity_and_nonoverlapping_conditions_are_supported() {
    use norn_checker::norm_precedence::ConstitutionLayer::Company;
    let mut norms = [
        norm(0, Obligation),
        norm(1, Obligation),
        norm(2, Prohibition),
    ];
    assert!(
        declared(&norms, &[Company; 3], &[(0, 1), (1, 2)])
            .diagnosis
            .cores
            .is_empty()
    );
    norms[2].applicable = BTreeSet::from([1]);
    assert!(
        declared(&norms, &[Company; 3], &[])
            .diagnosis
            .cores
            .is_empty()
    );
}
