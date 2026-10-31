//! The separate obligations of a mapping between two systems (norn#62
//! Astra verdict): schema compatibility, instance validation and mapping
//! totality. "Exactly one" against "one or more" overlap, so a source that
//! permits more than the target is a warning with a witness, not a
//! contradiction; only a source tuple that can never be exchanged, or data
//! that actually breaks a constraint, is an error.

use std::collections::{BTreeMap, BTreeSet};

use super::chase::{ChaseBudget, ChaseError, ChaseOutcome, Fact, Value, chase, first_violation};
use super::theory::{Atom, Dependency, RuleOrigin, Term, Tgd, Theory, TheoryError};
use super::{codes, conflict_message, finding, origin_of, refusal};
use crate::{Diagnostic, DiagnosticSeverity};

/// A data-exchange setting: source constraints, source-to-target TGDs and
/// target (canon) constraints. Relation names are disjoint across the two
/// systems; derivation rules belong in `mapping`, which is chased, while
/// `source` and `target` are checked closed-world.
#[derive(Debug, Clone, Copy)]
pub struct Exchange<'a> {
    /// Every source relation with its column names, constrained or not.
    pub source_schema: &'a BTreeMap<String, Vec<String>>,
    /// Source system constraints.
    pub source: &'a Theory,
    /// TGDs from source to target relations.
    pub mapping: &'a Theory,
    /// Target constraints.
    pub target: &'a Theory,
}

impl Exchange<'_> {
    fn merged(&self) -> Theory {
        join(&[self.source, self.mapping, self.target], |_| true)
    }

    fn validate(&self) -> Result<(), ChaseError> {
        if let Some(egd) = self
            .mapping
            .dependencies
            .iter()
            .find(|d| matches!(d, Dependency::Egd(_)))
        {
            return Err(ChaseError::Theory(TheoryError::EgdInMapping(
                egd.id().to_owned(),
            )));
        }
        self.merged().validate().map_err(ChaseError::Theory)
    }
}

fn join(theories: &[&Theory], keep: impl Fn(&Dependency) -> bool) -> Theory {
    Theory {
        dependencies: theories
            .iter()
            .flat_map(|theory| &theory.dependencies)
            .filter(|dependency| keep(dependency))
            .cloned()
            .collect(),
    }
}

/// Schema compatibility: an error when no tuple of a source relation can be
/// exchanged at all, and a warning, with a witness source instance, when the
/// source and mapping permit data that breaks a target constraint.
#[must_use]
pub fn check_schema_compatibility(exchange: &Exchange<'_>, budget: ChaseBudget) -> Vec<Diagnostic> {
    if let Err(error) = exchange.validate() {
        return vec![refusal(exchange.mapping, &error)];
    }
    let merged = exchange.merged();
    let mut findings = Vec::new();
    for (relation, columns) in exchange.source_schema {
        let frozen = Fact {
            relation: relation.clone(),
            values: (0..columns.len()).map(Value::Null).collect(),
        };
        match chase(&merged, &[frozen], budget) {
            Ok(ChaseOutcome::Model(_)) => {}
            Ok(ChaseOutcome::Conflict(conflict)) => findings.push(finding(
                codes::INCOMPATIBLE,
                DiagnosticSeverity::Error,
                format!(
                    "no {relation} tuple can be mapped: {}",
                    conflict_message(&conflict)
                ),
                origin_of(&merged, &conflict.egd),
            )),
            Err(error) => {
                findings.push(refusal(&merged, &error));
                return findings;
            }
        }
    }
    let source_side = join(&[exchange.source, exchange.mapping], |_| true);
    // Validated above, so the source signature is available.
    let source_signature = exchange.source.signature().unwrap_or_default();
    let source: BTreeSet<&str> = exchange
        .source_schema
        .keys()
        .chain(source_signature.keys())
        .map(String::as_str)
        .collect();
    for constraint in &exchange.target.dependencies {
        match unpreserved(
            constraint,
            exchange.mapping,
            exchange.target,
            &source_side,
            &source,
            budget,
        ) {
            Ok(None) => {}
            Ok(Some(witness)) => findings.push(finding(
                codes::UNPRESERVED,
                DiagnosticSeverity::Warning,
                format!(
                    "the source admits data that breaks {}: {}",
                    constraint.id(),
                    render(&witness)
                ),
                Some(constraint.origin()),
            )),
            Err(error) => {
                findings.push(refusal(&merged, &error));
                return findings;
            }
        }
    }
    findings
}

