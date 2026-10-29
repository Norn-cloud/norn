//! Literal values of object literals in a decide graph's expression sites.
//!
//! `norn-expr` does not expose its ZEN Expression AST. This small scanner
//! finds object literal boundaries; the expression compiler/evaluator parses
//! scalar literal tokens so escapes and exact numbers follow the admitted
//! language. A nested object literal is read as [`Value::Object`], and any
//! other expression is [`Value::Computed`]. Object literals inside computed
//! values (ternary branches, list items) are records too.
//!
//! A computed value is bounded by the output schema's enum, which the
//! evaluator enforces; a literal value is checked directly.

/// One token of the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token<'s> {
    Ident(&'s str),
    Str(String),
    Scalar(norn_ir::RuntimeValue),
    Punct(char),
    Other,
}

fn tokens(source: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut chars = source.char_indices().peekable();
    while let Some((start, character)) = chars.next() {
        match character {
            c if c.is_whitespace() => {}
            quote @ ('\'' | '"') => {
                let mut token_end = start.saturating_add(quote.len_utf8());
                let mut closed = false;
                for (index, next) in chars.by_ref() {
                    token_end = index.saturating_add(next.len_utf8());
                    // ZEN's string lexer closes at the next matching quote;
                    // backslashes are ordinary string characters.
                    if next == quote {
                        closed = true;
                        break;
                    }
                }
                out.push(if closed {
                    let literal = source.get(start..token_end).unwrap_or_default();
                    match evaluate_scalar_literal(literal) {
                        Some(norn_ir::RuntimeValue::String(value)) => Token::Str(value),
                        _ => Token::Other,
                    }
                } else {
                    Token::Other
                });
            }
            '`' => {
                while let Some((_, next)) = chars.next() {
                    match next {
                        '\\' => {
                            chars.next();
                        }
                        '`' => break,
                        _ => {}
                    }
                }
                out.push(Token::Other);
            }
            c if c.is_alphabetic() || c == '_' || c == '$' => {
                let mut end = start.saturating_add(c.len_utf8());
                while let Some(&(index, next)) = chars.peek() {
                    if next.is_alphanumeric() || next == '_' || next == '$' {
                        end = index.saturating_add(next.len_utf8());
                        chars.next();
                    } else {
                        break;
                    }
                }
                let token = source.get(start..end).unwrap_or_default();
                let scalar = matches!(token, "true" | "false" | "null")
                    .then(|| evaluate_scalar_literal(token))
                    .flatten();
                out.push(match scalar {
                    Some(
                        value @ (norn_ir::RuntimeValue::Bool(_) | norn_ir::RuntimeValue::Null),
                    ) => Token::Scalar(value),
                    _ => Token::Ident(token),
                });
            }
            c if c.is_ascii_digit() => {
                let mut end = start.saturating_add(c.len_utf8());
                while let Some(&(index, next)) = chars.peek() {
                    if next.is_alphanumeric() || next == '.' || next == '_' {
                        end = index.saturating_add(next.len_utf8());
                        chars.next();
                    } else {
                        break;
                    }
                }
                let token = source.get(start..end).unwrap_or_default();
                out.push(evaluate_scalar_literal(token).map_or(Token::Other, Token::Scalar));
            }
            c => out.push(Token::Punct(c)),
        }
    }
    out
}

/// Evaluate one scalar token with the decision expression engine. The empty
/// context is used only after tokenization has identified a literal; an
/// unbound identifier may evaluate to null and must never enter this path.
fn evaluate_scalar_literal(source: &str) -> Option<norn_ir::RuntimeValue> {
    let expression = norn_expr::Expression::compile(source).ok()?;
    let evaluation = expression
        .evaluate(
            &norn_expr::Value::object(std::iter::empty::<(&str, norn_expr::Value)>()),
            norn_expr::EvaluationBudget::new(64),
        )
        .ok()?;
    match evaluation.value() {
        norn_expr::Value::Null => Some(norn_ir::RuntimeValue::Null),
        norn_expr::Value::Bool(value) => Some(norn_ir::RuntimeValue::Bool(*value)),
        norn_expr::Value::Number(value) => {
            norn_ir::RuntimeValue::from_json_text(value.as_str()).ok()
        }
        norn_expr::Value::String(value) => Some(norn_ir::RuntimeValue::String(value.clone())),
        norn_expr::Value::Array(_) | norn_expr::Value::Object(_) => None,
    }
}

/// A member value of an object literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Value {
    /// Exactly one string literal.
    Str(String),
    /// Exactly one boolean, number, or `null` literal.
    Scalar(norn_ir::RuntimeValue),
    /// Exactly one object literal.
    Object(Record),
    /// Anything else.
    Computed,
}

/// One object literal: its members in source order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Record {
    members: Vec<(String, Value)>,
}

