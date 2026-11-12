//! Concrete witnesses for the seeded `law_exceptions/shape.rs` mutants (#718 U1).
//!
//! Each witness's expected result is the one the Lean theorems fix
//! (`check_shape_correct`: `Ok` exactly on a well-formed tree; `check_shape_refusal`:
//! the first failing check names its first offending declaration; `lifted_correct`:
//! the semantic fold at fuel `|ids| + 1`; `applied_sound`: the reported set). `scripts/verify-law-exceptions-aeneas.sh`
//! compiles this file beside a copy of `shape.rs` and requires the production
//! file to agree on every witness and each mutant to disagree on its own.

#[path = "shape.rs"]
mod shape;

fn check(ids: &[&str], parents: &[&str]) -> bool {
    let declared = vec![true; ids.len()];
    shape::check_shape(&"law", ids, parents, &declared).is_ok()
}

fn refusal(ids: &[&str], parents: &[&str]) -> Result<(), shape::ExceptionTreeRefusal> {
    let declared = vec![true; ids.len()];
    shape::check_shape(&"law", ids, parents, &declared)
}

fn lifted(ids: &[&str], parents: &[&str], matched: &[bool]) -> bool {
    let mut level = vec![false; ids.len()];
    let mut next = vec![false; ids.len()];
    shape::lifted(&"law", ids, parents, matched, &mut level, &mut next)
}

fn applied(ids: &[&str], parents: &[&str], matched: &[bool]) -> Vec<bool> {
    let mut level = vec![false; ids.len()];
    let mut next = vec![false; ids.len()];
    shape::lifted(&"law", ids, parents, matched, &mut level, &mut next);
    let mut applied = vec![false; ids.len()];
    shape::applied_exceptions(&"law", ids, parents, matched, &level, &mut applied);
    applied
}

/// Whether the code under test returns the result the theorems fix on the
/// named witness; `None` for an unknown name.
fn agrees(name: &str) -> Option<bool> {
    Some(match name {
        // An exception reusing the law's id is not well-formed.
        "invalid-allows-law-id" => !check(&["law"], &["law"]),
        // A repeated id is not well-formed.
        "invalid-allows-duplicates" => !check(&["a", "a"], &["law", "law"]),
        // A chain as deep as the tree is well-formed.
        "cycle-walk-too-short" => check(&["a", "b"], &["law", "a"]),
        // Exactly MAX_LAW_EXCEPTIONS exceptions is within the bound.
        "limit-off-by-one" => {
            let names: Vec<String> = (0..64).map(|k| format!("e{k}")).collect();
            let ids: Vec<&str> = names.iter().map(String::as_str).collect();
            check(&ids, &vec!["law"; 64])
        }
        // A missing target is reported as dangling, at its own index.
        "dangling-reported-as-cycle" => {
            refusal(&["a"], &["b"]) == Err(shape::ExceptionTreeRefusal::DanglingOverride(0))
        }
        // One matched exception under the law lifts it.
        "fold-negation-dropped" => lifted(&["a"], &["law"], &[true]),
        // An unmatched exception does not.
        "fold-ignores-condition" => !lifted(&["a"], &["law"], &[false]),
        // Fuel |ids| + 1 on an oscillating input: L3(law) = ¬L2(a) = true.
        "fold-one-round-short" => lifted(&["a", "a"], &["law", "a"], &[true, true]),
        // `b` holds under `a`, whose condition fails: nothing is reported.
        "applied-skips-ancestors" | "ancestors-ignore-condition" => {
            applied(&["a", "b"], &["law", "a"], &[false, true]) == [false, false]
        }
        // `b` stops `a`: only `b` is reported.
        "applied-ignores-level" => {
            applied(&["a", "b"], &["law", "a"], &[true, true]) == [false, true]
        }
        _ => return None,
    })
}

const ALL: [&str; 11] = [
    "invalid-allows-law-id",
    "invalid-allows-duplicates",
    "cycle-walk-too-short",
    "limit-off-by-one",
    "dangling-reported-as-cycle",
    "fold-negation-dropped",
    "fold-ignores-condition",
    "fold-one-round-short",
    "applied-skips-ancestors",
    "ancestors-ignore-condition",
    "applied-ignores-level",
];

/// `all`: exit 0 when every witness agrees (the production file).
/// `<name>`: exit 0 when that witness disagrees (the mutant is refuted).
fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    let ok = if arg == "all" {
        ALL.iter().all(|name| agrees(name) == Some(true))
    } else {
        agrees(&arg) == Some(false)
    };
    std::process::exit(if ok { 0 } else { 1 });
}
