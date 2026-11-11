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

mod check;

use std::collections::BTreeMap;

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

/// Every assignment of `body`'s variables to elements that matches it in
/// `state`, up to `limit`.
fn matches(state: &State, body: &[Atom], limit: usize) -> Vec<Vec<usize>> {
    fn go(
        state: &State,
        body: &[Atom],
        asg: &mut Vec<Option<usize>>,
        out: &mut Vec<Vec<usize>>,
        limit: usize,
    ) {
        if out.len() >= limit {
            return;
        }
        let Some((atom, rest)) = body.split_first() else {
            out.push(asg.iter().map(|e| e.unwrap_or(0)).collect());
            return;
        };
        for fact in &state.atoms {
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
                go(state, rest, asg, out, limit);
            }
            *asg = saved;
        }
    }
    let size = width(body).unwrap_or(1);
    let mut out = Vec::new();
    go(state, body, &mut vec![None; size], &mut out, limit);
    out
}

/// Whether a TGD's head already holds for some extension of `asg`.
fn head_holds(state: &State, body: &[Atom], head: &[Atom], asg: &[usize]) -> bool {
    let mut all = body.to_vec();
    all.extend(head.iter().cloned());
    matches(state, &all, 4096).iter().any(|found| {
        body.iter().flat_map(|atom| atom.args.iter()).all(|v| {
            rep_of(&state.rep, value_at(found, *v)) == rep_of(&state.rep, value_at(asg, *v))
        })
    })
}

/// Chase one case until `left` and `right` merge, recording each step.
fn chase_case(theory: &[Dep], mut state: State, left: usize, right: usize) -> Option<Vec<Step>> {
    let mut trace = Vec::new();
    while rep_of(&state.rep, left) != rep_of(&state.rep, right) {
        if trace.len() >= MAX_STEPS || state.atoms.len() >= MAX_ATOMS {
            return None;
        }
        let mut progressed = false;
        'deps: for (index, dep) in theory.iter().enumerate() {
            match dep {
                Dep::Egd {
                    body,
                    left: l,
                    right: r,
                } => {
                    for asg in matches(&state, body, 4096) {
                        if rep_of(&state.rep, value_at(&asg, *l))
                            != rep_of(&state.rep, value_at(&asg, *r))
                        {
                            let step = Step { dep: index, asg };
                            state = apply(theory, state, &step)?;
                            trace.push(step);
                            progressed = true;
                            break 'deps;
                        }
                    }
                }
                Dep::Tgd { body, head } => {
                    for asg in matches(&state, body, 4096) {
                        if !head_holds(&state, body, head, &asg) {
                            let step = Step { dep: index, asg };
                            state = apply(theory, state, &step)?;
                            trace.push(step);
                            progressed = true;
                            break 'deps;
                        }
                    }
                }
            }
        }
        if !progressed {
            return None;
        }
    }
    Some(trace)
}

/// Untrusted search: a certificate for the target EGD, or `None` when the
/// search gives up. Its output still has to pass [`check`].
pub(crate) fn certify(
    theory: &[Dep],
    rules: &[Rule],
    body: &[Atom],
    left: usize,
    right: usize,
) -> Option<Vec<Vec<Step>>> {
    let cs = case_choices(rules, body);
    if cs.len() > MAX_CASES {
        return None;
    }
    cs.iter()
        .map(|choice| chase_case(theory, initial(rules, body, choice)?, left, right))
        .collect()
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
/// trusted [`check`]. Anything this module does not model (constants,
/// target TGDs, non-full mapping rules, search limits) returns `false`, and
/// the caller keeps its bounded evidence.
pub(crate) fn certified(source: &Theory, mapping: &Theory, target: &Theory) -> bool {
    let mut interner = Interner::default();
    let Some(theory) = source
        .dependencies
        .iter()
        .map(|d| interner.dependency(d))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let Some(rules) = mapping
        .dependencies
        .iter()
        .map(|d| interner.rule(d))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    target.dependencies.iter().all(|dependency| {
        let Some(Dep::Egd { body, left, right }) = interner.dependency(dependency) else {
            return false;
        };
        certify(&theory, &rules, &body, left, right)
            .is_some_and(|traces| check(&theory, &rules, &body, left, right, &traces))
    })
}
