//! Shared Datalog contract regression tests.

#![allow(clippy::expect_used, reason = "bounded contract fixtures fail fast")]

use norn_datalog::{
    Aggregate, Atom, Budget, Code, Literal, Mode, Predicate, Program, RelationKind, Rule, Term,
    Value, Version, overlap, validate,
};

fn atom(predicate: &str, variables: &[&str]) -> Atom {
    Atom {
        predicate: predicate.to_owned(),
        terms: variables
            .iter()
            .map(|name| Term::Variable((*name).to_owned()))
            .collect(),
    }
}

fn rule(id: &str, head: &str, positive: &[&str], negative: &[&str]) -> Rule {
    Rule {
        id: id.to_owned(),
        mode: Mode::Validation,
        head: atom(head, &["x"]),
        body: positive
            .iter()
            .map(|name| Literal::Positive(atom(name, &["x"])))
            .chain(
                negative
                    .iter()
                    .map(|name| Literal::Negative(atom(name, &["x"]))),
            )
            .collect(),
    }
}

fn program(inputs: &[&str], derived: &[&str], rules: Vec<Rule>) -> Program {
    Program {
        version: Version::V1,
        predicates: inputs
            .iter()
            .map(|name| (name, RelationKind::Input))
            .chain(derived.iter().map(|name| (name, RelationKind::Derived)))
            .map(|(name, kind)| Predicate {
                name: (*name).to_owned(),
                arity: 1,
                kind,
            })
            .collect(),
        rules,
    }
}

fn code(program: &Program) -> Code {
    validate(program, Budget::default())
        .expect_err("program must be refused")
        .code
}

#[test]
fn positive_mutual_recursion_is_admitted_in_one_component() {
    let program = program(
        &["seed"],
        &["a", "b"],
        vec![
            rule("a.seed", "a", &["seed"], &[]),
            rule("a.b", "a", &["b"], &[]),
            rule("b.a", "b", &["a"], &[]),
        ],
    );
    let plan = validate(&program, Budget::default()).expect("finite positive recursion");
    let component = plan
        .groups
        .iter()
        .find(|group| group.predicates == ["a", "b"])
        .expect("mutual SCC");
    assert!(component.recursive);
    assert_eq!(component.stratum, 0);
}

#[test]
fn stratified_negation_uses_minimum_strata_and_body_order_does_not_bind() {
    let mut program = program(
        &["seed", "held"],
        &["clear", "result"],
        vec![
            rule("clear", "clear", &["seed"], &["held"]),
            rule("result", "result", &["seed"], &["clear"]),
        ],
    );
    for rule in &mut program.rules {
        rule.body.reverse();
    }
    let plan = validate(&program, Budget::default()).expect("stratified negation");
    let strata: Vec<_> = plan
        .groups
        .iter()
        .map(|group| (group.predicates.clone(), group.stratum))
        .collect();
    assert_eq!(
        strata,
        vec![
            (vec!["held".to_owned()], 0),
            (vec!["seed".to_owned()], 0),
            (vec!["clear".to_owned()], 1),
            (vec!["result".to_owned()], 2)
        ]
    );
}

#[test]
fn self_and_mutual_negative_cycles_are_refused() {
    let self_cycle = program(&["seed"], &["a"], vec![rule("a", "a", &["seed"], &["a"])]);
    let mutual = program(
        &["seed"],
        &["a", "b"],
        vec![
            rule("a", "a", &["b"], &[]),
            rule("b", "b", &["seed"], &["a"]),
        ],
    );
    assert_eq!(code(&self_cycle), Code::RecursionThroughNegation);
    assert_eq!(code(&mutual), Code::RecursionThroughNegation);
}

#[test]
fn unbound_head_and_negative_variables_are_refused() {
    let mut head = rule("invent", "out", &["seed"], &[]);
    head.head = atom("out", &["fresh"]);
    assert_eq!(
        code(&program(&["seed"], &["out"], vec![head])),
        Code::UnsafeHeadVariable
    );
    let mut negative = rule("absent", "out", &["seed"], &[]);
    negative
        .body
        .push(Literal::Negative(atom("other", &["unbound"])));
    assert_eq!(
        code(&program(&["seed", "other"], &["out"], vec![negative])),
        Code::UnsafeNegativeVariable
    );
}

#[test]
fn duplicate_ids_signatures_arity_and_undefined_symbols_are_refused() {
    let valid = program(
        &["seed"],
        &["out"],
        vec![rule("out", "out", &["seed"], &[])],
    );
    let mut duplicate = valid.clone();
    duplicate.rules.extend(valid.rules.clone());
    assert_eq!(code(&duplicate), Code::DuplicateRuleId);
    let mut duplicate = valid.clone();
    duplicate.predicates.extend(valid.predicates.clone());
    assert_eq!(code(&duplicate), Code::DuplicatePredicate);
    let mut mismatch = valid;
    mismatch.rules.first_mut().expect("rule").head.terms.clear();
    assert_eq!(code(&mismatch), Code::ArityMismatch);
    let missing = program(&[], &["out"], vec![rule("out", "out", &["missing"], &[])]);
    assert_eq!(code(&missing), Code::UndefinedPredicate);
    let unproduced = program(&[], &["out"], vec![]);
    assert_eq!(code(&unproduced), Code::UndefinedPredicate);
    let input_head = program(&["seed"], &[], vec![rule("seed", "seed", &["seed"], &[])]);
    assert_eq!(code(&input_head), Code::InputInHead);
}