impl Record {
    /// Add a member while assembling the scalar columns of one table fact.
    pub(super) fn insert(&mut self, name: &str, value: Value) {
        if let Some((_, previous)) = self.members.iter_mut().find(|(member, _)| member == name) {
            *previous = value;
        } else {
            self.members.push((name.to_owned(), value));
        }
    }

    /// Reconstruct one dotted table column, within the scanner's depth limit.
    pub(super) fn insert_path(&mut self, path: &str, value: Value) {
        self.insert_path_at(path, value, MAX_DEPTH);
    }

    fn insert_path_at(&mut self, path: &str, value: Value, remaining: usize) {
        let Some((head, tail)) = path.split_once('.') else {
            self.insert(path, value);
            return;
        };
        if remaining == 0 {
            self.insert(head, Value::Computed);
            return;
        }
        let mut nested = match self.get(head) {
            Some(Value::Object(record)) => record.clone(),
            _ => Self::default(),
        };
        nested.insert_path_at(tail, value, remaining.saturating_sub(1));
        self.insert(head, Value::Object(nested));
    }

    /// The value of member `name`.
    pub(super) fn get(&self, name: &str) -> Option<&Value> {
        self.members
            .iter()
            .find(|(member, _)| member == name)
            .map(|(_, value)| value)
    }

    /// The string literal of member `name`.
    pub(super) fn literal(&self, name: &str) -> Option<&str> {
        match self.get(name) {
            Some(Value::Str(value)) => Some(value),
            _ => None,
        }
    }

    /// The record's members in source order.
    pub(super) fn entries(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.members
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    fn runtime_value(&self) -> Option<norn_ir::RuntimeValue> {
        let mut entries = std::collections::BTreeMap::new();
        for (name, value) in &self.members {
            entries.insert(name.clone(), value.runtime_value()?);
        }
        Some(norn_ir::RuntimeValue::Object(entries))
    }
}

impl Value {
    /// The exact runtime value when this member is a literal value.
    pub(super) fn runtime_value(&self) -> Option<norn_ir::RuntimeValue> {
        match self {
            Self::Str(value) => Some(norn_ir::RuntimeValue::String(value.clone())),
            Self::Scalar(value) => Some(value.clone()),
            Self::Object(record) => record.runtime_value(),
            Self::Computed => None,
        }
    }
}

/// Deepest object nesting the parse follows; deeper objects are computed.
const MAX_DEPTH: usize = 32;

struct Parser<'t, 's> {
    tokens: &'t [Token<'s>],
    pos: usize,
    records: Vec<Record>,
}

impl Parser<'_, '_> {
    fn peek(&self, ahead: usize) -> Option<&Token<'_>> {
        self.tokens.get(self.pos.saturating_add(ahead))
    }

    fn advance(&mut self) {
        self.pos = self.pos.saturating_add(1);
    }

    /// Every object literal, in the order they close.
    fn scan(&mut self) {
        while let Some(token) = self.peek(0) {
            if *token == Token::Punct('{') {
                let record = self.object(0);
                self.records.push(record);
            } else {
                self.advance();
            }
        }
    }

    /// The object literal at `{`; consumes through its `}`.
    fn object(&mut self, depth: usize) -> Record {
        self.advance();
        let mut record = Record::default();
        loop {
            match self.peek(0) {
                None => return record,
                Some(Token::Punct('}')) => {
                    self.advance();
                    return record;
                }
                Some(Token::Punct(',')) => self.advance(),
                Some(Token::Ident(_) | Token::Str(_))
                    if self.peek(1) == Some(&Token::Punct(':')) =>
                {
                    let key = match self.peek(0) {
                        Some(Token::Ident(name)) => (*name).to_owned(),
                        Some(Token::Str(name)) => name.clone(),
                        _ => String::new(),
                    };
                    self.advance();
                    self.advance();
                    let value = self.value(depth);
                    record.members.push((key, value));
                }
                Some(_) => self.skip_value(depth),
            }
        }
    }

    /// One member value; stops before the `,` or `}` that ends it.
    fn value(&mut self, depth: usize) -> Value {
        let ends =
            |token: Option<&Token<'_>>| matches!(token, None | Some(Token::Punct(',' | '}')));
        match self.peek(0) {
            Some(Token::Str(value)) if ends(self.peek(1)) => {
                let value = value.clone();
                self.advance();
                Value::Str(value)
            }
            Some(Token::Scalar(value)) if ends(self.peek(1)) => {
                let value = value.clone();
                self.advance();
                Value::Scalar(value)
            }
            Some(Token::Punct('-'))
                if matches!(
                    self.peek(1),
                    Some(Token::Scalar(norn_ir::RuntimeValue::Number(_)))
                ) && ends(self.peek(2)) =>
            {
                self.advance();
                let Some(Token::Scalar(norn_ir::RuntimeValue::Number(number))) = self.peek(0)
                else {
                    return Value::Computed;
                };
                let value =
                    norn_ir::RuntimeValue::from_json_text(format!("-{}", number.canonical()));
                self.advance();
                value.map_or(Value::Computed, Value::Scalar)
            }
            Some(Token::Punct('{')) if depth < MAX_DEPTH => {
                let record = self.object(depth.saturating_add(1));
                if ends(self.peek(0)) {
                    self.records.push(record.clone());
                    Value::Object(record)
                } else {
                    self.records.push(record);
                    self.skip_value(depth);
                    Value::Computed
                }
            }
            _ => {
                self.skip_value(depth);
                Value::Computed
            }
        }
    }

    /// Skip a computed value up to the `,` or `}` that ends it, reading the
    /// object literals inside it as records.
    fn skip_value(&mut self, depth: usize) {
        let mut nesting: usize = 0;
        while let Some(token) = self.peek(0) {
            match token {
                Token::Punct(',' | '}') if nesting == 0 => return,
                Token::Punct('{') if depth < MAX_DEPTH => {
                    let record = self.object(depth.saturating_add(1));
                    self.records.push(record);
                    continue;
                }
                Token::Punct('(' | '[') => nesting = nesting.saturating_add(1),
                Token::Punct(')' | ']') => nesting = nesting.saturating_sub(1),
                Token::Ident(_)
                | Token::Str(_)
                | Token::Scalar(_)
                | Token::Punct(_)
                | Token::Other => {}
            }
            self.advance();
        }
    }
}

