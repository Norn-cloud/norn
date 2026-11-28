//! ZEN expression lexical/structural bounds and `actor` read analysis.
//!
//! Faithful Rust port of `tools/norn-ts/src/zen-reads.ts`. Decides whether a
//! ZEN expression may read the verified `actor` principal, mirroring
//! `norn_expr::check::bounds` and `vendor/zen/core/expression`.

use super::zen_parser::{LiteralContainer, ZenNode, parse_and_check_actor};

/// Structural limits matching `StructuralLimits::NORN_EXPR_4`.
const MAX_SOURCE_BYTES: usize = 16_384;
const MAX_NODES: usize = 4_096;
const MAX_DEPTH: usize = 128;
const MAX_PARSE_NESTING: usize = 512;
const MAX_STRING_BYTES: usize = 4_096;
const MAX_LITERAL_ITEMS: usize = 1_024;
const MAX_DECIMAL_MANTISSA: u128 = (1_u128 << 96).saturating_sub(1);

/// Token kind produced by the ZEN lexer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZenTokenKind {
    /// Context/root/pointer/null identifier (`$`, `$root`, `#`, `null`).
    Identifier,
    /// Unquoted name token.
    Literal,
    /// Numeric literal token.
    Number,
    /// Single- or double-quoted string literal token.
    String,
    /// Boolean literal (`true` or `false`).
    Boolean,
    /// Operator or punctuation token.
    Operator,
    /// Bracket or delimiter (`(`, `)`, `[`, `]`, `{`, `}`).
    Bracket,
    /// Template delimiter (`` ` ``, `${`, `}`).
    Template,
    /// Template raw text segment.
    Text,
}

/// Single lexed ZEN token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZenToken {
    /// Token category.
    pub(crate) kind: ZenTokenKind,
    /// Token text value.
    pub(crate) value: String,
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_opening_byte(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'^'
            | b'!'
            | b'<'
            | b'>'
            | b'='
            | b'&'
            | b'|'
            | b'?'
            | b':'
    )
}

/// Enforces `bounds::lexical` on raw UTF-8 ZEN source without string tracking,
/// matching `lexicalBounds` in `tools/norn-ts/src/zen-reads.ts`.
#[must_use]
pub fn lexical_bounds(source: &str) -> bool {
    let bytes = source.as_bytes();
    if bytes.len() > MAX_SOURCE_BYTES {
        return false;
    }
    let mut brackets: usize = 0;
    let mut levels: usize = 0;
    let mut word = String::new();
    let digit_at = |idx: Option<usize>| -> bool {
        idx.and_then(|i| bytes.get(i))
            .is_some_and(u8::is_ascii_digit)
    };
    for idx in 0..=bytes.len() {
        let byte = bytes.get(idx).copied().unwrap_or(b' ');
        if is_word_byte(byte) {
            word.push(char::from(byte));
            continue;
        }
        let keyword = matches!(word.as_str(), "and" | "or" | "not" | "in");
        word.clear();
        let opens = match byte {
            b'(' | b'[' | b'{' => {
                brackets = brackets.saturating_add(1);
                if brackets > MAX_DEPTH {
                    return false;
                }
                true
            }
            b')' | b']' | b'}' => {
                brackets = brackets.saturating_sub(1);
                true
            }
            b'.' => !(digit_at(idx.checked_sub(1)) && digit_at(idx.checked_add(1))),
            other => is_opening_byte(other),
        };
        levels = levels
            .saturating_add(usize::from(opens))
            .saturating_add(usize::from(keyword));
        if levels > MAX_PARSE_NESTING {
            return false;
        }
    }
    true
}

/// Returns `true` if `source` may read the verified `actor` binding (or fails
/// lexical/structural validation or parsing).
///
/// Mirrors `mayReadActor` in `tools/norn-ts/src/zen-reads.ts`.
#[must_use]
pub fn may_read_actor(source: &str) -> bool {
    if !lexical_bounds(source) {
        return true;
    }
    let Ok(tokens) = lex_zen(source) else {
        return true;
    };
    parse_and_check_actor(tokens)
}

fn is_alpha(ch: char) -> bool {
    ch.is_ascii_alphabetic() || matches!(ch, '$' | '_' | '#')
}

fn is_alphanumeric(ch: char) -> bool {
    is_alpha(ch) || ch.is_ascii_digit()
}

