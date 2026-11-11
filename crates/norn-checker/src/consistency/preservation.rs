//! U7 (#718): chase certificates for path-mapping preservation.
//!
//! A path mapping lowers to mapping rules (full TGDs, one head atom each)
//! that pull source data into the target. It preserves a target EGD when
//! every source instance satisfying the source theory pulls back to an
//! instance satisfying that EGD.
//!
//! - [`check`] is the trusted decision. It mirrors `check` in
//!   `verification/semantics-v1/NornSem/Exchange.lean`, whose `check_sound`
//!   proves that acceptance implies preservation for every source instance
//!   over every domain.
//! - [`certify`] is untrusted search. For each way the target body can be
//!   produced, it chases the canonical source instance and records the steps.
//!   Its output counts only once [`check`] accepts it.
//!
//! The bounded counterexample search stays in place for refutation, and for
//! every obligation this module cannot certify.

use std::collections::BTreeMap;

use super::theory::{Atom as NamedAtom, Dependency, Term, Theory};

/// An atom over variables or elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Atom {
    pub(crate) rel: usize,
    pub(crate) args: Vec<usize>,
}

/// A dependency over variables `0, 1, …` (body variables first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dep {
    Tgd {
        body: Vec<Atom>,
        head: Vec<Atom>,
    },
    Egd {
        body: Vec<Atom>,
        left: usize,
        right: usize,
    },
}

/// A mapping rule: a full TGD with one head atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    pub(crate) body: Vec<Atom>,
    pub(crate) head: Atom,
}

/// One chase step: theory dependency `dep` at assignment `asg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) dep: usize,
    pub(crate) asg: Vec<usize>,
}

/// A chase state over elements `0 … next - 1`. `rep[x]` is the class of
/// element `x` (an element at or beyond `rep.len()` is its own class).
#[derive(Debug, Clone)]
struct State {
    atoms: Vec<(usize, Vec<usize>)>,
    rep: Vec<usize>,
}

impl State {
    fn next(&self) -> usize {
        self.rep.len()
    }

    fn rep_of(&self, x: usize) -> usize {
        self.rep.get(x).copied().unwrap_or(x)
    }

    fn same(&self, xs: &[usize], ys: &[usize]) -> bool {
        xs.len() == ys.len()
            && xs
                .iter()
                .zip(ys)
                .all(|(x, y)| self.rep_of(*x) == self.rep_of(*y))
    }

    fn has(&self, rel: usize, es: &[usize]) -> bool {
        self.atoms
            .iter()
            .any(|(r, xs)| *r == rel && self.same(xs, es))
    }

    fn merge(&mut self, a: usize, b: usize) {
        let into = self.rep_of(a);
        let from = self.rep_of(b);
        for class in &mut self.rep {
            if *class == from {
                *class = into;
            }
        }
    }
}

fn vars(atoms: &[Atom]) -> impl Iterator<Item = usize> + '_ {
    atoms.iter().flat_map(|atom| atom.args.iter().copied())
}

/// One more than the largest variable (1 for none).
fn width(atoms: &[Atom]) -> Option<usize> {
    vars(atoms).max().unwrap_or(0).checked_add(1)
}

fn rule_width(rules: &[Rule]) -> Option<usize> {
    let mut widest = 1;
    for rule in rules {
        let mut all = Vec::with_capacity(rule.body.len().saturating_add(1));
        all.push(rule.head.clone());
        all.extend(rule.body.iter().cloned());
        widest = widest.max(width(&all)?);
    }
    Some(widest)
}

fn at(asg: &[usize], v: usize) -> usize {
    asg.get(v).copied().unwrap_or(0)
}

fn body_matches(state: &State, body: &[Atom], asg: &[usize]) -> bool {
    vars(body).all(|v| at(asg, v) < state.next())
        && body.iter().all(|atom| {
            let es: Vec<usize> = atom.args.iter().map(|v| at(asg, *v)).collect();
            state.has(atom.rel, &es)
        })
}

/// Apply one step, or refuse (`None`).
fn apply(theory: &[Dep], state: &State, step: &Step) -> Option<State> {
    match theory.get(step.dep)? {
        Dep::Egd { body, left, right } => {
            let (a, b) = (at(&step.asg, *left), at(&step.asg, *right));
            if !body_matches(state, body, &step.asg) || a >= state.next() || b >= state.next() {
                return None;
            }
            let mut next = state.clone();
            next.merge(a, b);
            Some(next)
        }
        Dep::Tgd { body, head } => {
            if !body_matches(state, body, &step.asg) {
                return None;
            }
            let base = state.next();
            let body_vars: Vec<usize> = vars(body).collect();
            let element = |v: usize| {
                if body_vars.contains(&v) {
                    Some(at(&step.asg, v))
                } else {
                    base.checked_add(v)
                }
            };
            let mut next = state.clone();
            for atom in head {
                let es = atom
                    .args
                    .iter()
                    .map(|v| element(*v))
                    .collect::<Option<Vec<_>>>()?;
                next.atoms.push((atom.rel, es));
            }
            let fresh = base.checked_add(width(head)?)?;
            next.rep.extend(base..fresh);
            Some(next)
        }
    }
}

