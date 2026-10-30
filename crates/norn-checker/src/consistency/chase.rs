//! A bounded, deterministic restricted chase with conflict explanation.
//!
//! The chase repairs an instance until every dependency holds: a TGD whose
//! head has no match adds head facts (fresh labelled nulls for existential
//! variables); an EGD equates two values, replacing a null. An EGD that
//! equates two distinct constants fails, and that failure is the
//! contradiction. Termination is guaranteed for weakly acyclic theories
//! (Fagin, Kolaitis, Miller and Popa 2005), which [`chase`] requires; every
//! run is also metered by a [`ChaseBudget`], and exhaustion is a refusal,
//! never a pass.
//!
//! Evidence level: exhaustive over the supplied theory and facts, within the
//! budget. It says nothing about constraints that were not lowered.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use norn_graph::{Budget, Direction, Graph, reachability};

use super::theory::{Atom, Dependency, Term, Tgd, Theory, TheoryError, record_arity, variables};

/// A value in an instance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    /// A known constant.
    Const(String),
    /// A labelled null: some value, not yet known.
    Null(usize),
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Const(text) => write!(formatter, "{text:?}"),
            Self::Null(id) => write!(formatter, "_:n{id}"),
        }
    }
}

/// One tuple of a relation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fact {
    /// The relation.
    pub relation: String,
    /// Positional values.
    pub values: Vec<Value>,
}

impl fmt::Display for Fact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}(", self.relation)?;
        for (position, value) in self.values.iter().enumerate() {
            if position > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{value}")?;
        }
        formatter.write_str(")")
    }
}

/// Resource ceilings for one chase, including conflict minimisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChaseBudget {
    /// Facts held at once.
    pub max_facts: usize,
    /// Labelled nulls created.
    pub max_nulls: usize,
    /// Metered match attempts per run.
    pub max_steps: usize,
}

impl Default for ChaseBudget {
    fn default() -> Self {
        Self {
            max_facts: 4096,
            max_nulls: 4096,
            max_steps: 1_000_000,
        }
    }
}

/// Why a chase was refused. A refusal never means "consistent".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChaseError {
    /// The theory or the facts are malformed.
    Theory(TheoryError),
    /// These TGDs create existential values on a cycle, so the chase may not
    /// terminate.
    NotWeaklyAcyclic {
        /// Offending TGD ids, sorted.
        rules: Vec<String>,
    },
    /// A ceiling of the [`ChaseBudget`] was reached.
    BudgetExceeded {
        /// `facts`, `nulls` or `steps`.
        resource: &'static str,
        /// The ceiling.
        limit: usize,
    },
    /// A graph-library invariant failed while checking weak acyclicity.
    Internal(String),
}

/// An EGD equated two distinct constants: the dependencies and facts have no
/// common model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The failing EGD.
    pub egd: String,
    /// The constants it equated.
    pub left: Value,
    /// The constants it equated.
    pub right: Value,
    /// A subset-minimal set of rule ids that still fails on [`Self::witness`]:
    /// dropping any one of them (within budget) removes every conflict.
    pub rules: Vec<String>,
    /// A subset-minimal set of the supplied facts that still fails.
    pub witness: Vec<Fact>,
    /// The facts the failing EGD matched, after chasing the witness.
    pub matched: Vec<Fact>,
}

/// The result of a chase that ran to completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChaseOutcome {
    /// Every dependency holds: a finite model (a universal one) that extends
    /// the facts.
    Model(Vec<Fact>),
    /// No model extends the facts.
    Conflict(Conflict),
}

