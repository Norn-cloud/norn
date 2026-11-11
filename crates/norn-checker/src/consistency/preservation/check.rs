//! The trusted chase-certificate check for path-mapping preservation (#718
//! U7b): the proved core of `preservation`. It mirrors `check` in
//! `verification/semantics-v1/NornSem/Exchange.lean` loop by loop, uses only
//! `core` and `Vec`, and indexes with `get`, so Charon/Aeneas extract this file
//! standalone and `Checker/Preservation/Proof.lean` proves that acceptance
//! implies the Lean check's, hence preservation.
//!
//! Every helper refuses (`None` or `false`) rather than panics when an index
//! is missing or arithmetic overflows; a refusal only withholds `certified`.

/// An atom over variables, or (in a state) over elements.
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

/// A chase state over elements `0 … rep.len() - 1`. `rep[x]` is the class of
/// element `x`; an element at or beyond `rep.len()` is its own class.
#[derive(Debug, Clone)]
pub(crate) struct State {
    pub(crate) atoms: Vec<Atom>,
    pub(crate) rep: Vec<usize>,
}

/// The class of element `x`.
pub(crate) fn rep_of(rep: &[usize], x: usize) -> usize {
    match rep.get(x) {
        Some(entry) => *entry,
        None => x,
    }
}

/// The two element lists have one length and equal classes pointwise.
fn same(rep: &[usize], xs: &[usize], ys: &[usize]) -> bool {
    if xs.len() != ys.len() {
        return false;
    }
    let mut index = 0_usize;
    while let Some(x) = xs.get(index) {
        let Some(y) = ys.get(index) else {
            return false;
        };
        if rep_of(rep, *x) != rep_of(rep, *y) {
            return false;
        }
        let Some(next) = index.checked_add(1) else {
            return false;
        };
        index = next;
    }
    true
}

/// The state holds an atom of relation `rel` with elements `es`, up to classes.
fn has(state: &State, rel: usize, es: &[usize]) -> bool {
    let mut index = 0_usize;
    while let Some(atom) = state.atoms.get(index) {
        if atom.rel == rel && same(&state.rep, &atom.args, es) {
            return true;
        }
        let Some(next) = index.checked_add(1) else {
            return false;
        };
        index = next;
    }
    false
}

/// `rep` with the class of `b` merged into the class of `a`.
fn merged(rep: &[usize], a: usize, b: usize) -> Vec<usize> {
    let into_class = rep_of(rep, a);
    let from_class = rep_of(rep, b);
    let mut out = Vec::new();
    let mut index = 0_usize;
    while let Some(entry) = rep.get(index) {
        if *entry == from_class {
            out.push(into_class);
        } else {
            out.push(*entry);
        }
        let Some(next) = index.checked_add(1) else {
            return out;
        };
        index = next;
    }
    out
}

/// The largest of `acc` and `args`.
fn max_arg(args: &[usize], acc: usize) -> usize {
    let mut most = acc;
    let mut index = 0_usize;
    while let Some(v) = args.get(index) {
        if *v > most {
            most = *v;
        }
        let Some(next) = index.checked_add(1) else {
            return most;
        };
        index = next;
    }
    most
}

/// The largest of `acc` and every variable of `atoms`.
fn max_var(atoms: &[Atom], acc: usize) -> usize {
    let mut most = acc;
    let mut index = 0_usize;
    while let Some(atom) = atoms.get(index) {
        most = max_arg(&atom.args, most);
        let Some(next) = index.checked_add(1) else {
            return most;
        };
        index = next;
    }
    most
}

/// One more than the largest variable (1 for none).
pub(crate) fn width(atoms: &[Atom]) -> Option<usize> {
    max_var(atoms, 0).checked_add(1)
}

/// `v` occurs in `args`.
fn contains(args: &[usize], v: usize) -> bool {
    let mut index = 0_usize;
    while let Some(w) = args.get(index) {
        if *w == v {
            return true;
        }
        let Some(next) = index.checked_add(1) else {
            return false;
        };
        index = next;
    }
    false
}

/// `v` is a variable of `atoms`.
pub(crate) fn contains_var(atoms: &[Atom], v: usize) -> bool {
    let mut index = 0_usize;
    while let Some(atom) = atoms.get(index) {
        if contains(&atom.args, v) {
            return true;
        }
        let Some(next) = index.checked_add(1) else {
            return false;
        };
        index = next;
    }
    false
}

