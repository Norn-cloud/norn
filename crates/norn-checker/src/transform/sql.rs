//! VIEW-SQL-1: a restricted SQL dialect as an authoring surface for `ViewIR`
//! v1. SQL never runs: it is parsed here, every name is resolved against the
//! effective catalog, and it lowers to the closed operator plan the checker
//! then types like any other transform. Anything without a typed operator
//! refuses `derived.validation.sql_unsupported` and is never passed through.
//!
//! The dialect: `SELECT … FROM type [AS a] [[LEFT|INNER] JOIN type [AS b] ON …]
//! [WHERE …] [GROUP BY …]`, joined by `UNION ALL`. A join's `ON` names a
//! declared link (`a.customer = b.id`) or a declared key of the joined type
//! (`l.email = c.email`). Aggregates are `SUM`, `COUNT`, `COUNT(DISTINCT …)`,
//! `MIN` and `MAX`; `DATE_TRUNC('week', col)` buckets a group key; windows are
//! `ROW_NUMBER`, `RANK`, `DENSE_RANK`, `LAG`, `LEAD` and a running `SUM … OVER`.
//! Expressions are comparisons, `AND`/`OR`/`NOT`, `+ - *`, `IS [NOT] NULL`,
//! `COALESCE` and `CASE WHEN … THEN … ELSE … END`. Rows have no SQL order: a
//! transform's rows are ordered by their grain, so `ORDER BY` and `LIMIT`
//! outside a window refuse.
//!
//! Nothing is guessed. A construct whose SQL meaning the plan would not keep
//! refuses rather than lowering to something close: division (SQL truncates
//! integers), a comparison with a `NULL` literal (never true in SQL), a
//! constant or position as a group or window key (`GROUP BY 1`), and a column
//! or alias that is not a plain identifier (it becomes a ZEN path).

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use norn_ir::{ArtifactEnvelope, PropertyTypeName};
use std::collections::BTreeSet;

use crate::entity_instance::EffectiveCatalog;
use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

/// The refusal of anything this dialect does not lower to a typed operator.
pub const SQL_UNSUPPORTED: &str = "derived.validation.sql_unsupported";

/// A lowered SQL transform: its inputs and the relational plan.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlPlan {
    /// Input alias → object type name, in `FROM`/`JOIN` order.
    pub inputs: Vec<(String, String)>,
    /// `calc.nodes` of the relational plan.
    pub nodes: Value,
    /// `calc.output`.
    pub output: String,
}

/// Why and where the dialect refuses; a diagnostic only at the boundary.
#[derive(Debug)]
struct Refusal {
    message: String,
    at: usize,
}

impl Refusal {
    fn into_diagnostic(self) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::new(SQL_UNSUPPORTED),
            severity: DiagnosticSeverity::Error,
            message: self.message,
            span: Some(Span::pointer(format!("/sql/{}", self.at))),
            artifact_id: None,
            remediation: Some(DiagnosticRemediation::FixDefinition),
        }
    }
}