/// A source instance, chased with the source constraints and the mapping,
/// whose image violates `constraint` closed-world. Candidates come from
/// unfolding the constraint's body through the mapping heads. Each one is
/// verified as concrete data (its nulls grounded to distinct values) the way
/// [`check_instance`] checks it, so a reported witness is genuine, while an
/// unfolding the procedure does not try can be missed.
fn unpreserved(
    constraint: &Dependency,
    mapping: &Theory,
    target: &Theory,
    source_side: &Theory,
    source: &BTreeSet<&str>,
    budget: ChaseBudget,
) -> Result<Option<Vec<Fact>>, ChaseError> {
    let body = match constraint {
        Dependency::Tgd(tgd) => &tgd.body,
        Dependency::Egd(egd) => &egd.body,
    };
    // The target side as `check_instance` sees it: the mapping plus the
    // target's equations, which may still resolve a mapping's unknown
    // (existential) values.
    let mut equations = mapping.clone();
    equations.dependencies.extend(
        target
            .dependencies
            .iter()
            .filter(|d| matches!(d, Dependency::Egd(_)))
            .cloned(),
    );
    for candidate in unfoldings(body, mapping, budget)? {
        let ChaseOutcome::Model(model) = chase(source_side, &candidate, budget)? else {
            continue;
        };
        // Report the verified source instance, not the unfolding seed.
        let witness: Vec<Fact> = model
            .into_iter()
            .filter(|fact| source.contains(fact.relation.as_str()))
            .collect();
        // Source data is concrete: each of the witness's nulls is its own
        // value, distinct from every other.
        let grounded: Vec<Fact> = witness
            .iter()
            .map(|fact| Fact {
                relation: fact.relation.clone(),
                values: fact
                    .values
                    .iter()
                    .map(|value| match value {
                        Value::Null(id) => Value::Const(format!("\u{0}witness:{id}")),
                        Value::Const(_) => value.clone(),
                    })
                    .collect(),
            })
            .collect();
        let violated = match chase(&equations, &grounded, budget)? {
            ChaseOutcome::Conflict(conflict) => conflict.egd == constraint.id(),
            ChaseOutcome::Model(mapped) => {
                matches!(constraint, Dependency::Tgd(_))
                    && first_violation(constraint, &mapped, budget)?.is_some()
            }
        };
        if violated {
            return Ok(Some(witness));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Body(String),
    Copy(usize, String),
    Const(String),
}

fn key(term: &Term, copy: Option<usize>) -> Key {
    match (term, copy) {
        (Term::Const(text), _) => Key::Const(text.clone()),
        (Term::Var(name), None) => Key::Body(name.clone()),
        (Term::Var(name), Some(copy)) => Key::Copy(copy, name.clone()),
    }
}

fn find(parent: &BTreeMap<Key, Key>, mut node: Key) -> Key {
    while let Some(next) = parent.get(&node) {
        node = next.clone();
    }
    node
}

/// Source instances whose mapping produces every atom of `body`: one mapping
/// head per atom (renamed apart), unified, with each class frozen to its
/// constant or a fresh null.
fn unfoldings(
    body: &[Atom],
    mapping: &Theory,
    budget: ChaseBudget,
) -> Result<Vec<Vec<Fact>>, ChaseError> {
    let options: Vec<Vec<(&[Atom], &Atom)>> = body
        .iter()
        .map(|atom| {
            mapping
                .dependencies
                .iter()
                .filter_map(|dependency| match dependency {
                    Dependency::Tgd(tgd) => Some(tgd),
                    Dependency::Egd(_) => None,
                })
                .flat_map(|tgd| tgd.head.iter().map(move |head| (tgd.body.as_slice(), head)))
                .filter(|(_, head)| {
                    head.relation == atom.relation && head.terms.len() == atom.terms.len()
                })
                .collect()
        })
        .collect();
    let mut combinations: Vec<Vec<(&[Atom], &Atom)>> = vec![Vec::new()];
    for choices in &options {
        let mut next = Vec::new();
        for prefix in &combinations {
            for &choice in choices {
                if next.len() >= budget.max_facts {
                    return Err(ChaseError::BudgetExceeded {
                        resource: "facts",
                        limit: budget.max_facts,
                    });
                }
                let mut extended = prefix.clone();
                extended.push(choice);
                next.push(extended);
            }
        }
        combinations = next;
    }
    let mut candidates = Vec::new();
    for combination in combinations {
        let mut parent: BTreeMap<Key, Key> = BTreeMap::new();
        for (copy, (atom, (_, head))) in body.iter().zip(&combination).enumerate() {
            for (left, right) in atom.terms.iter().zip(&head.terms) {
                let (a, b) = (
                    find(&parent, key(left, None)),
                    find(&parent, key(right, Some(copy))),
                );
                // Constants stay roots, so a class holds at most one constant;
                // two different constants are left apart and caught below.
                let (a_const, b_const) = (matches!(a, Key::Const(_)), matches!(b, Key::Const(_)));
                if a == b || (a_const && b_const) {
                    continue;
                }
                if a_const {
                    parent.insert(b, a);
                } else {
                    parent.insert(a, b);
                }
            }
        }
        let inconsistent =
            body.iter()
                .zip(&combination)
                .enumerate()
                .any(|(copy, (atom, (_, head)))| {
                    atom.terms.iter().zip(&head.terms).any(|(left, right)| {
                        find(&parent, key(left, None)) != find(&parent, key(right, Some(copy)))
                    })
                });
        if inconsistent {
            continue;
        }
        let mut nulls: BTreeMap<Key, usize> = BTreeMap::new();
        let mut facts = BTreeSet::new();
        for (copy, (source_body, _)) in combination.iter().enumerate() {
            for atom in *source_body {
                let values = atom
                    .terms
                    .iter()
                    .map(|term| match find(&parent, key(term, Some(copy))) {
                        Key::Const(text) => Value::Const(text),
                        class @ (Key::Body(_) | Key::Copy(..)) => {
                            let next = nulls.len();
                            Value::Null(*nulls.entry(class).or_insert(next))
                        }
                    })
                    .collect();
                facts.insert(Fact {
                    relation: atom.relation.clone(),
                    values,
                });
            }
        }
        candidates.push(facts.into_iter().collect());
    }
    Ok(candidates)
}

/// Instance validation: `facts` (sample or real source data) checked
/// closed-world against the source constraints, then mapped and checked
/// against the target constraints. Every finding is an error with the rows
/// that break the rule.
#[must_use]
pub fn check_instance(
    exchange: &Exchange<'_>,
    facts: &[Fact],
    budget: ChaseBudget,
) -> Vec<Diagnostic> {
    if let Err(error) = exchange.validate() {
        return vec![refusal(exchange.mapping, &error)];
    }
    let empty = Theory::default();
    let (mut findings, normalized) = closed_world(exchange.source, &empty, facts, budget);
    // The mapping reads the data as the source's own equations resolve it.
    let mapped = normalized.as_deref().unwrap_or(facts);
    findings.extend(closed_world(exchange.target, exchange.mapping, mapped, budget).0);
    findings
}

/// Chase `facts` with `derivations` and the EGDs of `constraints` (only nulls
/// may be equated), then require every TGD of `constraints` to hold as is.
fn closed_world(
    constraints: &Theory,
    derivations: &Theory,
    facts: &[Fact],
    budget: ChaseBudget,
) -> (Vec<Diagnostic>, Option<Vec<Fact>>) {
    let mut equations = derivations.clone();
    equations.dependencies.extend(
        constraints
            .dependencies
            .iter()
            .filter(|d| matches!(d, Dependency::Egd(_)))
            .cloned(),
    );
    let model = match chase(&equations, facts, budget) {
        Ok(ChaseOutcome::Model(model)) => model,
        Ok(ChaseOutcome::Conflict(conflict)) => {
            let violation = finding(
                codes::INSTANCE_VIOLATION,
                DiagnosticSeverity::Error,
                format!(
                    "the data breaks {}: {}",
                    conflict.egd,
                    conflict_message(&conflict)
                ),
                origin_of(&equations, &conflict.egd),
            );
            return (vec![violation], None);
        }
        Err(error) => return (vec![refusal(&equations, &error)], None),
    };
    let mut findings = Vec::new();
    for constraint in constraints
        .dependencies
        .iter()
        .filter(|d| matches!(d, Dependency::Tgd(_)))
    {
        match first_violation(constraint, &model, budget) {
            Ok(None) => {}
            Ok(Some(matched)) => findings.push(finding(
                codes::INSTANCE_VIOLATION,
                DiagnosticSeverity::Error,
                format!(
                    "the data breaks {}: {} has no required match",
                    constraint.id(),
                    render(&matched)
                ),
                Some(constraint.origin()),
            )),
            Err(error) => {
                findings.push(refusal(constraints, &error));
                return (findings, None);
            }
        }
    }
    (findings, Some(model))
}

/// Mapping totality: a warning for every source relation no mapping reads
/// and every source column whose value never reaches the target, unless it
/// is a declared gap (`Relation` or `Relation.column`). A constant, or a
/// variable that occurs twice in the core of a mapping body (a join or
/// filter, not a redundant copy), counts as a use of its column. A mapping
/// containing an EGD is refused.
#[must_use]
pub fn check_mapping_totality(exchange: &Exchange<'_>, gaps: &BTreeSet<String>) -> Vec<Diagnostic> {
    if let Err(error) = exchange.validate() {
        return vec![refusal(exchange.mapping, &error)];
    }
    let mut findings = Vec::new();
    let tgds: Vec<_> = exchange
        .mapping
        .dependencies
        .iter()
        .filter_map(|dependency| match dependency {
            Dependency::Tgd(tgd) => Some(tgd),
            Dependency::Egd(_) => None,
        })
        .collect();
    let mut cores = Vec::new();
    for tgd in tgds {
        match core_body(tgd) {
            Ok(core) => cores.push((tgd, core)),
            Err(error) => return vec![refusal(exchange.mapping, &error)],
        }
    }
    for (relation, columns) in exchange.source_schema {
        if gaps.contains(relation) {
            continue;
        }
        let reads: Vec<(&Atom, BTreeSet<&str>)> = cores
            .iter()
            .flat_map(|(tgd, core)| {
                // A variable repeated in the body's core (a join or filter)
                // selects what reaches the target, like a constant does.
                let mut used = super::theory::variables(&tgd.head);
                let mut seen = BTreeSet::new();
                for term in core.iter().flat_map(|atom| &atom.terms) {
                    if let Term::Var(name) = term
                        && !seen.insert(name.as_str())
                    {
                        used.insert(name.as_str());
                    }
                }
                core.iter().map(move |atom| (*atom, used.clone()))
            })
            .filter(|(atom, _)| &atom.relation == relation)
            .collect();
        if reads.is_empty() {
            findings.push(non_total(format!(
                "source relation {relation} is not mapped"
            )));
            continue;
        }
        for (position, column) in columns.iter().enumerate() {
            let used = reads
                .iter()
                .any(|(atom, head)| match atom.terms.get(position) {
                    Some(Term::Const(_)) => true,
                    Some(Term::Var(name)) => head.contains(name.as_str()),
                    None => false,
                });
            if !used && !gaps.contains(&format!("{relation}.{column}")) {
                findings.push(non_total(format!(
                    "{relation}.{column} never reaches the target"
                )));
            }
        }
    }
    findings
}

/// The atoms of `tgd`'s body that no homomorphism fixing the head's
/// variables makes redundant (its core), so a variable repeated only across
/// a redundant copy, as in `S(id, n), S(other, n) -> T(id)`, is not taken for
/// a join.
fn core_body(tgd: &Tgd) -> Result<Vec<&Atom>, ChaseError> {
    let head = super::theory::variables(&tgd.head);
    let fixed: Vec<&str> = super::theory::variables(&tgd.body)
        .into_iter()
        .filter(|name| head.contains(name))
        .collect();
    let mut core: Vec<&Atom> = tgd.body.iter().collect();
    let mut index = 0;
    while index < core.len() {
        let rest: Vec<&Atom> = core
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, atom)| *atom)
            .collect();
        if !rest.is_empty() && maps_into(&core, &rest, &fixed)? {
            core.remove(index);
        } else {
            index = index.saturating_add(1);
        }
    }
    Ok(core)
}