/// Chase `facts` with `theory`.
///
/// # Errors
/// A malformed theory, a theory that is not weakly acyclic, or an exhausted
/// budget.
pub fn chase(
    theory: &Theory,
    facts: &[Fact],
    budget: ChaseBudget,
) -> Result<ChaseOutcome, ChaseError> {
    let mut signature = theory.signature().map_err(ChaseError::Theory)?;
    for fact in facts {
        record_arity(&mut signature, &fact.relation, fact.values.len())
            .map_err(ChaseError::Theory)?;
    }
    weakly_acyclic(theory, &signature, budget)?;
    let all_rules: Vec<usize> = (0..theory.dependencies.len()).collect();
    let all_facts: Vec<usize> = (0..facts.len()).collect();
    let failure = match run(&theory.dependencies, &all_rules, facts, &all_facts, budget) {
        Ok(model) => return Ok(ChaseOutcome::Model(model)),
        Err(Halt::Refused(error)) => return Err(error),
        Err(Halt::Conflict(failure)) => failure,
    };
    // Provenance gives a sufficient support; deletion then makes it minimal.
    let mut rules: Vec<usize> = failure.rules.iter().copied().collect();
    let mut inputs: Vec<usize> = failure.inputs.iter().copied().collect();
    if !fails(theory, &rules, facts, &inputs, budget) {
        rules.clone_from(&all_rules);
        inputs.clone_from(&all_facts);
    }
    shrink(&mut rules, |kept| {
        fails(theory, kept, facts, &inputs, budget)
    });
    shrink(&mut inputs, |kept| {
        fails(theory, &rules, facts, kept, budget)
    });
    let failure = match run(&theory.dependencies, &rules, facts, &inputs, budget) {
        Err(Halt::Conflict(minimal)) => minimal,
        Ok(_) | Err(Halt::Refused(_)) => failure,
    };
    let rule_id = |index: &usize| theory.dependencies.get(*index).map(|d| d.id().to_owned());
    Ok(ChaseOutcome::Conflict(Conflict {
        egd: rule_id(&failure.egd).unwrap_or_default(),
        left: failure.left,
        right: failure.right,
        rules: rules.iter().filter_map(rule_id).collect(),
        witness: inputs
            .iter()
            .filter_map(|i| facts.get(*i).cloned())
            .collect(),
        matched: failure.matched,
    }))
}

fn fails(
    theory: &Theory,
    rules: &[usize],
    facts: &[Fact],
    inputs: &[usize],
    budget: ChaseBudget,
) -> bool {
    matches!(
        run(&theory.dependencies, rules, facts, inputs, budget),
        Err(Halt::Conflict(_))
    )
}

/// Drop each element whose removal keeps `still_fails` true.
fn shrink(items: &mut Vec<usize>, mut still_fails: impl FnMut(&[usize]) -> bool) {
    let mut position = 0;
    while position < items.len() {
        let mut candidate = items.clone();
        candidate.remove(position);
        if still_fails(&candidate) {
            *items = candidate;
        } else {
            position = position.saturating_add(1);
        }
    }
}

/// Positions are `(relation, index)`. A frontier variable at body position
/// `p` gives an ordinary edge to each of its head positions and a special
/// edge to each existential head position; the theory is weakly acyclic when
/// no cycle passes through a special edge.
fn weakly_acyclic(
    theory: &Theory,
    signature: &BTreeMap<String, usize>,
    budget: ChaseBudget,
) -> Result<(), ChaseError> {
    let nodes: Vec<(String, usize)> = signature
        .iter()
        .flat_map(|(relation, arity)| (0..*arity).map(|index| (relation.clone(), index)))
        .collect();
    let mut edges = BTreeSet::new();
    let mut special = BTreeSet::new();
    for dependency in &theory.dependencies {
        let Dependency::Tgd(tgd) = dependency else {
            continue;
        };
        let body_vars = variables(&tgd.body);
        for (variable, from) in positions(&tgd.body) {
            for (head_var, to) in positions(&tgd.head) {
                if head_var == variable {
                    edges.insert((from.clone(), to));
                } else if !body_vars.contains(head_var) && occurs(&tgd.head, variable) {
                    edges.insert((from.clone(), to.clone()));
                    special.insert((from.clone(), to, tgd.id.as_str()));
                }
            }
        }
    }
    let graph = Graph {
        nodes,
        edges: edges.into_iter().collect(),
    };
    let limits = Budget {
        max_nodes: graph.nodes.len(),
        max_edges: graph.edges.len(),
        max_steps: budget.max_steps,
    };
    let mut cyclic = BTreeSet::new();
    for (from, to, rule) in special {
        let reached = reachability(&graph, &[to], Direction::Downstream, limits).map_err(
            |error| match error {
                norn_graph::Error::BudgetExceeded(exceeded) => ChaseError::BudgetExceeded {
                    resource: "steps",
                    limit: exceeded.limit,
                },
                other => ChaseError::Internal(other.to_string()),
            },
        )?;
        if reached.contains(&from) {
            cyclic.insert(rule.to_owned());
        }
    }
    if cyclic.is_empty() {
        Ok(())
    } else {
        Err(ChaseError::NotWeaklyAcyclic {
            rules: cyclic.into_iter().collect(),
        })
    }
}