struct Lexer {
    chars: Vec<char>,
    at: usize,
    tokens: Vec<ZenToken>,
}

impl Lexer {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            at: 0,
            tokens: Vec::new(),
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.at
            .checked_add(offset)
            .and_then(|idx| self.chars.get(idx).copied())
    }

    fn push(&mut self, kind: ZenTokenKind, value: String) {
        self.tokens.push(ZenToken { kind, value });
    }

    fn lex_all(mut self) -> Result<Vec<ZenToken>, ()> {
        while self.at < self.chars.len() {
            self.item()?;
        }
        Ok(self.tokens)
    }

    fn item(&mut self) -> Result<(), ()> {
        let Some(ch) = self.peek(0) else {
            return Ok(());
        };
        if matches!(ch, ' ' | '\n' | '\t') {
            self.at = self.at.saturating_add(1);
            return Ok(());
        }
        if ch == '\'' || ch == '"' {
            return self.string(ch);
        }
        if ch.is_ascii_digit() {
            self.number();
            return Ok(());
        }
        if matches!(ch, '(' | ')' | '[' | ']' | '{' | '}') {
            self.push(ZenTokenKind::Bracket, ch.to_string());
            self.at = self.at.saturating_add(1);
            return Ok(());
        }
        if matches!(ch, '>' | '<' | '!') {
            self.at = self.at.saturating_add(1);
            if self.peek(0) == Some('=') {
                self.at = self.at.saturating_add(1);
                self.push(ZenTokenKind::Operator, format!("{ch}="));
            } else {
                self.push(ZenTokenKind::Operator, ch.to_string());
            }
            return Ok(());
        }
        if matches!(ch, ',' | ':' | '+' | '-' | '/' | '*' | '^' | '%') {
            self.push(ZenTokenKind::Operator, ch.to_string());
            self.at = self.at.saturating_add(1);
            return Ok(());
        }
        if ch == '?' {
            self.at = self.at.saturating_add(1);
            if self.peek(0) == Some('?') {
                self.at = self.at.saturating_add(1);
                self.push(ZenTokenKind::Operator, "??".to_owned());
            } else {
                self.push(ZenTokenKind::Operator, "?".to_owned());
            }
            return Ok(());
        }
        if ch == '=' {
            self.at = self.at.saturating_add(1);
            if self.peek(0) == Some('=') {
                self.at = self.at.saturating_add(1);
                self.push(ZenTokenKind::Operator, "==".to_owned());
            } else {
                self.push(ZenTokenKind::Operator, "=".to_owned());
            }
            return Ok(());
        }
        if ch == '`' {
            return self.template();
        }
        if ch == '.' {
            self.at = self.at.saturating_add(1);
            if self.peek(0) == Some('.') {
                self.at = self.at.saturating_add(1);
                self.push(ZenTokenKind::Operator, "..".to_owned());
            } else {
                self.push(ZenTokenKind::Operator, ".".to_owned());
            }
            return Ok(());
        }
        if ch == ';' {
            self.push(ZenTokenKind::Operator, ";".to_owned());
            self.at = self.at.saturating_add(1);
            return Ok(());
        }
        if is_alpha(ch) {
            self.identifier();
            return Ok(());
        }
        Err(())
    }

    fn string(&mut self, quote: char) -> Result<(), ()> {
        self.at = self.at.saturating_add(1);
        let start = self.at;
        while let Some(ch) = self.peek(0) {
            if ch == quote {
                break;
            }
            self.at = self.at.saturating_add(1);
        }
        if self.at >= self.chars.len() {
            return Err(());
        }
        let text: String = self
            .chars
            .get(start..self.at)
            .unwrap_or(&[])
            .iter()
            .collect();
        self.push(ZenTokenKind::String, text);
        self.at = self.at.saturating_add(1);
        Ok(())
    }

    fn number(&mut self) {
        let start = self.at;
        self.at = self.at.saturating_add(1);
        let mut fractal = false;
        while let Some(ch) = self.peek(0) {
            if ch == '.' {
                if fractal || self.peek(1) == Some('.') {
                    break;
                }
                if self.at.saturating_add(1) < self.chars.len() {
                    fractal = true;
                }
            } else if !ch.is_ascii_digit() && ch != '_' {
                break;
            }
            self.at = self.at.saturating_add(1);
        }
        if self.peek(0) == Some('e') {
            let e_pos = self.at;
            self.at = self.at.saturating_add(1);
            if matches!(self.peek(0), Some('+' | '-')) {
                self.at = self.at.saturating_add(1);
            }
            let digits = self.at;
            while self.peek(0).is_some_and(|ch| ch.is_ascii_digit()) {
                self.at = self.at.saturating_add(1);
            }
            if self.at == digits {
                self.at = e_pos;
            }
        }
        let text: String = self
            .chars
            .get(start..self.at)
            .unwrap_or(&[])
            .iter()
            .collect();
        self.push(ZenTokenKind::Number, text);
    }

    fn template(&mut self) -> Result<(), ()> {
        self.at = self.at.saturating_add(1);
        self.push(ZenTokenKind::Template, "`".to_owned());
        let mut in_expr = false;
        let mut text_start = self.at;
        while let Some(ch) = self.peek(0) {
            if ch == '`' {
                if text_start < self.at {
                    let seg: String = self
                        .chars
                        .get(text_start..self.at)
                        .unwrap_or(&[])
                        .iter()
                        .collect();
                    self.push(ZenTokenKind::Text, seg);
                }
                self.at = self.at.saturating_add(1);
                self.push(ZenTokenKind::Template, "`".to_owned());
                return Ok(());
            }
            if !in_expr {
                if ch == '$' && self.peek(1) == Some('{') {
                    let seg: String = self
                        .chars
                        .get(text_start..self.at)
                        .unwrap_or(&[])
                        .iter()
                        .collect();
                    self.push(ZenTokenKind::Text, seg);
                    self.at = self.at.saturating_add(2);
                    in_expr = true;
                    self.push(ZenTokenKind::Template, "${".to_owned());
                    continue;
                }
                self.at = self.at.saturating_add(1);
                continue;
            }
            if ch == '}' {
                self.push(ZenTokenKind::Template, "}".to_owned());
                self.at = self.at.saturating_add(1);
                text_start = self.at;
                in_expr = false;
                continue;
            }
            self.item()?;
        }
        if !in_expr && text_start < self.chars.len() {
            let seg: String = self.chars.get(text_start..).unwrap_or(&[]).iter().collect();
            self.push(ZenTokenKind::Text, seg);
        }
        self.push(ZenTokenKind::Template, "`".to_owned());
        Ok(())
    }

    fn identifier(&mut self) {
        let start = self.at;
        self.at = self.at.saturating_add(1);
        while self.peek(0).is_some_and(is_alphanumeric) {
            self.at = self.at.saturating_add(1);
        }
        let value: String = self
            .chars
            .get(start..self.at)
            .unwrap_or(&[])
            .iter()
            .collect();
        match value.as_str() {
            "and" | "or" | "in" => self.push(ZenTokenKind::Operator, value),
            "true" | "false" => self.push(ZenTokenKind::Boolean, value),
            "not" => {
                let next_four: String = self
                    .chars
                    .get(self.at..self.at.saturating_add(4))
                    .unwrap_or(&[])
                    .iter()
                    .collect();
                if next_four == " in " {
                    self.at = self.at.saturating_add(4);
                    self.push(ZenTokenKind::Operator, "not in".to_owned());
                } else {
                    self.push(ZenTokenKind::Operator, "not".to_owned());
                }
            }
            "$" | "$root" | "#" | "null" => self.push(ZenTokenKind::Identifier, value),
            _ => self.push(ZenTokenKind::Literal, value),
        }
    }
}

