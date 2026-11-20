#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "conformance corpus test: the tally is printed and failures panic"
)]

//! IBM General Decimal Arithmetic testcases (dectest 2.62) against
//! `norn_ir::runtime::Decimal` (#1429).
//!
//! The testcases are not vendored: their licence is unresolved (#1429), so
//! `scripts/importers/erpnext_runtime.py` fetches the pinned
//! `speleotrove.com/decimal/dectest.zip` outside the tree, checks its digest
//! and each member's, and `scripts/ci/core.sh` hands the directory over in
//! `NORN_UPSTREAM_FIXTURE_DIR` like every other upstream test input.
//!
//! Claimed subset, the operations `Decimal` has:
//! - **parse** (`toSci`, `toEng`, `apply`): `Decimal::parse` accepts exactly the
//!   JSON number grammar. Every operand outside it (`NaN`, `Inf`, `.5`, `1.`,
//!   `+1`, `01`, spaces) must be refused. A JSON number whose exact value is in
//!   range (96-bit coefficient, scale at most 28) must read back as that value,
//!   in canonical text; one out of range must be refused, never rounded.
//! - **add**, **subtract**: `checked_add`/`checked_sub` return the exact result
//!   when it is in range, and `None` otherwise.
//! - **compare**: `Ord` agrees with the testcase.
//!
//! Skipped with a reason, never silently: every other operation (multiply,
//! divide, rounding, quantize, ...: `Decimal` has none of them); results the
//! testcase rounds to its context precision (`Inexact`, `Overflow`,
//! `Underflow`; Norn never rounds); special values (`NaN`, `Infinity`; Norn has
//! none); and arithmetic on operands outside JSON syntax or Norn's range.
//! The expected value comes from an exact decimal reader in this file, not
//! from `Decimal` itself.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::PathBuf;

use norn_ir::runtime::Decimal;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const FILES: [&str; 8] = [
    "base.decTest",
    "dqBase.decTest",
    "add.decTest",
    "dqAdd.decTest",
    "subtract.decTest",
    "dqSubtract.decTest",
    "compare.decTest",
    "dqCompare.decTest",
];

fn corpus() -> PathBuf {
    let root = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR")
        .map(PathBuf::from)
        .expect("NORN_UPSTREAM_FIXTURE_DIR is unset: this test reads the checksum-pinned dectest files fetched outside the tree. Run it through `bash scripts/ci/core.sh test`, or run `python3 scripts/importers/erpnext_runtime.py --output EXTERNAL_SCRATCH --interop-only` and set NORN_UPSTREAM_FIXTURE_DIR to that directory");
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    let root = root
        .canonicalize()
        .expect("upstream input directory exists");
    assert!(
        !root.starts_with(repo),
        "dectest inputs must remain external"
    );
    root.join("dectest")
}

fn read_pinned(name: &str) -> String {
    let lock: Value = serde_json::from_str(include_str!(
        "../../../scripts/importers/erpnext-runtime.lock.json"
    ))
    .expect("runtime input pins");
    let bytes = std::fs::read(corpus().join(name)).expect("pinned dectest member");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        lock["interop"]["dectest-2.62.zip"]["members"][name],
        "{name} must match its acquisition pin"
    );
    String::from_utf8(bytes).expect("dectest files are ASCII")
}

/// One testcase line: `id operation operand... -> result condition...`.
struct Case {
    id: String,
    operation: String,
    operands: Vec<String>,
    result: String,
    conditions: Vec<String>,
}

/// Split a line into tokens; quotes (`'` or `"`, doubled to escape) group.
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&next) = chars.peek() {
        if next.is_whitespace() {
            chars.next();
        } else if next == '-' && line_rest_is_comment(&mut chars.clone()) {
            break;
        } else if next == '\'' || next == '"' {
            chars.next();
            let mut token = String::new();
            while let Some(c) = chars.next() {
                if c == next {
                    if chars.peek() == Some(&next) {
                        chars.next();
                        token.push(next);
                    } else {
                        break;
                    }
                } else {
                    token.push(c);
                }
            }
            out.push(token);
        } else {
            let mut token = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                token.push(c);
                chars.next();
            }
            out.push(token);
        }
    }
    out
}

fn line_rest_is_comment(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    chars.next() == Some('-') && chars.next() == Some('-')
}

fn cases(text: &str) -> Vec<Case> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("--") {
            continue;
        }
        let parts = tokens(line);
        // Directives (`precision: 9`) and the version line.
        if parts.first().is_some_and(|first| first.ends_with(':')) {
            continue;
        }
        let Some(arrow) = parts.iter().position(|part| part == "->") else {
            panic!("unparsed dectest line: {line}");
        };
        assert!(arrow >= 2, "testcase without operation: {line}");
        out.push(Case {
            id: parts[0].clone(),
            operation: parts[1].to_ascii_lowercase(),
            operands: parts[2..arrow].to_vec(),
            result: parts.get(arrow + 1).cloned().unwrap_or_default(),
            conditions: parts
                .get(arrow + 2..)
                .unwrap_or_default()
                .iter()
                .map(|c| c.to_ascii_lowercase())
                .collect(),
        });
    }
    out
}