fn positions(atoms: &[Atom]) -> Vec<(&str, (String, usize))> {
    let mut found = Vec::new();
    for atom in atoms {
        for (index, term) in atom.terms.iter().enumerate() {
            if let Term::Var(name) = term {
                found.push((name.as_str(), (atom.relation.clone(), index)));
            }
        }
    }
    found
}

fn occurs(atoms: &[Atom], variable: &str) -> bool {
    variables(atoms).contains(variable)
}

/// The first match of `dependency` that `facts` leave unsatisfied, checked
/// closed-world (nothing is repaired): a TGD body match with no head match,
/// or an EGD body match whose sides differ. Returns the matched facts.
///
/// # Errors
/// An exhausted step budget.
pub fn first_violation(
    dependency: &Dependency,
    facts: &[Fact],
    budget: ChaseBudget,
) -> Result<Option<Vec<Fact>>, ChaseError> {
    let records: Vec<Record> = facts
        .iter()
        .map(|fact| Record {
            fact: fact.clone(),
            rules: BTreeSet::new(),
            inputs: BTreeSet::new(),
        })
        .collect();
    let mut meter = Meter {
        steps: 0,
        limit: budget.max_steps,
    };
    let refused = |halt| match halt {
        Halt::Refused(error) => error,
        Halt::Conflict(_) => ChaseError::Internal("closed-world check cannot conflict".to_owned()),
    };
    let body = match dependency {
        Dependency::Tgd(tgd) => &tgd.body,
        Dependency::Egd(egd) => &egd.body,
    };
    let mut found = Vec::new();
    extend(
        &records,
        &mut meter,
        body,
        &mut Binding::new(),
        &mut Vec::new(),
        &mut found,
        false,
    )
    .map_err(refused)?;
    for (mut binding, used) in found {
        let violated = match dependency {
            Dependency::Tgd(tgd) => {
                let mut heads = Vec::new();
                extend(
                    &records,
                    &mut meter,
                    &tgd.head,
                    &mut binding,
                    &mut Vec::new(),
                    &mut heads,
                    true,
                )
                .map_err(refused)?;
                heads.is_empty()
            }
            Dependency::Egd(egd) => resolve(&egd.left, &binding) != resolve(&egd.right, &binding),
        };
        if violated {
            return Ok(Some(
                used.iter()
                    .filter_map(|&i| records.get(i).map(|r| r.fact.clone()))
                    .collect(),
            ));
        }
    }
    Ok(None)
}

type Binding = BTreeMap<String, Value>;

struct Record {
    fact: Fact,
    rules: BTreeSet<usize>,
    inputs: BTreeSet<usize>,
}

struct Failure {
    egd: usize,
    left: Value,
    right: Value,
    matched: Vec<Fact>,
    rules: BTreeSet<usize>,
    inputs: BTreeSet<usize>,
}

enum Halt {
    Conflict(Failure),
    Refused(ChaseError),
}

struct Meter {
    steps: usize,
    limit: usize,
}

impl Meter {
    fn step(&mut self) -> Result<(), Halt> {
        if self.steps >= self.limit {
            return Err(Halt::Refused(ChaseError::BudgetExceeded {
                resource: "steps",
                limit: self.limit,
            }));
        }
        self.steps = self.steps.saturating_add(1);
        Ok(())
    }
}

struct State {
    records: Vec<Record>,
    seen: BTreeSet<Fact>,
    next_null: usize,
    meter: Meter,
    budget: ChaseBudget,
}

