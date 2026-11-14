//! Bottom-up evaluation of an admitted program over finite input facts.
//!
//! [`evaluate`] calls [`validate()`] first, so a program the admission contract
//! refuses is never run; validation also bounds each rule's width (arity,
//! variables, body literals). Evaluation is semi-naive, one stratum at a time,
//! so a negated relation is complete before any rule reads it.
//!
//! Memory is charged before it is held. Joins run depth-first over one binding
//! per rule, so no intermediate result set exists. A derived fact is charged
//! for its values and for the encoded bytes of those values when it is first
//! derived, before it is stored; the rendered result therefore never exceeds
//! what was charged. An exhausted budget refuses and returns nothing partial.
//!
//! The active domain is fixed before the first rule runs: the constants of the
//! input facts and of the rules. Rules have no functions, arithmetic or fresh
//! values, so evaluation cannot leave that domain and always terminates.
//! Constants are numbered in the order of their canonical encoding and rules
//! run in identifier order, so the result, the usage and the name of an
//! exhausted rule depend only on the program and the set of input facts: not
//! on the order facts or rules were supplied in, nor on repeated facts.
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
    /// Maximum supplied tuples, counted before repeats collapse.
    pub max_input_facts: usize,
    /// Maximum UTF-8 bytes of text across all supplied tuples, repeats included.
    pub max_text_bytes: usize,
    /// Maximum distinct derived facts across all relations.
    pub max_derived_facts: usize,
    /// Maximum values across all distinct derived facts (facts times arity).
    pub max_derived_values: usize,
    /// Maximum canonical-encoding bytes across all values of all distinct
    /// derived facts: the size of the result.
    pub max_result_bytes: usize,
    /// Maximum work units. See [`Usage::work`].
    pub max_work: usize,
}

impl Default for ExecBudget {
    fn default() -> Self {
        Self {
            max_input_facts: 65_536,
            max_text_bytes: 4_194_304,
            max_derived_facts: 1_048_576,
            max_derived_values: 4_194_304,
            max_result_bytes: 67_108_864,
            max_work: 16_000_000,
        }
    }
}

/// What one evaluation consumed. Every figure except `supplied_facts`
/// depends only on the program and the set of distinct input facts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Supplied tuples, repeats included.
    pub supplied_facts: usize,
    /// Distinct input facts.
    pub input_facts: usize,
    /// Distinct derived facts.
    pub derived_facts: usize,
    /// Values across distinct derived facts.
    pub derived_values: usize,
    /// Canonical-encoding bytes across those values.
    pub result_bytes: usize,
    /// Work units: one per value of a distinct input fact; per round, one per
    /// rule and per body literal of its stratum; per join step, one plus the
    /// width of each row scanned; per negation probe, one plus its width;
    /// per head produced, one plus its width.
    pub work: usize,
    /// Fixed-point rounds across all strata.
    pub rounds: usize,
    /// The most rows a round held before adding them to the model, in values.
    /// Never more than `derived_values`: each row is charged before it is held.
    pub peak_pending_values: usize,
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

/// An index this module computed itself did not resolve: a defect.
fn broken() -> Diagnostic {
    refusal(Code::ExecutorInvariant, None, None)
}

fn add(
    total: &mut usize,
    amount: usize,
    limit: usize,
    rule: Option<&str>,
) -> Result<(), Diagnostic> {
    match total.checked_add(amount) {
        Some(sum) if sum <= limit => {
            *total = sum;
            Ok(())
        }
        Some(_) | None => Err(exhausted(rule)),
    }
}

struct Meter {
    budget: ExecBudget,
    usage: Usage,
}

impl Meter {
    fn work(&mut self, amount: usize, rule: Option<&str>) -> Result<(), Diagnostic> {
        add(&mut self.usage.work, amount, self.budget.max_work, rule)
    }

