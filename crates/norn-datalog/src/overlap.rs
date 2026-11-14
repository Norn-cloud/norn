//! Small declared-metadata overlap pack. No evaluator, findings plumbing, or
//! runtime OWL rules. Relation columns are owned by `norn_ir::facts` v1.
//!
//! Covers declared inheritance cycles, a field/link member-name collision,
//! and explicitly redeclared direct-parent link cardinality widening. These
//! checks do not establish inherited completeness, record cardinality, or
//! semantic type disjointness; the projection reports those coverage gaps.

use norn_ir::facts::Predicate as FactPredicate;

use crate::{Atom, Literal, Mode, Predicate, Program, RelationKind, Rule, Term, Value, Version};

/// Shared AST pack, consumed unchanged by future definition/record adapters.
/// Every relation carries an explicit scope column and joins it throughout.
/// Callers must additionally isolate the selected versioned definition catalog:
/// equal scopes do not make unrelated snapshots or packages one model.
#[must_use]
pub fn declared_metadata_v1() -> Program {
    let mut predicates: Vec<_> = [
        FactPredicate::Extends,
        FactPredicate::Field,
        FactPredicate::Link,
    ]
    .into_iter()
    .map(|predicate| Predicate {
        name: predicate.name().to_owned(),
        arity: predicate.arity(),
        kind: RelationKind::Input,
    })
    .collect();
    for (name, arity) in [
        ("ancestor", 3),
        ("inheritance_cycle", 2),
        ("member_collision", 3),
        ("cardinality_widening", 4),
    ] {
        predicates.push(Predicate {
            name: name.to_owned(),
            arity,
            kind: RelationKind::Derived,
        });
    }
    let mut rules = structural_rules();
    rules.extend(cardinality_rules());
    Program {
        version: Version::V1,
        predicates,
        rules,
    }
}

/// Fixed one-edge query for a complete, selected registered definition catalog.
///
/// This is deliberately not the unbounded all-pairs `ancestor` pack. The
/// checker supplies one root, one frontier and at most one registered parent
/// edge per invocation, then meters the next invocation. Scope identifies the
/// selected catalog within that invocation; IDs never cross catalog calls.
/// Raw declared/incomplete `facts::Projection` is not a closed-world input.
#[must_use]
pub fn registered_inheritance_step_v1() -> Program {
    Program {
        version: Version::V1,
        predicates: [
            ("query_root", 2, RelationKind::Input),
            ("frontier", 2, RelationKind::Input),
            ("extends", 3, RelationKind::Input),
            ("next", 2, RelationKind::Derived),
            ("inheritance_cycle", 2, RelationKind::Derived),
        ]
        .into_iter()
        .map(|(name, arity, kind)| Predicate {
            name: name.to_owned(),
            arity,
            kind,
        })
        .collect(),
        rules: vec![
            rule(
                "registered.inheritance.step",
                Mode::Entailment,
                atom("next", &["scope", "parent"]),
                vec![
                    atom("frontier", &["scope", "node"]),
                    atom("extends", &["scope", "node", "parent"]),
                ],
            ),
            rule(
                "registered.inheritance.cycle",
                Mode::Validation,
                atom("inheritance_cycle", &["scope", "root"]),
                vec![
                    atom("query_root", &["scope", "root"]),
                    atom("next", &["scope", "root"]),
                ],
            ),
        ],
    }
}

fn structural_rules() -> Vec<Rule> {
    vec![
        rule(
            "overlap.ancestor.direct",
            Mode::Entailment,
            atom("ancestor", &["scope", "child", "parent"]),
            vec![atom("extends", &["scope", "child", "parent"])],
        ),
        rule(
            "overlap.ancestor.transitive",
            Mode::Entailment,
            atom("ancestor", &["scope", "child", "root"]),
            vec![
                atom("ancestor", &["scope", "child", "parent"]),
                atom("extends", &["scope", "parent", "root"]),
            ],
        ),
        rule(
            "overlap.inheritance.cycle",
            Mode::Validation,
            atom("inheritance_cycle", &["scope", "type"]),
            vec![atom("ancestor", &["scope", "type", "type"])],
        ),
        rule(
            "overlap.members.disjoint",
            Mode::Validation,
            atom("member_collision", &["scope", "owner", "member"]),
            vec![
                atom(
                    "field",
                    &["scope", "owner", "member", "kind", "nullable", "computed"],
                ),
                atom(
                    "link",
                    &[
                        "scope", "owner", "member", "target", "to", "from", "required", "acyclic",
                    ],
                ),
            ],
        ),
    ]
}

fn cardinality_rules() -> Vec<Rule> {
    let mut rules = Vec::new();
    for (dimension, parent, child) in [
        (
            "to",
            [constant("one"), variable("p_from"), variable("p_required")],
            [constant("many"), variable("c_from"), variable("c_required")],
        ),
        (
            "from",
            [variable("p_to"), constant("one"), variable("p_required")],
            [variable("c_to"), constant("many"), variable("c_required")],
        ),
        (
            "required",
            [
                variable("p_to"),
                variable("p_from"),
                Term::Constant(Value::Bool(true)),
            ],
            [
                variable("c_to"),
                variable("c_from"),
                Term::Constant(Value::Bool(false)),
            ],
        ),
    ] {
        let mut head = atom("cardinality_widening", &["scope", "child", "member"]);
        head.terms.push(constant(dimension));
        rules.push(rule(
            &format!("overlap.cardinality.{dimension}"),
            Mode::Validation,
            head,
            vec![
                atom("extends", &["scope", "child", "parent"]),
                link("parent", "p_target", "p_acyclic", parent),
                link("child", "c_target", "c_acyclic", child),
            ],
        ));
    }
    rules
}

fn variable(name: &str) -> Term {
    Term::Variable(name.to_owned())
}

fn constant(text: &str) -> Term {
    Term::Constant(Value::Text(text.to_owned()))
}

fn atom(predicate: &str, variables: &[&str]) -> Atom {
    Atom {
        predicate: predicate.to_owned(),
        terms: variables.iter().map(|name| variable(name)).collect(),
    }
}

fn link(owner: &str, target: &str, acyclic: &str, cardinality: [Term; 3]) -> Atom {
    let mut link = atom("link", &["scope", owner, "member", target]);
    link.terms.extend(cardinality);
    link.terms.push(variable(acyclic));
    link
}

fn rule(id: &str, mode: Mode, head: Atom, premises: Vec<Atom>) -> Rule {
    Rule {
        id: id.to_owned(),
        mode,
        head,
        body: premises.into_iter().map(Literal::Positive).collect(),
    }
}
