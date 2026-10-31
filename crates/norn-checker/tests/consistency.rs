//! The consistency engine must bite: each diagnostic fires on a planted
//! contradiction and stays silent on clean theories (norn#63 lane D).

use norn_checker::consistency::{
    Atom, ChaseBudget, ChaseError, ChaseOutcome, Dependency, Egd, Fact, RuleOrigin, Term, Tgd,
    Theory, TheoryError, Value, chase, check_non_vacuity, codes, non_vacuity_witness,
};

fn var(name: &str) -> Term {
    Term::Var(name.to_owned())
}

fn konst(text: &str) -> Term {
    Term::Const(text.to_owned())
}

fn atom(relation: &str, terms: &[Term]) -> Atom {
    Atom {
        relation: relation.to_owned(),
        terms: terms.to_vec(),
    }
}

fn origin(id: &str) -> RuleOrigin {
    RuleOrigin {
        artifact_id: Some(format!("artifact.{id}")),
        pointer: format!("/rules/{id}"),
    }
}

fn tgd(id: &str, body: &[Atom], head: &[Atom]) -> Dependency {
    Dependency::Tgd(Tgd {
        id: id.to_owned(),
        origin: origin(id),
        body: body.to_vec(),
        head: head.to_vec(),
    })
}

fn egd(id: &str, body: &[Atom], left: Term, right: Term) -> Dependency {
    Dependency::Egd(Egd {
        id: id.to_owned(),
        origin: origin(id),
        body: body.to_vec(),
        left,
        right,
    })
}

fn fact(relation: &str, values: &[&str]) -> Fact {
    Fact {
        relation: relation.to_owned(),
        values: values
            .iter()
            .map(|v| Value::Const((*v).to_owned()))
            .collect(),
    }
}

fn theory(dependencies: Vec<Dependency>) -> Theory {
    Theory { dependencies }
}

/// System A allows many customers per invoice; the canon says exactly one.
fn invoice_mapping() -> Theory {
    theory(vec![
        tgd(
            "map.a_invoice",
            &[atom("AInvoice", &[var("i"), var("c")])],
            &[atom("Invoice", &[var("i"), var("c")])],
        ),
        egd(
            "canon.invoice_one_customer",
            &[
                atom("Invoice", &[var("i"), var("c1")]),
                atom("Invoice", &[var("i"), var("c2")]),
            ],
            var("c1"),
            var("c2"),
        ),
        tgd(
            "canon.line_has_invoice",
            &[atom("Line", &[var("l")])],
            &[atom("LineOf", &[var("l"), var("i")])],
        ),
    ])
}

fn codes_of(diagnostics: &[norn_checker::Diagnostic]) -> Vec<&str> {
    diagnostics.iter().map(|d| d.code.as_str()).collect()
}

#[test]
fn clean_theory_has_a_witness_for_every_relation() {
    let theory = invoice_mapping();
    assert!(check_non_vacuity(&theory, ChaseBudget::default()).is_empty());
    let ChaseOutcome::Model(model) =
        non_vacuity_witness(&theory, "Line", ChaseBudget::default()).unwrap()
    else {
        panic!("expected a model");
    };
    assert!(model.iter().any(|f| f.relation == "LineOf"));
}

#[test]
fn one_customer_and_many_customers_overlap() {
    // "exactly one" against "one or more" is not a contradiction (norn#62):
    // data with one customer per invoice maps cleanly.
    let facts = [
        fact("AInvoice", &["inv1", "acme"]),
        fact("AInvoice", &["inv2", "acme"]),
    ];
    let outcome = chase(&invoice_mapping(), &facts, ChaseBudget::default()).unwrap();
    assert!(matches!(outcome, ChaseOutcome::Model(_)), "{outcome:?}");
}

#[test]
fn two_customers_for_one_invoice_is_a_minimal_conflict() {
    let facts = [
        fact("AInvoice", &["inv2", "acme"]),
        fact("AInvoice", &["inv1", "acme"]),
        fact("AInvoice", &["inv1", "globex"]),
    ];
    let ChaseOutcome::Conflict(conflict) =
        chase(&invoice_mapping(), &facts, ChaseBudget::default()).unwrap()
    else {
        panic!("expected a conflict");
    };
    assert_eq!(conflict.egd, "canon.invoice_one_customer");
    assert_eq!(
        conflict.rules,
        ["map.a_invoice", "canon.invoice_one_customer"]
    );
    assert_eq!(conflict.witness, [facts[1].clone(), facts[2].clone()]);
    let mut equated = [conflict.left.to_string(), conflict.right.to_string()];
    equated.sort();
    assert_eq!(equated, ["\"acme\"", "\"globex\""]);
    assert_eq!(conflict.matched.len(), 2);
}