/// The value of variable `v` under `asg` (`0` when missing).
pub(crate) fn value_at(asg: &[usize], v: usize) -> usize {
    match asg.get(v) {
        Some(e) => *e,
        None => 0,
    }
}

/// Every variable of `args` is assigned an element below `next`.
fn args_below(args: &[usize], asg: &[usize], next: usize) -> bool {
    let mut index = 0_usize;
    while let Some(v) = args.get(index) {
        if value_at(asg, *v) >= next {
            return false;
        }
        let Some(after) = index.checked_add(1) else {
            return false;
        };
        index = after;
    }
    true
}

/// Every variable of `atoms` is assigned an element below `next`.
fn vars_below(atoms: &[Atom], asg: &[usize], next: usize) -> bool {
    let mut index = 0_usize;
    while let Some(atom) = atoms.get(index) {
        if !args_below(&atom.args, asg, next) {
            return false;
        }
        let Some(after) = index.checked_add(1) else {
            return false;
        };
        index = after;
    }
    true
}

/// The elements `asg` assigns to `args`.
fn elements(args: &[usize], asg: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut index = 0_usize;
    while let Some(v) = args.get(index) {
        out.push(value_at(asg, *v));
        let Some(next) = index.checked_add(1) else {
            return out;
        };
        index = next;
    }
    out
}

/// Every atom of `body` is in the state under `asg`.
fn atoms_held(state: &State, body: &[Atom], asg: &[usize]) -> bool {
    let mut index = 0_usize;
    while let Some(atom) = body.get(index) {
        if !has(state, atom.rel, &elements(&atom.args, asg)) {
            return false;
        }
        let Some(next) = index.checked_add(1) else {
            return false;
        };
        index = next;
    }
    true
}

/// `asg` maps every body variable to an existing element, and every body atom
/// is in the state.
fn body_matches(state: &State, body: &[Atom], asg: &[usize]) -> bool {
    vars_below(body, asg, state.rep.len()) && atoms_held(state, body, asg)
}

/// A TGD step's elements for head arguments: the assignment's on a body
/// variable, the fresh `base + v` otherwise.
fn head_args(body: &[Atom], asg: &[usize], base: usize, args: &[usize]) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    let mut index = 0_usize;
    while let Some(v) = args.get(index) {
        if contains_var(body, *v) {
            out.push(value_at(asg, *v));
        } else {
            let Some(fresh) = base.checked_add(*v) else {
                return None;
            };
            out.push(fresh);
        }
        let Some(next) = index.checked_add(1) else {
            return None;
        };
        index = next;
    }
    Some(out)
}

/// `atoms` followed by every head atom of a TGD step.
fn with_heads(
    atoms: Vec<Atom>,
    body: &[Atom],
    head: &[Atom],
    asg: &[usize],
    base: usize,
) -> Option<Vec<Atom>> {
    let mut out = atoms;
    let mut index = 0_usize;
    while let Some(atom) = head.get(index) {
        let Some(args) = head_args(body, asg, base, &atom.args) else {
            return None;
        };
        out.push(Atom {
            rel: atom.rel,
            args,
        });
        let Some(next) = index.checked_add(1) else {
            return None;
        };
        index = next;
    }
    Some(out)
}

/// `rep` extended with fresh singleton classes up to `size` elements.
fn extended(rep: Vec<usize>, size: usize) -> Vec<usize> {
    let mut out = rep;
    while out.len() < size {
        let fresh = out.len();
        out.push(fresh);
    }
    out
}

/// Apply one step, or refuse (`None`).
pub(crate) fn apply(theory: &[Dep], state: State, step: &Step) -> Option<State> {
    match theory.get(step.dep) {
        Some(Dep::Egd { body, left, right }) => {
            let a = value_at(&step.asg, *left);
            let b = value_at(&step.asg, *right);
            let next = state.rep.len();
            if body_matches(&state, body, &step.asg) && a < next && b < next {
                let rep = merged(&state.rep, a, b);
                Some(State {
                    atoms: state.atoms,
                    rep,
                })
            } else {
                None
            }
        }
        Some(Dep::Tgd { body, head }) => {
            if !body_matches(&state, body, &step.asg) {
                return None;
            }
            let base = state.rep.len();
            let Some(w) = width(head) else {
                return None;
            };
            let Some(size) = base.checked_add(w) else {
                return None;
            };
            let Some(atoms) = with_heads(state.atoms, body, head, &step.asg, base) else {
                return None;
            };
            Some(State {
                atoms,
                rep: extended(state.rep, size),
            })
        }
        None => None,
    }
}

