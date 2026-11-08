use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use norn_graph::{Direction, Graph};
use serde::{Deserialize, Serialize};

use crate::{Atom, Literal, Program, RelationKind, Rule, Term, Value};

/// Stable shared refusal codes; callers may render their own diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Code {
    /// A structural, text, or metered-work ceiling was exceeded.
    #[serde(rename = "datalog.budget_exhausted")]
    BudgetExhausted,
    /// Symbols must be nonempty ASCII identifiers.
    #[serde(rename = "datalog.invalid_symbol")]
    InvalidSymbol,
    /// Canonical encoding must not collapse distinct constant spellings.
    #[serde(rename = "datalog.noncanonical_constant")]
    NoncanonicalConstant,
    /// More than one rule has this identity.
    #[serde(rename = "datalog.duplicate_rule_id")]
    DuplicateRuleId,
    /// A relation signature was declared more than once.
    #[serde(rename = "datalog.duplicate_predicate")]
    DuplicatePredicate,
    /// An atom references an undeclared relation, or a derived relation has no rules.
    #[serde(rename = "datalog.undefined_predicate")]
    UndefinedPredicate,
    /// Atom width disagrees with the declared signature.
    #[serde(rename = "datalog.arity_mismatch")]
    ArityMismatch,
    /// Rules cannot write supplied input relations.
    #[serde(rename = "datalog.input_in_head")]
    InputInHead,
    /// A head variable is not bound by a positive body premise.
    #[serde(rename = "datalog.unsafe_head_variable")]
    UnsafeHeadVariable,
    /// A negative variable is not bound by a positive body premise.
    #[serde(rename = "datalog.unsafe_negative_variable")]
    UnsafeNegativeVariable,
    /// No aggregate has an admitted finite-domain implementation in v1.
    #[serde(rename = "datalog.aggregate_not_admitted")]
    AggregateNotAdmitted,
    /// A negative dependency occurs within a recursive component.
    #[serde(rename = "datalog.recursion_through_negation")]
    RecursionThroughNegation,
    /// An internal graph invariant failed; never an admission result.
    #[serde(rename = "datalog.graph_refused")]
    GraphRefused,
}

/// One deterministic refusal. No partial admission plan is returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Diagnostic {
    /// Stable machine-readable reason.
    pub code: Code,
    /// Responsible rule, when the refusal concerns one rule.
    pub rule_id: Option<String>,
    /// Responsible predicate or variable, when available.
    pub symbol: Option<String>,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Datalog refused: {:?}", self.code)
    }
}

impl std::error::Error for Diagnostic {}

fn refusal(code: Code, rule: Option<&str>, symbol: Option<&str>) -> Diagnostic {
    Diagnostic {
        code,
        rule_id: rule.map(str::to_owned),
        symbol: symbol.map(str::to_owned),
    }
}

/// Explicit ceilings checked before building graph storage. Work units include
/// every scan and conservative full-call charges for the existing graph library.
/// These are validator limits, not bounds on future executor fact counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Maximum relation signatures.
    pub max_predicates: usize,
    /// Maximum clauses.
    pub max_rules: usize,
    /// Maximum atoms across heads and bodies.
    pub max_atoms: usize,
    /// Maximum terms across all atoms, and maximum signature arity.
    pub max_terms: usize,
    /// Maximum total UTF-8 bytes of identifiers and constants.
    pub max_text_bytes: usize,
    /// Shared metered work ceiling, including graph calls.
    pub max_work: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_predicates: 128,
            max_rules: 512,
            max_atoms: 4_096,
            max_terms: 16_384,
            max_text_bytes: 262_144,
            max_work: 1_000_000,
        }
    }
}

/// One strongly connected component, in dependency order within each stratum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    /// Sorted predicate names, including declared inputs.
    pub predicates: Vec<String>,
    /// Minimum stratum satisfying positive and negative dependencies.
    pub stratum: usize,
    /// True for mutual recursion or a positive self-dependency.
    pub recursive: bool,
}

/// Structural admission result only: no runtime input validation, authorization,
/// execution, proof receipt, or derivation witness is implied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Deterministic SCC partition and strata.
    pub groups: Vec<Group>,
}