/// Chase the facts at `inputs` with the dependencies at `rules`.
fn run(
    dependencies: &[Dependency],
    rules: &[usize],
    facts: &[Fact],
    inputs: &[usize],
    budget: ChaseBudget,
) -> Result<Vec<Fact>, Halt> {
    let mut state = State {
        records: Vec::new(),
        seen: BTreeSet::new(),
        next_null: 0,
        meter: Meter {
            steps: 0,
            limit: budget.max_steps,
        },
        budget,
    };
    for &index in inputs {
        let Some(fact) = facts.get(index) else {
            continue;
        };
        for value in &fact.values {
            if let Value::Null(id) = value {
                state.next_null = state.next_null.max(id.saturating_add(1));
            }
        }
        state.insert(fact.clone(), BTreeSet::new(), BTreeSet::from([index]))?;
    }
    let active: Vec<(usize, &Dependency)> = rules
        .iter()
        .filter_map(|&index| dependencies.get(index).map(|d| (index, d)))
        .collect();
    loop {
        state.equate(&active)?;
        let mut changed = false;
        for &(index, dependency) in &active {
            if let Dependency::Tgd(tgd) = dependency {
                changed |= state.generate(index, tgd)?;
            }
        }
        if !changed {
            return Ok(state
                .records
                .into_iter()
                .map(|record| record.fact)
                .collect());
        }
    }
}

impl State {
    fn insert(
        &mut self,
        fact: Fact,
        rules: BTreeSet<usize>,
        inputs: BTreeSet<usize>,
    ) -> Result<bool, Halt> {
        if self.seen.contains(&fact) {
            return Ok(false);
        }
        if self.records.len() >= self.budget.max_facts {
            return Err(Halt::Refused(ChaseError::BudgetExceeded {
                resource: "facts",
                limit: self.budget.max_facts,
            }));
        }
        self.seen.insert(fact.clone());
        self.records.push(Record {
            fact,
            rules,
            inputs,
        });
        Ok(true)
    }

    fn fresh_null(&mut self) -> Result<Value, Halt> {
        if self.next_null >= self.budget.max_nulls {
            return Err(Halt::Refused(ChaseError::BudgetExceeded {
                resource: "nulls",
                limit: self.budget.max_nulls,
            }));
        }
        let id = self.next_null;
        self.next_null = id.saturating_add(1);
        Ok(Value::Null(id))
    }

    /// Every match of `atoms` extending `binding`, with the records used.
    fn matches(
        &mut self,
        atoms: &[Atom],
        binding: &mut Binding,
        first_only: bool,
    ) -> Result<Vec<(Binding, Vec<usize>)>, Halt> {
        let mut found = Vec::new();
        let mut used = Vec::new();
        extend(
            &self.records,
            &mut self.meter,
            atoms,
            binding,
            &mut used,
            &mut found,
            first_only,
        )?;
        Ok(found)
    }