/// Every object literal of `source`, nested ones included, in the order
/// they close (an inner object before the object that holds it).
pub(super) fn records(source: &str) -> Vec<Record> {
    let tokens = tokens(source);
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        records: Vec::new(),
    };
    parser.scan();
    parser.records
}

/// One complete scalar or object literal; other expressions stay computed.
pub(super) fn whole_value(source: &str) -> Value {
    let tokens = tokens(source);
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        records: Vec::new(),
    };
    let value = parser.value(0);
    if parser.pos == tokens.len() {
        value
    } else {
        Value::Computed
    }
}

/// The literal when `source` is exactly one string literal.
pub(super) fn whole_literal(source: &str) -> Option<String> {
    match tokens(source).as_slice() {
        [Token::Str(value)] => Some(value.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Record, Value, records, whole_literal};

    #[test]
    fn table_columns_preserve_nested_patch_members() {
        let mut patch = Record::default();
        patch.insert_path("metadata.city", Value::Str("Cairo".to_owned()));
        patch.insert_path("metadata.region", Value::Computed);
        let Some(Value::Object(metadata)) = patch.get("metadata") else {
            panic!("metadata is an object, not a dotted stored-property name");
        };
        assert_eq!(metadata.literal("city"), Some("Cairo"));
        assert_eq!(metadata.get("region"), Some(&Value::Computed));
        assert_eq!(patch.entries().count(), 1);
    }

    #[test]
    fn records_read_literal_members_and_nested_objects() {
        let source = r#"x ? [{op: 'create', 'objectType': 'a"b', key: k,
                      values: {status: 'draft', n: 1}}]
                      : [{op: 'transition', lifecycle: c ? 'a' : 'b', transition: 'x' + y,
                      note: `t: 'no'`, text: "op: 'no'"}]"#;
        norn_expr::Expression::compile(source).expect("the scanner fixture is valid ZEN");
        let found = records(source);
        assert_eq!(found.len(), 3);
        let create = &found[1];
        assert_eq!(create.literal("op"), Some("create"));
        assert_eq!(create.literal("objectType"), Some("a\"b"));
        assert_eq!(create.get("key"), Some(&Value::Computed));
        let Some(Value::Object(values)) = create.get("values") else {
            panic!("values is an object literal");
        };
        assert_eq!(values.literal("status"), Some("draft"));
        assert_eq!(
            values.get("n").and_then(Value::runtime_value),
            Some(norn_ir::RuntimeValue::from_json_text("1").expect("literal number"))
        );
        assert_eq!(found[0], *values);
        let transition = &found[2];
        assert_eq!(transition.literal("op"), Some("transition"));
        assert_eq!(transition.get("lifecycle"), Some(&Value::Computed));
        assert_eq!(transition.get("transition"), Some(&Value::Computed));
        assert_eq!(transition.literal("text"), Some("op: 'no'"));
        assert_eq!(transition.get("note"), Some(&Value::Computed));
    }

    #[test]
    fn objects_inside_computed_values_are_records() {
        let found = records("{f: c ? {op: 'set'} : {op: 'delete'}}");
        let ops: Vec<Option<&str>> = found.iter().map(|record| record.literal("op")).collect();
        assert_eq!(ops, [Some("set"), Some("delete"), None]);
    }

    #[test]
    fn whole_literal_is_one_string_token() {
        assert_eq!(whole_literal(" 'approve' ").as_deref(), Some("approve"));
        assert_eq!(whole_literal("'a' + 'b'"), None);
        assert_eq!(whole_literal("next"), None);
        assert_eq!(whole_literal(r"'a\b'").as_deref(), Some(r"a\b"));
    }
}