fn run(theory: &[Dep], mut state: State, trace: &[Step]) -> Option<State> {
    for step in trace {
        state = apply(theory, &state, step)?;
    }
    Some(state)
}

/// Every rule choice whose heads have the body atoms' relations and arities,
/// first atom outermost (`cases` in the Lean specification).
fn cases(rules: &[Rule], body: &[Atom]) -> Vec<Vec<usize>> {
    let Some((first, rest)) = body.split_first() else {
        return vec![Vec::new()];
    };
    let tails = cases(rules, rest);
    let mut out = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        if rule.head.rel == first.rel && rule.head.args.len() == first.args.len() {
            for tail in &tails {
                let mut choice = Vec::with_capacity(tail.len().saturating_add(1));
                choice.push(index);
                choice.extend(tail.iter().copied());
                out.push(choice);
            }
        }
    }
    out
}

fn case_elem(nb: usize, w: usize, i: usize, v: usize) -> Option<usize> {
    nb.checked_add(i.checked_mul(w)?)?.checked_add(v)
}

/// The canonical state of one case: the chosen rules' bodies kept apart,
/// each head merged into its target atom.
fn initial(rules: &[Rule], body: &[Atom], choice: &[usize]) -> Option<State> {
    let nb = width(body)?;
    let w = rule_width(rules)?;
    let next = nb.checked_add(choice.len().checked_mul(w)?)?;
    let mut state = State {
        atoms: Vec::new(),
        rep: (0..next).collect(),
    };
    for (i, index) in choice.iter().enumerate() {
        let rule = rules.get(*index)?;
        for atom in &rule.body {
            let es = atom
                .args
                .iter()
                .map(|v| case_elem(nb, w, i, *v))
                .collect::<Option<Vec<_>>>()?;
            state.atoms.push((atom.rel, es));
        }
    }
    for (i, index) in choice.iter().enumerate() {
        let (rule, target) = (rules.get(*index)?, body.get(i)?);
        for (hv, bv) in rule.head.args.iter().zip(&target.args) {
            state.merge(case_elem(nb, w, i, *hv)?, *bv);
        }
    }
    Some(state)
}

/// The trusted certificate check (`NornSem.Exchange.check`): one trace per
/// case, each ending with the target EGD's two variables merged.
pub(crate) fn check(
    theory: &[Dep],
    rules: &[Rule],
    body: &[Atom],
    left: usize,
    right: usize,
    traces: &[Vec<Step>],
) -> bool {
    let cs = cases(rules, body);
    vars(body).any(|v| v == left)
        && vars(body).any(|v| v == right)
        && cs.len() == traces.len()
        && cs.iter().zip(traces).all(|(choice, trace)| {
            initial(rules, body, choice)
                .and_then(|state| run(theory, state, trace))
                .is_some_and(|state| state.rep_of(left) == state.rep_of(right))
        })
}

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
        for (rel, es) in &state.atoms {
            if *rel != atom.rel || es.len() != atom.args.len() {
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
                    Some(bound) if state.rep_of(*bound) != state.rep_of(*e) => {
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
    let body_vars: Vec<usize> = vars(body).collect();
    let mut all = body.to_vec();
    all.extend(head.iter().cloned());
    matches(state, &all, 4096).iter().any(|found| {
        body_vars
            .iter()
            .all(|v| state.rep_of(at(found, *v)) == state.rep_of(at(asg, *v)))
    })
}

/// Chase one case until `left` and `right` merge, recording each step.
fn chase_case(theory: &[Dep], mut state: State, left: usize, right: usize) -> Option<Vec<Step>> {
    let mut trace = Vec::new();
    while state.rep_of(left) != state.rep_of(right) {
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
                        if state.rep_of(at(&asg, *l)) != state.rep_of(at(&asg, *r)) {
                            let step = Step { dep: index, asg };
                            state = apply(theory, &state, &step)?;
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
                            state = apply(theory, &state, &step)?;
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
    let cs = cases(rules, body);
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
        if head.args.iter().any(|v| !vars(&body).any(|w| w == *v)) {
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
