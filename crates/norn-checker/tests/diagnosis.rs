//! DIAG-CORE-1, DIAG-REPAIR-1 and DIAG-UNKNOWN-1.
use norn_checker::consistency::diagnosis::{
    ChaseOracle, Declaration, Oracle, Unknown, Verdict, diagnose,
};
use norn_checker::consistency::{
    Atom, ChaseBudget, Dependency, Egd, Fact, RuleOrigin, Term, Theory, Value,
};
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

/// Every oracle over four declarations is an antichain of non-empty minimal
/// cores (Dedekind M(4) = 168, less the antichain holding the empty set).
#[test]
fn every_conflict_antichain_matches_exhaustive() {
    let members =
        |mask: u8| -> BTreeSet<usize> { (0..4).filter(|i| mask & (1 << i) != 0).collect() };
    let masks: Vec<u8> = (1_u8..16).collect();
    let antichains: Vec<Vec<u8>> = (0_u16..(1 << masks.len()))
        .map(|family| {
            masks
                .iter()
                .enumerate()
                .filter(|(bit, _)| family & (1 << bit) != 0)
                .map(|(_, mask)| *mask)
                .collect::<Vec<u8>>()
        })
        .filter(|cores| {
            !cores.iter().any(|small| {
                cores
                    .iter()
                    .any(|large| small != large && small & large == *small)
            })
        })
        .collect();
    assert_eq!(antichains.len(), 167);
    for cores in &antichains {
        for editable_mask in 0_u8..16 {
            let mut oracle = Finite(cores.iter().map(|mask| members(*mask)).collect());
            let editable = members(editable_mask);
            let result = diagnose(&mut oracle, 4, &editable, 10_000).unwrap();
            assert_eq!(
                (result.cores, result.corrections),
                exhaustive(&mut oracle, 4, &editable),
                "cores {cores:?} editable {editable:?}"
            );
        }
    }
}

#[test]
fn jewelry_staffing_closed_world_and_independent_repair() {
    use norn_checker::consistency::finite::FiniteOracle;
    use norn_expr::schema::Schema;
    let people = Schema::String(Some(
        ["person-a", "person-b", "person-c"]
            .map(String::from)
            .into(),
    ));
    let schema = Schema::Object {
        properties: [("first".into(), people.clone()), ("second".into(), people)].into(),
        required: ["first".into(), "second".into()].into(),
    };
    // Exactly two accountable assayers, seller person-a excluded, closed roster a/b.
    // The finite universe admits person-c only as a proposed roster correction.
    let predicates = [
        "first != second",
        "first != 'person-a' and second != 'person-a'",
        "first != 'person-c' and second != 'person-c'",
    ];
    let mut oracle = FiniteOracle::new(&schema, &predicates).unwrap();
    assert_eq!(oracle.assignments, 9);
    let result = diagnose(&mut oracle, 3, &BTreeSet::from([2]), 1000).unwrap();
    assert_eq!(result.cores, BTreeSet::from([BTreeSet::from([0, 1, 2])]));
    assert_eq!(result.corrections, BTreeSet::from([BTreeSet::from([2])]));
    assert_eq!(
        (result.cores, result.corrections),
        exhaustive(&mut oracle, 3, &BTreeSet::from([2]))
    );
    let corrected = [predicates[0], predicates[1], "true"];
    assert_eq!(
        FiniteOracle::new(&schema, &corrected)
            .unwrap()
            .check(&BTreeSet::from([0, 1, 2])),
        Verdict::Sat
    );
    assert_eq!(
        FiniteOracle::new(&Schema::Integer, &[]).err(),
        Some(norn_checker::table_analysis::Unknown::UnboundedDomain)
    );
    let too_many = vec!["true"; 1000];
    assert_eq!(
        FiniteOracle::new(&schema, &too_many).err(),
        Some(norn_checker::table_analysis::Unknown::Limit)
    );
}

#[test]
fn a_backend_that_changes_its_answers_never_publishes_a_core() {
    struct Flaky(BTreeSet<BTreeSet<usize>>);
    impl Oracle for Flaky {
        type Unknown = ();
        fn check(&mut self, active: &BTreeSet<usize>) -> Verdict<()> {
            if !active.is_empty() && self.0.insert(active.clone()) {
                Verdict::Unsat
            } else {
                Verdict::Sat
            }
        }
    }
    assert_eq!(
        diagnose(&mut Flaky(BTreeSet::new()), 2, &BTreeSet::new(), 1000),
        Err(Unknown::InconsistentOracle)
    );
}
