//! PROOF-TRANSITION-1: exact affine views of the checker-owned relation.
//!
//! This consumes the same normalized expressions as Quint, never declarations
//! or emitted solver text. Integers are coefficients in the retained common
//! scale/unit, not floating point. All finite input/presence assumptions remain
//! on the relation; these views supply neither writer closure nor proof authority.

use std::collections::{BTreeMap, BTreeSet};

pub use super::finite::{Batch, Bounds};
pub use norn_ir::verification::finite::{
    AssignmentOrigin, CreateRelation, Derivation, FiniteInput, ScalarState,
};

/// An exact integer constant plus a sorted map of symbolic integer coefficients.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Affine {
    /// Constant in the relation's scaled integer interpretation.
    pub constant: i128,
    /// Dimensionless coefficients keyed by retained state/input symbols.
    pub coefficients: BTreeMap<String, i128>,
}

impl Affine {
    fn combine(mut self, rhs: Self, sign: i128) -> Result<Self, String> {
        self.constant = accumulate(self.constant, rhs.constant, sign)?;
        for (symbol, value) in rhs.coefficients {
            let value = accumulate(
                self.coefficients.get(&symbol).copied().unwrap_or(0),
                value,
                sign,
            )?;
            if value == 0 {
                self.coefficients.remove(&symbol);
            } else {
                self.coefficients.insert(symbol, value);
            }
        }
        Ok(self)
    }
}

fn accumulate(left: i128, right: i128, sign: i128) -> Result<i128, String> {
    left.checked_add(right.checked_mul(sign).ok_or("affine overflow")?)
        .ok_or_else(|| "affine overflow".into())
}

/// Arithmetic views only; each unsupported part has its own Unknown reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffineRelation {
    /// Empty-to-created per-property delta; the absent numeric pre-state is zero.
    pub deltas: Vec<(String, Result<Affine, String>)>,
    /// Ordered derivation values, with targets and input requirements on the relation.
    pub derivations: Vec<Result<Affine, String>>,
    /// Left minus right of one equality, interpreted as equal to zero.
    /// Guards and the law condition are not discharged or erased by this view.
    pub law_equality: Result<Affine, String>,
}

/// Project already lowered relations; finite coverage is never generalized.
#[must_use]
pub fn project(relation: &CreateRelation) -> AffineRelation {
    let inputs = relation
        .inputs
        .iter()
        .map(|input| input.symbol.clone())
        .collect();
    let state = relation
        .state
        .iter()
        .map(|field| field.symbol.clone())
        .collect();
    AffineRelation {
        deltas: relation
            .updates
            .iter()
            .map(|(symbol, expression)| (symbol.clone(), affine(expression.as_str(), &inputs)))
            .collect(),
        derivations: relation
            .derivations
            .iter()
            .map(|derivation| affine(derivation.expression.as_str(), &inputs))
            .collect(),
        law_equality: equality(relation.response.as_str(), &state),
    }
}

/// Parse only normalized additive arithmetic over explicitly retained symbols.
/// # Errors
/// Unsupported syntax, unknown symbols, budget exhaustion or overflow is Unknown.
pub fn affine(source: &str, symbols: &BTreeSet<String>) -> Result<Affine, String> {
    let tokens = tokens(source)?;
    arithmetic(&tokens, symbols)
}