struct Meter {
    budget: Budget,
    work: usize,
    atoms: usize,
    terms: usize,
    bytes: usize,
}

impl Meter {
    fn charge(&mut self, work: usize) -> Result<(), Diagnostic> {
        self.work = self.work.checked_add(work).ok_or_else(exhausted)?;
        check(self.work, self.budget.max_work)
    }

    fn text(&mut self, text: &str) -> Result<(), Diagnostic> {
        self.bytes = self.bytes.checked_add(text.len()).ok_or_else(exhausted)?;
        check(self.bytes, self.budget.max_text_bytes)?;
        self.charge(text.len())
    }

    fn symbol(&mut self, text: &str) -> Result<(), Diagnostic> {
        self.text(text)?;
        if text.is_empty()
            || !text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        {
            return Err(refusal(Code::InvalidSymbol, None, None));
        }
        Ok(())
    }

    fn constant_text(&mut self, text: &str) -> Result<(), Diagnostic> {
        self.text(text)?;
        if norn_canonical::nfc(text) != text {
            return Err(refusal(Code::NoncanonicalConstant, None, None));
        }
        Ok(())
    }

    fn atom(&mut self, atom: &Atom) -> Result<(), Diagnostic> {
        self.atoms = self.atoms.checked_add(1).ok_or_else(exhausted)?;
        check(self.atoms, self.budget.max_atoms)?;
        self.terms = self
            .terms
            .checked_add(atom.terms.len())
            .ok_or_else(exhausted)?;
        check(self.terms, self.budget.max_terms)?;
        self.symbol(&atom.predicate)?;
        for term in &atom.terms {
            self.charge(1)?;
            match term {
                Term::Variable(name) => self.symbol(name)?,
                Term::Constant(Value::Text(text)) => self.constant_text(text)?,
                Term::Constant(Value::Bool(_)) => {}
                Term::Constant(Value::Scope(scope)) => {
                    for text in [
                        &scope.org_id,
                        &scope.tenant_id,
                        &scope.workspace_id,
                        &scope.environment_id,
                        &scope.business_unit_id,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        self.constant_text(text)?;
                    }
                }
                Term::Aggregate(_) => return Err(refusal(Code::AggregateNotAdmitted, None, None)),
            }
        }
        Ok(())
    }

    // Both algorithms are already metered by norn-graph. Reserve a conservative
    // full traversal allowance on the shared meter before each call (including
    // preparation and cycle explanation); never reset the caller's budget.
    fn graph_budget<N>(&mut self, graph: &Graph<N>) -> Result<norn_graph::Budget, Diagnostic> {
        let size = graph
            .nodes
            .len()
            .checked_add(graph.edges.len())
            .ok_or_else(exhausted)?;
        let steps = size
            .checked_mul(8)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(exhausted)?;
        self.charge(steps)?;
        Ok(norn_graph::Budget {
            max_nodes: graph.nodes.len(),
            max_edges: graph.edges.len(),
            max_steps: steps,
        })
    }
}

fn exhausted() -> Diagnostic {
    refusal(Code::BudgetExhausted, None, None)
}

fn check(size: usize, limit: usize) -> Result<(), Diagnostic> {
    if size > limit {
        Err(exhausted())
    } else {
        Ok(())
    }
}

fn preflight(program: &Program, budget: Budget) -> Result<Meter, Diagnostic> {
    check(program.predicates.len(), budget.max_predicates)?;
    check(program.rules.len(), budget.max_rules)?;
    let mut meter = Meter {
        budget,
        work: 0,
        atoms: 0,
        terms: 0,
        bytes: 0,
    };
    for predicate in &program.predicates {
        meter.charge(1)?;
        check(predicate.arity, budget.max_terms)?;
        meter.symbol(&predicate.name)?;
    }
    for rule in &program.rules {
        meter.charge(1)?;
        meter.symbol(&rule.id)?;
        meter.atom(&rule.head)?;
        for literal in &rule.body {
            meter.charge(1)?;
            meter.atom(literal.atom())?;
        }
    }
    Ok(meter)
}