/// Whether `text` is one JSON number token (RFC 8259 §6).
fn json_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = 0;
    let digits = |at: &mut usize| {
        let start = *at;
        while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
            *at += 1;
        }
        *at > start
    };
    if bytes.get(at) == Some(&b'-') {
        at += 1;
    }
    match bytes.get(at) {
        Some(b'0') => at += 1,
        Some(b'1'..=b'9') => {
            digits(&mut at);
        }
        _ => return false,
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        if !digits(&mut at) {
            return false;
        }
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        if !digits(&mut at) {
            return false;
        }
    }
    at == bytes.len()
}

/// An exact decimal read independently of `Decimal`: `digits × 10^exponent`,
/// normalized (no leading or trailing zeros; zero is empty digits).
#[derive(Debug, PartialEq, Eq)]
struct Exact {
    negative: bool,
    digits: String,
    exponent: i64,
}

impl Exact {
    /// A finite dectest numeric string (`-1.20E+3`, `0E-7`), or `None`.
    fn read(text: &str) -> Option<Self> {
        let (negative, body) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let (mantissa, exponent) = match body.find(['e', 'E']) {
            Some(at) => (&body[..at], body[at + 1..].parse::<i64>().ok()?),
            None => (body, 0),
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if whole.is_empty() && fraction.is_empty()
            || !whole
                .chars()
                .chain(fraction.chars())
                .all(|c| c.is_ascii_digit())
        {
            return None;
        }
        let mut digits = format!("{whole}{fraction}");
        let mut exponent = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
        let leading = digits.len() - digits.trim_start_matches('0').len();
        digits.drain(..leading);
        while digits.ends_with('0') {
            digits.pop();
            exponent = exponent.checked_add(1)?;
        }
        if digits.is_empty() {
            return Some(Self {
                negative: false,
                digits,
                exponent: 0,
            });
        }
        Some(Self {
            negative,
            digits,
            exponent,
        })
    }

    /// Whether `Decimal` can hold the value: scale at most 28 and a
    /// coefficient below 2^96.
    fn in_range(&self) -> bool {
        if self.digits.is_empty() {
            return true;
        }
        if self.exponent < -28 || self.exponent > 40 {
            return false;
        }
        let zeros = usize::try_from(self.exponent.max(0)).expect("small exponent");
        let coefficient = format!("{}{}", self.digits, "0".repeat(zeros));
        coefficient.len() <= 29 && coefficient.parse::<u128>().is_ok_and(|c| c < 1 << 96)
    }

    /// Canonical plain text: `-12.5`, `0.001`, `1200`, `0`.
    fn canonical(&self) -> String {
        if self.digits.is_empty() {
            return "0".to_owned();
        }
        let sign = if self.negative { "-" } else { "" };
        let text = if self.exponent >= 0 {
            let zeros = usize::try_from(self.exponent).expect("in range");
            format!("{}{}", self.digits, "0".repeat(zeros))
        } else {
            let scale = usize::try_from(-self.exponent).expect("in range");
            if self.digits.len() > scale {
                let (whole, fraction) = self.digits.split_at(self.digits.len() - scale);
                format!("{whole}.{fraction}")
            } else {
                format!("0.{}{}", "0".repeat(scale - self.digits.len()), self.digits)
            }
        };
        format!("{sign}{text}")
    }
}

/// Conditions under which the testcase result is not the exact value.
const ROUNDING: [&str; 5] = [
    "inexact",
    "overflow",
    "underflow",
    "invalid_operation",
    "division_impossible",
];

#[derive(Default)]
struct Tally {
    passed: BTreeMap<&'static str, usize>,
    failed: Vec<String>,
    skipped: BTreeMap<&'static str, usize>,
}

impl Tally {
    fn pass(&mut self, kind: &'static str) {
        *self.passed.entry(kind).or_default() += 1;
    }

    fn fail(&mut self, case: &Case, detail: String) {
        self.failed.push(format!(
            "{} {} {:?} -> {}: {detail}",
            case.id, case.operation, case.operands, case.result
        ));
    }

