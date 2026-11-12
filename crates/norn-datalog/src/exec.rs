//! Bottom-up evaluation of an admitted program over finite input facts.
//!
//! [`evaluate`] calls [`validate()`] first, so a program the admission contract
//! refuses is never run. Evaluation is semi-naive, one stratum at a time, so a
//! negated relation is complete before any rule reads it. Every scanned tuple
//! and every derived fact is charged to an [`ExecBudget`]; an exhausted budget
//! refuses and returns no partial result.
//!
//! The active domain is fixed before the first rule runs: the constants of the
//! input facts and of the rules. Rules have no functions, arithmetic or fresh
//! values, so evaluation cannot leave that domain and always terminates.
//! Constants are numbered in the order of their canonical encoding, so the
//! result and the metered work depend only on the set of input facts, never on
//! the order they were supplied in.
//!
//! The result is the derived relations as data. It carries no witness and no
//! authorization: what a derived fact means is the rule pack's business.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::validate::{constant_texts, refusal};
use crate::{
    Atom, Budget, Code, Diagnostic, Literal, Program, RelationKind, Rule, Term, Value, validate,
};

/// One fact: the arguments of a relation, in declared column order.
pub type Tuple = Vec<Value>;

/// Finite input relations by declared name. A declared input with no entry is
/// empty. Repeated tuples are one fact.
pub type Facts = BTreeMap<String, Vec<Tuple>>;

/// Ceilings on one evaluation. Reaching any of them refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecBudget {
    /// Maximum supplied tuples, counted before duplicates collapse.
    pub max_input_facts: usize,
    /// Maximum distinct derived facts across all relations.
    pub max_derived_facts: usize,
    /// Maximum UTF-8 bytes of text across all supplied tuples.
    pub max_text_bytes: usize,
    /// Maximum work units: one per value read on input, per tuple a join
    /// scans, per negation probe and per fact a rule produces.
    pub max_work: usize,
}

impl Default for ExecBudget {
    fn default() -> Self {
        Self {
            max_input_facts: 65_536,
            max_derived_facts: 1_048_576,
            max_text_bytes: 4_194_304,
            max_work: 16_000_000,
        }
    }
}

/// What one evaluation consumed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Supplied tuples, counted before duplicates collapse.
    pub input_facts: usize,
    /// Distinct derived facts.
    pub derived_facts: usize,
    /// Work units charged.
    pub work: usize,
    /// Fixed-point rounds across all strata.
    pub rounds: usize,
}

/// One derived relation, with its facts in canonical order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relation {
    /// Declared relation name.
    pub predicate: String,
    /// Distinct facts, ordered by the canonical encoding of their values.
    pub tuples: Vec<Tuple>,
}

/// The least model's derived relations, in name order, and the work it took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evaluation {
    /// Every derived relation, including empty ones.
    pub relations: Vec<Relation>,
    /// Metered consumption.
    pub usage: Usage,
}

impl Evaluation {
    /// The facts of one derived relation; empty when it has none or is not a
    /// derived relation of the program.
    #[must_use]
    pub fn tuples(&self, predicate: &str) -> &[Tuple] {
        self.relations
            .iter()
            .find(|relation| relation.predicate == predicate)
            .map_or(&[], |relation| relation.tuples.as_slice())
    }
}

/// A fact as domain numbers.
type Row = Vec<u32>;
/// One relation's facts.
type Table = BTreeSet<Row>;

fn exhausted(rule: Option<&str>) -> Diagnostic {
    refusal(Code::BudgetExhausted, rule, None)
}

/// An index this module computed itself did not resolve.
fn broken() -> Diagnostic {
    refusal(Code::GraphRefused, None, None)
}

struct Meter {
    budget: ExecBudget,
    usage: Usage,
    text_bytes: usize,
}

fn reserve(value: &mut usize, amount: usize, limit: usize) -> bool {
    match value.checked_add(amount) {
        Some(total) if total <= limit => {
            *value = total;
            true
        }
        Some(_) | None => false,
    }
}

impl Meter {
    fn work(&mut self, amount: usize, rule: Option<&str>) -> Result<(), Diagnostic> {
        if reserve(&mut self.usage.work, amount, self.budget.max_work) {
            Ok(())
        } else {
            Err(exhausted(rule))
        }
    }

    fn input(&mut self) -> Result<(), Diagnostic> {
        if reserve(&mut self.usage.input_facts, 1, self.budget.max_input_facts) {
            Ok(())
        } else {
            Err(exhausted(None))
        }
    }

    fn text(&mut self, bytes: usize) -> Result<(), Diagnostic> {
        if reserve(&mut self.text_bytes, bytes, self.budget.max_text_bytes) {
            Ok(())
        } else {
            Err(exhausted(None))
        }
    }