fn lex_zen(source: &str) -> Result<Vec<ZenToken>, ()> {
    Lexer::new(source).lex_all()
}

fn literal_path_root(node: &ZenNode) -> Option<&str> {
    match node {
        ZenNode::Identifier(name) => Some(name.as_str()),
        ZenNode::Parenthesized(inner) => literal_path_root(inner),
        ZenNode::Member { node, property } => {
            if matches!(property.as_ref(), ZenNode::String(_)) {
                literal_path_root(node)
            } else {
                None
            }
        }
        ZenNode::Leaf
        | ZenNode::Number(_)
        | ZenNode::Call { .. }
        | ZenNode::String(_)
        | ZenNode::Root
        | ZenNode::Pointer
        | ZenNode::Closure
        | ZenNode::Assignments
        | ZenNode::Unary(_)
        | ZenNode::List { .. } => None,
    }
}

pub(super) fn reads_actor(node: &ZenNode) -> bool {
    if let Some(root) = literal_path_root(node) {
        return root == "actor";
    }
    match node {
        ZenNode::Leaf | ZenNode::Number(_) | ZenNode::String(_) | ZenNode::Identifier(_) => false,
        ZenNode::Root | ZenNode::Pointer | ZenNode::Closure | ZenNode::Assignments => true,
        ZenNode::Parenthesized(inner) | ZenNode::Unary(inner) => reads_actor(inner),
        ZenNode::Member { node, property } => reads_actor(node) || reads_actor(property),
        ZenNode::List { parts, .. } => parts.iter().any(reads_actor),
        ZenNode::Call { args, .. } => args.iter().any(reads_actor),
    }
}

