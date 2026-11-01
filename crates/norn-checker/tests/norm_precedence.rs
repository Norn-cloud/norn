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