#[test]
fn redundant_rules_are_dropped_from_the_conflict() {
    let mut theory = invoice_mapping();
    theory.dependencies.push(tgd(
        "map.a_invoice_copy",
        &[atom("AInvoice", &[var("i"), var("c")])],
        &[atom("Invoice", &[var("i"), var("c")])],
    ));
    let facts = [
        fact("AInvoice", &["inv1", "acme"]),
        fact("AInvoice", &["inv1", "globex"]),
    ];
    let ChaseOutcome::Conflict(conflict) = chase(&theory, &facts, ChaseBudget::default()).unwrap()
    else {
        panic!("expected a conflict");
    };
    assert_eq!(conflict.rules.len(), 2, "{:?}", conflict.rules);
    assert!(
        conflict
            .rules
            .contains(&"canon.invoice_one_customer".to_owned())
    );
}

#[test]
fn equations_resolve_labelled_nulls() {
    let mut theory = invoice_mapping();
    theory.dependencies.push(egd(
        "canon.single_invoice",
        &[atom("LineOf", &[var("l"), var("i")])],
        var("i"),
        konst("inv1"),
    ));
    let outcome = chase(&theory, &[fact("Line", &["l1"])], ChaseBudget::default()).unwrap();
    assert_eq!(
        outcome,
        ChaseOutcome::Model(vec![fact("Line", &["l1"]), fact("LineOf", &["l1", "inv1"])])
    );
}

/// A derived state contradicts an authored default: every posted invoice has
/// status `posted`, every invoice has status `draft`, and every invoice is
/// posted.
fn contradictory_lifecycle() -> Theory {
    theory(vec![
        tgd(
            "derived.invoice_posted",
            &[atom("Invoice", &[var("i"), var("s")])],
            &[atom("Posted", &[var("i")])],
        ),
        egd(
            "lifecycle.posted_status",
            &[
                atom("Posted", &[var("i")]),
                atom("Invoice", &[var("i"), var("s")]),
            ],
            var("s"),
            konst("posted"),
        ),
        egd(
            "lifecycle.initial_draft",
            &[atom("Invoice", &[var("i"), var("s")])],
            var("s"),
            konst("draft"),
        ),
        tgd(
            "unrelated",
            &[atom("Line", &[var("l")])],
            &[atom("LineOf", &[var("l"), var("i")])],
        ),
    ])
}

#[test]
fn vacuous_relation_names_the_minimal_rules_and_points_at_the_egd() {
    let theory = contradictory_lifecycle();
    let ChaseOutcome::Conflict(conflict) =
        non_vacuity_witness(&theory, "Invoice", ChaseBudget::default()).unwrap()
    else {
        panic!("expected a conflict");
    };
    assert_eq!(
        conflict.rules,
        [
            "derived.invoice_posted",
            "lifecycle.posted_status",
            "lifecycle.initial_draft"
        ]
    );
    let findings = check_non_vacuity(&theory, ChaseBudget::default());
    // Invoice and Posted (whose tuples imply an invoice only through Invoice)
    // are checked separately; only Invoice is forced into the contradiction.
    assert_eq!(codes_of(&findings), [codes::VACUOUS]);
    let finding = &findings[0];
    assert!(
        finding
            .message
            .starts_with("Invoice can never hold a tuple"),
        "{}",
        finding.message
    );
    assert!(finding.is_error());
    assert!(
        finding
            .artifact_id
            .as_deref()
            .is_some_and(|id| id.starts_with("artifact.lifecycle."))
    );
}

#[test]
fn existential_cycles_are_refused() {
    let theory = theory(vec![
        tgd(
            "person_has_parent",
            &[atom("Person", &[var("x")])],
            &[atom("Parent", &[var("x"), var("y")])],
        ),
        tgd(
            "parent_is_person",
            &[atom("Parent", &[var("x"), var("y")])],
            &[atom("Person", &[var("y")])],
        ),
    ]);
    assert_eq!(
        chase(&theory, &[], ChaseBudget::default()),
        Err(ChaseError::NotWeaklyAcyclic {
            rules: vec!["person_has_parent".to_owned()]
        })
    );
    let findings = check_non_vacuity(&theory, ChaseBudget::default());
    assert_eq!(codes_of(&findings), [codes::NOT_WEAKLY_ACYCLIC]);
    assert_eq!(
        findings[0].span.as_ref().unwrap().pointer,
        "/rules/person_has_parent"
    );
}