    fn derived(&mut self, rule: &str) -> Result<(), Diagnostic> {
        if reserve(
            &mut self.usage.derived_facts,
            1,
            self.budget.max_derived_facts,
        ) {
            Ok(())
        } else {
            Err(exhausted(Some(rule)))
        }
    }
}

/// The canonical encoding that orders and identifies a constant.
fn key(value: &Value, meter: &mut Meter) -> Result<Vec<u8>, Diagnostic> {
    let Ok(key) = norn_canonical::canonical_json_of(value) else {
        return Err(exhausted(None));
    };
    meter.work(key.len(), None)?;
    Ok(key)
}

/// A term of a compiled atom.
enum Slot {
    /// Index into the rule's variable bindings.
    Variable(usize),
    /// Domain number of a constant.
    Constant(u32),
}

struct CompiledAtom {
    relation: usize,
    slots: Vec<Slot>,
}

struct CompiledRule<'a> {
    id: &'a str,
    stratum: usize,
    head: CompiledAtom,
    positive: Vec<CompiledAtom>,
    negative: Vec<CompiledAtom>,
    variables: usize,
}

/// Declared relations in name order, and the fixed active domain.
struct Catalog<'a> {
    relations: BTreeMap<&'a str, usize>,
    domain: BTreeMap<Vec<u8>, u32>,
}

impl Catalog<'_> {
    fn atom(
        &self,
        atom: &Atom,
        variables: &mut BTreeMap<String, usize>,
        meter: &mut Meter,
    ) -> Result<CompiledAtom, Diagnostic> {
        let relation = *self
            .relations
            .get(atom.predicate.as_str())
            .ok_or_else(broken)?;
        let mut slots = Vec::with_capacity(atom.terms.len());
        for term in &atom.terms {
            slots.push(match term {
                Term::Variable(name) => {
                    let next = variables.len();
                    Slot::Variable(*variables.entry(name.clone()).or_insert(next))
                }
                Term::Constant(value) => {
                    Slot::Constant(*self.domain.get(&key(value, meter)?).ok_or_else(broken)?)
                }
                Term::Aggregate(_) => {
                    return Err(refusal(Code::AggregateNotAdmitted, None, None));
                }
            });
        }
        Ok(CompiledAtom { relation, slots })
    }

    fn rule<'r>(
        &self,
        rule: &'r Rule,
        strata: &BTreeMap<&str, usize>,
        meter: &mut Meter,
    ) -> Result<CompiledRule<'r>, Diagnostic> {
        let mut variables = BTreeMap::new();
        let mut positive = Vec::new();
        let mut negative = Vec::new();
        for literal in &rule.body {
            match literal {
                Literal::Positive(atom) => positive.push(self.atom(atom, &mut variables, meter)?),
                Literal::Negative(atom) => negative.push(self.atom(atom, &mut variables, meter)?),
            }
        }
        let head = self.atom(&rule.head, &mut variables, meter)?;
        Ok(CompiledRule {
            id: &rule.id,
            stratum: *strata
                .get(rule.head.predicate.as_str())
                .ok_or_else(broken)?,
            head,
            positive,
            negative,
            variables: variables.len(),
        })
    }
}

/// Check the supplied facts against the declared signatures and collect every
/// constant they and the rules mention, by canonical key.
fn constants(
    program: &Program,
    facts: &Facts,
    meter: &mut Meter,
) -> Result<BTreeMap<Vec<u8>, Value>, Diagnostic> {
    let signatures: BTreeMap<&str, &crate::Predicate> = program
        .predicates
        .iter()
        .map(|predicate| (predicate.name.as_str(), predicate))
        .collect();
    let mut keys = BTreeMap::new();
    for (name, tuples) in facts {
        let signature = signatures
            .get(name.as_str())
            .ok_or_else(|| refusal(Code::UndefinedPredicate, None, Some(name)))?;
        if signature.kind == RelationKind::Derived {
            return Err(refusal(Code::DerivedInInput, None, Some(name)));
        }
        for tuple in tuples {
            meter.input()?;
            if tuple.len() != signature.arity {
                return Err(refusal(Code::ArityMismatch, None, Some(name)));
            }
            for value in tuple {
                for text in constant_texts(value) {
                    meter.text(text.len())?;
                    if norn_canonical::nfc(text) != text {
                        return Err(refusal(Code::NoncanonicalConstant, None, Some(name)));
                    }
                }
                keys.insert(key(value, meter)?, value.clone());
            }
        }
    }
    for rule in &program.rules {
        for atom in std::iter::once(&rule.head).chain(rule.body.iter().map(Literal::atom)) {
            for term in &atom.terms {
                if let Term::Constant(value) = term {
                    keys.insert(key(value, meter)?, value.clone());
                }
            }
        }
    }
    Ok(keys)
}

