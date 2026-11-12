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
//! Expressions are comparisons, `AND`/`OR`/`NOT`, `+ - * /`, `IS [NOT] NULL`,
//! `COALESCE` and `CASE WHEN … THEN … ELSE … END`. Rows have no SQL order: a
//! transform's rows are ordered by their grain, so `ORDER BY` and `LIMIT`
//! outside a window refuse.

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

fn refusal(message: impl Into<String>, at: usize) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(SQL_UNSUPPORTED),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        span: Some(Span::pointer(format!("/sql/{at}"))),
        artifact_id: None,
        remediation: Some(DiagnosticRemediation::FixDefinition),
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
fn tokenize(source: &str) -> Result<Vec<(Token, usize)>, Diagnostic> {
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
            out.push((
                Token::Number(rest.get(..end).unwrap_or_default().to_owned()),
                at,
            ));
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

#[derive(Debug, Clone, PartialEq)]
struct Call {
    name: String,
    args: Vec<Ex>,
    star: bool,
    distinct: bool,
    over: Option<Over>,
    at: usize,
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

    fn expect_keyword(&mut self, word: &str) -> Result<(), Diagnostic> {
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

    fn expect_symbol(&mut self, symbol: &str) -> Result<(), Diagnostic> {
        if self.eat_symbol(symbol) {
            Ok(())
        } else {
            Err(refusal(format!("expected `{symbol}`"), self.at()))
        }
    }

    fn identifier(&mut self) -> Result<String, Diagnostic> {
        let at = self.at();
        match self.next() {
            Some(Token::Word(word)) if !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&word)) => {
                Ok(word)
            }
            Some(Token::Quoted(name)) => Ok(name),
            _ => Err(refusal("expected a name", at)),
        }
    }

    fn query(&mut self) -> Result<Vec<Select>, Diagnostic> {
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

    fn source(&mut self) -> Result<Source, Diagnostic> {
        let name = self.identifier()?;
        let name = if self.eat_symbol(".") {
            format!("{name}.{}", self.identifier()?)
        } else {
            name
        };
        self.eat_keyword("as");
        let alias = if matches!(self.peek(), Some(Token::Word(w)) if !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(w)))
            || matches!(self.peek(), Some(Token::Quoted(_)))
        {
            self.identifier()?
        } else {
            name.rsplit('.')
                .next()
                .unwrap_or(&name)
                .to_ascii_lowercase()
        };
        Ok(Source { name, alias })
    }

    fn select(&mut self) -> Result<Select, Diagnostic> {
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
                    Some(self.identifier()?)
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
                group_by.push((self.expression()?, at));
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

    fn expression(&mut self) -> Result<Ex, Diagnostic> {
        let mut left = self.conjunction()?;
        while self.eat_keyword("or") {
            left = Ex::Binary("or", Box::new(left), Box::new(self.conjunction()?));
        }
        Ok(left)
    }

    fn conjunction(&mut self) -> Result<Ex, Diagnostic> {
        let mut left = self.negation()?;
        while self.eat_keyword("and") {
            left = Ex::Binary("and", Box::new(left), Box::new(self.negation()?));
        }
        Ok(left)
    }

    fn negation(&mut self) -> Result<Ex, Diagnostic> {
        if self.eat_keyword("not") {
            return Ok(Ex::Not(Box::new(self.negation()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Ex, Diagnostic> {
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
                return Ok(Ex::Binary(zen, Box::new(left), Box::new(self.sum()?)));
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

    fn sum(&mut self) -> Result<Ex, Diagnostic> {
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

    fn product(&mut self) -> Result<Ex, Diagnostic> {
        let mut left = self.unary()?;
        loop {
            if self.eat_symbol("*") {
                left = Ex::Binary("*", Box::new(left), Box::new(self.unary()?));
            } else if self.eat_symbol("/") {
                left = Ex::Binary("/", Box::new(left), Box::new(self.unary()?));
            } else {
                return Ok(left);
            }
        }
    }

    fn unary(&mut self) -> Result<Ex, Diagnostic> {
        if self.eat_symbol("-") {
            return Ok(Ex::Negate(Box::new(self.unary()?)));
        }
        self.primary()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one primary-expression grammar, read top to bottom"
    )]
    fn primary(&mut self) -> Result<Ex, Diagnostic> {
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
                let first = self.identifier()?;
                if self.eat_symbol("(") {
                    return self.call(first, at);
                }
                if self.eat_symbol(".") {
                    let column = self.identifier()?;
                    return Ok(Ex::Column(Some(first), column));
                }
                Ok(Ex::Column(None, first))
            }
            Some(Token::Symbol(_)) | None => Err(refusal("expected an expression", at)),
        }
    }

    fn call(&mut self, name: String, at: usize) -> Result<Ex, Diagnostic> {
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
                    over.partition.push(self.expression()?);
                    if !self.eat_symbol(",") {
                        break;
                    }
                }
            }
            if self.eat_keyword("order") {
                self.expect_keyword("by")?;
                loop {
                    let key = self.expression()?;
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

const RESERVED: [&str; 31] = [
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
];

/// Parse and lower one SQL query against `catalog`.
///
/// # Errors
/// `derived.validation.sql_unsupported` at the first construct without a
/// typed operator, an unknown type, alias or column, or an undeclared join.
pub fn lower_sql(source: &str, catalog: &EffectiveCatalog) -> Result<SqlPlan, Vec<Diagnostic>> {
    let tokens = tokenize(source).map_err(|d| vec![d])?;
    let mut parser = Parser {
        tokens,
        position: 0,
        end: source.len(),
    };
    let selects = parser.query().map_err(|d| vec![d])?;
    let mut lowering = Lowering {
        catalog,
        nodes: Map::new(),
        inputs: Vec::new(),
        counter: 0,
    };
    let mut outputs = Vec::new();
    for select in &selects {
        outputs.push(lowering.select(select).map_err(|d| vec![d])?);
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
    fn column(&self, alias: Option<&str>, column: &str, at: usize) -> Result<String, Diagnostic> {
        let alias = alias.unwrap_or(&self.base);
        let (_, prefix) = self
            .aliases
            .get(alias)
            .ok_or_else(|| refusal(format!("no source is named {alias:?}"), at))?;
        Ok(format!("{prefix}{column}"))
    }
}

struct Lowering<'c> {
    catalog: &'c EffectiveCatalog,
    nodes: Map<String, Value>,
    inputs: Vec<(String, String)>,
    counter: usize,
}

fn zen_text(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Lowering<'_> {
    fn fresh(&mut self, stem: &str) -> String {
        self.counter = self.counter.saturating_add(1);
        format!("{stem}_{}", self.counter)
    }

    fn scan(&mut self, source: &Source, at: usize) -> Result<String, Diagnostic> {
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

    /// ZEN over `input.<column>` for a row expression; aggregates and windows refuse.
    fn zen(&self, ex: &Ex, scope: &Scope, at: usize) -> Result<String, Diagnostic> {
        Ok(match ex {
            Ex::Column(alias, column) => {
                format!("input.{}", scope.column(alias.as_deref(), column, at)?)
            }
            Ex::Number(number) => number.clone(),
            Ex::Text(text) => zen_text(text),
            Ex::Bool(flag) => flag.to_string(),
            Ex::Null => "null".to_owned(),
            Ex::Not(inner) => format!("not ({})", self.zen(inner, scope, at)?),
            Ex::Negate(inner) => format!("-({})", self.zen(inner, scope, at)?),
            Ex::Binary(op, left, right) => format!(
                "({} {op} {})",
                self.zen(left, scope, at)?,
                self.zen(right, scope, at)?
            ),
            Ex::IsNull(inner, negated) => format!(
                "({} {} null)",
                self.zen(inner, scope, at)?,
                if *negated { "!=" } else { "==" }
            ),
            Ex::Case(condition, then, otherwise) => format!(
                "({} ? {} : {})",
                self.zen(condition, scope, at)?,
                self.zen(then, scope, at)?,
                self.zen(otherwise, scope, at)?
            ),
            Ex::Coalesce(args) => {
                if args.len() < 2 {
                    return Err(refusal("COALESCE takes two or more values", at));
                }
                let parts = args
                    .iter()
                    .map(|arg| self.zen(arg, scope, at))
                    .collect::<Result<Vec<_>, _>>()?;
                format!("({})", parts.join(" ?? "))
            }
            Ex::Call(call) => {
                return Err(refusal(
                    format!("{} is not a row expression here", call.name.to_uppercase()),
                    call.at,
                ));
            }
        })
    }

    fn join(
        &mut self,
        left_node: String,
        scope: &mut Scope,
        join: &JoinClause,
    ) -> Result<String, Diagnostic> {
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
    fn select(&mut self, select: &Select) -> Result<String, Diagnostic> {
        let mut node = self.scan(&select.from, 0)?;
        let mut scope = Scope {
            aliases: BTreeMap::from([(
                select.from.alias.clone(),
                (select.from.name.clone(), String::new()),
            )]),
            base: select.from.alias.clone(),
        };
        for join in &select.joins {
            node = self.join(node, &mut scope, join)?;
        }
        if let Some(filter) = &select.filter {
            let id = self.fresh("filter");
            self.nodes.insert(
                id.clone(),
                json!({"op": "filter", "input": node, "predicate": self.zen(filter, &scope, 0)?}),
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
            item.alias.clone().unwrap_or_else(|| match &item.ex {
                Ex::Column(_, column) => column.clone(),
                Ex::Call(call) => match call.args.first() {
                    Some(Ex::Column(_, column)) => format!("{}_{column}", call.name),
                    _ => call.name.clone(),
                },
                _ => format!("column_{index}"),
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
                let (value, grain_unit) = match ex {
                    Ex::Call(call) if call.name == "date_trunc" => match call.args.as_slice() {
                        [Ex::Text(unit), value] => (value.clone(), Some(unit.to_ascii_lowercase())),
                        _ => return Err(refusal("DATE_TRUNC('week', column)", call.at)),
                    },
                    other => (other.clone(), None),
                };
                let name = select
                    .items
                    .iter()
                    .enumerate()
                    .find(|(_, item)| item.ex == *ex)
                    .map_or_else(
                        || match &value {
                            Ex::Column(_, column) => column.clone(),
                            _ => format!("key_{}", keys.len()),
                        },
                        |(index, item)| name_of(item, index),
                    );
                prepared.push((name.clone(), self.zen(&value, &scope, *at)?));
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
                match &item.ex {
                    Ex::Call(call) if aggregate(&item.ex) => {
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
                            let measured = format!("{name}_value");
                            prepared.push((measured.clone(), self.zen(arg, &scope, call.at)?));
                            measures
                                .push(json!({"name": name, "fn": function, "column": measured}));
                        }
                    }
                    ex if windowed(ex) => windows.push((name, ex.clone(), item.at)),
                    _ => {
                        return Err(refusal(
                            "in a grouped query every column is a group key or an aggregate",
                            item.at,
                        ));
                    }
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
                let names: BTreeMap<String, String> = select
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| (format!("{:?}", item.ex), name_of(item, index)))
                    .collect();
                node = self.window(node, &windows, &names)?;
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
            columns.push(json!({"name": name, "expression": self.zen(&item.ex, &scope, item.at)?}));
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
            let names: BTreeMap<String, String> = select
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| (format!("{:?}", item.ex), name_of(item, index)))
                .collect();
            node = self.window(node, &windows, &names)?;
        }
        Ok(node)
    }

    /// One window node over output columns named by the select list.
    fn window(
        &mut self,
        input: String,
        windows: &[(String, Ex, usize)],
        names: &BTreeMap<String, String>,
    ) -> Result<String, Diagnostic> {
        let column_of = |ex: &Ex, at: usize| -> Result<String, Diagnostic> {
            match ex {
                Ex::Column(_, column) => Ok(names
                    .get(&format!("{ex:?}"))
                    .cloned()
                    .unwrap_or_else(|| column.clone())),
                other => names
                    .get(&format!("{other:?}"))
                    .cloned()
                    .ok_or_else(|| refusal("a window reads a selected column", at)),
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
            .collect::<Result<Vec<_>, Diagnostic>>()?;
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
                vec![refusal(
                    format!("no installed artifact declares {object_type:?}"),
                    0,
                )]
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
        vec![refusal(
            format!("the lowered artifact does not decode: {error}"),
            0,
        )]
    })?;
    let mut admitted_pool = pool.to_vec();
    admitted_pool.push(envelope.clone());
    super::admit(&envelope, &admitted_pool, registry)?;
    Ok(artifact)
}
