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
fn every_strictly_ranked_three_norm_grounding_has_exactly_the_expected_survivors() {
    let grades = [Permission, Prohibition, Obligation];
    for a in grades {
        for b in grades {
            for c in grades {
                for applicable in 0_u8..8 {
                    for exceptions in 0_u8..8 {
                        let mut norms = [norm(3, a), norm(2, b), norm(1, c)];
                        for (i, n) in norms.iter_mut().enumerate() {
                            if applicable & (1 << i) == 0 {
                                n.applicable.clear();
                            }
                            if exceptions & (1 << i) != 0 {
                                n.excepted.insert(0);
                            }
                        }
                        let active: Vec<_> = (0..3)
                            .filter(|i| applicable & (1 << i) != 0 && exceptions & (1 << i) == 0)
                            .collect();
                        let expected: BTreeSet<_> =
                            active.first().map_or_else(BTreeSet::new, |highest| {
                                active
                                    .iter()
                                    .copied()
                                    .filter(|i| {
                                        (norms[*i].modality == Prohibition)
                                            == (norms[*highest].modality == Prohibition)
                                    })
                                    .collect()
                            });
                        let results =
                            check_norm_precedence(&norms, 1).expect("complete finite model");
                        let result = results.first().expect("one context");
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
                            expected
                                .iter()
                                .copied()
                                .filter(|i| norms[*i].modality == Obligation)
                                .collect()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn posterior_precedes_specialis_and_specialis_uses_pre_exception_domains() {
    let mut broad = norm(1, Prohibition);
    broad.applicable.insert(1);
    let mut narrow = norm(2, Permission);
    narrow.authority = broad.authority.clone();
    narrow.authority_level = broad.authority_level;
    let result = check_norm_precedence(&[broad, narrow], 2).expect("same source");
    assert_eq!(result[0].defeats[0].reason, PriorityReason::Specialis);
    let mut broad = norm(1, Prohibition);
    broad.enacted = 1;
    broad.applicable.insert(1);
    broad.excepted.insert(1);
    let mut narrow = norm(2, Permission);
    narrow.authority = broad.authority.clone();
    narrow.authority_level = broad.authority_level;
    let result = check_norm_precedence(&[broad, narrow], 2).expect("same lineage");
    assert_eq!(result[0].defeats[0].reason, PriorityReason::Posterior);
    assert!(result[1].survivors.is_empty());
}

#[test]
fn incomparable_authorities_keep_conflicts_and_missing_selection_is_unknown() {
    let high = norm(1, Prohibition);
    let mut other = norm(2, Obligation);
    other.authority_level = high.authority_level;
    other.enacted = 100;
    let mut norms = [high, other];
    let result = check_norm_precedence(&norms, 1).expect("selected but incomparable");
    assert_eq!(result[0].conflicts, vec![(0, 1)]);
    assert_eq!(result[0].obligations, vec![1]);
    norms[0].authority_level = None;
    assert_eq!(
        check_norm_precedence(&norms, 1),
        Err(NormUnknown::MissingAuthority)
    );
}
