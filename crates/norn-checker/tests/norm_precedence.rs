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

fn origins(norms: &[GroundedNorm]) -> Vec<norn_checker::consistency::RuleOrigin> {
    norms
        .iter()
        .map(|n| norn_checker::consistency::RuleOrigin {
            artifact_id: Some("rules".into()),
            pointer: format!("/{}", n.id),
        })
        .collect()
}

fn resolve(
    norms: &[GroundedNorm],
    contexts: usize,
    edges: &[(usize, usize)],
) -> Result<
    Vec<norn_checker::norm_precedence::NormContext>,
    norn_checker::norm_precedence::NormRefusal,
> {
    check_norm_precedence(
        norms,
        &origins(norms),
        &vec![Company; norms.len()],
        edges,
        &BTreeSet::new(),
        contexts,
        1000,
    )
}

#[test]
fn every_declared_three_norm_grounding_has_the_expected_survivors() -> Result<(), String> {
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
                            if exceptions & bit != 0 && applicable & bit != 0 {
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
                        let result =
                            resolve(&norms, 1, &[(0, 1), (1, 2)]).map_err(|e| format!("{e:?}"))?;
                        let context = result.first().ok_or("missing context")?;
                        assert_eq!(
                            context.survivors.iter().copied().collect::<BTreeSet<_>>(),
                            expected
                        );
                        assert_eq!(context.conflicts, []);
                        assert!(context.defeats.iter().all(|d| expected.contains(&d.winner)
                            && !expected.contains(&d.loser)
                            && d.reason == PriorityReason::Declared));
                        assert_eq!(
                            context.obligations.iter().copied().collect::<BTreeSet<_>>(),
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
fn authority_enactment_and_specificity_cannot_supply_priority() -> Result<(), String> {
    let mut broad = norm(1, Prohibition);
    broad.applicable.insert(1);
    let mut narrow = norm(2, Permission);
    narrow.authority = broad.authority.clone();
    narrow.authority_level = broad.authority_level;
    let mut norms = [broad, narrow];
    for enacted in [0, 100] {
        norms.first_mut().ok_or("missing norm")?.enacted = enacted;
        assert!(matches!(
            resolve(&norms, 2, &[]),
            Err(NormRefusal::Conflict(_))
        ));
    }
    norms.get_mut(1).ok_or("missing norm")?.authority_level = Some(900);
    assert!(matches!(
        resolve(&norms, 2, &[]),
        Err(NormRefusal::Conflict(_))
    ));
    let result = resolve(&norms, 2, &[(0, 1)]).map_err(|e| format!("{e:?}"))?;
    assert_eq!(result.first().ok_or("missing context")?.survivors, vec![0]);
    for n in &mut norms {
        n.authority_level = None;
    }
    assert!(resolve(&norms, 2, &[(0, 1)]).is_ok());
    Ok(())
}

#[test]
fn cross_layer_and_protection_refuse_before_precedence() {
    let norms = [norm(2, Obligation), norm(1, Prohibition)];
    let found = check_norm_precedence(
        &norms,
        &origins(&norms),
        &[Religious, Company],
        &[(0, 1)],
        &BTreeSet::new(),
        1,
        1000,
    );
    assert!(matches!(found, Err(NormRefusal::Conflict(_))));
    let protected = check_norm_precedence(
        &norms,
        &origins(&norms),
        &[Company; 2],
        &[(0, 1)],
        &BTreeSet::from([1]),
        1,
        1000,
    );
    assert!(matches!(protected, Err(NormRefusal::Conflict(_))));
}

#[test]
fn invalid_grounding_never_resolves() -> Result<(), String> {
    let mut norms = [norm(1, Permission), norm(2, Prohibition)];
    assert!(resolve(&norms, 0, &[]).is_err());
    assert!(resolve(&norms, 257, &[]).is_err());
    norms
        .first_mut()
        .ok_or("missing norm")?
        .applicable
        .insert(1);
    assert!(matches!(
        resolve(&norms, 1, &[]),
        Err(NormRefusal::Grounding(NormUnknown::Invalid))
    ));
    Ok(())
}

use norn_checker::consistency::RuleOrigin;
use norn_checker::norm_precedence::{
    ConstitutionLayer::{self, Company, Legal, Religious},
    DeclaredOrderReport, NormRefusal, diagnose_declared_order,
};

#[allow(
    clippy::unwrap_used,
    reason = "test helper asserts successful finite diagnosis"
)]
fn declared(
    norms: &[GroundedNorm],
    layers: &[ConstitutionLayer],
    edges: &[(usize, usize)],
    protected: &[usize],
) -> DeclaredOrderReport {
    let origins: Vec<_> = norms
        .iter()
        .map(|n| RuleOrigin {
            artifact_id: Some("synthetic.rules".into()),
            pointer: format!("/rules/{}", n.id),
        })
        .collect();
    diagnose_declared_order(
        norms,
        &origins,
        layers,
        edges,
        &protected.iter().copied().collect(),
        2,
        1000,
    )
    .unwrap()
}

#[test]
fn declared_only_priorities_and_constitution_layers() {
    let norms = [norm(2, Obligation), norm(1, Prohibition)];
    let resolved = declared(&norms, &[Company; 2], &[(0, 1)], &[]);
    assert_eq!(resolved.diagnosis.cores.len(), 0);
    let refused = declared(&norms, &[Company; 2], &[], &[]);
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
    let clash = declared(&norms, &[Religious, Company], &[(0, 1)], &[]);
    assert_ne!(clash.diagnosis.cores.len(), 0);
    let mut inactive = [norm(0, Prohibition), norm(1, Permission)];
    if let Some(n) = inactive.get_mut(1) {
        n.excepted.insert(0);
    }
    let masked = declared(&inactive, &[Religious, Company], &[(1, 0)], &[]);
    assert_ne!(masked.diagnosis.cores.len(), 0);
    let stack = [
        norm(0, Obligation),
        norm(1, Obligation),
        norm(2, Permission),
    ];
    let valid = declared(&stack, &[Religious, Legal, Company], &[], &[]);
    assert_eq!(valid.diagnosis.cores.len(), 0);
}

#[test]
fn exact_cycles_transitivity_and_protected_laws() {
    let norms = [
        norm(0, Obligation),
        norm(1, Obligation),
        norm(2, Obligation),
        norm(3, Obligation),
    ];
    let cyclic = declared(&norms, &[Company; 4], &[(0, 1), (1, 2), (2, 0)], &[]);
    assert_eq!(
        cyclic.diagnosis.cores,
        BTreeSet::from([BTreeSet::from([0, 1, 2])])
    );
    let reflexive = declared(&norms, &[Company; 4], &[(0, 0)], &[]);
    assert_eq!(
        reflexive.diagnosis.cores,
        BTreeSet::from([BTreeSet::from([0])])
    );
    let endpoints = [
        norm(0, Obligation),
        norm(1, Obligation),
        norm(2, Prohibition),
    ];
    let transitive = declared(&endpoints, &[Company; 3], &[(0, 1), (1, 2)], &[]);
    assert_eq!(transitive.diagnosis.cores.len(), 0);
    let protected = declared(&endpoints, &[Company; 3], &[(0, 1), (1, 2)], &[2]);
    assert_eq!(
        protected.diagnosis.cores,
        BTreeSet::from([BTreeSet::from([1, 2])])
    );
}

#[test]
fn declared_order_is_independent_of_catalog_permutation() {
    for swap in [false, true] {
        for reverse in [false, true] {
            let case = format!("swap {swap} reverse {reverse}");
            let mut norms = [norm(0, Obligation), norm(1, Prohibition)];
            for n in &mut norms {
                n.authority = ("board".into(), "1".into());
                n.authority_level = Some(0);
            }
            if let Some(n) = norms.get_mut(1) {
                n.applicable.insert(1);
                n.enacted = u64::from(reverse);
            }
            let mut edge = if reverse { (1, 0) } else { (0, 1) };
            if swap {
                norms.swap(0, 1);
                edge = (
                    1_usize.saturating_sub(edge.0),
                    1_usize.saturating_sub(edge.1),
                );
            }
            assert_eq!(
                declared(&norms, &[Company; 2], &[edge], &[])
                    .diagnosis
                    .cores
                    .len(),
                0,
                "{case}"
            );
            let refusal = declared(&norms, &[Company; 2], &[], &[]);
            assert_eq!(refusal.diagnosis.cores.len(), 1, "{case}");
            assert!(
                refusal
                    .diagnostics
                    .iter()
                    .any(|d| d.code.as_str() == "consistency.normative.legacy_resolution"),
                "{case}"
            );
            assert_eq!(
                declared(&norms, &[Company; 2], &[edge], &[edge.1])
                    .diagnosis
                    .cores
                    .len(),
                1,
                "{case}"
            );
            assert!(
                matches!(resolve(&norms, 2, &[]), Err(NormRefusal::Conflict(_))),
                "{case}"
            );
            let resolved = resolve(&norms, 2, &[edge]).unwrap_or_else(|e| panic!("{case}: {e:?}"));
            assert!(
                resolved
                    .first()
                    .is_some_and(|c| c.survivors == vec![edge.0]),
                "{case}"
            );
        }
    }
}

#[test]
fn malformed_binding_and_budget_exhaustion_remain_unknown() {
    let norms = [norm(0, Obligation), norm(1, Prohibition)];
    let origins: [RuleOrigin; 2] = std::array::from_fn(|i| RuleOrigin {
        artifact_id: Some("rules".into()),
        pointer: format!("/{i}"),
    });
    for (edges, protected, budget) in [
        (vec![(0, 2)], BTreeSet::new(), 100),
        (vec![], BTreeSet::from([2]), 100),
        (vec![], BTreeSet::new(), 0),
    ] {
        assert!(
            diagnose_declared_order(
                &norms,
                &origins,
                &[Company; 2],
                &edges,
                &protected,
                2,
                budget
            )
            .is_err()
        );
    }
}
