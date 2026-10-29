//! Norn (#3404): pins the semantics of `matches()` / `extract()` on the only
//! regex backend, `regex-lite`. It is not Unicode-aware beyond matching
//! codepoint by codepoint: `\d`, `\s`, `\w` and `\b` are ASCII-only, `\p{..}` /
//! `\P{..}` do not compile, and `(?i)` only folds ASCII case. Any backend change
//! that alters these rows is a Norn profile change.

use serde_json::json;
use zen_expression::evaluate_expression;
use zen_expression::variable::Variable;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Outcome {
    Match,
    NoMatch,
    /// The pattern does not compile (`matches()` returns an error).
    Invalid,
}

use Outcome::*;

#[rustfmt::skip]
const CASES: &[(&str, &str, Outcome)] = &[
    // ASCII patterns and input.
    (r"^[a-z]+$", "abc", Match),
    (r"^\d{3}-\d{4}$", "555-1234", Match),
    (r"^\w+@\w+\.com$", "a_b@example.com", Match),
    (r"(?i)^hello$", "HeLLo", Match),
    (r"^\s+$", " \t\n", Match),
    (r"\bcat\b", "a cat sat", Match),
    (r"^(foo|bar)+?$", "foobar", Match),
    (r"^[[:alpha:]]+$", "abcXYZ", Match),
    // `.` and literal codepoints are Unicode scalar values.
    (r"^.$", "é", Match),
    (r"^café$", "café", Match),
    // Invalid syntax is refused.
    (r"(unclosed", "x", Invalid),
    // Perl classes are ASCII-only.
    (r"^\w+$", "café", NoMatch),
    (r"^\d$", "\u{0663}", NoMatch),
    (r"^\s$", "\u{00A0}", NoMatch),
    // Word boundaries follow the ASCII `\w`.
    (r"\bé", "é", NoMatch),
    // Unicode property classes do not compile.
    (r"^\p{L}+$", "héllo", Invalid),
    (r"^\P{N}$", "a", Invalid),
    // Case-insensitive matching folds ASCII only.
    (r"(?i)^é$", "É", NoMatch),
];

fn outcome(pattern: &str, input: &str) -> Outcome {
    let context: Variable = json!({ "p": pattern, "s": input }).into();
    match evaluate_expression("matches(s, p)", context) {
        Ok(Variable::Bool(true)) => Match,
        Ok(Variable::Bool(false)) => NoMatch,
        Ok(other) => panic!("matches({input:?}, {pattern:?}) returned {other:?}"),
        Err(_) => Invalid,
    }
}

#[test]
fn matches_follows_regex_lite() {
    for &(pattern, input, expected) in CASES {
        assert_eq!(
            outcome(pattern, input),
            expected,
            "pattern {pattern:?} on {input:?}"
        );
    }
}

/// `extract()` captures follow the same ASCII class definitions.
#[test]
fn extract_follows_regex_lite() {
    let context: Variable = json!({ "s": "id: café42", "p": r"(\w+)(\d+)$" }).into();
    let result = evaluate_expression("extract(s, p)", context).unwrap();
    assert_eq!(serde_json::Value::from(result), json!(["42", "4", "2"]));
}