/// The supplied facts as domain rows, one table per declared relation.
fn load(facts: &Facts, catalog: &Catalog<'_>, meter: &mut Meter) -> Result<Vec<Table>, Diagnostic> {
    let mut tables = vec![Table::new(); catalog.relations.len()];
    for (name, tuples) in facts {
        let relation = *catalog.relations.get(name.as_str()).ok_or_else(broken)?;
        let table = tables.get_mut(relation).ok_or_else(broken)?;
        for tuple in tuples {
            let mut row = Row::with_capacity(tuple.len());
            for value in tuple {
                row.push(*catalog.domain.get(&key(value, meter)?).ok_or_else(broken)?);
            }
            table.insert(row);
        }
    }
    Ok(tables)
}

/// The values an atom's leading terms already fix, for a range scan.
fn prefix(atom: &CompiledAtom, binding: &[Option<u32>]) -> Row {
    let mut prefix = Row::new();
    for slot in &atom.slots {
        let fixed = match slot {
            Slot::Constant(value) => Some(*value),
            Slot::Variable(index) => binding.get(*index).copied().flatten(),
        };
        let Some(value) = fixed else { break };
        prefix.push(value);
    }
    prefix
}

/// The binding extended by one matching row, or `None` when it disagrees.
fn unify(atom: &CompiledAtom, row: &[u32], binding: &[Option<u32>]) -> Option<Vec<Option<u32>>> {
    let mut extended = binding.to_vec();
    for (slot, value) in atom.slots.iter().zip(row) {
        match slot {
            Slot::Constant(constant) if constant == value => {}
            Slot::Constant(_) => return None,
            Slot::Variable(index) => match extended.get_mut(*index)? {
                Some(bound) if *bound == *value => {}
                Some(_) => return None,
                unbound @ None => *unbound = Some(*value),
            },
        }
    }
    Some(extended)
}

/// An atom's row under a complete binding.
fn ground(atom: &CompiledAtom, binding: &[Option<u32>]) -> Result<Row, Diagnostic> {
    atom.slots
        .iter()
        .map(|slot| match slot {
            Slot::Constant(value) => Ok(*value),
            Slot::Variable(index) => binding.get(*index).copied().flatten().ok_or_else(broken),
        })
        .collect()
}

/// Every head row one rule derives. With `delta`, the positive premise at
/// that position reads only the facts new in the last round.
fn derive(
    rule: &CompiledRule<'_>,
    full: &[Table],
    delta: Option<(usize, &[Table])>,
    meter: &mut Meter,
) -> Result<Vec<Row>, Diagnostic> {
    let mut bindings = vec![vec![None; rule.variables]];
    for (position, atom) in rule.positive.iter().enumerate() {
        let tables = match delta {
            Some((at, recent)) if at == position => recent,
            Some(_) | None => full,
        };
        let table = tables.get(atom.relation).ok_or_else(broken)?;
        let mut next = Vec::new();
        for binding in &bindings {
            let leading = prefix(atom, binding);
            for row in table
                .range(leading.clone()..)
                .take_while(|row| row.starts_with(&leading))
            {
                meter.work(1, Some(rule.id))?;
                if let Some(extended) = unify(atom, row, binding) {
                    next.push(extended);
                }
            }
        }
        bindings = next;
        if bindings.is_empty() {
            return Ok(Vec::new());
        }
    }
    let mut rows = Vec::with_capacity(bindings.len());
    'binding: for binding in &bindings {
        for atom in &rule.negative {
            meter.work(1, Some(rule.id))?;
            let table = full.get(atom.relation).ok_or_else(broken)?;
            if table.contains(&ground(atom, binding)?) {
                continue 'binding;
            }
        }
        meter.work(1, Some(rule.id))?;
        rows.push(ground(&rule.head, binding)?);
    }
    Ok(rows)
}