    /// Charge one new derived fact before it is stored.
    fn derived(&mut self, rule: &str, width: usize, bytes: usize) -> Result<(), Diagnostic> {
        let budget = self.budget;
        let usage = &mut self.usage;
        add(
            &mut usage.derived_facts,
            1,
            budget.max_derived_facts,
            Some(rule),
        )?;
        add(
            &mut usage.derived_values,
            width,
            budget.max_derived_values,
            Some(rule),
        )?;
        add(
            &mut usage.result_bytes,
            bytes,
            budget.max_result_bytes,
            Some(rule),
        )
    }
}

/// The canonical encoding that orders and identifies a constant.
fn key(value: &Value) -> Result<Vec<u8>, Diagnostic> {
    norn_canonical::canonical_json_of(value)
        .map_err(|_unencodable| refusal(Code::UnencodableValue, None, None))
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

/// The fixed active domain: constants in canonical order, with their
/// encoded sizes, and each declared relation's index.
struct Domain<'a> {
    relations: BTreeMap<&'a str, usize>,
    numbers: BTreeMap<Vec<u8>, u32>,
    values: Vec<Value>,
    sizes: Vec<usize>,
}

impl Domain<'_> {
    fn number(&self, key: &[u8]) -> Result<u32, Diagnostic> {
        self.numbers.get(key).copied().ok_or_else(broken)
    }

    /// Encoded bytes of a row's values.
    fn bytes(&self, row: &[u32]) -> Result<usize, Diagnostic> {
        let mut total = 0_usize;
        for number in row {
            let index = usize::try_from(*number).map_err(|_overflow| broken())?;
            let size = *self.sizes.get(index).ok_or_else(broken)?;
            total = total.checked_add(size).ok_or_else(broken)?;
        }
        Ok(total)
    }

    fn atom(
        &self,
        atom: &Atom,
        variables: &mut BTreeMap<String, usize>,
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
                Term::Constant(value) => Slot::Constant(self.number(&key(value)?)?),
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
    ) -> Result<CompiledRule<'r>, Diagnostic> {
        let mut variables = BTreeMap::new();
        let mut positive = Vec::new();
        let mut negative = Vec::new();
        for literal in &rule.body {
            match literal {
                Literal::Positive(atom) => positive.push(self.atom(atom, &mut variables)?),
                Literal::Negative(atom) => negative.push(self.atom(atom, &mut variables)?),
            }
        }
        let head = self.atom(&rule.head, &mut variables)?;
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

/// Supplied facts, checked against the declared signatures and reduced to
/// distinct encoded tuples. Which refusal a bad input gets does not depend on
/// tuple order: relations are checked in name order, and within one relation
/// every tuple's width is checked before any value's text.
///
/// The cost of this pass is bounded by `max_input_facts` and `max_text_bytes`
/// and is not counted as work, so work does not depend on repeated facts.
/// Distinct input rows by relation, as canonical keys, and every input
/// constant by key. Each value is encoded once.
type Inputs<'f> = (
    BTreeMap<String, BTreeSet<Vec<Vec<u8>>>>,
    BTreeMap<Vec<u8>, &'f Value>,
);