/// Whether `atoms` map into the frozen `target` with every `fixed` variable
/// sent to itself, checked by chasing a probe rule over the frozen atoms.
fn maps_into(atoms: &[&Atom], target: &[&Atom], fixed: &[&str]) -> Result<bool, ChaseError> {
    const PROBE: &str = "\u{0}probe";
    let freeze = |term: &Term| match term {
        Term::Const(text) => format!("c:{text}"),
        Term::Var(name) => format!("v:{name}"),
    };
    let frozen = |atom: &Atom| Fact {
        relation: atom.relation.clone(),
        values: atom.terms.iter().map(|t| Value::Const(freeze(t))).collect(),
    };
    let probe = Theory {
        dependencies: vec![Dependency::Tgd(Tgd {
            id: PROBE.to_owned(),
            origin: RuleOrigin::default(),
            body: atoms
                .iter()
                .map(|atom| Atom {
                    relation: atom.relation.clone(),
                    terms: atom
                        .terms
                        .iter()
                        .map(|term| match term {
                            Term::Const(_) => Term::Const(freeze(term)),
                            Term::Var(_) => term.clone(),
                        })
                        .collect(),
                })
                .collect(),
            head: vec![Atom {
                relation: PROBE.to_owned(),
                terms: fixed
                    .iter()
                    .map(|name| Term::Var((*name).to_owned()))
                    .collect(),
            }],
        })],
    };
    let facts: Vec<Fact> = target.iter().copied().map(frozen).collect();
    let identity = Fact {
        relation: PROBE.to_owned(),
        values: fixed
            .iter()
            .map(|name| Value::Const(format!("v:{name}")))
            .collect(),
    };
    Ok(match chase(&probe, &facts, ChaseBudget::default())? {
        ChaseOutcome::Model(model) => model.contains(&identity),
        ChaseOutcome::Conflict(_) => false,
    })
}

fn non_total(message: String) -> Diagnostic {
    finding(codes::NON_TOTAL, DiagnosticSeverity::Warning, message, None)
}

fn render(facts: &[Fact]) -> String {
    facts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