/// Validate the finite function-free fragment and return deterministic SCCs
/// and minimum strata. Positive recursion is admitted; all aggregates are
/// explicitly refused until finite-domain lattice semantics are implemented.
///
/// # Errors
/// Returns a stable diagnostic for invalid programs or exhausted budgets.
/// Input facts must separately be finite and match the declared signatures.
pub fn validate(program: &Program, budget: Budget) -> Result<Plan, Diagnostic> {
    let mut meter = preflight(program, budget)?;
    let mut signatures = BTreeMap::new();
    for predicate in &program.predicates {
        meter.charge(1)?;
        if signatures
            .insert(predicate.name.as_str(), predicate)
            .is_some()
        {
            return Err(refusal(
                Code::DuplicatePredicate,
                None,
                Some(&predicate.name),
            ));
        }
    }
    let mut rules = BTreeMap::new();
    for rule in &program.rules {
        meter.charge(1)?;
        if rules.insert(rule.id.as_str(), rule).is_some() {
            return Err(refusal(Code::DuplicateRuleId, Some(&rule.id), None));
        }
    }
    let mut defined = BTreeSet::new();
    let mut edges = BTreeSet::new();
    let mut negative = BTreeSet::new();
    for rule in rules.values() {
        check_rule(rule, &signatures, &mut meter)?;
        defined.insert(rule.head.predicate.as_str());
        for literal in &rule.body {
            meter.charge(1)?;
            let dependency = (
                literal.atom().predicate.as_str(),
                rule.head.predicate.as_str(),
            );
            edges.insert(dependency);
            if matches!(literal, Literal::Negative(_)) {
                negative.insert(dependency);
            }
        }
    }
    for predicate in signatures.values() {
        meter.charge(1)?;
        if predicate.kind == RelationKind::Derived && !defined.contains(predicate.name.as_str()) {
            return Err(refusal(
                Code::UndefinedPredicate,
                None,
                Some(&predicate.name),
            ));
        }
    }
    let graph = Graph {
        nodes: signatures.keys().copied().collect(),
        edges: edges.into_iter().collect(),
    };
    stratify(&graph, &negative, &mut meter)
}

fn signature<'a>(
    atom: &Atom,
    rule: &Rule,
    signatures: &'a BTreeMap<&str, &crate::Predicate>,
) -> Result<&'a crate::Predicate, Diagnostic> {
    let predicate = signatures
        .get(atom.predicate.as_str())
        .copied()
        .ok_or_else(|| {
            refusal(
                Code::UndefinedPredicate,
                Some(&rule.id),
                Some(&atom.predicate),
            )
        })?;
    if predicate.arity != atom.terms.len() {
        return Err(refusal(
            Code::ArityMismatch,
            Some(&rule.id),
            Some(&atom.predicate),
        ));
    }
    Ok(predicate)
}

fn check_rule(
    rule: &Rule,
    signatures: &BTreeMap<&str, &crate::Predicate>,
    meter: &mut Meter,
) -> Result<(), Diagnostic> {
    if signature(&rule.head, rule, signatures)?.kind == RelationKind::Input {
        return Err(refusal(
            Code::InputInHead,
            Some(&rule.id),
            Some(&rule.head.predicate),
        ));
    }
    let mut bound = BTreeSet::new();
    for literal in &rule.body {
        meter.charge(1)?;
        signature(literal.atom(), rule, signatures)?;
        if let Literal::Positive(atom) = literal {
            for term in &atom.terms {
                meter.charge(1)?;
                if let Term::Variable(name) = term {
                    bound.insert(name.as_str());
                }
            }
        }
    }
    for (atom, code) in std::iter::once((&rule.head, Code::UnsafeHeadVariable)).chain(
        rule.body.iter().filter_map(|literal| {
            if let Literal::Negative(atom) = literal {
                Some((atom, Code::UnsafeNegativeVariable))
            } else {
                None
            }
        }),
    ) {
        for term in &atom.terms {
            meter.charge(1)?;
            if let Term::Variable(name) = term
                && !bound.contains(name.as_str())
            {
                return Err(refusal(code, Some(&rule.id), Some(name)));
            }
        }
    }
    Ok(())
}