#[test]
fn exhausted_budgets_are_refusals_not_passes() {
    let budget = ChaseBudget {
        max_steps: 3,
        ..ChaseBudget::default()
    };
    let facts = [
        fact("AInvoice", &["inv1", "acme"]),
        fact("AInvoice", &["inv2", "acme"]),
    ];
    assert!(matches!(
        chase(&invoice_mapping(), &facts, budget),
        Err(ChaseError::BudgetExceeded {
            resource: "steps",
            ..
        })
    ));
    let findings = check_non_vacuity(&invoice_mapping(), budget);
    assert_eq!(codes_of(&findings), [codes::CHASE_REFUSED]);
    let budget = ChaseBudget {
        max_facts: 1,
        ..ChaseBudget::default()
    };
    assert!(matches!(
        chase(&invoice_mapping(), &facts, budget),
        Err(ChaseError::BudgetExceeded {
            resource: "facts",
            ..
        })
    ));
}

#[test]
fn malformed_theories_are_refused() {
    let duplicate = theory(vec![
        tgd("r", &[atom("A", &[var("x")])], &[atom("B", &[var("x")])]),
        tgd("r", &[atom("B", &[var("x")])], &[atom("C", &[var("x")])]),
    ]);
    assert_eq!(
        duplicate.validate(),
        Err(TheoryError::DuplicateRuleId("r".to_owned()))
    );
    let arity = theory(vec![tgd(
        "r",
        &[atom("A", &[var("x")])],
        &[atom("A", &[var("x"), var("x")])],
    )]);
    assert!(matches!(
        arity.validate(),
        Err(TheoryError::ArityMismatch { .. })
    ));
    let unbound = theory(vec![egd(
        "e",
        &[atom("A", &[var("x")])],
        var("x"),
        var("y"),
    )]);
    assert!(matches!(
        unbound.validate(),
        Err(TheoryError::UnboundVariable { .. })
    ));
    let empty = theory(vec![tgd("r", &[], &[atom("A", &[var("x")])])]);
    assert_eq!(
        empty.validate(),
        Err(TheoryError::EmptyConjunction("r".to_owned()))
    );
    for bad in [duplicate, arity, unbound, empty] {
        assert_eq!(
            codes_of(&check_non_vacuity(&bad, ChaseBudget::default())),
            [codes::MALFORMED]
        );
    }
    assert!(matches!(
        chase(
            &invoice_mapping(),
            &[fact("Invoice", &["only-one-column"])],
            ChaseBudget::default()
        ),
        Err(ChaseError::Theory(TheoryError::ArityMismatch { .. }))
    ));
    assert_eq!(
        non_vacuity_witness(&invoice_mapping(), "Nope", ChaseBudget::default()),
        Err(ChaseError::Theory(TheoryError::UnknownRelation(
            "Nope".to_owned()
        )))
    );
}

#[test]
fn a_later_refusal_keeps_earlier_contradictions() {
    // `Invoice` is vacuous within the budget; the frozen `Zeta` tuple, checked
    // later, drives a 40-step existential chain that exhausts it.
    let mut theory = contradictory_lifecycle();
    theory.dependencies.push(tgd(
        "zeta.0",
        &[atom("Zeta", &[var("x")])],
        &[atom("Zeta1", &[var("x"), var("y")])],
    ));
    for k in 1..40 {
        theory.dependencies.push(tgd(
            &format!("zeta.{k}"),
            &[atom(&format!("Zeta{k}"), &[var("x"), var("y")])],
            &[atom(&format!("Zeta{}", k + 1), &[var("y"), var("z")])],
        ));
    }
    let budget = ChaseBudget {
        max_steps: 2000,
        ..ChaseBudget::default()
    };
    assert!(matches!(
        non_vacuity_witness(&theory, "Invoice", budget),
        Ok(ChaseOutcome::Conflict(_))
    ));
    assert!(matches!(
        non_vacuity_witness(&theory, "Zeta", budget),
        Err(ChaseError::BudgetExceeded { .. })
    ));
    let findings = check_non_vacuity(&theory, budget);
    assert_eq!(codes_of(&findings), [codes::VACUOUS, codes::CHASE_REFUSED]);
}