/// Run a trace.
pub(crate) fn run(theory: &[Dep], state: State, trace: &[Step]) -> Option<State> {
    let mut state = state;
    let mut index = 0_usize;
    while let Some(step) = trace.get(index) {
        match apply(theory, state, step) {
            Some(next) => state = next,
            None => return None,
        }
        let Some(next) = index.checked_add(1) else {
            return None;
        };
        index = next;
    }
    Some(state)
}

/// One more than the largest variable of a rule's head and body.
fn width_of(rule: &Rule) -> Option<usize> {
    max_var(&rule.body, max_arg(&rule.head.args, 0)).checked_add(1)
}

/// The width of every rule's variables (head and body), at least 1.
fn rule_width(rules: &[Rule]) -> Option<usize> {
    let mut widest = 1_usize;
    let mut index = 0_usize;
    while let Some(rule) = rules.get(index) {
        let Some(w) = width_of(rule) else {
            return None;
        };
        if w > widest {
            widest = w;
        }
        let Some(next) = index.checked_add(1) else {
            return None;
        };
        index = next;
    }
    Some(widest)
}

/// The element of case-`i` variable `v`, after the target variables.
fn case_elem(nb: usize, w: usize, i: usize, v: usize) -> Option<usize> {
    match i.checked_mul(w) {
        Some(offset) => match nb.checked_add(offset) {
            Some(start) => start.checked_add(v),
            None => None,
        },
        None => None,
    }
}

/// The elements of case `i` for `args`.
fn case_args(args: &[usize], nb: usize, w: usize, i: usize) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    let mut index = 0_usize;
    while let Some(v) = args.get(index) {
        let Some(e) = case_elem(nb, w, i, *v) else {
            return None;
        };
        out.push(e);
        let Some(next) = index.checked_add(1) else {
            return None;
        };
        index = next;
    }
    Some(out)
}

/// `atoms` followed by the body atoms of the rule chosen for case `i`.
fn with_case_body(
    atoms: Vec<Atom>,
    body: &[Atom],
    nb: usize,
    w: usize,
    i: usize,
) -> Option<Vec<Atom>> {
    let mut out = atoms;
    let mut index = 0_usize;
    while let Some(atom) = body.get(index) {
        let Some(args) = case_args(&atom.args, nb, w, i) else {
            return None;
        };
        out.push(Atom {
            rel: atom.rel,
            args,
        });
        let Some(next) = index.checked_add(1) else {
            return None;
        };
        index = next;
    }
    Some(out)
}

/// The bodies of every chosen rule, case by case.
fn case_bodies(rules: &[Rule], choice: &[usize], nb: usize, w: usize) -> Option<Vec<Atom>> {
    let mut out = Vec::new();
    let mut i = 0_usize;
    while let Some(index) = choice.get(i) {
        let Some(rule) = rules.get(*index) else {
            return None;
        };
        let Some(next_out) = with_case_body(out, &rule.body, nb, w, i) else {
            return None;
        };
        out = next_out;
        let Some(next) = i.checked_add(1) else {
            return None;
        };
        i = next;
    }
    Some(out)
}

/// `rep` with each head variable of case `i` merged into its target variable.
fn merged_pairs(
    rep: Vec<usize>,
    head: &[usize],
    target: &[usize],
    nb: usize,
    w: usize,
    i: usize,
) -> Option<Vec<usize>> {
    let mut out = rep;
    let mut index = 0_usize;
    while let (Some(hv), Some(bv)) = (head.get(index), target.get(index)) {
        let Some(a) = case_elem(nb, w, i, *hv) else {
            return None;
        };
        if a >= out.len() || *bv >= out.len() {
            return None;
        }
        out = merged(&out, a, *bv);
        let Some(next) = index.checked_add(1) else {
            return None;
        };
        index = next;
    }
    Some(out)
}