fn graph_error<N>(error: &norn_graph::Error<N>) -> Diagnostic {
    if matches!(error, norn_graph::Error::BudgetExceeded(_)) {
        exhausted()
    } else {
        refusal(Code::GraphRefused, None, None)
    }
}

fn components<'a>(
    graph: &Graph<&'a str>,
    meter: &mut Meter,
) -> Result<Vec<BTreeSet<&'a str>>, Diagnostic> {
    let mut assigned = BTreeSet::new();
    let mut groups = Vec::new();
    for node in &graph.nodes {
        meter.charge(1)?;
        if assigned.contains(node) {
            continue;
        }
        // SCC(seed) = downstream(seed) intersect upstream(seed). Reuse the
        // existing bounded traversals, avoiding another general graph engine.
        let down = norn_graph::reachability(
            graph,
            &[*node],
            Direction::Downstream,
            meter.graph_budget(graph)?,
        )
        .map_err(|error| graph_error(&error))?;
        let up = norn_graph::reachability(
            graph,
            &[*node],
            Direction::Upstream,
            meter.graph_budget(graph)?,
        )
        .map_err(|error| graph_error(&error))?;
        let mut group = BTreeSet::new();
        for member in down.intersection(&up) {
            meter.charge(1)?;
            assigned.insert(*member);
            group.insert(*member);
        }
        groups.push(group);
    }
    Ok(groups)
}

fn stratify(
    graph: &Graph<&str>,
    negative: &BTreeSet<(&str, &str)>,
    meter: &mut Meter,
) -> Result<Plan, Diagnostic> {
    let groups = components(graph, meter)?;
    let mut owners = BTreeMap::new();
    for (index, members) in groups.iter().enumerate() {
        for member in members {
            meter.charge(1)?;
            owners.insert(*member, index);
        }
    }
    let owner = |name: &str| {
        owners
            .get(name)
            .copied()
            .ok_or_else(|| refusal(Code::GraphRefused, None, None))
    };
    let mut condensed = Graph {
        nodes: (0..groups.len()).collect(),
        edges: Vec::new(),
    };
    let mut incoming: BTreeMap<usize, Vec<(usize, bool)>> = BTreeMap::new();
    let mut recursive = BTreeSet::new();
    for (from, to) in &graph.edges {
        meter.charge(1)?;
        let (from_group, to_group) = (owner(from)?, owner(to)?);
        let negated = negative.contains(&(*from, *to));
        if from_group == to_group {
            if negated {
                return Err(refusal(Code::RecursionThroughNegation, None, Some(to)));
            }
            recursive.insert(from_group);
        } else {
            condensed.edges.push((from_group, to_group));
            incoming
                .entry(to_group)
                .or_default()
                .push((from_group, negated));
        }
    }
    let order = norn_graph::topological_order(&condensed, meter.graph_budget(&condensed)?)
        .map_err(|error| graph_error(&error))?;
    let mut strata: BTreeMap<usize, usize> = BTreeMap::new();
    for &index in &order {
        meter.charge(1)?;
        let mut stratum = 0;
        if let Some(dependencies) = incoming.get(&index) {
            for (dependency, negative) in dependencies {
                meter.charge(1)?;
                let previous = strata
                    .get(dependency)
                    .ok_or_else(|| refusal(Code::GraphRefused, None, None))?;
                stratum = stratum.max(
                    previous
                        .checked_add(usize::from(*negative))
                        .ok_or_else(exhausted)?,
                );
            }
        }
        strata.insert(index, stratum);
    }
    let mut result = Vec::new();
    for index in order {
        meter.charge(1)?;
        let members = groups
            .get(index)
            .ok_or_else(|| refusal(Code::GraphRefused, None, None))?;
        let stratum = strata
            .get(&index)
            .copied()
            .ok_or_else(|| refusal(Code::GraphRefused, None, None))?;
        result.push(Group {
            predicates: members.iter().map(|&name| name.to_owned()).collect(),
            stratum,
            recursive: recursive.contains(&index),
        });
    }
    // Stable sorting retains topological order for positive dependencies at
    // the same stratum. Alphabetical sorting here would schedule a consumer
    // before its prerequisites (for example a(x) :- z(x)).
    result.sort_by_key(|group| group.stratum);
    Ok(Plan { groups: result })
}