#[test]
fn declaration_and_clause_permutations_preserve_sccs_and_strata() {
    let original = overlap::declared_metadata_v1();
    let expected = validate(&original, Budget::default()).expect("pack");
    let mut permuted = original;
    permuted.predicates.reverse();
    permuted.rules.reverse();
    for rule in &mut permuted.rules {
        rule.body.reverse();
    }
    assert_eq!(
        validate(&permuted, Budget::default()).expect("permuted pack"),
        expected
    );
}

#[test]
fn positive_dependencies_are_scheduled_before_alphabetically_earlier_consumers() {
    let program = program(&["z"], &["a"], vec![rule("a", "a", &["z"], &[])]);
    let plan = validate(&program, Budget::default()).expect("dependency order");
    assert_eq!(
        plan.groups
            .iter()
            .map(|group| group.predicates.clone())
            .collect::<Vec<_>>(),
        vec![vec!["z".to_owned()], vec!["a".to_owned()]]
    );
}

#[test]
fn structural_and_shared_traversal_budgets_fail_closed() {
    let program = program(
        &["seed"],
        &["out"],
        vec![rule("out", "out", &["seed"], &[])],
    );
    for budget in [
        Budget {
            max_predicates: 0,
            ..Budget::default()
        },
        Budget {
            max_rules: 0,
            ..Budget::default()
        },
        Budget {
            max_atoms: 0,
            ..Budget::default()
        },
        Budget {
            max_terms: 0,
            ..Budget::default()
        },
        Budget {
            max_text_bytes: 0,
            ..Budget::default()
        },
        Budget {
            max_work: 0,
            ..Budget::default()
        },
        // Clears preflight, then refuses graph work on the same meter.
        Budget {
            max_work: 100,
            ..Budget::default()
        },
    ] {
        assert_eq!(
            validate(&program, budget)
                .expect_err("bounded refusal")
                .code,
            Code::BudgetExhausted
        );
    }
}

#[test]
fn every_aggregate_is_refused_even_min_max_and_even_outside_recursion() {
    for aggregate in [
        Aggregate::Min,
        Aggregate::Max,
        Aggregate::Sum,
        Aggregate::Count,
    ] {
        for recursive in [false, true] {
            let mut clause = rule(
                "aggregate",
                "out",
                if recursive { &["out"] } else { &["seed"] },
                &[],
            );
            clause.head.terms = vec![Term::Aggregate(aggregate)];
            assert_eq!(
                code(&program(&["seed"], &["out"], vec![clause])),
                Code::AggregateNotAdmitted
            );
        }
    }
}

#[test]
fn function_arithmetic_fresh_values_and_unknown_fields_do_not_decode() {
    for kind in ["function", "add", "fresh", "constructor"] {
        let encoded = serde_json::json!({"kind":kind,"value":{"name":"successor","arguments":[{"kind":"variable","value":"x"}]}});
        assert!(serde_json::from_value::<Term>(encoded).is_err());
    }
    assert!(
        serde_json::from_value::<Atom>(
            serde_json::json!({"predicate":"p","terms":[],"callback":"invent"})
        )
        .is_err()
    );
}

#[test]
fn canonical_wire_uses_existing_encoder_and_stable_codes() {
    let program = overlap::declared_metadata_v1();
    let bytes = program.canonical_json().expect("canonical pack");
    assert_eq!(
        serde_json::from_slice::<Program>(&bytes).expect("round trip"),
        program
    );
    assert_eq!(
        bytes,
        norn_canonical::canonical_json_of(&program).expect("canonical owner")
    );
    assert_eq!(
        serde_json::to_value(Code::UnsafeHeadVariable).expect("code"),
        "datalog.unsafe_head_variable"
    );
    assert!(
        program
            .rules
            .iter()
            .any(|rule| rule.mode == Mode::Validation)
    );
    assert!(
        program
            .rules
            .iter()
            .any(|rule| rule.mode == Mode::Entailment)
    );
}

#[test]
fn finite_ground_seeds_are_allowed_but_canonical_collisions_are_refused() {
    let mut clause = rule("seed", "out", &[], &[]);
    clause.head.terms = vec![Term::Constant(Value::Text("é".to_owned()))];
    let mut program = program(&[], &["out"], vec![clause]);
    assert!(validate(&program, Budget::default()).is_ok());
    program.rules.first_mut().expect("rule").head.terms =
        vec![Term::Constant(Value::Text("e\u{301}".to_owned()))];
    assert_eq!(code(&program), Code::NoncanonicalConstant);
}

#[test]
fn registered_inheritance_step_is_admitted_bounded_nonrecursive_shared_ast() {
    let program = overlap::registered_inheritance_step_v1();
    let plan = validate(
        &program,
        Budget {
            max_predicates: 5,
            max_rules: 2,
            max_atoms: 6,
            max_terms: 16,
            max_text_bytes: 1_024,
            max_work: 20_000,
        },
    )
    .expect("fixed inheritance-step schema");
    assert!(
        plan.groups
            .iter()
            .all(|group| !group.recursive && group.stratum == 0)
    );
    assert_eq!(
        program
            .rules
            .iter()
            .filter(|rule| rule.mode == Mode::Validation)
            .count(),
        1
    );
    assert_eq!(
        program.rules.last().expect("cycle rule").id,
        "registered.inheritance.cycle"
    );
    let mut unsafe_program = program;
    unsafe_program.rules[0]
        .head
        .terms
        .push(Term::Variable("fresh".to_owned()));
    assert!(validate(&unsafe_program, Budget::default()).is_err());
}