fn refusal(message: impl Into<String>, at: usize) -> Refusal {
    Refusal {
        message: message.into(),
        at,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Quoted(String),
    Number(String),
    Text(String),
    Symbol(&'static str),
}

/// Tokens with their byte offsets.
fn tokenize(source: &str) -> Result<Vec<(Token, usize)>, Refusal> {
    const SYMBOLS: [&str; 15] = [
        "<>", "!=", "<=", ">=", "=", "<", ">", "+", "-", "*", "/", "(", ")", ",", ".",
    ];
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(c) = rest.chars().next() {
        let at = source.len().saturating_sub(rest.len());
        if c.is_whitespace() {
            rest = rest.get(c.len_utf8()..).unwrap_or_default();
        } else if rest.starts_with("--") {
            rest = rest
                .find('\n')
                .and_then(|end| rest.get(end..))
                .unwrap_or_default();
        } else if c == ';' && rest.get(1..).unwrap_or_default().trim().is_empty() {
            rest = "";
        } else if c.is_ascii_alphabetic() || c == '_' {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            out.push((
                Token::Word(rest.get(..end).unwrap_or_default().to_owned()),
                at,
            ));
            rest = rest.get(end..).unwrap_or_default();
        } else if c.is_ascii_digit() {
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(rest.len());
            let number = rest.get(..end).unwrap_or_default();
            let glued = rest
                .get(end..)
                .and_then(|after| after.chars().next())
                .is_some_and(|next| next.is_ascii_alphanumeric() || next == '_');
            if number.ends_with('.') || number.matches('.').count() > 1 || glued {
                return Err(refusal(
                    "a number is digits with an optional fraction (no exponent, radix or suffix)",
                    at,
                ));
            }
            out.push((Token::Number(number.to_owned()), at));
            rest = rest.get(end..).unwrap_or_default();
        } else if c == '\'' || c == '"' {
            let mut text = String::new();
            let mut chars = rest.char_indices().skip(1).peekable();
            let mut closed = None;
            while let Some((index, d)) = chars.next() {
                if d == c {
                    if chars.peek().is_some_and(|(_, e)| *e == c) {
                        chars.next();
                        text.push(c);
                        continue;
                    }
                    closed = Some(index);
                    break;
                }
                text.push(d);
            }
            let end = closed.ok_or_else(|| refusal("an unterminated quote", at))?;
            out.push((
                if c == '\'' {
                    Token::Text(text)
                } else {
                    Token::Quoted(text)
                },
                at,
            ));
            rest = rest.get(end.saturating_add(1)..).unwrap_or_default();
        } else if let Some(symbol) = SYMBOLS.iter().find(|s| rest.starts_with(**s)) {
            out.push((Token::Symbol(symbol), at));
            rest = rest.get(symbol.len()..).unwrap_or_default();
        } else {
            return Err(refusal(format!("{c:?} is not part of the dialect"), at));
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
enum Ex {
    Column(Option<String>, String),
    Number(String),
    Text(String),
    Bool(bool),
    Null,
    Not(Box<Ex>),
    Negate(Box<Ex>),
    Binary(&'static str, Box<Ex>, Box<Ex>),
    IsNull(Box<Ex>, bool),
    Case(Box<Ex>, Box<Ex>, Box<Ex>),
    Coalesce(Vec<Ex>),
    Call(Call),
}

#[derive(Debug, Clone)]
struct Call {
    name: String,
    args: Vec<Ex>,
    star: bool,
    distinct: bool,
    over: Option<Over>,
    at: usize,
}

/// One call is the same expression wherever it is written: `DATE_TRUNC(…)`
/// in the select list matches the same call in GROUP BY.
impl PartialEq for Call {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.args == other.args
            && self.star == other.star
            && self.distinct == other.distinct
            && self.over == other.over
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Over {
    partition: Vec<Ex>,
    order: Vec<(Ex, bool)>,
}

#[derive(Debug, Clone, PartialEq)]
struct Item {
    ex: Ex,
    alias: Option<String>,
    star: bool,
    at: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct Source {
    name: String,
    alias: String,
}

#[derive(Debug, Clone, PartialEq)]
struct JoinClause {
    source: Source,
    left: bool,
    on: Ex,
    at: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct Select {
    items: Vec<Item>,
    from: Source,
    joins: Vec<JoinClause>,
    filter: Option<Ex>,
    group_by: Vec<(Ex, usize)>,
}

struct Parser {
    tokens: Vec<(Token, usize)>,
    position: usize,
    end: usize,
}

impl Parser {
    fn at(&self) -> usize {
        self.tokens
            .get(self.position)
            .map_or(self.end, |(_, at)| *at)
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position).map(|(token, _)| token)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self
            .tokens
            .get(self.position)
            .map(|(token, _)| token.clone());
        if token.is_some() {
            self.position = self.position.saturating_add(1);
        }
        token
    }

    fn keyword(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Word(w)) if w.eq_ignore_ascii_case(word))
    }

    fn eat_keyword(&mut self, word: &str) -> bool {
        let found = self.keyword(word);
        if found {
            self.next();
        }
        found
    }

    fn expect_keyword(&mut self, word: &str) -> Result<(), Refusal> {
        if self.eat_keyword(word) {
            Ok(())
        } else {
            Err(refusal(format!("expected {word}"), self.at()))
        }
    }

    fn symbol(&self, symbol: &str) -> bool {
        matches!(self.peek(), Some(Token::Symbol(s)) if *s == symbol)
    }

    fn eat_symbol(&mut self, symbol: &str) -> bool {
        let found = self.symbol(symbol);
        if found {
            self.next();
        }
        found
    }

    fn expect_symbol(&mut self, symbol: &str) -> Result<(), Refusal> {
        if self.eat_symbol(symbol) {
            Ok(())
        } else {
            Err(refusal(format!("expected `{symbol}`"), self.at()))
        }
    }

    fn identifier(&mut self) -> Result<String, Refusal> {
        let at = self.at();
        match self.next() {
            Some(Token::Word(word)) if !reserved(&word) => Ok(word),
            Some(Token::Word(word)) => Err(refusal(
                format!("{} is not a name here", word.to_ascii_uppercase()),
                at,
            )),
            Some(Token::Quoted(name)) => Ok(name),
            Some(Token::Number(_) | Token::Text(_) | Token::Symbol(_)) | None => {
                Err(refusal("expected a name", at))
            }
        }
    }

    /// A column or alias name. It becomes part of a ZEN path, so it is a
    /// plain identifier, quoted or not.
    fn name(&mut self) -> Result<String, Refusal> {
        let at = self.at();
        let name = self.identifier()?;
        plain(&name, at)?;
        Ok(name)
    }

    fn query(&mut self) -> Result<Vec<Select>, Refusal> {
        let mut selects = vec![self.select()?];
        while self.eat_keyword("union") {
            if !self.eat_keyword("all") {
                return Err(refusal(
                    "UNION without ALL removes duplicates, which has no typed operator; write UNION ALL and dedupe by a key",
                    self.at(),
                ));
            }
            selects.push(self.select()?);
        }
        if self.keyword("order") || self.keyword("limit") {
            return Err(refusal(
                "a transform's rows are ordered by their grain; ORDER BY and LIMIT belong to the read, not the definition",
                self.at(),
            ));
        }
        if self.peek().is_some() {
            return Err(refusal("unexpected text after the query", self.at()));
        }
        Ok(selects)
    }

    fn source(&mut self) -> Result<Source, Refusal> {
        let name = self.identifier()?;
        let name = if self.eat_symbol(".") {
            format!("{name}.{}", self.identifier()?)
        } else {
            name
        };
        self.eat_keyword("as");
        let alias = if matches!(self.peek(), Some(Token::Word(w)) if !reserved(w))
            || matches!(self.peek(), Some(Token::Quoted(_)))
        {
            self.name()?
        } else {
            let alias = name
                .rsplit('.')
                .next()
                .unwrap_or(&name)
                .to_ascii_lowercase();
            if plain(&alias, self.at()).is_err() {
                return Err(refusal(
                    format!("name the source {name:?} with AS"),
                    self.at(),
                ));
            }
            alias
        };
        Ok(Source { name, alias })
    }

    fn select(&mut self) -> Result<Select, Refusal> {
        self.expect_keyword("select")?;
        if self.keyword("distinct") {
            return Err(refusal(
                "SELECT DISTINCT has no typed operator; dedupe by a key",
                self.at(),
            ));
        }
        let mut items = Vec::new();
        loop {
            let at = self.at();
            if self.eat_symbol("*") {
                items.push(Item {
                    ex: Ex::Null,
                    alias: None,
                    star: true,
                    at,
                });
            } else {
                let ex = self.expression()?;
                let alias = if self.eat_keyword("as") {
                    Some(self.name()?)
                } else {
                    None
                };
                items.push(Item {
                    ex,
                    alias,
                    star: false,
                    at,
                });
            }
            if !self.eat_symbol(",") {
                break;
            }
        }
        self.expect_keyword("from")?;
        let from = self.source()?;
        let mut joins = Vec::new();
        loop {
            let at = self.at();
            let left = if self.eat_keyword("left") {
                self.eat_keyword("outer");
                true
            } else {
                self.eat_keyword("inner");
                false
            };
            if !self.eat_keyword("join") {
                if left {
                    return Err(refusal("expected JOIN", self.at()));
                }
                break;
            }
            let source = self.source()?;
            self.expect_keyword("on")?;
            let on = self.expression()?;
            joins.push(JoinClause {
                source,
                left,
                on,
                at,
            });
        }
        let filter = if self.eat_keyword("where") {
            Some(self.expression()?)
        } else {
            None
        };
        let mut group_by = Vec::new();
        if self.eat_keyword("group") {
            self.expect_keyword("by")?;
            loop {
                let at = self.at();
                let key = self.expression()?;
                if constant(&key) {
                    return Err(refusal(
                        "GROUP BY names a column or an expression; a position or a constant is not a key",
                        at,
                    ));
                }
                group_by.push((key, at));
                if !self.eat_symbol(",") {
                    break;
                }
            }
        }
        if self.keyword("having") {
            return Err(refusal(
                "HAVING: filter the grouped rows in a second transform",
                self.at(),
            ));
        }
        Ok(Select {
            items,
            from,
            joins,
            filter,
            group_by,
        })
    }

    fn expression(&mut self) -> Result<Ex, Refusal> {
        let mut left = self.conjunction()?;
        while self.eat_keyword("or") {
            left = Ex::Binary("or", Box::new(left), Box::new(self.conjunction()?));
        }
        Ok(left)
    }

    fn conjunction(&mut self) -> Result<Ex, Refusal> {
        let mut left = self.negation()?;
        while self.eat_keyword("and") {
            left = Ex::Binary("and", Box::new(left), Box::new(self.negation()?));
        }
        Ok(left)
    }

    fn negation(&mut self) -> Result<Ex, Refusal> {
        if self.eat_keyword("not") {
            return Ok(Ex::Not(Box::new(self.negation()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Ex, Refusal> {
        let at = self.at();
        let left = self.sum()?;
        if self.eat_keyword("is") {
            let negated = self.eat_keyword("not");
            self.expect_keyword("null")?;
            return Ok(Ex::IsNull(Box::new(left), negated));
        }
        for (symbol, zen) in [
            ("=", "=="),
            ("<>", "!="),
            ("!=", "!="),
            ("<=", "<="),
            (">=", ">="),
            ("<", "<"),
            (">", ">"),
        ] {
            if self.eat_symbol(symbol) {
                let right = self.sum()?;
                if matches!(left, Ex::Null) || matches!(right, Ex::Null) {
                    return Err(refusal(
                        "a comparison with NULL is never true in SQL; write IS [NOT] NULL",
                        at,
                    ));
                }
                return Ok(Ex::Binary(zen, Box::new(left), Box::new(right)));
            }
        }
        if self.keyword("in") || self.keyword("like") || self.keyword("between") {
            return Err(refusal(
                "IN, LIKE and BETWEEN are not in the dialect yet; compare explicitly",
                self.at(),
            ));
        }
        Ok(left)
    }

    fn sum(&mut self) -> Result<Ex, Refusal> {
        let mut left = self.product()?;
        loop {
            if self.eat_symbol("+") {
                left = Ex::Binary("+", Box::new(left), Box::new(self.product()?));
            } else if self.eat_symbol("-") {
                left = Ex::Binary("-", Box::new(left), Box::new(self.product()?));
            } else {
                return Ok(left);
            }
        }
    }

    fn product(&mut self) -> Result<Ex, Refusal> {
        let mut left = self.unary()?;
        loop {
            if self.eat_symbol("*") {
                left = Ex::Binary("*", Box::new(left), Box::new(self.unary()?));
            } else if self.symbol("/") {
                return Err(refusal(
                    "division has no typed operator in the dialect: SQL truncates integer division and its scale is the engine's",
                    self.at(),
                ));
            } else {
                return Ok(left);
            }
        }
    }

    fn unary(&mut self) -> Result<Ex, Refusal> {
        if self.eat_symbol("-") {
            return Ok(Ex::Negate(Box::new(self.unary()?)));
        }
        self.primary()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one primary-expression grammar, read top to bottom"
    )]
    fn primary(&mut self) -> Result<Ex, Refusal> {
        let at = self.at();
        if self.eat_symbol("(") {
            let inner = self.expression()?;
            self.expect_symbol(")")?;
            return Ok(inner);
        }
        match self.peek().cloned() {
            Some(Token::Number(number)) => {
                self.next();
                Ok(Ex::Number(number))
            }
            Some(Token::Text(text)) => {
                self.next();
                Ok(Ex::Text(text))
            }
            Some(Token::Word(word)) if word.eq_ignore_ascii_case("true") => {
                self.next();
                Ok(Ex::Bool(true))
            }
            Some(Token::Word(word)) if word.eq_ignore_ascii_case("false") => {
                self.next();
                Ok(Ex::Bool(false))
            }
            Some(Token::Word(word)) if word.eq_ignore_ascii_case("null") => {
                self.next();
                Ok(Ex::Null)
            }
            Some(Token::Word(word)) if word.eq_ignore_ascii_case("case") => {
                self.next();
                self.expect_keyword("when")?;
                let condition = self.expression()?;
                self.expect_keyword("then")?;
                let then = self.expression()?;
                if self.keyword("when") {
                    return Err(refusal(
                        "one WHEN per CASE; nest a CASE in the ELSE",
                        self.at(),
                    ));
                }
                self.expect_keyword("else")?;
                let otherwise = self.expression()?;
                self.expect_keyword("end")?;
                Ok(Ex::Case(
                    Box::new(condition),
                    Box::new(then),
                    Box::new(otherwise),
                ))
            }
            Some(Token::Word(_) | Token::Quoted(_)) => {
                let first = self.name()?;
                if self.eat_symbol("(") {
                    return self.call(&first, at);
                }
                if self.eat_symbol(".") {
                    let column = self.name()?;
                    return Ok(Ex::Column(Some(first), column));
                }
                Ok(Ex::Column(None, first))
            }
            Some(Token::Symbol(_)) | None => Err(refusal("expected an expression", at)),
        }
    }

    fn call(&mut self, name: &str, at: usize) -> Result<Ex, Refusal> {
        let lower = name.to_ascii_lowercase();
        let mut call = Call {
            name: lower.clone(),
            args: Vec::new(),
            star: false,
            distinct: false,
            over: None,
            at,
        };
        if self.eat_symbol("*") {
            call.star = true;
        } else if !self.symbol(")") {
            call.distinct = self.eat_keyword("distinct");
            loop {
                call.args.push(self.expression()?);
                if !self.eat_symbol(",") {
                    break;
                }
            }
        }
        self.expect_symbol(")")?;
        if lower == "coalesce" {
            return Ok(Ex::Coalesce(call.args));
        }
        if self.eat_keyword("over") {
            self.expect_symbol("(")?;
            let mut over = Over {
                partition: Vec::new(),
                order: Vec::new(),
            };
            if self.eat_keyword("partition") {
                self.expect_keyword("by")?;
                loop {
                    let at = self.at();
                    let key = self.expression()?;
                    if constant(&key) {
                        return Err(refusal("a window key names a column, not a constant", at));
                    }
                    over.partition.push(key);
                    if !self.eat_symbol(",") {
                        break;
                    }
                }
            }
            if self.eat_keyword("order") {
                self.expect_keyword("by")?;
                loop {
                    let at = self.at();
                    let key = self.expression()?;
                    if constant(&key) {
                        return Err(refusal("a window key names a column, not a constant", at));
                    }
                    let descending = if self.eat_keyword("desc") {
                        true
                    } else {
                        self.eat_keyword("asc");
                        false
                    };
                    over.order.push((key, descending));
                    if !self.eat_symbol(",") {
                        break;
                    }
                }
            }
            if self.keyword("rows") || self.keyword("range") {
                return Err(refusal(
                    "window frames are fixed: a running SUM reads from the partition start to the row",
                    self.at(),
                ));
            }
            self.expect_symbol(")")?;
            call.over = Some(over);
        }
        Ok(Ex::Call(call))
    }
}

/// Words that are never a name: the dialect's keywords and the SQL
/// keywords it refuses, so `FROM t FULL JOIN …` cannot read `FULL` as an alias.
const RESERVED: &[&str] = &[
    "select",
    "from",
    "where",
    "group",
    "by",
    "join",
    "left",
    "inner",
    "outer",
    "on",
    "as",
    "and",
    "or",
    "not",
    "union",
    "all",
    "order",
    "limit",
    "having",
    "is",
    "null",
    "case",
    "when",
    "then",
    "else",
    "end",
    "over",
    "partition",
    "true",
    "false",
    "distinct",
    "right",
    "full",
    "cross",
    "natural",
    "lateral",
    "using",
    "with",
    "recursive",
    "window",
    "qualify",
    "offset",
    "fetch",
    "top",
    "into",
    "values",
    "exists",
    "in",
    "like",
    "ilike",
    "between",
    "similar",
    "escape",
    "interval",
    "cast",
    "extract",
    "rows",
    "range",
    "groups",
    "filter",
    "within",
    "intersect",
    "except",
    "nulls",
    "asc",
    "desc",
    "current_date",
    "current_time",
    "current_timestamp",
    "localtime",
    "localtimestamp",
    "grouping",
    "rollup",
    "cube",
    "insert",
    "update",
    "delete",
    "drop",
    "create",
    "alter",
    "copy",
    "attach",
    "pragma",
    "install",
    "load",
    "explain",
];

fn reserved(word: &str) -> bool {
    RESERVED.iter().any(|r| r.eq_ignore_ascii_case(word))
}

/// A column or alias name is a plain identifier: it is spliced into a ZEN path.
fn plain(name: &str, at: usize) -> Result<(), Refusal> {
    let mut chars = name.chars();
    let ok = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(refusal(
            format!("{name:?} is not a plain name (letters, digits and _)"),
            at,
        ))
    }
}

/// A literal, possibly negated: never a group or window key.
fn constant(ex: &Ex) -> bool {
    if let Ex::Negate(inner) = ex {
        return constant(inner);
    }
    matches!(ex, Ex::Number(_) | Ex::Text(_) | Ex::Bool(_) | Ex::Null)
}

/// The dialect's syntax tree of one query: its `UNION ALL` branches.
fn parse(source: &str) -> Result<Vec<Select>, Refusal> {
    Parser {
        tokens: tokenize(source)?,
        position: 0,
        end: source.len(),
    }
    .query()
}

/// Parse and lower one SQL query against `catalog`.
///
/// # Errors
/// `derived.validation.sql_unsupported` at the first construct without a
/// typed operator, an unknown type, alias or column, or an undeclared join.
pub fn lower_sql(source: &str, catalog: &EffectiveCatalog) -> Result<SqlPlan, Vec<Diagnostic>> {
    let selects = parse(source).map_err(|refused| vec![refused.into_diagnostic()])?;
    let mut lowering = Lowering {
        catalog,
        nodes: Map::new(),
        inputs: Vec::new(),
        counter: 0,
    };
    let mut outputs = Vec::new();
    for select in &selects {
        outputs.push(
            lowering
                .select(select)
                .map_err(|refused| vec![refused.into_diagnostic()])?,
        );
    }
    let output = if let [only] = outputs.as_slice() {
        only.clone()
    } else {
        let id = lowering.fresh("unioned");
        lowering.nodes.insert(
            id.clone(),
            json!({"op": "union", "inputs": outputs, "branch": "branch"}),
        );
        id
    };
    Ok(SqlPlan {
        inputs: lowering.inputs,
        nodes: Value::Object(lowering.nodes),
        output,
    })
}

struct Scope {
    /// Alias → (object type, column prefix in the joined row).
    aliases: BTreeMap<String, (String, String)>,
    base: String,
}

impl Scope {
    fn column(&self, alias: Option<&str>, column: &str, at: usize) -> Result<String, Refusal> {
        let alias = alias.unwrap_or(&self.base);
        let (_, prefix) = self
            .aliases
            .get(alias)
            .ok_or_else(|| refusal(format!("no source is named {alias:?}"), at))?;
        Ok(format!("{prefix}{column}"))
    }

    /// ZEN over `input.<column>` for a row expression; aggregates and windows refuse.
    fn zen(&self, ex: &Ex, at: usize) -> Result<String, Refusal> {
        Ok(match ex {
            Ex::Column(alias, column) => {
                zen_column(&self.column(alias.as_deref(), column, at)?, at)?
            }
            Ex::Number(number) => zen_number(number),
            Ex::Text(text) => zen_text(text, at)?,
            Ex::Bool(flag) => flag.to_string(),
            Ex::Null => "null".to_owned(),
            // Every compound form is wrapped whole: ZEN binds `not` looser
            // than `??`, so an unwrapped `not (a) ?? b` would read as
            // `not (a ?? b)`.
            Ex::Not(inner) => format!("(not ({}))", self.zen(inner, at)?),
            Ex::Negate(inner) => format!("(-({}))", self.zen(inner, at)?),
            Ex::Binary(op, left, right) => {
                format!("({} {op} {})", self.zen(left, at)?, self.zen(right, at)?)
            }
            Ex::IsNull(inner, negated) => format!(
                "({} {} null)",
                self.zen(inner, at)?,
                if *negated { "!=" } else { "==" }
            ),
            Ex::Case(condition, then, otherwise) => format!(
                "({} ? {} : {})",
                self.zen(condition, at)?,
                self.zen(then, at)?,
                self.zen(otherwise, at)?
            ),
            Ex::Coalesce(args) => {
                if args.len() < 2 {
                    return Err(refusal("COALESCE takes two or more values", at));
                }
                // Nested left to right, so the tree does not rest on how ZEN
                // associates `??`.
                let mut parts = args.iter().map(|arg| self.zen(arg, at));
                let first = parts
                    .next()
                    .ok_or_else(|| refusal("COALESCE takes two or more values", at))??;
                parts.try_fold(
                    first,
                    |left, right: Result<String, Refusal>| -> Result<String, Refusal> {
                        Ok(format!("({left} ?? {})", right?))
                    },
                )?
            }
            Ex::Call(call) => {
                return Err(refusal(
                    format!("{} is not a row expression here", call.name.to_uppercase()),
                    call.at,
                ));
            }
        })
    }
}

struct Lowering<'c> {
    catalog: &'c EffectiveCatalog,
    nodes: Map<String, Value>,
    inputs: Vec<(String, String)>,
    counter: usize,
}

/// A SQL number's ZEN literal: the same value as plain decimal text, without
/// the leading zeros SQL allows and ZEN does not (`007.50` is `7.50`).
fn zen_number(number: &str) -> String {
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    if fraction.is_empty() {
        whole.to_owned()
    } else {
        format!("{whole}.{fraction}")
    }
}

/// The ZEN path of one row column, as the engine's own ZEN parser reads it:
/// `input.c`, or `input["c"]` where `c` cannot follow a dot (a ZEN keyword
/// such as `true`); a column neither form reads back as refuses.
fn zen_column(column: &str, at: usize) -> Result<String, Refusal> {
    let root = super::ROW_ROOT;
    for path in [format!("{root}.{column}"), format!("{root}[\"{column}\"]")] {
        if matches!(
            norn_expr::row_expression(&path, root),
            Ok(Some(norn_expr::RowExpr::Column(read))) if read == column
        ) {
            return Ok(path);
        }
    }
    Err(refusal(
        format!("column {column:?} has no ZEN path the engine reads back"),
        at,
    ))
}

/// A ZEN string literal holding exactly `text`. ZEN strings take no escapes:
/// one ends at the next quote of its own kind, so `text` is quoted with a
/// kind it does not contain, and text holding both kinds has no ZEN literal.
fn zen_text(text: &str, at: usize) -> Result<String, Refusal> {
    if !text.contains('"') {
        Ok(format!("\"{text}\""))
    } else if !text.contains('\'') {
        Ok(format!("'{text}'"))
    } else {
        Err(refusal(
            "a text holding both ' and \" has no exact ZEN literal; split it",
            at,
        ))
    }
}

impl Lowering<'_> {
    fn fresh(&mut self, stem: &str) -> String {
        self.counter = self.counter.saturating_add(1);
        format!("{stem}_{}", self.counter)
    }

    fn scan(&mut self, source: &Source, at: usize) -> Result<String, Refusal> {
        if !matches!(self.catalog.object_type_named(&source.name), Some(Ok(_))) {
            return Err(refusal(
                format!("no object type is named {:?}", source.name),
                at,
            ));
        }
        if self.inputs.iter().any(|(alias, _)| *alias == source.alias) {
            return Err(refusal(
                format!(
                    "alias {:?} is used twice; name each source once",
                    source.alias
                ),
                at,
            ));
        }
        self.inputs
            .push((source.alias.clone(), source.name.clone()));
        let id = format!("scan_{}", source.alias);
        self.nodes
            .insert(id.clone(), json!({"op": "scan", "input": source.alias}));
        Ok(id)
    }

    fn join(
        &mut self,
        left_node: &str,
        scope: &mut Scope,
        join: &JoinClause,
    ) -> Result<String, Refusal> {
        let right_node = self.scan(&join.source, join.at)?;
        let alias = join.source.alias.clone();
        let prefix = format!("{alias}_");
        // `a.x = b.y` in either order, where b is the joined source.
        let Ex::Binary("==", lhs, rhs) = &join.on else {
            return Err(refusal(
                "ON is one equality between a source and the joined type",
                join.at,
            ));
        };
        let (Ex::Column(Some(la), lc), Ex::Column(Some(ra), rc)) = (lhs.as_ref(), rhs.as_ref())
        else {
            return Err(refusal(
                "ON compares two qualified columns (a.x = b.y)",
                join.at,
            ));
        };
        let ((outer, outer_column), joined_column) = if *ra == alias {
            ((la.clone(), lc.clone()), rc.clone())
        } else if *la == alias {
            ((ra.clone(), rc.clone()), lc.clone())
        } else {
            return Err(refusal(
                format!("ON must name the joined source {alias:?}"),
                join.at,
            ));
        };
        let (outer_type, _) = scope
            .aliases
            .get(&outer)
            .cloned()
            .ok_or_else(|| refusal(format!("no source is named {outer:?}"), join.at))?;
        let is_link = self
            .catalog
            .links_for_object_type(&outer_type)
            .is_some_and(|links| links.iter().any(|link| link.id.as_str() == outer_column));
        let on = if is_link && joined_column == "id" {
            json!({"link": {"column": scope.column(Some(&outer), "id", join.at)?, "link": outer_column}})
        } else {
            let key = if joined_column == "id" {
                "id".to_owned()
            } else {
                let Some(Ok(object)) = self.catalog.object_type_named(&join.source.name) else {
                    return Err(refusal(
                        format!("no object type is named {:?}", join.source.name),
                        join.at,
                    ));
                };
                object
                    .keys
                    .iter()
                    .find(|key| key.components.len() == 1 && key.components.iter().any(|c| c.as_str() == joined_column))
                    .map(|key| key.name.as_str().to_owned())
                    .ok_or_else(|| {
                        refusal(
                            format!(
                                "{}.{joined_column} is neither the id nor a declared key, and {outer}.{outer_column} is not a declared link: joins are declared, never ad hoc",
                                join.source.name
                            ),
                            join.at,
                        )
                    })?
            };
            json!({"key": {"left": [scope.column(Some(&outer), &outer_column, join.at)?], "right": key}})
        };
        scope
            .aliases
            .insert(alias, (join.source.name.clone(), prefix.clone()));
        let id = self.fresh("join");
        self.nodes.insert(
            id.clone(),
            json!({"op": "join", "left": left_node, "right": right_node, "on": on,
                "kind": if join.left { "left" } else { "inner" }, "prefix": prefix}),
        );
        Ok(id)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one SELECT lowering, read top to bottom"
    )]
    fn select(&mut self, select: &Select) -> Result<String, Refusal> {
        let mut node = self.scan(&select.from, 0)?;
        let mut scope = Scope {
            aliases: BTreeMap::from([(
                select.from.alias.clone(),
                (select.from.name.clone(), String::new()),
            )]),
            base: select.from.alias.clone(),
        };
        for join in &select.joins {
            node = self.join(&node, &mut scope, join)?;
        }
        if let Some(filter) = &select.filter {
            let id = self.fresh("filter");
            self.nodes.insert(
                id.clone(),
                json!({"op": "filter", "input": node, "predicate": scope.zen(filter, 0)?}),
            );
            node = id;
        }
        if select.items.iter().any(|item| item.star) {
            if select.items.len() != 1 || !select.group_by.is_empty() {
                return Err(refusal("`*` stands alone, without GROUP BY", 0));
            }
            return Ok(node);
        }
        let aggregate = |ex: &Ex| matches!(ex, Ex::Call(call) if call.over.is_none() && ["sum", "count", "min", "max"].contains(&call.name.as_str()));
        let windowed = |ex: &Ex| matches!(ex, Ex::Call(call) if call.over.is_some());
        let grouped =
            !select.group_by.is_empty() || select.items.iter().any(|item| aggregate(&item.ex));
        let name_of = |item: &Item, index: usize| -> String {
            item.alias.clone().unwrap_or_else(|| {
                if let Ex::Column(_, column) = &item.ex {
                    return column.clone();
                }
                let Ex::Call(call) = &item.ex else {
                    return format!("column_{index}");
                };
                if let Some(Ex::Column(_, column)) = call.args.first() {
                    format!("{}_{column}", call.name)
                } else {
                    call.name.clone()
                }
            })
        };
        // Columns computed before grouping or windowing: grain passthroughs plus each needed value.
        let mut prepared: Vec<(String, String)> = Vec::new();
        // Every source's id passes through: whatever the joins make the grain,
        // it is among these columns.
        let grain_columns: Vec<String> = std::iter::once(select.from.alias.clone())
            .chain(select.joins.iter().map(|join| join.source.alias.clone()))
            .map(|alias| scope.column(Some(&alias), "id", 0))
            .collect::<Result<_, _>>()?;
        if grouped {
            let mut group_by = Vec::new();
            let mut measures = Vec::new();
            let mut keys: Vec<(Ex, String)> = Vec::new();
            for (ex, at) in &select.group_by {
                let (value, grain_unit) = if let Ex::Call(call) = ex
                    && call.name == "date_trunc"
                {
                    let [Ex::Text(unit), value] = call.args.as_slice() else {
                        return Err(refusal("DATE_TRUNC('week', column)", call.at));
                    };
                    (value.clone(), Some(unit.to_ascii_lowercase()))
                } else {
                    (ex.clone(), None)
                };
                let name = select
                    .items
                    .iter()
                    .enumerate()
                    .find(|(_, item)| item.ex == *ex)
                    .map_or_else(
                        || {
                            if let Ex::Column(_, column) = &value {
                                column.clone()
                            } else {
                                format!("key_{}", keys.len())
                            }
                        },
                        |(index, item)| name_of(item, index),
                    );
                prepared.push((name.clone(), scope.zen(&value, *at)?));
                if let Some(unit) = &grain_unit {
                    if !["day", "week", "month", "quarter", "year"].contains(&unit.as_str()) {
                        return Err(refusal(
                            format!("DATE_TRUNC unit {unit:?}: day, week, month, quarter or year"),
                            *at,
                        ));
                    }
                    group_by.push(json!({"column": name, "grain": unit}));
                } else {
                    group_by.push(json!({"column": name}));
                }
                keys.push((ex.clone(), name));
            }
            let mut windows = Vec::new();
            for (index, item) in select.items.iter().enumerate() {
                let name = name_of(item, index);
                if keys.iter().any(|(ex, _)| *ex == item.ex) {
                    continue;
                }
                if windowed(&item.ex) {
                    windows.push((name, item.ex.clone(), item.at));
                    continue;
                }
                let (Ex::Call(call), true) = (&item.ex, aggregate(&item.ex)) else {
                    return Err(refusal(
                        "in a grouped query every column is a group key or an aggregate",
                        item.at,
                    ));
                };
                let function = match (call.name.as_str(), call.distinct) {
                    ("count", true) => "count_distinct",
                    ("count", false) => "count",
                    ("sum", false) => "sum",
                    ("min", false) => "min",
                    ("max", false) => "max",
                    _ => {
                        return Err(refusal(
                            format!(
                                "{}(DISTINCT …) has no typed operator",
                                call.name.to_uppercase()
                            ),
                            call.at,
                        ));
                    }
                };
                if call.star {
                    measures.push(json!({"name": name, "fn": function}));
                } else {
                    let [arg] = call.args.as_slice() else {
                        return Err(refusal("an aggregate takes one column", call.at));
                    };
                    let value_column = format!("{name}_value");
                    prepared.push((value_column.clone(), scope.zen(arg, call.at)?));
                    measures.push(json!({"name": name, "fn": function, "column": value_column}));
                }
            }
            let projected = self.fresh("prepared");
            let mut columns: Vec<Value> = grain_columns
                .iter()
                .map(|column| json!({"name": column, "expression": format!("input.{column}")}))
                .collect();
            for (name, expression) in &prepared {
                if !grain_columns.contains(name) {
                    columns.push(json!({"name": name, "expression": expression}));
                }
            }
            self.nodes.insert(
                projected.clone(),
                json!({"op": "project", "input": node, "columns": columns}),
            );
            let id = self.fresh("grouped");
            self.nodes.insert(
                id.clone(),
                json!({"op": "aggregate", "input": projected, "groupBy": group_by, "measures": measures}),
            );
            node = id;
            if !windows.is_empty() {
                let names: Vec<(Ex, String)> = select
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| (item.ex.clone(), name_of(item, index)))
                    .collect();
                node = self.window(&node, &windows, &names)?;
            }
            return Ok(node);
        }
        // A row-level SELECT: a projection that keeps the grain, then windows.
        let mut columns: Vec<Value> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut windows = Vec::new();
        for (index, item) in select.items.iter().enumerate() {
            let name = name_of(item, index);
            if windowed(&item.ex) {
                windows.push((name, item.ex.clone(), item.at));
                continue;
            }
            seen.insert(name.clone());
            columns.push(json!({"name": name, "expression": scope.zen(&item.ex, item.at)?}));
        }
        for column in &grain_columns {
            if !seen.contains(column) {
                columns.insert(
                    0,
                    json!({"name": column, "expression": format!("input.{column}")}),
                );
            }
        }
        let id = self.fresh("selected");
        self.nodes.insert(
            id.clone(),
            json!({"op": "project", "input": node, "columns": columns}),
        );
        node = id;
        if !windows.is_empty() {
            let names: Vec<(Ex, String)> = select
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| (item.ex.clone(), name_of(item, index)))
                .collect();
            node = self.window(&node, &windows, &names)?;
        }
        Ok(node)
    }

    /// One window node over output columns named by the select list.
    fn window(
        &mut self,
        input: &str,
        windows: &[(String, Ex, usize)],
        names: &[(Ex, String)],
    ) -> Result<String, Refusal> {
        let column_of = |ex: &Ex, at: usize| -> Result<String, Refusal> {
            let selected = names
                .iter()
                .find(|(selected, _)| selected == ex)
                .map(|(_, name)| name.clone());
            if let Ex::Column(_, column) = ex {
                Ok(selected.unwrap_or_else(|| column.clone()))
            } else {
                selected.ok_or_else(|| refusal("a window reads a selected column", at))
            }
        };
        let mut over: Option<&Over> = None;
        let mut columns = Vec::new();
        for (name, ex, at) in windows {
            let Ex::Call(call) = ex else {
                return Err(refusal("expected a window function", *at));
            };
            let Some(this) = &call.over else {
                return Err(refusal("expected OVER", *at));
            };
            if over.is_some_and(|first| first != this) {
                return Err(refusal("windows in one SELECT share one OVER (…)", *at));
            }
            over = Some(this);
            let function = match (call.name.as_str(), call.args.as_slice()) {
                ("row_number" | "rank" | "dense_rank", []) => {
                    json!({"name": name, "fn": call.name})
                }
                ("lag" | "lead", [value]) => {
                    json!({"name": name, "fn": call.name, "column": column_of(value, *at)?})
                }
                ("lag" | "lead", [value, Ex::Number(offset)]) => {
                    json!({"name": name, "fn": call.name, "column": column_of(value, *at)?, "offset": offset.parse::<u32>().map_err(|_error| refusal("an offset is a whole number", *at))?})
                }
                ("sum", [value]) => {
                    json!({"name": name, "fn": "running_sum", "column": column_of(value, *at)?})
                }
                _ => {
                    return Err(refusal(
                        format!(
                            "{} OVER (…) has no typed window operator",
                            call.name.to_uppercase()
                        ),
                        *at,
                    ));
                }
            };
            columns.push(function);
        }
        let over = over.ok_or_else(|| refusal("expected a window", 0))?;
        let partition = over
            .partition
            .iter()
            .map(|ex| column_of(ex, 0))
            .collect::<Result<Vec<_>, _>>()?;
        let order = over
            .order
            .iter()
            .map(|(ex, descending)| Ok(json!({"column": column_of(ex, 0)?, "direction": if *descending { "desc" } else { "asc" }})))
            .collect::<Result<Vec<_>, Refusal>>()?;
        let id = self.fresh("windowed");
        self.nodes.insert(
            id.clone(),
            json!({"op": "window", "input": input, "partitionBy": partition, "orderBy": order, "columns": columns}),
        );
        Ok(id)
    }
}

/// Who a SQL transform is: its id, title, version and the scope it is authored in.
#[derive(Debug, Clone)]
pub struct SqlMeta<'m> {
    /// The transform id (`shop.revenue_by_region`).
    pub id: &'m str,
    /// Its title.
    pub title: &'m str,
    /// Its version.
    pub version: &'m str,
    /// The artifact scope (`{"orgId": …, "environmentId": …}`).
    pub scope: Value,
}

/// VIEW-SQL-1: the `derived` artifact a SQL query declares, pinned to the
/// entity artifacts of `pool` that declare its types, and admitted against
/// `pool` like any authored transform.
///
/// # Errors
/// `derived.validation.sql_unsupported` for the dialect, then every
/// admission finding of the lowered plan.
pub fn sql_artifact(
    source: &str,
    meta: &SqlMeta<'_>,
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<Value, Vec<Diagnostic>> {
    let catalog = EffectiveCatalog::from_artifacts_with_registry(pool, registry);
    let plan = lower_sql(source, &catalog)?;
    let mut inputs = Vec::new();
    let mut dependencies = Vec::new();
    for (alias, object_type) in &plan.inputs {
        let artifact = catalog
            .artifact_of(object_type)
            .and_then(|id| pool.iter().find(|a| a.artifact_id == id))
            .ok_or_else(|| {
                vec![
                    refusal(format!("no installed artifact declares {object_type:?}"), 0)
                        .into_diagnostic(),
                ]
            })?;
        inputs.push(json!({"artifactId": artifact.artifact_id, "kind": "entity", "version": artifact.version, "alias": alias}));
        let pin = json!({"artifactId": artifact.artifact_id, "kind": "entity", "version": artifact.version, "required": true});
        if !dependencies.contains(&pin) {
            dependencies.push(pin);
        }
    }
    let artifact = json!({
        "irVersion": "0.1",
        "artifactId": meta.id,
        "kind": "derived",
        "namespace": meta.id.split('.').next().unwrap_or(meta.id),
        "name": meta.title,
        "version": meta.version,
        "lifecycle": "draft",
        "scope": meta.scope,
        "dependencies": dependencies,
        "provenance": {"source": "authoring_sdk"},
        "payload": {
            "viewVersion": "0.1",
            "viewId": meta.id,
            "inputs": inputs,
            "calc": {"kind": "relational", "nodes": plan.nodes, "output": plan.output},
            "outputSchema": {},
            "maintenance": "recompute_on_read"
        }
    });
    let envelope: ArtifactEnvelope = serde_json::from_value(artifact.clone()).map_err(|error| {
        vec![refusal(format!("the lowered artifact does not decode: {error}"), 0).into_diagnostic()]
    })?;
    let mut admitted_pool = pool.to_vec();
    admitted_pool.push(envelope.clone());
    super::admit(&envelope, &admitted_pool, registry)?;
    Ok(artifact)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "property tests: a failed setup is a failed test; one generator reads top to bottom"
)]
mod properties {
    //! The bar for a hand-written dialect (#341): a printed query parses back
    //! to itself, every query the parser accepts is admitted or refused with a
    //! registered code, and no input panics.

    use std::collections::BTreeSet;

    use norn_ir::ArtifactEnvelope;
    use proptest::collection::vec;
    use proptest::option;
    use proptest::prelude::*;
    use proptest::sample::select;
    use serde_json::{Value, json};

    use norn_expr::{RowArithmetic, RowComparison, RowExpr, row_expression};

    use super::{
        Call, Ex, Item, JoinClause, Over, RESERVED, Scope, Select, Source, SqlMeta, constant,
        lower_sql, parse, sql_artifact,
    };
    use crate::Diagnostic;
    use crate::codes;
    use crate::entity_instance::EffectiveCatalog;

    fn print_query(selects: &[Select]) -> String {
        selects
            .iter()
            .map(print_select)
            .collect::<Vec<_>>()
            .join(" UNION ALL ")
    }

    fn print_select(select: &Select) -> String {
        let items = select
            .items
            .iter()
            .map(|item| match (&item.alias, item.star) {
                (_, true) => "*".to_owned(),
                (Some(alias), false) => format!("{} AS {alias}", print_ex(&item.ex)),
                (None, false) => print_ex(&item.ex),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut parts = vec![format!(
            "SELECT {items} FROM {}",
            print_source(&select.from)
        )];
        parts.extend(select.joins.iter().map(|join| {
            format!(
                "{}JOIN {} ON {}",
                if join.left { "LEFT " } else { "" },
                print_source(&join.source),
                print_ex(&join.on)
            )
        }));
        if let Some(filter) = &select.filter {
            parts.push(format!("WHERE {}", print_ex(filter)));
        }
        if !select.group_by.is_empty() {
            let keys = select
                .group_by
                .iter()
                .map(|(key, _)| print_ex(key))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("GROUP BY {keys}"));
        }
        parts.join(" ")
    }

    fn print_source(source: &Source) -> String {
        format!(
            "\"{}\" AS {}",
            source.name.replace('"', "\"\""),
            source.alias
        )
    }

    fn print_list(items: &[Ex]) -> String {
        items.iter().map(print_ex).collect::<Vec<_>>().join(", ")
    }

    fn print_ex(ex: &Ex) -> String {
        print_ex_with(ex, false)
    }

    /// `quoted`: every column name double-quoted, so a name the dialect
    /// reserves (`null`, `in`) is still a column.
    fn print_ex_with(ex: &Ex, quoted: bool) -> String {
        let print_ex = |inner: &Ex| print_ex_with(inner, quoted);
        let print_list = |items: &[Ex]| items.iter().map(print_ex).collect::<Vec<_>>().join(", ");
        match ex {
            Ex::Column(Some(alias), column) if quoted => format!("{alias}.\"{column}\""),
            Ex::Column(None, column) if quoted => format!("\"{column}\""),
            Ex::Column(Some(alias), column) => format!("{alias}.{column}"),
            Ex::Column(None, column) | Ex::Number(column) => column.clone(),
            Ex::Text(text) => format!("'{}'", text.replace('\'', "''")),
            Ex::Bool(flag) => String::from(if *flag { "TRUE" } else { "FALSE" }),
            Ex::Null => "NULL".to_owned(),
            Ex::Not(inner) => format!("(NOT {})", print_ex(inner)),
            Ex::Negate(inner) => format!("(-{})", print_ex(inner)),
            Ex::Binary(op, left, right) => {
                let op = match *op {
                    "==" => "=",
                    "!=" => "<>",
                    "and" => "AND",
                    "or" => "OR",
                    other => other,
                };
                format!("({} {op} {})", print_ex(left), print_ex(right))
            }
            Ex::IsNull(inner, negated) => format!(
                "({} IS {}NULL)",
                print_ex(inner),
                if *negated { "NOT " } else { "" }
            ),
            Ex::Case(condition, then, otherwise) => format!(
                "CASE WHEN {} THEN {} ELSE {} END",
                print_ex(condition),
                print_ex(then),
                print_ex(otherwise)
            ),
            Ex::Coalesce(args) => format!("COALESCE({})", print_list(args)),
            Ex::Call(call) => print_call(call),
        }
    }

    /// The tree the ZEN that `zen()` emits for `ex` must parse to: `ex` node
    /// for node, its columns as `scope` names them. `None` for a call, which
    /// is not a row expression.
    fn expected(ex: &Ex, scope: &Scope) -> Option<RowExpr> {
        let boxed = |inner: &Ex| expected(inner, scope).map(Box::new);
        Some(match ex {
            Ex::Column(alias, column) => {
                RowExpr::Column(scope.column(alias.as_deref(), column, 0).ok()?)
            }
            Ex::Number(text) => RowExpr::Number(text.parse().ok()?),
            Ex::Text(text) => RowExpr::Text(text.clone()),
            Ex::Bool(flag) => RowExpr::Bool(*flag),
            Ex::Null => RowExpr::Null,
            Ex::Not(inner) => RowExpr::Not(boxed(inner)?),
            Ex::Negate(inner) => RowExpr::Negate(boxed(inner)?),
            Ex::Binary(op, left, right) => {
                let (left, right) = (boxed(left)?, boxed(right)?);
                let compare =
                    |comparison| RowExpr::Compare(comparison, left.clone(), right.clone());
                match *op {
                    "+" => RowExpr::Arithmetic(RowArithmetic::Add, left, right),
                    "-" => RowExpr::Arithmetic(RowArithmetic::Subtract, left, right),
                    "*" => RowExpr::Arithmetic(RowArithmetic::Multiply, left, right),
                    "==" => compare(RowComparison::Equal),
                    "!=" => compare(RowComparison::NotEqual),
                    "<" => compare(RowComparison::Less),
                    "<=" => compare(RowComparison::LessOrEqual),
                    ">" => compare(RowComparison::Greater),
                    ">=" => compare(RowComparison::GreaterOrEqual),
                    "and" => RowExpr::And(left, right),
                    "or" => RowExpr::Or(left, right),
                    _ => return None,
                }
            }
            Ex::IsNull(inner, negated) => RowExpr::Compare(
                if *negated {
                    RowComparison::NotEqual
                } else {
                    RowComparison::Equal
                },
                boxed(inner)?,
                Box::new(RowExpr::Null),
            ),
            Ex::Case(condition, then, otherwise) => {
                RowExpr::Conditional(boxed(condition)?, boxed(then)?, boxed(otherwise)?)
            }
            Ex::Coalesce(args) => {
                let mut args = args.iter();
                let first = expected(args.next()?, scope)?;
                args.try_fold(first, |left, arg| {
                    Some(RowExpr::Coalesce(
                        Box::new(left),
                        Box::new(expected(arg, scope)?),
                    ))
                })?
            }
            Ex::Call(_) => return None,
        })
    }

    fn print_call(call: &Call) -> String {
        let args = if call.star {
            "*".to_owned()
        } else if call.distinct {
            format!("DISTINCT {}", print_list(&call.args))
        } else {
            print_list(&call.args)
        };
        let head = format!("{}({args})", call.name.to_ascii_uppercase());
        let Some(over) = &call.over else {
            return head;
        };
        let mut parts = Vec::new();
        if !over.partition.is_empty() {
            parts.push(format!("PARTITION BY {}", print_list(&over.partition)));
        }
        if !over.order.is_empty() {
            let keys = over
                .order
                .iter()
                .map(|(key, descending)| {
                    format!(
                        "{}{}",
                        print_ex(key),
                        if *descending { " DESC" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("ORDER BY {keys}"));
        }
        format!("{head} OVER ({})", parts.join(" "))
    }

    const COMPARISONS: [&str; 6] = ["==", "!=", "<", "<=", ">", ">="];
    const OPERATORS: [&str; 11] = ["==", "!=", "<", "<=", ">", ">=", "+", "-", "*", "and", "or"];
    const FUNCTIONS: [&str; 10] = [
        "sum",
        "count",
        "min",
        "max",
        "date_trunc",
        "row_number",
        "rank",
        "lag",
        "lead",
        "upper",
    ];

    fn word() -> BoxedStrategy<String> {
        "[a-z_][a-z0-9_]{0,5}"
            .prop_filter("a keyword is not a name", |word| {
                !RESERVED.contains(&word.as_str())
            })
            .boxed()
    }

    fn call(name: &str, args: Vec<Ex>, distinct: bool, over: Option<Over>) -> Ex {
        Ex::Call(Call {
            name: name.to_owned(),
            args,
            star: false,
            distinct,
            over,
            at: 0,
        })
    }

    fn key(ex: BoxedStrategy<Ex>) -> BoxedStrategy<Ex> {
        ex.prop_filter("a key is not a constant", |ex| !constant(ex))
            .boxed()
    }

    fn over(ex: BoxedStrategy<Ex>) -> impl Strategy<Value = Over> {
        (
            vec(key(ex.clone()), 0..2),
            vec((key(ex), any::<bool>()), 0..2),
        )
            .prop_map(|(partition, order)| Over { partition, order })
    }

    /// Row expressions over `columns`, up to three levels deep.
    fn expression(columns: BoxedStrategy<Ex>) -> BoxedStrategy<Ex> {
        let leaf = prop_oneof![
            4 => columns,
            1 => "[0-9]{1,4}(\\.[0-9]{1,2})?".prop_map(Ex::Number),
            1 => "[a-z '%-]{0,6}".prop_map(Ex::Text),
            1 => any::<bool>().prop_map(Ex::Bool),
            1 => Just(Ex::Null),
        ];
        leaf.prop_recursive(3, 16, 3, |inner| {
            prop_oneof![
                inner.clone().prop_map(|ex| Ex::Not(Box::new(ex))),
                inner.clone().prop_map(|ex| Ex::Negate(Box::new(ex))),
                (select(&OPERATORS[..]), inner.clone(), inner.clone())
                    .prop_filter(
                        "a comparison with a NULL literal refuses",
                        |(op, left, right)| {
                            !(COMPARISONS.contains(op)
                                && (matches!(left, Ex::Null) || matches!(right, Ex::Null)))
                        }
                    )
                    .prop_map(|(op, left, right)| Ex::Binary(op, Box::new(left), Box::new(right))),
                (inner.clone(), any::<bool>())
                    .prop_map(|(ex, negated)| Ex::IsNull(Box::new(ex), negated)),
                (inner.clone(), inner.clone(), inner.clone()).prop_map(|(c, t, e)| Ex::Case(
                    Box::new(c),
                    Box::new(t),
                    Box::new(e)
                )),
                vec(inner.clone(), 2..4).prop_map(Ex::Coalesce),
                (
                    select(&FUNCTIONS[..]),
                    vec(inner.clone(), 0..3),
                    any::<bool>(),
                    any::<bool>(),
                    option::of(over(inner))
                )
                    .prop_map(|(name, args, star, distinct, over)| {
                        let star = star && args.is_empty();
                        let distinct = distinct && !args.is_empty();
                        Ex::Call(Call {
                            name: name.to_owned(),
                            args,
                            star,
                            distinct,
                            over,
                            at: 0,
                        })
                    }),
            ]
        })
        .boxed()
    }

    fn any_select() -> impl Strategy<Value = Select> {
        let column = (option::of(word()), word())
            .prop_map(|(alias, column)| Ex::Column(alias, column))
            .boxed();
        let ex = expression(column);
        let item = prop_oneof![
            1 => Just(Item { ex: Ex::Null, alias: None, star: true, at: 0 }),
            6 => (ex.clone(), option::of(word()))
                .prop_map(|(ex, alias)| Item { ex, alias, star: false, at: 0 }),
        ];
        let source =
            ("[A-Za-z][A-Za-z0-9_.]{0,8}", word()).prop_map(|(name, alias)| Source { name, alias });
        let join =
            (source.clone(), any::<bool>(), ex.clone()).prop_map(|(source, left, on)| JoinClause {
                source,
                left,
                on,
                at: 0,
            });
        (
            vec(item, 1..4),
            source,
            vec(join, 0..3),
            option::of(ex.clone()),
            vec(key(ex), 0..3),
        )
            .prop_map(|(items, from, joins, filter, keys)| Select {
                items,
                from,
                joins,
                filter,
                group_by: keys.into_iter().map(|key| (key, 0)).collect(),
            })
    }

    fn column(alias: &str, name: &str) -> Ex {
        Ex::Column(Some(alias.to_owned()), name.to_owned())
    }

    fn aliased(index: usize, ex: Ex) -> Item {
        Item {
            ex,
            alias: Some(format!("a{index}")),
            star: false,
            at: 0,
        }
    }

    /// In-dialect queries over the shop catalog: a projection, a grouped
    /// query or a window, with up to two declared joins and an optional filter.
    fn shop_query() -> impl Strategy<Value = Vec<Select>> {
        (0..3_usize, any::<bool>()).prop_flat_map(|(joins, left)| {
            let mut names = vec![
                ("o", "id"),
                ("o", "amount"),
                ("o", "rate"),
                ("o", "note"),
                ("o", "placed_at"),
            ];
            let mut clauses = Vec::new();
            if joins >= 1 {
                names.extend([("c", "id"), ("c", "email"), ("c", "region")]);
                clauses.push(JoinClause {
                    source: Source {
                        name: "shop.Customer".to_owned(),
                        alias: "c".to_owned(),
                    },
                    left,
                    on: Ex::Binary(
                        "==",
                        Box::new(column("o", "customer")),
                        Box::new(column("c", "id")),
                    ),
                    at: 0,
                });
            }
            if joins >= 2 {
                names.extend([("l", "id"), ("l", "email"), ("l", "source")]);
                clauses.push(JoinClause {
                    source: Source {
                        name: "shop.Lead".to_owned(),
                        alias: "l".to_owned(),
                    },
                    left: false,
                    on: Ex::Binary(
                        "==",
                        Box::new(column("l", "email")),
                        Box::new(column("c", "email")),
                    ),
                    at: 0,
                });
            }
            let columns = select(names)
                .prop_map(|(alias, name)| column(alias, name))
                .boxed();
            let ex = expression(columns.clone());
            let week = call(
                "date_trunc",
                vec![Ex::Text("week".to_owned()), column("o", "placed_at")],
                false,
                None,
            );
            let projection = vec(ex.clone(), 1..4)
                .prop_map(|items| -> (Vec<Item>, Vec<Ex>) {
                    (
                        items
                            .into_iter()
                            .enumerate()
                            .map(|(i, ex)| aliased(i, ex))
                            .collect(),
                        Vec::new(),
                    )
                })
                .boxed();
            let grouped = (
                vec(prop_oneof![3 => columns.clone(), 1 => Just(week)], 1..3),
                vec(
                    (
                        select(&["sum", "count", "min", "max"][..]),
                        columns.clone(),
                        any::<bool>(),
                    ),
                    0..3,
                ),
            )
                .prop_map(|(keys, measures)| -> (Vec<Item>, Vec<Ex>) {
                    let mut items: Vec<Item> = keys
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(i, key)| aliased(i, key))
                        .collect();
                    for (function, measured, distinct) in measures {
                        let distinct = distinct && function == "count";
                        items.push(aliased(
                            items.len(),
                            call(function, vec![measured], distinct, None),
                        ));
                    }
                    (items, keys)
                })
                .boxed();
            let windowed = (
                vec(columns.clone(), 1..3),
                select(&["row_number", "rank", "lag", "sum"][..]),
                columns.clone(),
                columns,
            )
                .prop_map(
                    |(plain, function, partition, value)| -> (Vec<Item>, Vec<Ex>) {
                        let args = if matches!(function, "lag" | "sum") {
                            vec![value]
                        } else {
                            Vec::new()
                        };
                        let over = Over {
                            partition: vec![partition],
                            order: vec![(column("o", "id"), false)],
                        };
                        let mut items: Vec<Item> = plain
                            .into_iter()
                            .enumerate()
                            .map(|(i, ex)| aliased(i, ex))
                            .collect();
                        items.push(aliased(
                            items.len(),
                            call(function, args, false, Some(over)),
                        ));
                        (items, Vec::new())
                    },
                )
                .boxed();
            (prop_oneof![projection, grouped, windowed], option::of(ex)).prop_map(
                move |((items, keys), filter): ((Vec<Item>, Vec<Ex>), Option<Ex>)| {
                    vec![Select {
                        items,
                        from: Source {
                            name: "shop.Order".to_owned(),
                            alias: "o".to_owned(),
                        },
                        joins: clauses.clone(),
                        filter,
                        group_by: keys.into_iter().map(|key| (key, 0)).collect(),
                    }]
                },
            )
        })
    }

    fn entity(name: &str, object_type: Value) -> ArtifactEnvelope {
        let mut object_type = object_type;
        object_type["name"] = json!(name);
        serde_json::from_value(json!({
            "irVersion": "0.1", "artifactId": format!("{name}.entity"), "kind": "entity",
            "namespace": "shop", "name": name, "version": "0.1.0", "lifecycle": "draft",
            "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": [],
            "provenance": {"source": "authoring_sdk"},
            "payload": {"entityVersion": "2.0", "objectType": object_type}
        }))
        .expect("entity")
    }

    fn pool() -> Vec<ArtifactEnvelope> {
        vec![
            entity(
                "shop.Customer",
                json!({
                    "properties": [
                        {"id": "email", "type": {"kind": "text"}},
                        {"id": "region", "type": {"kind": "text"}}
                    ],
                    "identity": [{"name": "email", "properties": ["email"]}]
                }),
            ),
            entity(
                "shop.Order",
                json!({
                    "properties": [
                        {"id": "placed_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                        {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
                        {"id": "rate", "type": {"kind": "decimal", "scale": 4}},
                        {"id": "note", "constraints": {"nullable": true}, "type": {"kind": "text"}}
                    ],
                    "links": [{"id": "customer", "target": "shop.Customer",
                        "cardinality": {"to": "one", "from": "many"}}]
                }),
            ),
            entity(
                "shop.Lead",
                json!({
                    "properties": [
                        {"id": "email", "type": {"kind": "text"}},
                        {"id": "source", "type": {"kind": "text"}}
                    ]
                }),
            ),
        ]
    }

    fn compile(text: &str) -> Result<Value, Vec<Diagnostic>> {
        sql_artifact(
            text,
            &SqlMeta {
                id: "shop.sql_view",
                title: "SQL view",
                version: "0.1.0",
                scope: json!({"orgId": "org_demo", "environmentId": "dev"}),
            },
            &pool(),
            &BTreeSet::new(),
        )
    }

    fn registered(found: &[Diagnostic]) -> Result<(), TestCaseError> {
        prop_assert!(!found.is_empty(), "a refusal names at least one code");
        for diagnostic in found {
            prop_assert!(
                codes::lookup(diagnostic.code.as_str()).is_some(),
                "unregistered code {}",
                diagnostic.code.as_str()
            );
        }
        Ok(())
    }

    const SOUP: [&str; 48] = [
        "SELECT",
        "FROM",
        "\"shop.Order\"",
        "\"shop.Customer\"",
        "o",
        "c",
        "AS",
        "o.amount",
        "o.note",
        "o.placed_at",
        "o.customer",
        "c.id",
        "c.region",
        ",",
        "(",
        ")",
        "=",
        "<>",
        "<",
        ">=",
        "+",
        "-",
        "*",
        "/",
        "AND",
        "OR",
        "NOT",
        "IS",
        "NULL",
        "JOIN",
        "LEFT",
        "ON",
        "WHERE",
        "GROUP",
        "BY",
        "SUM",
        "COUNT",
        "DATE_TRUNC",
        "'week'",
        "OVER",
        "PARTITION",
        "ORDER",
        "UNION",
        "ALL",
        "CASE",
        "WHEN",
        "1",
        ";",
    ];

    /// `ex` reads a column that no ZEN path reads back.
    fn pathless(ex: &Ex, scope: &Scope) -> bool {
        match ex {
            Ex::Column(alias, column) => scope
                .column(alias.as_deref(), column, 0)
                .and_then(|name| super::zen_column(&name, 0))
                .is_err(),
            Ex::Not(inner) | Ex::Negate(inner) | Ex::IsNull(inner, _) => pathless(inner, scope),
            Ex::Binary(_, left, right) => pathless(left, scope) || pathless(right, scope),
            Ex::Case(condition, then, otherwise) => {
                pathless(condition, scope) || pathless(then, scope) || pathless(otherwise, scope)
            }
            Ex::Coalesce(args) => args.iter().any(|arg| pathless(arg, scope)),
            Ex::Call(call) => call.args.iter().any(|arg| pathless(arg, scope)),
            Ex::Text(_) | Ex::Number(_) | Ex::Bool(_) | Ex::Null => false,
        }
    }

    /// `ex` holds a text with both quote kinds, which no ZEN literal holds.
    fn unquotable(ex: &Ex) -> bool {
        match ex {
            Ex::Text(text) => text.contains('"') && text.contains('\''),
            Ex::Not(inner) | Ex::Negate(inner) | Ex::IsNull(inner, _) => unquotable(inner),
            Ex::Binary(_, left, right) => unquotable(left) || unquotable(right),
            Ex::Case(condition, then, otherwise) => {
                unquotable(condition) || unquotable(then) || unquotable(otherwise)
            }
            Ex::Coalesce(args) => args.iter().any(unquotable),
            Ex::Call(call) => call.args.iter().any(unquotable),
            Ex::Column(..) | Ex::Number(_) | Ex::Bool(_) | Ex::Null => false,
        }
    }

    /// Column names that are plain SQL identifiers but name ZEN keywords,
    /// builtins and the row root.
    const TRICKY: [&str; 20] = [
        "amount",
        "input",
        "null",
        "true",
        "false",
        "and",
        "or",
        "not",
        "in",
        "len",
        "map",
        "some",
        "none",
        "all",
        "date",
        "d",
        "_",
        "__proto__",
        "Amount",
        "x1",
    ];

    /// Row expressions over adversarial names and text: ZEN keywords as
    /// (quoted) column names, and text with quotes, backslashes, template
    /// markers, control characters and non-ASCII.
    fn adversarial_expression() -> BoxedStrategy<Ex> {
        let columns = (option::of(Just("o".to_owned())), select(&TRICKY[..]))
            .prop_map(|(alias, column)| Ex::Column(alias, column.to_owned()))
            .boxed();
        let leaf = prop_oneof![
            3 => columns,
            1 => "[0-9]{1,4}(\\.[0-9]{1,2})?".prop_map(Ex::Number),
            2 => "[a-z\"\\\\'`$}{\n\t é漢😀-]{0,8}".prop_map(Ex::Text),
            1 => any::<bool>().prop_map(Ex::Bool),
            1 => Just(Ex::Null),
        ];
        leaf.prop_recursive(3, 16, 3, |inner| {
            prop_oneof![
                inner.clone().prop_map(|ex| Ex::Not(Box::new(ex))),
                inner.clone().prop_map(|ex| Ex::Negate(Box::new(ex))),
                (select(&OPERATORS[..]), inner.clone(), inner.clone())
                    .prop_filter(
                        "a comparison with a NULL literal refuses",
                        |(op, left, right)| {
                            !(COMPARISONS.contains(op)
                                && (matches!(left, Ex::Null) || matches!(right, Ex::Null)))
                        }
                    )
                    .prop_map(|(op, left, right)| Ex::Binary(op, Box::new(left), Box::new(right))),
                (inner.clone(), any::<bool>())
                    .prop_map(|(ex, negated)| Ex::IsNull(Box::new(ex), negated)),
                (inner.clone(), inner.clone(), inner.clone()).prop_map(|(c, t, e)| Ex::Case(
                    Box::new(c),
                    Box::new(t),
                    Box::new(e)
                )),
                vec(inner, 2..4).prop_map(Ex::Coalesce),
            ]
        })
        .boxed()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// `zen()` builds ZEN text; this proves the text means the SQL. A
        /// query filtering and projecting adversarial expressions is lowered,
        /// and each emitted ZEN expression, parsed by the engine's own ZEN
        /// parser, equals the parsed SQL expression node for node: the same
        /// operators, column references, literals and null handling.
        #[test]
        fn emitted_zen_parses_to_the_sql_tree(
            filter in adversarial_expression(),
            value in adversarial_expression(),
        ) {
            let sql = format!(
                "SELECT {} AS v FROM \"shop.Order\" AS o WHERE {}",
                print_ex_with(&value, true),
                print_ex_with(&filter, true)
            );
            let parsed = parse(&sql).map_err(|refused| {
                TestCaseError::fail(format!("{sql}: {} at {}", refused.message, refused.at))
            })?;
            let select = parsed.first().expect("one select");
            let scope = Scope {
                aliases: std::collections::BTreeMap::from([(
                    "o".to_owned(),
                    ("shop.Order".to_owned(), String::new()),
                )]),
                base: "o".to_owned(),
            };
            let catalog = EffectiveCatalog::from_artifacts_with_registry(&pool(), &BTreeSet::new());
            let plan = match lower_sql(&sql, &catalog) {
                Ok(plan) => plan,
                // The one refusal of a lowerable tree: a text no ZEN quote holds.
                Err(found) => {
                    prop_assert!(
                        unquotable(&filter)
                            || unquotable(&value)
                            || pathless(&filter, &scope)
                            || pathless(&value, &scope),
                        "{}: {:?}",
                        sql,
                        found
                    );
                    prop_assert!(found.iter().all(|d| d.code.as_str() == super::SQL_UNSUPPORTED));
                    return Ok(());
                }
            };
            let nodes = plan.nodes.as_object().expect("nodes");
            let predicate = nodes
                .values()
                .find(|node| node["op"] == "filter")
                .and_then(|node| node["predicate"].as_str())
                .expect("a filter node");
            let projected = nodes
                .values()
                .filter(|node| node["op"] == "project")
                .flat_map(|node| node["columns"].as_array().into_iter().flatten())
                .find(|column| column["name"] == "v")
                .and_then(|column| column["expression"].as_str())
                .expect("the projected value");
            for (zen, ex) in [(predicate, select.filter.as_ref().expect("filter")), (projected, &select.items[0].ex)] {
                let tree = row_expression(zen, "input")
                    .map_err(|error| TestCaseError::fail(format!("{zen:?} does not parse: {error}")))?;
                prop_assert_eq!(tree, expected(ex, &scope), "SQL {} lowered to {:?}", sql, zen);
            }
        }

        /// print → parse → print is the identity: the parser reads every
        /// construct of the dialect as written, with nothing dropped.
        #[test]
        fn a_printed_query_parses_back_to_itself(query in vec(any_select(), 1..3)) {
            let text = print_query(&query);
            let parsed = parse(&text).map_err(|refused| {
                TestCaseError::fail(format!("{text}: {} at {}", refused.message, refused.at))
            })?;
            prop_assert_eq!(print_query(&parsed), text);
        }

        /// Every query the parser accepts lowers to a plan that the checker
        /// admits or refuses with a registered code.
        #[test]
        fn an_accepted_query_is_admitted_or_refused_with_a_registered_code(query in shop_query()) {
            let text = print_query(&query);
            prop_assert!(parse(&text).is_ok(), "the parser refused its own print: {}", text);
            match compile(&text) {
                Ok(artifact) => {
                    prop_assert_eq!(artifact["payload"]["calc"]["kind"].as_str(), Some("relational"));
                }
                Err(found) => registered(&found)?,
            }
        }

        /// No text panics the dialect or the checker behind it; anything
        /// refused is refused with a registered code.
        #[test]
        fn no_text_panics_and_every_refusal_is_registered(
            text in prop_oneof![
                "\\PC{0,80}",
                vec(select(&SOUP[..]), 0..30).prop_map(|tokens| tokens.join(" ")),
            ]
        ) {
            if let Err(found) = compile(&text) {
                registered(&found)?;
            }
        }
    }
}