/// `rep` with every chosen rule's head merged into its target atom.
fn merged_heads(
    rep: Vec<usize>,
    rules: &[Rule],
    body: &[Atom],
    choice: &[usize],
    nb: usize,
    w: usize,
) -> Option<Vec<usize>> {
    let mut out = rep;
    let mut i = 0_usize;
    while let Some(index) = choice.get(i) {
        let (Some(rule), Some(target)) = (rules.get(*index), body.get(i)) else {
            return None;
        };
        let Some(next_out) = merged_pairs(out, &rule.head.args, &target.args, nb, w, i) else {
            return None;
        };
        out = next_out;
        let Some(next) = i.checked_add(1) else {
            return None;
        };
        i = next;
    }
    Some(out)
}

/// The canonical state of one case: the chosen rules' bodies kept apart,
/// each head merged into its target atom.
pub(crate) fn initial(rules: &[Rule], body: &[Atom], choice: &[usize]) -> Option<State> {
    let Some(nb) = width(body) else {
        return None;
    };
    let Some(w) = rule_width(rules) else {
        return None;
    };
    let Some(size) = choice.len().checked_mul(w) else {
        return None;
    };
    let Some(size) = nb.checked_add(size) else {
        return None;
    };
    let Some(atoms) = case_bodies(rules, choice, nb, w) else {
        return None;
    };
    let Some(rep) = merged_heads(extended(Vec::new(), size), rules, body, choice, nb, w) else {
        return None;
    };
    Some(State { atoms, rep })
}

/// `index :: tail`.
fn prepend(index: usize, tail: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    out.push(index);
    let mut i = 0_usize;
    while let Some(v) = tail.get(i) {
        out.push(*v);
        let Some(next) = i.checked_add(1) else {
            return out;
        };
        i = next;
    }
    out
}

/// `out` followed by `index :: tail` for every tail.
fn with_cases(out: Vec<Vec<usize>>, index: usize, tails: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut all = out;
    let mut i = 0_usize;
    while let Some(tail) = tails.get(i) {
        all.push(prepend(index, tail));
        let Some(next) = i.checked_add(1) else {
            return all;
        };
        i = next;
    }
    all
}

/// The rule's head has `atom`'s relation and arity.
fn produces(rule: &Rule, atom: &Atom) -> bool {
    rule.head.rel == atom.rel && rule.head.args.len() == atom.args.len()
}

/// Every rule whose head has `atom`'s relation and arity, followed by each
/// of `tails`.
fn cases_for(rules: &[Rule], atom: &Atom, tails: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut index = 0_usize;
    while let Some(rule) = rules.get(index) {
        if produces(rule, atom) {
            out = with_cases(out, index, tails);
        }
        let Some(next) = index.checked_add(1) else {
            return out;
        };
        index = next;
    }
    out
}

/// Every rule choice whose heads have the body atoms' relations and arities,
/// first atom outermost (`cases` in the Lean specification), built from the
/// last atom back.
pub(crate) fn case_choices(rules: &[Rule], body: &[Atom]) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    out.push(Vec::new());
    let mut remaining = body.len();
    while let Some(index) = remaining.checked_sub(1) {
        let Some(atom) = body.get(index) else {
            return out;
        };
        out = cases_for(rules, atom, &out);
        remaining = index;
    }
    out
}

/// Every case's trace runs from its canonical state and merges `left` and
/// `right`.
fn traces_merge(
    theory: &[Dep],
    rules: &[Rule],
    body: &[Atom],
    left: usize,
    right: usize,
    cases: &[Vec<usize>],
    traces: &[Vec<Step>],
) -> bool {
    let mut index = 0_usize;
    while let Some(choice) = cases.get(index) {
        let Some(trace) = traces.get(index) else {
            return false;
        };
        let Some(start) = initial(rules, body, choice) else {
            return false;
        };
        let Some(last) = run(theory, start, trace) else {
            return false;
        };
        if rep_of(&last.rep, left) != rep_of(&last.rep, right) {
            return false;
        }
        let Some(next) = index.checked_add(1) else {
            return false;
        };
        index = next;
    }
    true
}

/// The trusted certificate check (`NornSem.Exchange.check`): `left` and
/// `right` are body variables, and there is one trace per case, each ending
/// with them merged.
pub(crate) fn check(
    theory: &[Dep],
    rules: &[Rule],
    body: &[Atom],
    left: usize,
    right: usize,
    traces: &[Vec<Step>],
) -> bool {
    if !contains_var(body, left) || !contains_var(body, right) {
        return false;
    }
    let all = case_choices(rules, body);
    all.len() == traces.len() && traces_merge(theory, rules, body, left, right, &all, traces)
}