/// Return the exact residual of a single affine equality.
/// # Errors
/// Non-equality/Boolean predicates remain Unknown, rather than losing conjuncts.
pub fn equality(source: &str, symbols: &BTreeSet<String>) -> Result<Affine, String> {
    let tokens = tokens(source)?;
    let mut expression = tokens.as_slice();
    while let ["(", inner @ .., ")"] = expression {
        let encloses = expression
            .iter()
            .enumerate()
            .try_fold(0_i32, |depth, (index, token)| {
                let depth = match *token {
                    "(" => depth.checked_add(1)?,
                    ")" => depth.checked_sub(1)?,
                    _ => depth,
                };
                (depth > 0 || index.checked_add(1) == Some(expression.len())).then_some(depth)
            })
            .is_some();
        if !encloses {
            break;
        }
        expression = inner;
    }
    let positions: Vec<_> = expression
        .iter()
        .enumerate()
        .filter_map(|(index, token)| (*token == "==").then_some(index))
        .collect();
    let [position] = positions.as_slice() else {
        return Err("one affine equality required".into());
    };
    arithmetic(expression.get(..*position).ok_or("equality lhs")?, symbols)?.combine(
        arithmetic(
            expression
                .get(position.checked_add(1).ok_or("equality index")?..)
                .ok_or("equality rhs")?,
            symbols,
        )?,
        -1,
    )
}

fn tokens(source: &str) -> Result<Vec<&str>, String> {
    if source.len() > 4096 {
        return Err("affine expression budget".into());
    }
    let lexer = regex_lite::Regex::new(r"^\s*([A-Za-z_][A-Za-z_0-9]*|[0-9]+|==|[()+-])")
        .map_err(|_error| "affine lexer")?;
    let mut rest = source;
    let mut tokens = Vec::new();
    while !rest.trim().is_empty() {
        let token = lexer.captures(rest).ok_or("unsupported affine syntax")?;
        tokens.push(token.get(1).ok_or("affine token")?.as_str());
        if tokens.len() > 512 {
            return Err("affine token budget".into());
        }
        rest = rest
            .get(token.get(0).ok_or("affine token")?.end()..)
            .ok_or("affine boundary")?;
    }
    Ok(tokens)
}

fn arithmetic(tokens: &[&str], symbols: &BTreeSet<String>) -> Result<Affine, String> {
    let mut parser = Parser {
        tokens,
        index: 0,
        symbols,
    };
    let value = parser.sum(0)?;
    if parser.index != tokens.len() {
        return Err("unsupported affine operation".into());
    }
    Ok(value)
}

struct Parser<'a> {
    tokens: &'a [&'a str],
    index: usize,
    symbols: &'a BTreeSet<String>,
}
impl Parser<'_> {
    fn sum(&mut self, depth: usize) -> Result<Affine, String> {
        let mut value = self.atom(depth)?;
        while let Some(operator @ ("+" | "-")) = self.tokens.get(self.index).copied() {
            self.index = self.index.checked_add(1).ok_or("affine token budget")?;
            value = value.combine(self.atom(depth)?, if operator == "+" { 1 } else { -1 })?;
        }
        Ok(value)
    }
    fn atom(&mut self, depth: usize) -> Result<Affine, String> {
        if depth >= 64 {
            return Err("affine nesting budget".into());
        }
        let token = self
            .tokens
            .get(self.index)
            .copied()
            .ok_or("missing affine operand")?;
        self.index = self.index.checked_add(1).ok_or("affine token budget")?;
        match token {
            "-" => Affine::default().combine(
                self.atom(depth.checked_add(1).ok_or("affine nesting budget")?)?,
                -1,
            ),
            "+" => self.atom(depth.checked_add(1).ok_or("affine nesting budget")?),
            "(" => {
                let value = self.sum(depth.checked_add(1).ok_or("affine nesting budget")?)?;
                if self.tokens.get(self.index) != Some(&")") {
                    return Err("unclosed affine operand".into());
                }
                self.index = self.index.checked_add(1).ok_or("affine token budget")?;
                Ok(value)
            }
            symbol if self.symbols.contains(symbol) => Ok(Affine {
                constant: 0,
                coefficients: BTreeMap::from([(symbol.to_owned(), 1)]),
            }),
            literal if literal.bytes().all(|byte| byte.is_ascii_digit()) => Ok(Affine {
                constant: literal
                    .parse()
                    .map_err(|_error| "affine integer overflow")?,
                coefficients: BTreeMap::new(),
            }),
            _ => Err("unsupported affine symbol".into()),
        }
    }
}
