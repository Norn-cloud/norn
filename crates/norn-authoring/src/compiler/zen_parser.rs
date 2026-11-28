//! ZEN recursive-descent parser and structural bounds checker split out of
//! [`super::zen_reads`] to keep every module under 800 LOC.

use std::collections::BTreeMap;

use super::zen_reads::{ZenToken, ZenTokenKind, reads_actor, structural_bounds};

fn is_internal_or_deprecated(name: &str) -> bool {
    matches!(
        name,
        "len"
            | "upper"
            | "lower"
            | "trim"
            | "trimStart"
            | "trimEnd"
            | "startsWith"
            | "endsWith"
            | "contains"
            | "matches"
            | "extract"
            | "split"
            | "tokens"
            | "take"
            | "join"
            | "flatten"
            | "keys"
            | "values"
            | "isNumeric"
            | "abs"
            | "sum"
            | "min"
            | "max"
            | "avg"
            | "median"
            | "mode"
            | "floor"
            | "ceil"
            | "round"
            | "trunc"
            | "rand"
            | "string"
            | "number"
            | "bool"
            | "type"
            | "fuzzyMatch"
            | "d"
            | "date"
            | "timestamp"
            | "days_between"
            | "add_days"
            | "seconds_between"
            | "add_seconds"
            | "intervals_overlap"
            | "interval_gap_seconds"
            | "interval_intersection_seconds"
            | "resolve_local_timestamp"
            | "time"
            | "duration"
            | "year"
            | "month"
            | "day"
            | "dayOfYear"
            | "weekday"
            | "week"
            | "hour"
            | "minute"
            | "second"
            | "timestampMillis"
            | "startOf"
            | "endOf"
            | "add"
            | "sub"
            | "diff"
            | "format"
            | "tz"
    )
}

fn is_closure_builtin(name: &str) -> bool {
    matches!(
        name,
        "all" | "some" | "none" | "one" | "filter" | "map" | "flatMap" | "count"
    )
}

fn is_date_method(name: &str) -> bool {
    matches!(
        name,
        "add"
            | "sub"
            | "diff"
            | "set"
            | "startOf"
            | "endOf"
            | "format"
            | "year"
            | "month"
            | "day"
            | "dayOfYear"
            | "weekday"
            | "week"
            | "hour"
            | "minute"
            | "second"
            | "timestamp"
            | "tz"
            | "isLeapYear"
            | "isSame"
            | "isBefore"
            | "isAfter"
            | "isSameOrBefore"
            | "isSameOrAfter"
            | "isYesterday"
            | "isToday"
            | "isTomorrow"
            | "quarter"
            | "daysInMonth"
            | "secondOfDay"
            | "minuteOfDay"
    )
}

fn binary_precedence(op: &str) -> Option<(u8, bool)> {
    match op {
        "or" => Some((10, false)),
        "and" => Some((15, false)),
        "==" | "!=" | "<" | ">" | "<=" | ">=" | "not in" | "in" => Some((20, false)),
        "+" | "-" => Some((30, false)),
        "*" | "/" | "%" => Some((60, false)),
        "^" => Some((70, true)),
        "??" => Some((80, false)),
        _ => None,
    }
}