/// Sol R1 S1: `e1` is redundant, but the chase without it enumerates
/// `e2`'s matches far past a small budget.
fn redundant_equation() -> Theory {
    theory(vec![
        tgd(
            "t",
            &[atom("A", &[var("x")])],
            &[
                atom("R", &[var("y1")]),
                atom("R", &[var("y2")]),
                atom("R", &[var("y3")]),
            ],
        ),
        egd("e1", &[atom("R", &[var("x")])], var("x"), konst("a")),
        egd(
            "e2",
            &[
                atom("R", &[var("u")]),
                atom("R", &[var("v")]),
                atom("R", &[var("w")]),
                atom("R", &[var("p")]),
                atom("R", &[var("q")]),
            ],
            konst("a"),
            konst("b"),
        ),
    ])
}

#[test]
fn minimisation_never_claims_minimality_on_a_refused_subset() {
    let facts = [fact("A", &["seed"])];
    let small = ChaseBudget {
        max_steps: 100,
        ..ChaseBudget::default()
    };
    assert!(matches!(
        chase(&redundant_equation(), &facts, small),
        Err(ChaseError::BudgetExceeded {
            resource: "steps",
            ..
        })
    ));
    let ChaseOutcome::Conflict(conflict) =
        chase(&redundant_equation(), &facts, ChaseBudget::default()).unwrap()
    else {
        panic!("expected a conflict");
    };
    assert_eq!(conflict.rules, ["t", "e2"]);
}

#[test]
fn the_null_budget_counts_created_nulls_not_labels() {
    // Sol R1 S2: a high input label must not exhaust the null budget.
    let theory = theory(vec![tgd(
        "a_has_b",
        &[atom("A", &[var("x")])],
        &[atom("B", &[var("x"), var("y")])],
    )]);
    let seeded = Fact {
        relation: "A".to_owned(),
        values: vec![Value::Null(4096)],
    };
    assert!(matches!(
        chase(&theory, &[seeded], ChaseBudget::default()),
        Ok(ChaseOutcome::Model(_))
    ));
    let one = ChaseBudget {
        max_nulls: 1,
        ..ChaseBudget::default()
    };
    assert!(matches!(
        non_vacuity_witness(&invoice_mapping(), "Line", one),
        Ok(ChaseOutcome::Model(_))
    ));
    let none = ChaseBudget {
        max_nulls: 0,
        ..ChaseBudget::default()
    };
    assert!(matches!(
        non_vacuity_witness(&invoice_mapping(), "Line", none),
        Err(ChaseError::BudgetExceeded {
            resource: "nulls",
            ..
        })
    ));
}

#[test]
fn a_generated_null_never_reuses_an_input_label() {
    // Sol R2 S3: with `Null(usize::MAX)` in the input, a fresh label would
    // alias it and fake a conflict; the chase must refuse instead.
    let theory = theory(vec![
        tgd(
            "t_has_q",
            &[atom("T", &[var("x")])],
            &[atom("Q", &[var("x"), var("y")])],
        ),
        egd(
            "q_matches_a",
            &[
                atom("Q", &[var("x"), var("y")]),
                atom("A", &[var("y"), var("z")]),
            ],
            var("x"),
            var("z"),
        ),
    ]);
    let facts = [
        Fact {
            relation: "A".to_owned(),
            values: vec![Value::Null(usize::MAX), Value::Const("a".to_owned())],
        },
        fact("T", &["b"]),
    ];
    assert!(matches!(
        chase(&theory, &facts, ChaseBudget::default()),
        Err(ChaseError::BudgetExceeded {
            resource: "null labels",
            ..
        })
    ));
}

#[test]
fn a_refused_relation_is_named() {
    let budget = ChaseBudget {
        max_steps: 3,
        ..ChaseBudget::default()
    };
    let findings = check_non_vacuity(&invoice_mapping(), budget);
    assert_eq!(codes_of(&findings), [codes::CHASE_REFUSED]);
    assert!(
        findings[0].message.starts_with("AInvoice: ")
            && findings[0]
                .message
                .ends_with("relations after it were not checked"),
        "{}",
        findings[0].message
    );
}
