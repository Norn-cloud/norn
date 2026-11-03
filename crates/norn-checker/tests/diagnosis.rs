//! DIAG-CORE-1, DIAG-REPAIR-1 and DIAG-UNKNOWN-1.
use norn_checker::consistency::diagnosis::{
    ChaseOracle, Declaration, Oracle, Unknown, Verdict, diagnose,
};
use norn_checker::consistency::{
    Atom, ChaseBudget, Dependency, Egd, Fact, RuleOrigin, Term, Theory, Value,
};
use proptest::prelude::*;
use std::collections::BTreeSet;

fn declaration(index: usize) -> Declaration {
    let origin = RuleOrigin {
        artifact_id: Some("fixture".into()),
        pointer: format!("/declarations/{index}"),
    };
    let relation = format!("r{}", index / 2);
    let mut declaration = Declaration {
        origin: origin.clone(),
        theory: Theory::default(),
        facts: vec![],
    };
    if index < 6 && index % 2 == 1 {
        declaration.theory.dependencies.push(Dependency::Egd(Egd {
            id: format!("law{index}"),
            origin,
            body: vec![Atom {
                relation,
                terms: vec![Term::Var("x".into())],
            }],
            left: Term::Const("a".into()),
            right: Term::Const("b".into()),
        }));
    } else {
        declaration.facts.push(Fact {
            relation,
            values: vec![Value::Const("a".into())],
        });
    }
    declaration
}

#[test]
fn three_independent_conflicts_among_thirty_declarations() {
    let background = Declaration {
        origin: RuleOrigin::default(),
        theory: Theory::default(),
        facts: vec![],
    };
    let declarations: Vec<_> = (0..30).map(declaration).collect();
    let editable = BTreeSet::from([0, 2, 4]);
    let mut oracle = ChaseOracle {
        background: &background,
        declarations: &declarations,
        budget: ChaseBudget::default(),
    };
    let result = diagnose(&mut oracle, 30, &editable, 10_000).unwrap();
    assert_eq!(
        result.cores,
        BTreeSet::from([
            BTreeSet::from([0, 1]),
            BTreeSet::from([2, 3]),
            BTreeSet::from([4, 5])
        ])
    );
    assert_eq!(result.corrections, BTreeSet::from([editable.clone()]));
    for core in &result.cores {
        assert_eq!(oracle.check(core), Verdict::Unsat);
        for member in core {
            let mut reduced = core.clone();
            reduced.remove(member);
            assert_eq!(oracle.check(&reduced), Verdict::Sat);
        }
    }
    assert_eq!(
        diagnose(&mut oracle, 30, &BTreeSet::new(), 10_000)
            .unwrap()
            .corrections
            .len(),
        0
    );
    assert_eq!(
        diagnose(&mut oracle, 30, &BTreeSet::new(), 0),
        Err(Unknown::Budget)
    );
    assert_eq!(
        diagnose(&mut oracle, 65, &editable, 10_000),
        Err(Unknown::TooManyDeclarations)
    );
    assert_eq!(
        diagnose(&mut oracle, 30, &BTreeSet::from([30]), 10_000),
        Err(Unknown::InvalidEditable)
    );
    oracle.budget.max_steps = 0;
    assert!(matches!(
        diagnose(&mut oracle, 30, &BTreeSet::new(), 100),
        Err(Unknown::Oracle(_))
    ));
}

struct Finite(Vec<BTreeSet<usize>>);
impl Oracle for Finite {
    type Unknown = ();
    fn check(&mut self, active: &BTreeSet<usize>) -> Verdict<()> {
        if self.0.iter().any(|core| core.is_subset(active)) {
            Verdict::Unsat
        } else {
            Verdict::Sat
        }
    }
}

fn exhaustive(
    oracle: &mut impl Oracle,
    count: usize,
    editable: &BTreeSet<usize>,
) -> (BTreeSet<BTreeSet<usize>>, BTreeSet<BTreeSet<usize>>) {
    let all: BTreeSet<_> = (0..count).collect();
    let mut cores = BTreeSet::new();
    let mut repairs = BTreeSet::new();
    for mask in 0..(1_usize << count) {
        let set: BTreeSet<_> = (0..count).filter(|i| mask & (1 << i) != 0).collect();
        if matches!(oracle.check(&set), Verdict::Unsat)
            && set.iter().all(|i| {
                let mut subset = set.clone();
                subset.remove(i);
                matches!(oracle.check(&subset), Verdict::Sat)
            })
        {
            cores.insert(set.clone());
        }
        let kept = all.difference(&set).copied().collect();
        if set.is_subset(editable)
            && matches!(oracle.check(&kept), Verdict::Sat)
            && set.iter().all(|i| {
                let mut restored: BTreeSet<_> = all.difference(&set).copied().collect();
                restored.insert(*i);
                matches!(oracle.check(&restored), Verdict::Unsat)
            })
        {
            repairs.insert(set);
        }
    }
    (cores, repairs)
}

#[test]
fn chase_matches_exhaustive_diagnosis() {
    let declarations: Vec<_> = (0..6).map(declaration).collect();
    let background = Declaration {
        origin: RuleOrigin::default(),
        theory: Theory::default(),
        facts: vec![],
    };
    let mut oracle = ChaseOracle {
        background: &background,
        declarations: &declarations,
        budget: ChaseBudget::default(),
    };
    let editable = BTreeSet::from([0, 2, 4]);
    let result = diagnose(&mut oracle, 6, &editable, 10_000).unwrap();
    assert_eq!(
        (result.cores, result.corrections),
        exhaustive(&mut oracle, 6, &editable)
    );
}

proptest! {
    #[test]
    fn generated_conflicts_match_exhaustive(masks in proptest::collection::vec(1_u8..16, 0..8), editable_mask in 0_u8..16) {
        let mut oracle = Finite(masks.into_iter().map(|mask| (0..4).filter(|i| mask & (1 << i) != 0).collect()).collect());
        let editable = (0..4).filter(|i| editable_mask & (1 << i) != 0).collect();
        let result = diagnose(&mut oracle, 4, &editable, 10_000).unwrap();
        prop_assert_eq!((result.cores, result.corrections), exhaustive(&mut oracle, 4, &editable));
    }
}