fn inputs<'f>(
    program: &Program,
    facts: &'f Facts,
    budget: ExecBudget,
    usage: &mut Usage,
) -> Result<Inputs<'f>, Diagnostic> {
    let supplied = facts
        .values()
        .try_fold(0_usize, |total, tuples| total.checked_add(tuples.len()))
        .ok_or_else(|| exhausted(None))?;
    if supplied > budget.max_input_facts {
        return Err(exhausted(None));
    }
    usage.supplied_facts = supplied;
    let signatures: BTreeMap<&str, &crate::Predicate> = program
        .predicates
        .iter()
        .map(|predicate| (predicate.name.as_str(), predicate))
        .collect();
    let mut text = 0_usize;
    for (name, tuples) in facts {
        let signature = signatures
            .get(name.as_str())
            .ok_or_else(|| refusal(Code::UndefinedPredicate, None, Some(name)))?;
        if signature.kind == RelationKind::Derived {
            return Err(refusal(Code::DerivedInInput, None, Some(name)));
        }
        if tuples.iter().any(|tuple| tuple.len() != signature.arity) {
            return Err(refusal(Code::ArityMismatch, None, Some(name)));
        }
        for text_value in tuples.iter().flatten().flat_map(constant_texts) {
            add(&mut text, text_value.len(), budget.max_text_bytes, None)?;
        }
    }
    let mut distinct = BTreeMap::new();
    let mut constants = BTreeMap::new();
    for (name, tuples) in facts {
        if tuples
            .iter()
            .flatten()
            .flat_map(constant_texts)
            .any(|text_value| norn_canonical::nfc(text_value) != text_value)
        {
            return Err(refusal(Code::NoncanonicalConstant, None, Some(name)));
        }
        let mut rows = BTreeSet::new();
        for tuple in tuples {
            let mut row = Vec::with_capacity(tuple.len());
            for value in tuple {
                let encoded = key(value)?;
                constants.entry(encoded.clone()).or_insert(value);
                row.push(encoded);
            }
            rows.insert(row);
        }
        distinct.insert(name.clone(), rows);
    }
    Ok((distinct, constants))
}

/// The fixed domain: every constant of the distinct inputs and the rules.
fn domain<'a>(
    program: &'a Program,
    mut constants: BTreeMap<Vec<u8>, &'a Value>,
) -> Result<Domain<'a>, Diagnostic> {
    for rule in &program.rules {
        for atom in std::iter::once(&rule.head).chain(rule.body.iter().map(Literal::atom)) {
            for term in &atom.terms {
                if let Term::Constant(value) = term {
                    constants.entry(key(value)?).or_insert(value);
                }
            }
        }
    }
    let mut numbers = BTreeMap::new();
    let mut values = Vec::with_capacity(constants.len());
    let mut sizes = Vec::with_capacity(constants.len());
    for (encoded, value) in constants {
        let number = u32::try_from(values.len()).map_err(|_overflow| exhausted(None))?;
        sizes.push(encoded.len());
        numbers.insert(encoded, number);
        values.push(value.clone());
    }
    let names: BTreeSet<&str> = program
        .predicates
        .iter()
        .map(|predicate| predicate.name.as_str())
        .collect();
    Ok(Domain {
        relations: names.into_iter().zip(0..).collect(),
        numbers,
        values,
        sizes,
    })
}