    /// Apply EGDs until none is violated; fail on two distinct constants.
    fn equate(&mut self, active: &[(usize, &Dependency)]) -> Result<(), Halt> {
        'restart: loop {
            for &(index, dependency) in active {
                let Dependency::Egd(egd) = dependency else {
                    continue;
                };
                for (binding, used) in self.matches(&egd.body, &mut Binding::new(), false)? {
                    let (Some(left), Some(right)) =
                        (resolve(&egd.left, &binding), resolve(&egd.right, &binding))
                    else {
                        continue;
                    };
                    if left == right {
                        continue;
                    }
                    let (mut rules, inputs) = self.support(&used);
                    rules.insert(index);
                    let (from, to) = match (&left, &right) {
                        (Value::Const(_), Value::Const(_)) => {
                            let matched = used
                                .iter()
                                .filter_map(|&i| self.records.get(i).map(|r| r.fact.clone()))
                                .collect();
                            return Err(Halt::Conflict(Failure {
                                egd: index,
                                left,
                                right,
                                matched,
                                rules,
                                inputs,
                            }));
                        }
                        (Value::Null(_), Value::Const(_)) => (left, right),
                        (Value::Const(_), Value::Null(_)) => (right, left),
                        (Value::Null(a), Value::Null(b)) if a > b => (left, right),
                        (Value::Null(_), Value::Null(_)) => (right, left),
                    };
                    self.substitute(&from, &to, &rules, &inputs)?;
                    continue 'restart;
                }
            }
            return Ok(());
        }
    }

    fn substitute(
        &mut self,
        from: &Value,
        to: &Value,
        rules: &BTreeSet<usize>,
        inputs: &BTreeSet<usize>,
    ) -> Result<(), Halt> {
        let records = std::mem::take(&mut self.records);
        self.seen.clear();
        for mut record in records {
            self.meter.step()?;
            if record.fact.values.contains(from) {
                for value in &mut record.fact.values {
                    if value == from {
                        value.clone_from(to);
                    }
                }
                record.rules.extend(rules);
                record.inputs.extend(inputs);
            }
            self.insert(record.fact, record.rules, record.inputs)?;
        }
        Ok(())
    }

    /// Fire every active trigger of `tgd` once; true when facts were added.
    fn generate(&mut self, index: usize, tgd: &Tgd) -> Result<bool, Halt> {
        let mut changed = false;
        for (mut binding, used) in self.matches(&tgd.body, &mut Binding::new(), false)? {
            if !self
                .matches(&tgd.head, &mut binding.clone(), true)?
                .is_empty()
            {
                continue;
            }
            let (mut rules, inputs) = self.support(&used);
            rules.insert(index);
            for atom in &tgd.head {
                let mut values = Vec::with_capacity(atom.terms.len());
                for term in &atom.terms {
                    let value = match term {
                        Term::Const(text) => Value::Const(text.clone()),
                        Term::Var(name) => {
                            if let Some(value) = binding.get(name) {
                                value.clone()
                            } else {
                                let fresh = self.fresh_null()?;
                                binding.insert(name.clone(), fresh.clone());
                                fresh
                            }
                        }
                    };
                    values.push(value);
                }
                let fact = Fact {
                    relation: atom.relation.clone(),
                    values,
                };
                changed |= self.insert(fact, rules.clone(), inputs.clone())?;
            }
        }
        Ok(changed)
    }

    fn support(&self, used: &[usize]) -> (BTreeSet<usize>, BTreeSet<usize>) {
        let mut rules = BTreeSet::new();
        let mut inputs = BTreeSet::new();
        for record in used.iter().filter_map(|&i| self.records.get(i)) {
            rules.extend(&record.rules);
            inputs.extend(&record.inputs);
        }
        (rules, inputs)
    }
}

fn resolve(term: &Term, binding: &Binding) -> Option<Value> {
    match term {
        Term::Const(text) => Some(Value::Const(text.clone())),
        Term::Var(name) => binding.get(name).cloned(),
    }
}

/// Backtracking join of `atoms` against `records`, in record order.
fn extend(
    records: &[Record],
    meter: &mut Meter,
    atoms: &[Atom],
    binding: &mut Binding,
    used: &mut Vec<usize>,
    found: &mut Vec<(Binding, Vec<usize>)>,
    first_only: bool,
) -> Result<(), Halt> {
    let Some((atom, rest)) = atoms.split_first() else {
        found.push((binding.clone(), used.clone()));
        return Ok(());
    };
    for (index, record) in records.iter().enumerate() {
        if first_only && !found.is_empty() {
            return Ok(());
        }
        meter.step()?;
        if record.fact.relation != atom.relation || record.fact.values.len() != atom.terms.len() {
            continue;
        }
        let mut bound = Vec::new();
        let unified = atom
            .terms
            .iter()
            .zip(&record.fact.values)
            .all(|(term, value)| match term {
                Term::Const(text) => matches!(value, Value::Const(c) if c == text),
                Term::Var(name) => match binding.get(name) {
                    Some(existing) => existing == value,
                    None => {
                        binding.insert(name.clone(), value.clone());
                        bound.push(name.clone());
                        true
                    }
                },
            });
        if unified {
            used.push(index);
            extend(records, meter, rest, binding, used, found, first_only)?;
            used.pop();
        }
        for name in bound {
            binding.remove(&name);
        }
    }
    Ok(())
}
