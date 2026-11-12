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
//! caller falls back to the bounded search. Every match attempt and chase step
//! is also charged to the admission's `AdmissionWork`; exhausting that refuses
//! the admission, as it does in the search.

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

/// The number of rule choices `case_choices` would enumerate, or `None` when
/// it exceeds `MAX_CASES`. Counted without building any choice.
fn case_count(rules: &[Rule], body: &[Atom]) -> Option<usize> {
    let mut count = 1_usize;
    for atom in body {
        let producers = rules
            .iter()
            .filter(|rule| rule.head.rel == atom.rel && rule.head.args.len() == atom.args.len())
            .count();
        count = count.checked_mul(producers)?;
        if count > MAX_CASES {
            return None;
        }
    }
    Some(count)
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
            out.push(asg.iter().map(|e| e.unwrap_or(0)).collect());
            return Ok(());
        };
        for fact in &state.atoms {
            work.charge()?;
            let es = &fact.args;
            if fact.rel != atom.rel || es.len() != atom.args.len() {
                continue;
            }
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
        // Applying the step here, and once more in `check`, each scan the state.
        work.charge_units(state.atoms.len().saturating_mul(2).saturating_add(2))?;
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
    // Refuse to enumerate a product of choices beyond the limit.
    if case_count(rules, body).is_none() {
        return Ok(None);
    }
    let mut traces = Vec::new();
    for choice in case_choices(rules, body) {
        work.charge_units(choice.len().saturating_add(1))?;
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
