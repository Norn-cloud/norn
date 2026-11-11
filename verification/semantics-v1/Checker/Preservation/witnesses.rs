//! Concrete witnesses for the seeded `preservation/check.rs` mutants (#718 U7b).
//!
//! `check_correct` fixes that the production check accepts only certificates
//! `NornSem.Exchange.check` accepts. Each witness below is a certificate the
//! specification refuses (or, for `accepts-valid`, accepts): a non-preserving
//! mapping, an extra trace, or a merged variable outside the body.
//! `scripts/verify-preservation-aeneas.sh` compiles this file beside a copy of
//! `check.rs` and requires the production file to agree on every witness and
//! each mutant to disagree on its own.

#[path = "check.rs"]
mod check;

use check::{Atom, Dep, Rule, Step};

const S: usize = 0;
const T: usize = 1;
const U: usize = 2;
const R: usize = 3;

fn atom(rel: usize, args: &[usize]) -> Atom {
    Atom {
        rel,
        args: args.to_vec(),
    }
}

fn step(dep: usize, asg: &[usize]) -> Step {
    Step {
        dep,
        asg: asg.to_vec(),
    }
}

/// The mapping `S(y0, y1) -> T(y0, y1)` and the target EGD `T(x0, x1) -> x0 = x1`
/// (case variables `y0, y1` are elements 2 and 3, merged into 0 and 1).
fn copy(theory: &[Dep], traces: &[Vec<Step>]) -> bool {
    let rules = [Rule {
        body: vec![atom(S, &[0, 1])],
        head: atom(T, &[0, 1]),
    }];
    check::check(theory, &rules, &[atom(T, &[0, 1])], 0, 1, traces)
}

/// Whether the code under test returns the result `check_correct` fixes on
/// the named witness; `None` for an unknown name.
fn agrees(name: &str) -> Option<bool> {
    let functional = [Dep::Egd {
        body: vec![atom(S, &[0, 1])],
        left: 0,
        right: 1,
    }];
    let unrelated = [Dep::Egd {
        body: vec![atom(R, &[0, 1])],
        left: 0,
        right: 1,
    }];
    Some(match name {
        // A source theory making `S` diagonal preserves the target EGD.
        "accepts-valid" => copy(&functional, &[vec![step(0, &[2, 3])]]),
        // An EGD step whose body is absent from the state proves nothing.
        "egd-skips-body" => !copy(&unrelated, &[vec![step(0, &[0, 1])]]),
        // A state atom of another relation does not match the body.
        "has-ignores-relation" => !copy(&unrelated, &[vec![step(0, &[0, 1])]]),
        // A TGD's existential variable is a fresh element, never an old one.
        "head-not-fresh" => {
            let theory = [
                Dep::Tgd {
                    body: vec![atom(S, &[0, 1])],
                    head: vec![atom(U, &[0, 3])],
                },
                Dep::Egd {
                    body: vec![atom(U, &[0, 1])],
                    left: 0,
                    right: 1,
                },
            ];
            !copy(&theory, &[vec![step(0, &[2, 3]), step(1, &[2, 3])]])
        }
        // A certificate has exactly one trace per case.
        "case-count-unchecked" => !copy(&functional, &[vec![step(0, &[2, 3])], Vec::new()]),
        // The merged variables must be variables of the target body.
        "left-unchecked" => {
            let rules = [Rule {
                body: vec![atom(S, &[0])],
                head: atom(T, &[0]),
            }];
            !check::check(&[], &rules, &[atom(T, &[0])], 1, 0, &[Vec::new()])
        }
        _ => return None,
    })
}

const ALL: [&str; 6] = [
    "accepts-valid",
    "egd-skips-body",
    "has-ignores-relation",
    "head-not-fresh",
    "case-count-unchecked",
    "left-unchecked",
];

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    let ok = if arg == "all" {
        ALL.iter().all(|name| agrees(name) == Some(true))
    } else {
        agrees(&arg) == Some(false)
    };
    std::process::exit(if ok { 0 } else { 1 });
}
