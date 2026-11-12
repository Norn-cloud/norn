//! U7 (#718): chase certificates for path-mapping preservation.
//!
//! A path mapping lowers to mapping rules (full TGDs, one head atom each)
//! that pull source data into the target. It preserves a target EGD when
//! every source instance satisfying the source theory pulls back to an
//! instance satisfying that EGD.
//!
//! - [`check`] (`preservation/check.rs`) is the trusted decision. Charon and
//!   Aeneas extract it, and `Checker/Preservation/Proof.lean` proves that its
//!   acceptance implies `check` in
//!   `verification/semantics-v1/NornSem/Exchange.lean`, whose `check_sound`
//!   proves preservation for every source instance over every domain.
//! - [`certify`] is untrusted search. For each way the target body can be
//!   produced, it chases the canonical source instance and records the steps.
//!   Its output counts only once [`check`] accepts it.
//!
//! The bounded counterexample search stays in place for refutation, and for
//! every obligation this module cannot certify.
//!
//! The search is bounded twice. Its own limits (`MAX_CASES`, `MAX_STEPS`,
//! `MAX_ATOMS`, counted before anything is enumerated) make it give up, and the
//! caller falls back to the bounded search. The work is also charged to the
//! admission's `AdmissionWork` before it is done, in proportion to the loops
//! that do it: counting and enumerating the choices, setting up each case,
//! every match attempt and every chase step. Setup and steps are charged
//! twice, because the proved `check` repeats them and cannot charge. Exhausting
//! the allowance refuses the admission, as it does in the search.

mod check;

use std::collections::BTreeMap;

use super::chase::{AdmissionWork, ChaseError};
use super::theory::{Atom as NamedAtom, Dependency, Term, Theory};
use check::check;
use check::{
    Atom, Dep, Rule, State, Step, apply, case_choices, contains_var, initial, rep_of, value_at,
    width,
};

/// Search limits for [`certify`]; exceeding one gives up (never refuses).
const MAX_CASES: usize = 64;
const MAX_STEPS: usize = 512;
const MAX_ATOMS: usize = 2048;
const MAX_MATCHES: usize = 4096;

/// The number of rule choices `case_choices` would enumerate: `None` above
/// `MAX_CASES`, and `Some(0)` as soon as one atom has no producing rule.
/// Nothing is built, and no atom after a zero or an overflow is looked at.
fn case_count(rules: &[Rule], body: &[Atom]) -> Option<usize> {
    let mut count = 1_usize;
    for atom in body {
        let producers = rules
            .iter()
            .filter(|rule| rule.head.rel == atom.rel && rule.head.args.len() == atom.args.len())
            .count();
        if producers == 0 {
            return Some(0);
        }
        count = count.checked_mul(producers)?;
        if count > MAX_CASES {
            return None;
        }
    }
    Some(count)
}

/// Atoms plus arguments: what one pass over `atoms` costs.
fn span(atoms: &[Atom]) -> usize {
    atoms
        .iter()
        .fold(atoms.len(), |sum, atom| sum.saturating_add(atom.args.len()))
}

/// The rule choices of `body`, enumerated only once their count is known to
/// be between 1 and `MAX_CASES`; `Ok(None)` otherwise (the caller gives up).
/// This is the module's only call of `case_choices`. The count, and twice the
/// enumeration (`check` repeats it), are charged first.
fn bounded_choices(
    rules: &[Rule],
    body: &[Atom],
    work: &AdmissionWork,
) -> Result<Option<Vec<Vec<usize>>>, ChaseError> {
    work.charge_units(body.len().saturating_mul(rules.len()))?;
    let count = match case_count(rules, body) {
        Some(count) if count > 0 => count,
        _ => return Ok(None),
    };
    let enumeration = count.saturating_mul(body.len().saturating_add(1));
    work.charge_units(
        enumeration
            .saturating_add(body.len().saturating_mul(rules.len()))
            .saturating_mul(2),
    )?;
    Ok(Some(case_choices(rules, body)))
}