fn exact_zen_number(text: &str) -> bool {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (int_part, frac_part) = match unsigned.split_once('.') {
        Some((i, f)) => {
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            (i, f)
        }
        None => (unsigned, ""),
    };
    if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if int_part.len() > 1 && int_part.starts_with('0') {
        return false;
    }
    if frac_part.len() > 28 {
        return false;
    }
    let mut digits = String::with_capacity(int_part.len().saturating_add(frac_part.len()));
    digits.push_str(int_part);
    digits.push_str(frac_part);
    digits
        .parse::<u128>()
        .is_ok_and(|val| val <= MAX_DECIMAL_MANTISSA)
}

fn bounded_call(name: &str, args: &[ZenNode]) -> bool {
    let literal_le = |arg: Option<&ZenNode>, max: u64| -> bool {
        let Some(ZenNode::Number(text)) = arg else {
            return false;
        };
        !text.is_empty()
            && text.bytes().all(|b| b.is_ascii_digit())
            && text.parse::<u64>().is_ok_and(|val| val <= max)
    };
    match name {
        "tokens" => {
            args.len() == 4
                && matches!(args.get(1), Some(ZenNode::String(s)) if s == "alnum")
                && literal_le(args.get(2), 256)
                && literal_le(args.get(3), 256)
        }
        "take" => args.len() == 2 && literal_le(args.get(1), 16_384),
        "join" => {
            args.len() == 2 && matches!(args.get(1), Some(ZenNode::String(s)) if s.len() <= 256)
        }
        _ => true,
    }
}

pub(super) fn structural_bounds(root: &ZenNode) -> bool {
    let mut nodes: usize = 0;
    let mut stack: Vec<(&ZenNode, usize)> = vec![(root, 1)];
    while let Some((node, depth)) = stack.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > MAX_NODES || depth > MAX_DEPTH {
            return false;
        }
        let next_depth = depth.saturating_add(1);
        match node {
            ZenNode::Number(text) => {
                if !exact_zen_number(text) {
                    return false;
                }
            }
            ZenNode::String(val) => {
                if val.len() > MAX_STRING_BYTES {
                    return false;
                }
            }
            ZenNode::List { parts, literal } => {
                if let Some(container) = literal {
                    let count = match container {
                        LiteralContainer::Array => parts.len(),
                        LiteralContainer::Object => parts.len().checked_div(2).unwrap_or(0),
                    };
                    if count > MAX_LITERAL_ITEMS {
                        return false;
                    }
                }
                for child in parts {
                    stack.push((child, next_depth));
                }
            }
            ZenNode::Call { name, args } => {
                if !bounded_call(name, args) {
                    return false;
                }
                for child in args {
                    stack.push((child, next_depth));
                }
            }
            ZenNode::Parenthesized(inner) | ZenNode::Unary(inner) => {
                stack.push((inner, next_depth));
            }
            ZenNode::Member { node, property } => {
                stack.push((node, next_depth));
                stack.push((property, next_depth));
            }
            ZenNode::Leaf
            | ZenNode::Identifier(_)
            | ZenNode::Root
            | ZenNode::Pointer
            | ZenNode::Closure
            | ZenNode::Assignments => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{lexical_bounds, may_read_actor};

    #[test]
    fn detects_actor_reads_and_ignores_object_keys_and_strings() {
        assert!(may_read_actor("actor.subjectId == 'u1'"));
        assert!(may_read_actor("$root.foo == 1"));
        assert!(!may_read_actor("input.amount > 0"));
        assert!(!may_read_actor("{actor: 1}"));
        assert!(!may_read_actor("'actor.id' == input.label"));
        assert!(lexical_bounds("input.x + 1"));
    }
}