/// Add one round's rows to `full`; the rows that were new are the next delta.
fn absorb(
    produced: Vec<(&CompiledRule<'_>, Vec<Row>)>,
    full: &mut [Table],
    meter: &mut Meter,
) -> Result<Vec<Table>, Diagnostic> {
    let mut delta = vec![Table::new(); full.len()];
    for (rule, rows) in produced {
        let table = full.get_mut(rule.head.relation).ok_or_else(broken)?;
        let recent = delta.get_mut(rule.head.relation).ok_or_else(broken)?;
        for row in rows {
            if table.insert(row.clone()) {
                meter.derived(rule.id)?;
                recent.insert(row);
            }
        }
    }
    Ok(delta)
}

/// Run one stratum's rules to their fixed point over `full`.
fn saturate(
    rules: &[&CompiledRule<'_>],
    full: &mut [Table],
    meter: &mut Meter,
) -> Result<(), Diagnostic> {
    let own: BTreeSet<usize> = rules.iter().map(|rule| rule.head.relation).collect();
    let mut produced = Vec::new();
    for rule in rules {
        produced.push((*rule, derive(rule, full, None, meter)?));
    }
    let mut delta = absorb(produced, full, meter)?;
    meter.usage.rounds = meter.usage.rounds.saturating_add(1);
    while delta.iter().any(|table| !table.is_empty()) {
        let mut produced = Vec::new();
        for rule in rules {
            for (position, atom) in rule.positive.iter().enumerate() {
                let fresh = delta
                    .get(atom.relation)
                    .is_some_and(|table| !table.is_empty());
                if own.contains(&atom.relation) && fresh {
                    produced.push((
                        *rule,
                        derive(rule, full, Some((position, delta.as_slice())), meter)?,
                    ));
                }
            }
        }
        delta = absorb(produced, full, meter)?;
        meter.usage.rounds = meter.usage.rounds.saturating_add(1);
    }
    Ok(())
}

/// The derived relations in name order, with domain numbers turned back into
/// their constants.
fn render(
    program: &Program,
    catalog: &Catalog<'_>,
    values: &[Value],
    full: &[Table],
) -> Result<Vec<Relation>, Diagnostic> {
    let derived: BTreeSet<&str> = program
        .predicates
        .iter()
        .filter(|predicate| predicate.kind == RelationKind::Derived)
        .map(|predicate| predicate.name.as_str())
        .collect();
    let mut relations = Vec::with_capacity(derived.len());
    for (name, relation) in &catalog.relations {
        if !derived.contains(name) {
            continue;
        }
        let mut tuples = Vec::new();
        for row in full.get(*relation).ok_or_else(broken)? {
            let mut tuple = Tuple::with_capacity(row.len());
            for number in row {
                let index = usize::try_from(*number).map_err(|_overflow| broken())?;
                tuple.push(values.get(index).ok_or_else(broken)?.clone());
            }
            tuples.push(tuple);
        }
        relations.push(Relation {
            predicate: (*name).to_owned(),
            tuples,
        });
    }
    Ok(relations)
}

/// Evaluate `program` over `facts` and return every derived relation.
///
/// The program is admitted with [`validate()`] under `admission` first. Facts
/// may be supplied only for declared input relations, with the declared width
/// and NFC text. The result is the unique least model of the stratified
/// program; it does not depend on the order of `facts` or of the rules.
///
/// # Errors
/// Any refusal of [`validate()`]; `datalog.undefined_predicate`,
/// `datalog.derived_in_input`, `datalog.arity_mismatch` or
/// `datalog.noncanonical_constant` for the supplied facts; and
/// `datalog.budget_exhausted` when `budget` is reached, naming the rule that
/// was running. No partial result is returned.
pub fn evaluate(
    program: &Program,
    facts: &Facts,
    admission: Budget,
    budget: ExecBudget,
) -> Result<Evaluation, Diagnostic> {
    let plan = validate(program, admission)?;
    let mut meter = Meter {
        budget,
        usage: Usage::default(),
        text_bytes: 0,
    };
    // Numbering constants in key order makes row order the canonical order.
    let mut domain = BTreeMap::new();
    let mut values = Vec::new();
    for (key, value) in constants(program, facts, &mut meter)? {
        let Ok(number) = u32::try_from(values.len()) else {
            return Err(exhausted(None));
        };
        domain.insert(key, number);
        values.push(value);
    }
    let names: BTreeSet<&str> = program
        .predicates
        .iter()
        .map(|predicate| predicate.name.as_str())
        .collect();
    let catalog = Catalog {
        relations: names.into_iter().zip(0..).collect(),
        domain,
    };
    let strata: BTreeMap<&str, usize> = plan
        .groups
        .iter()
        .flat_map(|group| {
            group
                .predicates
                .iter()
                .map(|name| (name.as_str(), group.stratum))
        })
        .collect();
    let mut rules = Vec::with_capacity(program.rules.len());
    for rule in &program.rules {
        rules.push(catalog.rule(rule, &strata, &mut meter)?);
    }
    let mut full = load(facts, &catalog, &mut meter)?;
    let levels: BTreeSet<usize> = rules.iter().map(|rule| rule.stratum).collect();
    for level in levels {
        let stratum: Vec<&CompiledRule<'_>> =
            rules.iter().filter(|rule| rule.stratum == level).collect();
        saturate(&stratum, &mut full, &mut meter)?;
    }
    Ok(Evaluation {
        relations: render(program, &catalog, &values, &full)?,
        usage: meter.usage,
    })
}