/// What `initial` does for one choice, in loop iterations: it measures the
/// body and every rule, builds `nb + n * w` classes, copies the chosen
/// bodies, and copies all classes once per merged head argument.
fn setup_cost(rules: &[Rule], body: &[Atom], choice: &[usize]) -> usize {
    let most = |atoms: &[Atom]| {
        atoms
            .iter()
            .flat_map(|atom| atom.args.iter().copied())
            .max()
            .unwrap_or(0)
    };
    let mut measured = span(body);
    let mut w = 1_usize;
    for rule in rules {
        measured = measured
            .saturating_add(span(&rule.body))
            .saturating_add(rule.head.args.len());
        let widest = most(&rule.body).max(rule.head.args.iter().copied().max().unwrap_or(0));
        w = w.max(widest.saturating_add(1));
    }
    let size = most(body)
        .saturating_add(1)
        .saturating_add(choice.len().saturating_mul(w));
    let mut copied = 0_usize;
    let mut merges = 0_usize;
    for (index, target) in choice.iter().zip(body) {
        if let Some(rule) = rules.get(*index) {
            copied = copied.saturating_add(span(&rule.body));
            merges = merges.saturating_add(rule.head.args.len().min(target.args.len()));
        }
    }
    measured
        .saturating_add(size)
        .saturating_add(copied)
        .saturating_add(merges.saturating_mul(size))
}

/// What `apply` does for one step, in loop iterations: it looks every body
/// atom up in the state, then merges all classes (EGD) or builds the head
/// (TGD).
fn step_cost(theory: &[Dep], state: &State, step: &Step) -> usize {
    let (body, head) = match theory.get(step.dep) {
        Some(Dep::Egd { body, .. }) => (span(body), 0),
        Some(Dep::Tgd { body, head }) => (span(body), span(head)),
        None => (0, 0),
    };
    let facts = state
        .atoms
        .len()
        .saturating_add(1)
        .saturating_mul(body.saturating_add(1));
    facts
        .saturating_add(head.saturating_mul(body.saturating_add(1)))
        .saturating_add(state.rep.len())
}

/// Every assignment of `body`'s variables to elements that matches it in
/// `state`, up to `MAX_MATCHES`. Each fact tried is charged to `work`.
fn matches(
    state: &State,
    body: &[Atom],
    work: &AdmissionWork,
) -> Result<Vec<Vec<usize>>, ChaseError> {
    fn go(
        state: &State,
        body: &[Atom],
        asg: &mut Vec<Option<usize>>,
        out: &mut Vec<Vec<usize>>,
        work: &AdmissionWork,
    ) -> Result<(), ChaseError> {
        if out.len() >= MAX_MATCHES {
            return Ok(());
        }
        let Some((atom, rest)) = body.split_first() else {
            work.charge_units(asg.len())?;
            out.push(asg.iter().map(|e| e.unwrap_or(0)).collect());
            return Ok(());
        };
        for fact in &state.atoms {
            work.charge()?;
            let es = &fact.args;
            if fact.rel != atom.rel || es.len() != atom.args.len() {
                continue;
            }
            // Saving and restoring the assignment, and comparing the arguments.
            work.charge_units(asg.len().saturating_mul(2).saturating_add(es.len()))?;
            let saved = asg.clone();
            let mut ok = true;
            for (v, e) in atom.args.iter().zip(es) {
                let Some(slot) = asg.get_mut(*v) else {
                    ok = false;
                    break;
                };
                match slot {
                    Some(bound) if rep_of(&state.rep, *bound) != rep_of(&state.rep, *e) => {
                        ok = false;
                        break;
                    }
                    Some(_) => {}
                    None => *slot = Some(*e),
                }
            }
            if ok {
                go(state, rest, asg, out, work)?;
            }
            *asg = saved;
        }
        Ok(())
    }
    let size = width(body).unwrap_or(1);
    let mut out = Vec::new();
    go(state, body, &mut vec![None; size], &mut out, work)?;
    Ok(out)
}

/// Whether a TGD's head already holds for some extension of `asg`.
fn head_holds(
    state: &State,
    body: &[Atom],
    head: &[Atom],
    asg: &[usize],
    work: &AdmissionWork,
) -> Result<bool, ChaseError> {
    work.charge_units(span(body).saturating_add(span(head)))?;
    let mut all = body.to_vec();
    all.extend(head.iter().cloned());
    Ok(matches(state, &all, work)?.iter().any(|found| {
        body.iter().flat_map(|atom| atom.args.iter()).all(|v| {
            rep_of(&state.rep, value_at(found, *v)) == rep_of(&state.rep, value_at(asg, *v))
        })
    }))
}

