//! Norn (#1436): the temporal builtins give the outputs recorded from the
//! chrono implementation they replaced, over a fixed corpus of dates,
//! timestamps, offsets, range ends and arithmetic (`temporal_corpus/corpus.rs`).
//! The differential run (old and new side by side, case by case) is recorded
//! on #1436; this test keeps the result.

mod corpus;

use std::collections::BTreeMap;

use corpus::Fingerprint;
use zen_expression::{Isolate, IsolateError, Variable};

/// `(category, cases, FNV-1a)` of the chrono implementation's outputs.
/// The comment counts the cases that evaluate rather than being refused.
const RECORDED: &[(&str, u64, u64)] = &[
    ("add_days", 6005, 0x8183_b45a_c53d_aa92), // 2402 evaluate
    ("add_seconds", 4490, 0x763a_b9ea_8bad_9530), // 2283 evaluate
    ("compare", 39000, 0x0a48_b758_4ee4_d0fe), // 35946 evaluate
    ("date", 50492, 0xd5b6_82c4_385d_0c87),    // 37472 evaluate
    ("days_between", 3010, 0xa791_fa1d_db6b_6369), // 3008 evaluate
    ("legacy", 159, 0x3755_9992_1d76_67d8),    // 0 evaluate
    ("seconds_between", 6100, 0xef2e_c93f_3c0a_e1b0), // 6084 evaluate
    ("timestamp", 67392, 0xa786_7af9_893a_4ae7), // 19637 evaluate
];

fn render(result: Result<Variable, IsolateError>) -> String {
    match result {
        Ok(Variable::Number(n)) => format!("N:{n}"),
        Ok(Variable::String(s)) => format!("S:{s}"),
        Ok(Variable::Bool(b)) => format!("B:{b}"),
        Ok(other) => format!("V:{}", other.to_value()),
        Err(error) => format!("E:{error}"),
    }
}

#[test]
fn temporal_builtins_reproduce_the_recorded_chrono_outputs() {
    let mut isolate = Isolate::new();
    let mut actual: BTreeMap<&str, Fingerprint> = BTreeMap::new();
    for (category, source) in corpus::cases() {
        let output = render(isolate.run_standard(&source));
        actual.entry(category).or_default().add(&source, &output);
    }
    let recorded: BTreeMap<&str, Fingerprint> = RECORDED
        .iter()
        .map(|&(category, cases, fnv1a)| (category, Fingerprint { cases, fnv1a }))
        .collect();
    assert_eq!(actual, recorded);
}