    fn skip(&mut self, reason: &'static str) {
        *self.skipped.entry(reason).or_default() += 1;
    }
}

fn operand(text: &str) -> Option<Decimal> {
    if text == "#" || !json_number(text) {
        return None;
    }
    Exact::read(text).filter(Exact::in_range)?;
    Decimal::parse(text).ok()
}

fn run_parse(case: &Case, tally: &mut Tally) {
    let [text] = case.operands.as_slice() else {
        tally.skip("malformed parse testcase");
        return;
    };
    let parsed = Decimal::parse(text);
    if !json_number(text) {
        match parsed {
            Err(_) => tally.pass("parse: non-JSON syntax refused"),
            Ok(value) => tally.fail(case, format!("accepted non-JSON text as {value}")),
        }
        return;
    }
    if case
        .conditions
        .iter()
        .any(|c| ROUNDING.contains(&c.as_str()))
    {
        tally.skip("parse: testcase rounds to context precision; Norn never rounds");
        return;
    }
    let Some(expected) = Exact::read(&case.result) else {
        tally.skip("parse: special-value result");
        return;
    };
    if Exact::read(text).as_ref() != Some(&expected) {
        tally.fail(
            case,
            "testcase result is not the operand's value".to_owned(),
        );
        return;
    }
    match (expected.in_range(), parsed) {
        (true, Ok(value)) if value.canonical() == expected.canonical() => {
            tally.pass("parse: exact value");
        }
        (true, Ok(value)) => tally.fail(
            case,
            format!("read {} want {}", value.canonical(), expected.canonical()),
        ),
        (true, Err(error)) => tally.fail(case, format!("refused an in-range value: {error}")),
        (false, Err(_)) => tally.pass("parse: out-of-range refused"),
        (false, Ok(value)) => tally.fail(case, format!("accepted out-of-range as {value}")),
    }
}

fn run_arithmetic(case: &Case, tally: &mut Tally) {
    let [left, right] = case.operands.as_slice() else {
        tally.skip("malformed arithmetic testcase");
        return;
    };
    if [left, right]
        .iter()
        .any(|text| *text == "#" || !json_number(text))
    {
        tally.skip("arithmetic: operand outside JSON number syntax (NaN, Inf, #, ...)");
        return;
    }
    let (Some(left), Some(right)) = (operand(left), operand(right)) else {
        tally.skip("arithmetic: operand outside Decimal's range");
        return;
    };
    if case.operation == "compare" {
        let expected = match case.result.as_str() {
            "-1" => Ordering::Less,
            "0" => Ordering::Equal,
            "1" => Ordering::Greater,
            _ => {
                tally.skip("compare: special-value result");
                return;
            }
        };
        if left.cmp(&right) == expected {
            tally.pass("compare");
        } else {
            tally.fail(case, format!("Ord gave {:?}", left.cmp(&right)));
        }
        return;
    }
    if case
        .conditions
        .iter()
        .any(|c| ROUNDING.contains(&c.as_str()))
    {
        tally.skip("arithmetic: testcase rounds to context precision; Norn never rounds");
        return;
    }
    let Some(expected) = Exact::read(&case.result) else {
        tally.skip("arithmetic: special-value result");
        return;
    };
    let (kind, got) = if case.operation == "add" {
        ("add", left.checked_add(right))
    } else {
        ("subtract", left.checked_sub(right))
    };
    match (expected.in_range(), got) {
        (true, Some(value)) if value.canonical() == expected.canonical() => tally.pass(kind),
        (true, Some(value)) => tally.fail(
            case,
            format!("got {} want {}", value.canonical(), expected.canonical()),
        ),
        (true, None) => tally.fail(case, "refused an exact in-range result".to_owned()),
        (false, None) => tally.pass("add/subtract: out-of-range result refused"),
        (false, Some(value)) => tally.fail(case, format!("held out-of-range result as {value}")),
    }
}

#[test]
fn dectest_cases_hold_for_the_claimed_decimal_operations() {
    let mut tally = Tally::default();
    for file in FILES {
        for case in cases(&read_pinned(file)) {
            match case.operation.as_str() {
                "tosci" | "toeng" | "apply" => run_parse(&case, &mut tally),
                "add" | "subtract" | "compare" => run_arithmetic(&case, &mut tally),
                _ => tally.skip("operation Decimal does not have"),
            }
        }
    }
    let passed: usize = tally.passed.values().sum();
    let skipped: usize = tally.skipped.values().sum();
    eprintln!(
        "dectest: run {}, passed {passed} {:?}, failed {}, skipped {skipped} {:?}",
        passed + tally.failed.len(),
        tally.passed,
        tally.failed.len(),
        tally.skipped
    );
    assert!(
        tally.failed.is_empty(),
        "{} dectest failures:\n{}",
        tally.failed.len(),
        tally.failed.join("\n")
    );
    assert!(passed >= 2000, "only {passed} dectest cases ran");
}

#[test]
fn the_exact_reader_agrees_with_hand_examples() {
    let canonical = |text: &str| Exact::read(text).map(|e| e.canonical());
    assert_eq!(canonical("-1.20E+3").as_deref(), Some("-1200"));
    assert_eq!(canonical("0E-7").as_deref(), Some("0"));
    assert_eq!(canonical("-0").as_deref(), Some("0"));
    assert_eq!(canonical("12.500").as_deref(), Some("12.5"));
    assert_eq!(
        canonical("1E-28").as_deref(),
        Some("0.0000000000000000000000000001")
    );
    assert_eq!(canonical("NaN"), None);
    assert!(Exact::read("79228162514264337593543950335").is_some_and(|e| e.in_range()));
    assert!(!Exact::read("79228162514264337593543950336").is_some_and(|e| e.in_range()));
    assert!(!Exact::read("1E-29").is_some_and(|e| e.in_range()));
    assert!(
        json_number("-0.5e+3") && !json_number(".5") && !json_number("1.") && !json_number("01")
    );
}