/// The next step the chase can take from `state`, if any.
fn next_step(
    theory: &[Dep],
    state: &State,
    work: &AdmissionWork,
) -> Result<Option<Step>, ChaseError> {
    for (index, dep) in theory.iter().enumerate() {
        match dep {
            Dep::Egd { body, left, right } => {
                for asg in matches(state, body, work)? {
                    if rep_of(&state.rep, value_at(&asg, *left))
                        != rep_of(&state.rep, value_at(&asg, *right))
                    {
                        return Ok(Some(Step { dep: index, asg }));
                    }
                }
            }
            Dep::Tgd { body, head } => {
                for asg in matches(state, body, work)? {
                    if !head_holds(state, body, head, &asg, work)? {
                        return Ok(Some(Step { dep: index, asg }));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Chase one case until `left` and `right` merge, recording each step.
/// `Ok(None)` gives up; `Err` is exhausted admission work.
fn chase_case(
    theory: &[Dep],
    mut state: State,
    left: usize,
    right: usize,
    work: &AdmissionWork,
) -> Result<Option<Vec<Step>>, ChaseError> {
    let mut trace = Vec::new();
    while rep_of(&state.rep, left) != rep_of(&state.rep, right) {
        if trace.len() >= MAX_STEPS || state.atoms.len() >= MAX_ATOMS {
            return Ok(None);
        }
        let Some(step) = next_step(theory, &state, work)? else {
            return Ok(None);
        };
        // The step is applied here and once more in `check`.
        work.charge_units(step_cost(theory, &state, &step).saturating_mul(2))?;
        let Some(next) = apply(theory, state, &step) else {
            return Ok(None);
        };
        state = next;
        trace.push(step);
    }
    Ok(Some(trace))
}

/// Untrusted search: a certificate for the target EGD, or `Ok(None)` when the
/// search gives up. Its output still has to pass [`check`].
///
/// # Errors
/// The admission's work allowance is exhausted.
pub(crate) fn certify(
    theory: &[Dep],
    rules: &[Rule],
    body: &[Atom],
    left: usize,
    right: usize,
    work: &AdmissionWork,
) -> Result<Option<Vec<Vec<Step>>>, ChaseError> {
    // Give up rather than enumerate no choices or more than the limit.
    let Some(choices) = bounded_choices(rules, body, work)? else {
        return Ok(None);
    };
    let mut traces = Vec::new();
    for choice in choices {
        // The case is set up here and once more in `check`.
        work.charge_units(setup_cost(rules, body, &choice).saturating_mul(2))?;
        let Some(state) = initial(rules, body, &choice) else {
            return Ok(None);
        };
        let Some(trace) = chase_case(theory, state, left, right, work)? else {
            return Ok(None);
        };
        traces.push(trace);
    }
    Ok(Some(traces))
}

/// Interns relation names and each dependency's variables.
#[derive(Default)]
struct Interner {
    relations: BTreeMap<String, usize>,
}

impl Interner {
    fn relation(&mut self, name: &str) -> usize {
        let next = self.relations.len();
        *self.relations.entry(name.to_owned()).or_insert(next)
    }

    /// Lower atoms, numbering variables in order of first appearance.
    fn atoms(
        &mut self,
        atoms: &[NamedAtom],
        variables: &mut BTreeMap<String, usize>,
    ) -> Option<Vec<Atom>> {
        atoms
            .iter()
            .map(|atom| {
                let rel = self.relation(&atom.relation);
                let args = atom
                    .terms
                    .iter()
                    .map(|term| match term {
                        Term::Var(name) => {
                            let next = variables.len();
                            Some(*variables.entry(name.clone()).or_insert(next))
                        }
                        Term::Const(_) => None,
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(Atom { rel, args })
            })
            .collect()
    }

    fn var(variables: &BTreeMap<String, usize>, term: &Term) -> Option<usize> {
        match term {
            Term::Var(name) => variables.get(name).copied(),
            Term::Const(_) => None,
        }
    }

    fn dependency(&mut self, dependency: &Dependency) -> Option<Dep> {
        let mut variables = BTreeMap::new();
        match dependency {
            Dependency::Tgd(tgd) => {
                let body = self.atoms(&tgd.body, &mut variables)?;
                let head = self.atoms(&tgd.head, &mut variables)?;
                Some(Dep::Tgd { body, head })
            }
            Dependency::Egd(egd) => {
                let body = self.atoms(&egd.body, &mut variables)?;
                let left = Self::var(&variables, &egd.left)?;
                let right = Self::var(&variables, &egd.right)?;
                Some(Dep::Egd { body, left, right })
            }
        }
    }

    fn rule(&mut self, dependency: &Dependency) -> Option<Rule> {
        let Dependency::Tgd(tgd) = dependency else {
            return None;
        };
        let mut variables = BTreeMap::new();
        let body = self.atoms(&tgd.body, &mut variables)?;
        let [head] = tgd.head.as_slice() else {
            return None;
        };
        let head = self.atoms(std::slice::from_ref(head), &mut variables)?;
        let head = head.into_iter().next()?;
        // A mapping rule is full: every head variable occurs in the body.
        if head.args.iter().any(|v| !contains_var(&body, *v)) {
            return None;
        }
        Some(Rule { body, head })
    }
}

/// Certify every dependency of `target` (all EGDs) under `source` and the
/// mapping rules `mapping`: `true` only when every certificate passes the
/// proved [`check`]. Anything this module does not model (constants, target
/// TGDs, non-full mapping rules, search limits) returns `false`, and the caller
/// keeps its bounded evidence.
///
/// # Errors
/// The admission's work allowance is exhausted; the admission refuses.
pub(crate) fn certified(
    source: &Theory,
    mapping: &Theory,
    target: &Theory,
    work: &AdmissionWork,
) -> Result<bool, ChaseError> {
    let mut interner = Interner::default();
    let Some(theory) = source
        .dependencies
        .iter()
        .map(|d| interner.dependency(d))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(false);
    };
    let Some(rules) = mapping
        .dependencies
        .iter()
        .map(|d| interner.rule(d))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(false);
    };
    for dependency in &target.dependencies {
        let Some(Dep::Egd { body, left, right }) = interner.dependency(dependency) else {
            return Ok(false);
        };
        let Some(traces) = certify(&theory, &rules, &body, left, right, work)? else {
            return Ok(false);
        };
        if !check(&theory, &rules, &body, left, right, &traces) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{AdmissionWork, Atom, MAX_CASES, Rule, case_count, certify};

    fn atom(rel: usize, args: &[usize]) -> Atom {
        Atom {
            rel,
            args: args.to_vec(),
        }
    }

    /// Two rules, each producing relation 1 from relation 0.
    fn doubled() -> [Rule; 2] {
        let rule = || Rule {
            body: vec![atom(0, &[0, 1])],
            head: atom(1, &[0, 1]),
        };
        [rule(), rule()]
    }

    /// A chain of `n` atoms of relation 1.
    fn chain(n: usize) -> Vec<Atom> {
        (0..n).map(|v| atom(1, &[v, v.saturating_add(1)])).collect()
    }

    /// Two rules produce each of 32 body atoms: 2^32 choices. The count
    /// exceeds the limit, and `certify` gives up having been charged only for
    /// the count (32 atoms by 2 rules): enumerating even one choice would
    /// charge more and fail instead.
    #[test]
    fn a_choice_product_beyond_the_limit_is_never_enumerated() {
        let rules = doubled();
        assert_eq!(case_count(&rules, &chain(32)), None);
        assert_eq!(case_count(&rules, &chain(6)), Some(MAX_CASES));
        assert_eq!(case_count(&rules, &chain(7)), None);
        let counting_only = AdmissionWork::new(64);
        assert_eq!(
            certify(&[], &rules, &chain(32), 0, 32, &counting_only),
            Ok(None)
        );
        // Within the limit the choices are enumerated, which is charged.
        assert!(certify(&[], &rules, &chain(1), 0, 1, &AdmissionWork::new(2)).is_err());
    }

    /// A first atom nothing produces makes the product zero while the other
    /// 32 atoms still have 2^32 choices among them. The count stops at the
    /// zero and `certify` gives up without enumerating, charged only for the
    /// count (33 atoms by 2 rules).
    #[test]
    fn a_zero_choice_product_is_never_enumerated() {
        let rules = doubled();
        let mut body = vec![atom(9, &[0, 1])];
        body.extend(chain(32));
        assert_eq!(case_count(&rules, &body), Some(0));
        let counting_only = AdmissionWork::new(66);
        assert_eq!(certify(&[], &rules, &body, 0, 32, &counting_only), Ok(None));
    }
}