/// The distinct inputs as domain rows, one table per declared relation.
fn load(
    keyed: &BTreeMap<String, BTreeSet<Vec<Vec<u8>>>>,
    domain: &Domain<'_>,
    meter: &mut Meter,
) -> Result<Vec<Table>, Diagnostic> {
    let mut tables = vec![Table::new(); domain.relations.len()];
    for (name, rows) in keyed {
        let relation = *domain.relations.get(name.as_str()).ok_or_else(broken)?;
        let table = tables.get_mut(relation).ok_or_else(broken)?;
        for encoded in rows {
            meter.work(encoded.len().max(1), None)?;
            meter.usage.input_facts = meter.usage.input_facts.saturating_add(1);
            let row = encoded
                .iter()
                .map(|value| domain.number(value))
                .collect::<Result<Row, _>>()?;
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

/// Bind `row` into `binding` in place. Returns the variables it newly bound
/// (to undo afterwards), or `None` when the row disagrees; on `None` the
/// binding is unchanged.
fn unify(atom: &CompiledAtom, row: &[u32], binding: &mut [Option<u32>]) -> Option<Vec<usize>> {
    let mut bound = Vec::new();
    for (slot, value) in atom.slots.iter().zip(row) {
        let agrees = match slot {
            Slot::Constant(constant) => constant == value,
            Slot::Variable(index) => match binding.get_mut(*index) {
                Some(Some(current)) => *current == *value,
                Some(unbound @ None) => {
                    *unbound = Some(*value);
                    bound.push(*index);
                    true
                }
                None => false,
            },
        };
        if !agrees {
            release(binding, &bound);
            return None;
        }
    }
    Some(bound)
}

fn release(binding: &mut [Option<u32>], bound: &[usize]) {
    for index in bound {
        if let Some(slot) = binding.get_mut(*index) {
            *slot = None;
        }
    }
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

/// One evaluation pass of one rule. With `delta`, the positive premise at
/// that position reads only the facts new in the last round.
struct Pass<'s, 'r> {
    rule: &'s CompiledRule<'r>,
    full: &'s [Table],
    delta: Option<(usize, &'s BTreeMap<usize, Table>)>,
}

/// Rows derived this round and not yet in the model, charged as they arrive.
struct Pending {
    rows: Vec<Table>,
    values: usize,
}

impl Pass<'_, '_> {
    /// Enumerate the join depth-first from premise `position`, holding only
    /// the one binding, and add every new head row to `pending`.
    fn join(
        &self,
        position: usize,
        binding: &mut [Option<u32>],
        domain: &Domain<'_>,
        pending: &mut Pending,
        meter: &mut Meter,
    ) -> Result<(), Diagnostic> {
        let rule = self.rule;
        let Some(atom) = rule.positive.get(position) else {
            return self.head(binding, domain, pending, meter);
        };
        let empty = Table::new();
        let table = match self.delta {
            Some((at, recent)) if at == position => recent.get(&atom.relation).unwrap_or(&empty),
            Some(_) | None => self.full.get(atom.relation).ok_or_else(broken)?,
        };
        let leading = prefix(atom, binding);
        for row in table
            .range(leading.clone()..)
            .take_while(|row| row.starts_with(&leading))
        {
            meter.work(row.len().saturating_add(1), Some(rule.id))?;
            if let Some(bound) = unify(atom, row, binding) {
                let result = self.join(position.saturating_add(1), binding, domain, pending, meter);
                release(binding, &bound);
                result?;
            }
        }
        Ok(())
    }

    fn head(
        &self,
        binding: &[Option<u32>],
        domain: &Domain<'_>,
        pending: &mut Pending,
        meter: &mut Meter,
    ) -> Result<(), Diagnostic> {
        let rule = self.rule;
        for atom in &rule.negative {
            meter.work(atom.slots.len().saturating_add(1), Some(rule.id))?;
            let table = self.full.get(atom.relation).ok_or_else(broken)?;
            if table.contains(&ground(atom, binding)?) {
                return Ok(());
            }
        }
        meter.work(rule.head.slots.len().saturating_add(1), Some(rule.id))?;
        let row = ground(&rule.head, binding)?;
        let known = self
            .full
            .get(rule.head.relation)
            .ok_or_else(broken)?
            .contains(&row);
        let queue = pending
            .rows
            .get_mut(rule.head.relation)
            .ok_or_else(broken)?;
        if known || queue.contains(&row) {
            return Ok(());
        }
        // Charged before it is held.
        meter.derived(rule.id, row.len(), domain.bytes(&row)?)?;
        pending.values = pending.values.saturating_add(row.len());
        meter.usage.peak_pending_values = meter.usage.peak_pending_values.max(pending.values);
        queue.insert(row);
        Ok(())
    }
}

/// Move one round's pending rows into the model; they are the next delta.
fn absorb(pending: Pending, full: &mut [Table]) -> Result<BTreeMap<usize, Table>, Diagnostic> {
    let mut delta = BTreeMap::new();
    for (relation, rows) in pending.rows.into_iter().enumerate() {
        if rows.is_empty() {
            continue;
        }
        let table = full.get_mut(relation).ok_or_else(broken)?;
        table.extend(rows.iter().cloned());
        delta.insert(relation, rows);
    }
    Ok(delta)
}

/// Run one stratum's rules, in identifier order, to their fixed point.
fn saturate(
    rules: &[&CompiledRule<'_>],
    full: &mut [Table],
    domain: &Domain<'_>,
    meter: &mut Meter,
) -> Result<(), Diagnostic> {
    let own: BTreeSet<usize> = rules.iter().map(|rule| rule.head.relation).collect();
    let overhead = rules.iter().fold(0_usize, |total, rule| {
        total
            .saturating_add(1)
            .saturating_add(rule.positive.len())
            .saturating_add(rule.negative.len())
    });
    let mut delta: Option<BTreeMap<usize, Table>> = None;
    loop {
        meter.work(overhead, None)?;
        meter.usage.rounds = meter.usage.rounds.saturating_add(1);
        let mut pending = Pending {
            rows: vec![Table::new(); full.len()],
            values: 0,
        };
        for rule in rules {
            let mut binding = vec![None; rule.variables];
            match &delta {
                None => Pass {
                    rule,
                    full: &*full,
                    delta: None,
                }
                .join(0, &mut binding, domain, &mut pending, meter)?,
                Some(recent) => {
                    for (position, atom) in rule.positive.iter().enumerate() {
                        if own.contains(&atom.relation) && recent.contains_key(&atom.relation) {
                            Pass {
                                rule,
                                full: &*full,
                                delta: Some((position, recent)),
                            }
                            .join(
                                0,
                                &mut binding,
                                domain,
                                &mut pending,
                                meter,
                            )?;
                        }
                    }
                }
            }
        }
        let next = absorb(pending, full)?;
        if next.is_empty() {
            return Ok(());
        }
        delta = Some(next);
    }
}

/// The derived relations in name order, values copied from the domain. The
/// copy is bounded by `result_bytes`, charged as each fact was derived.
fn render(
    program: &Program,
    domain: &Domain<'_>,
    full: &[Table],
) -> Result<Vec<Relation>, Diagnostic> {
    let derived: BTreeSet<&str> = program
        .predicates
        .iter()
        .filter(|predicate| predicate.kind == RelationKind::Derived)
        .map(|predicate| predicate.name.as_str())
        .collect();
    let mut relations = Vec::with_capacity(derived.len());
    for (name, relation) in &domain.relations {
        if !derived.contains(name) {
            continue;
        }
        let mut tuples = Vec::new();
        for row in full.get(*relation).ok_or_else(broken)? {
            let mut tuple = Tuple::with_capacity(row.len());
            for number in row {
                let index = usize::try_from(*number).map_err(|_overflow| broken())?;
                tuple.push(domain.values.get(index).ok_or_else(broken)?.clone());
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
/// program; it and the reported usage (apart from `supplied_facts`) do not
/// depend on the order of `facts` or of the rules, nor on repeated facts.
///
/// # Errors
/// Any refusal of [`validate()`]; `datalog.undefined_predicate`,
/// `datalog.derived_in_input`, `datalog.arity_mismatch` or
/// `datalog.noncanonical_constant` for the supplied facts;
/// `datalog.unencodable_value` for a value without a canonical encoding;
/// `datalog.budget_exhausted` when `budget` is reached, naming the rule that
/// was running when one was. No partial result is returned.
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
    };
    let (keyed, constants) = inputs(program, facts, budget, &mut meter.usage)?;
    let domain = domain(program, constants)?;
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
        rules.push(domain.rule(rule, &strata)?);
    }
    rules.sort_by(|left, right| left.id.cmp(right.id));
    let mut full = load(&keyed, &domain, &mut meter)?;
    let levels: BTreeSet<usize> = rules.iter().map(|rule| rule.stratum).collect();
    for level in levels {
        let stratum: Vec<&CompiledRule<'_>> =
            rules.iter().filter(|rule| rule.stratum == level).collect();
        saturate(&stratum, &mut full, &domain, &mut meter)?;
    }
    Ok(Evaluation {
        relations: render(program, &domain, &full)?,
        usage: meter.usage,
    })
}