fn unary_precedence(op: &str) -> Option<u8> {
    match op {
        "not" | "!" => Some(50),
        "+" | "-" => Some(200),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LiteralContainer {
    Array,
    Object,
}

#[derive(Clone, Debug)]
pub(super) enum ZenNode {
    Leaf,
    Number(String),
    Call {
        name: String,
        args: Vec<Self>,
    },
    Identifier(String),
    String(String),
    Root,
    Pointer,
    Closure,
    Assignments,
    Parenthesized(Box<Self>),
    Unary(Box<Self>),
    Member {
        node: Box<Self>,
        property: Box<Self>,
    },
    List {
        parts: Vec<Self>,
        literal: Option<LiteralContainer>,
    },
}

/// Parses `tokens`, checks structural bounds, and reports whether the AST may read `actor`.
pub(crate) fn parse_and_check_actor(tokens: Vec<ZenToken>) -> bool {
    let mut parser = Parser::new(tokens);
    let Ok(root) = parser.parse() else {
        return true;
    };
    if !structural_bounds(&root) {
        return true;
    }
    reads_actor(&root)
}

struct Parser {
    tokens: Vec<ZenToken>,
    at: usize,
    depth: usize,
    has_range: bool,
    memo: BTreeMap<(usize, usize), Result<(ZenNode, usize), ()>>,
}

impl Parser {
    fn new(tokens: Vec<ZenToken>) -> Self {
        let has_range = tokens
            .iter()
            .any(|t| t.kind == ZenTokenKind::Operator && t.value == "..");
        Self {
            tokens,
            at: 0,
            depth: 0,
            has_range,
            memo: BTreeMap::new(),
        }
    }

    fn current(&self) -> Option<&ZenToken> {
        self.tokens.get(self.at)
    }

    fn is_tok(&self, kind: ZenTokenKind, value: Option<&str>) -> bool {
        self.current()
            .is_some_and(|t| t.kind == kind && value.is_none_or(|v| t.value == v))
    }

    fn consume_tok(&mut self, kind: ZenTokenKind, value: &str) -> Result<(), ()> {
        if !self.is_tok(kind, Some(value)) {
            return Err(());
        }
        self.at = self.at.saturating_add(1);
        Ok(())
    }

    fn parse(&mut self) -> Result<ZenNode, ()> {
        let root = self.binary(0)?;
        if self.at < self.tokens.len() {
            return Err(());
        }
        Ok(root)
    }

    fn binary(&mut self, precedence: u8) -> Result<ZenNode, ()> {
        if precedence != 0 {
            return self.binary_at(precedence);
        }
        let key = (self.at, self.depth);
        if let Some(cached) = self.memo.get(&key).cloned() {
            return match cached {
                Ok((node, end)) => {
                    self.at = end;
                    Ok(node)
                }
                Err(()) => Err(()),
            };
        }
        match self.binary_at(0) {
            Ok(node) => {
                self.memo.insert(key, Ok((node.clone(), self.at)));
                Ok(node)
            }
            Err(()) => {
                self.memo.insert(key, Err(()));
                Err(())
            }
        }
    }

    fn binary_at(&mut self, precedence: u8) -> Result<ZenNode, ()> {
        let mut left = self.unary()?;
        loop {
            let Some(tok) = self.current() else {
                break;
            };
            if tok.kind != ZenTokenKind::Operator {
                break;
            }
            let Some((prec, right_assoc)) = binary_precedence(&tok.value) else {
                break;
            };
            if prec < precedence {
                break;
            }
            self.at = self.at.saturating_add(1);
            let next_prec = if right_assoc {
                prec
            } else {
                prec.saturating_add(1)
            };
            let right = self.binary(next_prec)?;
            left = ZenNode::List {
                parts: vec![left, right],
                literal: None,
            };
        }
        if precedence == 0 && self.is_tok(ZenTokenKind::Operator, Some("?")) {
            self.at = self.at.saturating_add(1);
            let on_true = self.binary(0)?;
            self.consume_tok(ZenTokenKind::Operator, ":")?;
            let on_false = self.binary(0)?;
            left = ZenNode::List {
                parts: vec![left, on_true, on_false],
                literal: None,
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<ZenNode, ()> {
        let tok = self.current().cloned().ok_or(())?;
        if self.depth > 0 && tok.kind == ZenTokenKind::Identifier && tok.value == "#" {
            self.at = self.at.saturating_add(1);
            return self.postfix(ZenNode::Pointer);
        }
        if tok.kind == ZenTokenKind::Operator {
            let prec = unary_precedence(&tok.value).ok_or(())?;
            self.at = self.at.saturating_add(1);
            let inner = self.binary(prec)?;
            return Ok(ZenNode::Unary(Box::new(inner)));
        }
        if let Some(interval) = self.interval() {
            return Ok(interval);
        }
        if tok.kind == ZenTokenKind::Bracket && tok.value == "(" {
            self.at = self.at.saturating_add(1);
            let inner = self.binary(0)?;
            self.consume_tok(ZenTokenKind::Bracket, ")")?;
            return self.postfix(ZenNode::Parenthesized(Box::new(inner)));
        }
        self.literal()
    }

    fn interval(&mut self) -> Option<ZenNode> {
        if !self.has_range || !self.is_tok(ZenTokenKind::Bracket, None) {
            return None;
        }
        let start = self.at;
        self.at = self.at.saturating_add(1);
        let parsed = (|| -> Result<ZenNode, ()> {
            let left = self.binary(0)?;
            self.consume_tok(ZenTokenKind::Operator, "..")?;
            let right = self.binary(0)?;
            if !self.is_tok(ZenTokenKind::Bracket, None) {
                return Err(());
            }
            self.at = self.at.saturating_add(1);
            self.postfix(ZenNode::List {
                parts: vec![left, right],
                literal: None,
            })
        })();
        match parsed {
            Ok(node) => Some(node),
            Err(()) => {
                self.at = start;
                None
            }
        }
    }

    fn literal(&mut self) -> Result<ZenNode, ()> {
        let tok = self.current().cloned().ok_or(())?;
        match tok.kind {
            ZenTokenKind::Identifier => {
                if tok.value == "null" {
                    self.at = self.at.saturating_add(1);
                    Ok(ZenNode::Leaf)
                } else {
                    self.identifier()
                }
            }
            ZenTokenKind::Literal => self.identifier(),
            ZenTokenKind::Boolean => {
                self.at = self.at.saturating_add(1);
                Ok(ZenNode::Leaf)
            }
            ZenTokenKind::Number => {
                self.at = self.at.saturating_add(1);
                Ok(ZenNode::Number(tok.value))
            }
            ZenTokenKind::String => {
                self.at = self.at.saturating_add(1);
                Ok(ZenNode::String(tok.value))
            }
            ZenTokenKind::Template => {
                if tok.value == "`" {
                    self.template()
                } else {
                    Err(())
                }
            }
            ZenTokenKind::Bracket => match tok.value.as_str() {
                "(" | ")" | "]" => self.interval().ok_or(()),
                "[" => match self.interval() {
                    Some(node) => Ok(node),
                    None => self.array(),
                },
                "{" => self.object(),
                _ => Err(()),
            },
            ZenTokenKind::Operator | ZenTokenKind::Text => Err(()),
        }
    }

    fn identifier(&mut self) -> Result<ZenNode, ()> {
        let name = self.current().ok_or(())?.value.clone();
        self.at = self.at.saturating_add(1);
        if !self.is_tok(ZenTokenKind::Bracket, Some("(")) {
            let base = if name == "$root" {
                ZenNode::Root
            } else {
                ZenNode::Identifier(name)
            };
            let node = self.postfix(base)?;
            if self.is_tok(ZenTokenKind::Operator, Some("=")) {
                return Ok(ZenNode::Assignments);
            }
            return Ok(node);
        }
        if !is_internal_or_deprecated(&name) && !is_closure_builtin(&name) {
            return Err(());
        }
        self.at = self.at.saturating_add(1);
        let mut parts = Vec::new();
        if is_closure_builtin(&name) {
            parts.push(self.binary(0)?);
            if self.is_tok(ZenTokenKind::Literal, Some("as")) {
                self.at = self.at.saturating_add(1);
                if !self.is_tok(ZenTokenKind::Literal, None) {
                    return Err(());
                }
                self.at = self.at.saturating_add(1);
            }
            self.consume_tok(ZenTokenKind::Operator, ",")?;
            self.depth = self.depth.saturating_add(1);
            let res = self.binary(0);
            self.depth = self.depth.saturating_sub(1);
            let _ = res?;
            parts.push(ZenNode::Closure);
            self.consume_tok(ZenTokenKind::Bracket, ")")?;
        } else {
            parts.extend(self.arguments()?);
        }
        self.postfix(ZenNode::Call { name, args: parts })
    }

    fn arguments(&mut self) -> Result<Vec<ZenNode>, ()> {
        let mut parts = Vec::new();
        while !self.is_tok(ZenTokenKind::Bracket, Some(")")) {
            parts.push(self.binary(0)?);
            if !self.is_tok(ZenTokenKind::Operator, Some(",")) {
                break;
            }
            self.at = self.at.saturating_add(1);
        }
        self.consume_tok(ZenTokenKind::Bracket, ")")?;
        Ok(parts)
    }

    fn postfix(&mut self, mut node: ZenNode) -> Result<ZenNode, ()> {
        loop {
            if self.is_tok(ZenTokenKind::Operator, Some(".")) {
                node = self.postfix_dot(node)?;
                continue;
            }
            if self.is_tok(ZenTokenKind::Bracket, Some("[")) {
                node = self.postfix_index(node)?;
                continue;
            }
            return Ok(node);
        }
    }

    fn postfix_dot(&mut self, node: ZenNode) -> Result<ZenNode, ()> {
        self.at = self.at.saturating_add(1);
        let property = self.current().cloned().ok_or(())?;
        self.at = self.at.saturating_add(1);
        let valid = matches!(
            property.kind,
            ZenTokenKind::Identifier | ZenTokenKind::Literal
        ) || (property.kind == ZenTokenKind::Operator
            && matches!(
                property.value.as_str(),
                "and" | "or" | "not" | "!" | "??" | "in"
            ));
        if !valid {
            return Err(());
        }
        if self.is_tok(ZenTokenKind::Bracket, Some("(")) {
            if !is_date_method(&property.value) {
                return Err(());
            }
            self.at = self.at.saturating_add(1);
            let mut parts = vec![node];
            parts.extend(self.arguments()?);
            Ok(ZenNode::List {
                parts,
                literal: None,
            })
        } else {
            Ok(ZenNode::Member {
                node: Box::new(node),
                property: Box::new(ZenNode::String(property.value)),
            })
        }
    }

    fn postfix_index(&mut self, node: ZenNode) -> Result<ZenNode, ()> {
        self.at = self.at.saturating_add(1);
        if self.current().is_none() {
            return Err(());
        }
        if self.is_tok(ZenTokenKind::Operator, Some(":")) {
            self.at = self.at.saturating_add(1);
            let mut parts = vec![node];
            if !self.is_tok(ZenTokenKind::Bracket, Some("]")) {
                parts.push(self.binary(0)?);
            }
            self.consume_tok(ZenTokenKind::Bracket, "]")?;
            return Ok(ZenNode::List {
                parts,
                literal: None,
            });
        }
        let from = self.binary(0)?;
        if self.current().is_none() {
            return Err(());
        }
        if self.is_tok(ZenTokenKind::Operator, Some(":")) {
            self.at = self.at.saturating_add(1);
            if self.current().is_none() {
                return Err(());
            }
            let mut parts = vec![node, from];
            if !self.is_tok(ZenTokenKind::Bracket, Some("]")) {
                parts.push(self.binary(0)?);
            }
            self.consume_tok(ZenTokenKind::Bracket, "]")?;
            return Ok(ZenNode::List {
                parts,
                literal: None,
            });
        }
        self.consume_tok(ZenTokenKind::Bracket, "]")?;
        Ok(ZenNode::Member {
            node: Box::new(node),
            property: Box::new(from),
        })
    }

    fn array(&mut self) -> Result<ZenNode, ()> {
        self.consume_tok(ZenTokenKind::Bracket, "[")?;
        let mut parts = Vec::new();
        while !self.is_tok(ZenTokenKind::Bracket, Some("]")) {
            if !parts.is_empty() {
                self.consume_tok(ZenTokenKind::Operator, ",")?;
                if self.is_tok(ZenTokenKind::Bracket, Some("]")) {
                    break;
                }
            }
            parts.push(self.binary(0)?);
        }
        self.consume_tok(ZenTokenKind::Bracket, "]")?;
        self.postfix(ZenNode::List {
            parts,
            literal: Some(LiteralContainer::Array),
        })
    }

    fn object(&mut self) -> Result<ZenNode, ()> {
        self.consume_tok(ZenTokenKind::Bracket, "{")?;
        let mut parts = Vec::new();
        if self.is_tok(ZenTokenKind::Bracket, Some("}")) {
            self.at = self.at.saturating_add(1);
            return Ok(ZenNode::List {
                parts,
                literal: Some(LiteralContainer::Object),
            });
        }
        loop {
            parts.push(self.object_key()?);
            self.consume_tok(ZenTokenKind::Operator, ":")?;
            parts.push(self.binary(0)?);
            let Some(tok) = self.current() else {
                break;
            };
            if tok.kind == ZenTokenKind::Operator && tok.value == "," {
                self.at = self.at.saturating_add(1);
                continue;
            }
            if tok.kind == ZenTokenKind::Bracket && tok.value == "}" {
                break;
            }
            return Err(());
        }
        self.consume_tok(ZenTokenKind::Bracket, "}")?;
        Ok(ZenNode::List {
            parts,
            literal: Some(LiteralContainer::Object),
        })
    }

    fn object_key(&mut self) -> Result<ZenNode, ()> {
        let tok = self.current().cloned().ok_or(())?;
        match tok.kind {
            ZenTokenKind::Identifier
            | ZenTokenKind::Boolean
            | ZenTokenKind::Number
            | ZenTokenKind::Literal
            | ZenTokenKind::String => {
                self.at = self.at.saturating_add(1);
                Ok(ZenNode::String(tok.value))
            }
            ZenTokenKind::Bracket => {
                if tok.value != "[" {
                    return Err(());
                }
                self.at = self.at.saturating_add(1);
                let key = self.binary(0)?;
                self.consume_tok(ZenTokenKind::Bracket, "]")?;
                Ok(key)
            }
            ZenTokenKind::Operator | ZenTokenKind::Template | ZenTokenKind::Text => Err(()),
        }
    }

    fn template(&mut self) -> Result<ZenNode, ()> {
        self.consume_tok(ZenTokenKind::Template, "`")?;
        let mut parts = Vec::new();
        while !self.is_tok(ZenTokenKind::Template, Some("`")) {
            let tok = self.current().cloned().ok_or(())?;
            if tok.kind == ZenTokenKind::Text {
                self.at = self.at.saturating_add(1);
                parts.push(ZenNode::String(tok.value));
                continue;
            }
            if tok.kind == ZenTokenKind::Template && tok.value == "}" {
                self.at = self.at.saturating_add(1);
                continue;
            }
            if !(tok.kind == ZenTokenKind::Template && tok.value == "${") {
                return Err(());
            }
            self.at = self.at.saturating_add(1);
            parts.push(self.binary(0)?);
            self.consume_tok(ZenTokenKind::Template, "}")?;
        }
        self.consume_tok(ZenTokenKind::Template, "`")?;
        Ok(ZenNode::List {
            parts,
            literal: None,
        })
    }
}
