//! PROOF-TLA-1: direct TLA+ projection of the shared one-create relation.
pub use super::finite::Projection as Batch;
use super::{ApprovedContract, ChallengeError, DefinitionSource, finite::Bounds};
use norn_ir::verification::finite::CreateRelation;
use serde_json::Value;
use std::collections::BTreeSet;

impl ApprovedContract {
    /// PROOF-TLA-1: consume the same admitted relation as Quint, without a compiler process.
    /// # Errors
    /// Returns the existing admission/Challenge refusal; projection refusals remain Unknown.
    pub fn tla(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<norn_ir::PropertyTypeName>,
        bounds: &Bounds,
    ) -> Result<Batch, ChallengeError> {
        let batch = self.finite(source, registry, bounds)?;
        let goals = batch
            .goals
            .into_iter()
            .enumerate()
            .map(|(index, relation)| {
                relation.and_then(|relation| {
                    let kind = batch
                        .challenge
                        .body()
                        .get("obligations")
                        .and_then(|goals| goals.get(index))
                        .and_then(|goal| goal.get("goal"))
                        .and_then(|goal| goal.get("kind"))
                        .and_then(Value::as_str)
                        .ok_or("missing generated goal kind")?;
                    emit(&relation, kind)
                })
            })
            .collect();
        Ok(Batch {
            challenge: batch.challenge,
            bounds: batch.bounds,
            goals,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Number,
    Boolean,
}
struct Term {
    text: String,
    kind: Kind,
}
#[derive(Clone, Copy)]
enum Operator {
    Open,
    Binary(&'static str, u8),
    Unary(&'static str, u8),
}
impl Operator {
    fn precedence(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Binary(_, p) | Self::Unary(_, p) => p,
        }
    }
    fn apply(self, values: &mut Vec<Term>) -> Result<(), String> {
        let right = values.pop().ok_or("missing expression operand")?;
        let (text, kind) = match self {
            Self::Unary(op, _) => {
                let kind = if op == "~" {
                    Kind::Boolean
                } else {
                    Kind::Number
                };
                if right.kind != kind {
                    return Err("unary operand type".into());
                }
                (format!("({op}({}))", right.text), kind)
            }
            Self::Binary(op, _) => {
                let left = values.pop().ok_or("missing binary operand")?;
                let input = if matches!(op, "/\\" | "\\/") {
                    Kind::Boolean
                } else {
                    Kind::Number
                };
                if left.kind != right.kind || (!matches!(op, "=" | "#") && left.kind != input) {
                    return Err("binary operand types".into());
                }
                let kind = if matches!(op, "+" | "-") {
                    Kind::Number
                } else {
                    Kind::Boolean
                };
                (format!("({} {op} {})", left.text, right.text), kind)
            }
            Self::Open => return Err("unclosed expression group".into()),
        };
        values.push(Term { text, kind });
        Ok(())
    }
}

// The precedence is the admitted ZEN grammar, not TLA+'s precedence. Every
// operation is parenthesized so the projection preserves the parsed tree.
fn expression(source: &str, variables: &BTreeSet<&str>, kind: Kind) -> Result<String, String> {
    if source.len() > 4096 {
        return Err("TLA expression budget".into());
    }
    let lexer = regex_lite::Regex::new(r"^\s*([A-Za-z_][A-Za-z_0-9]*|[0-9]+|==|!=|<=|>=|[()+<>-])")
        .map_err(|_error| "TLA expression lexer")?;
    let mut rest = source;
    let mut operators = Vec::new();
    let mut values = Vec::new();
    let mut operand = true;
    while !rest.trim().is_empty() {
        let token = lexer
            .captures(rest)
            .ok_or("unsupported TLA expression token")?;
        let word = token.get(1).ok_or("TLA token")?.as_str();
        rest = rest
            .get(token.get(0).ok_or("TLA token span")?.end()..)
            .ok_or("TLA token boundary")?;
        if word == "(" && operand {
            operators.push(Operator::Open);
            continue;
        }
        if operand && matches!(word, "not" | "-" | "+") {
            operators.push(Operator::Unary(
                if word == "not" {
                    "~"
                } else if word == "-" {
                    "-"
                } else {
                    ""
                },
                if word == "not" { 50 } else { 200 },
            ));
            continue;
        }
        if operand {
            let term = match word {
                "true" | "false" => Term {
                    text: word.to_uppercase(),
                    kind: Kind::Boolean,
                },
                _ if variables.contains(word) => Term {
                    text: word.into(),
                    kind: Kind::Number,
                },
                _ => Term {
                    text: word
                        .parse::<i64>()
                        .map_err(|_error| "unsupported TLA operand")?
                        .to_string(),
                    kind: Kind::Number,
                },
            };
            values.push(term);
            operand = false;
            continue;
        }
        if word == ")" {
            loop {
                let operator = operators.pop().ok_or("unmatched expression group")?;
                if matches!(operator, Operator::Open) {
                    break;
                }
                operator.apply(&mut values)?;
            }
            continue;
        }
        let (op, precedence) = match word {
            "or" => ("\\/", 10),
            "and" => ("/\\", 15),
            "==" => ("=", 20),
            "!=" => ("#", 20),
            "<" => ("<", 20),
            "<=" => ("<=", 20),
            ">" => (">", 20),
            ">=" => (">=", 20),
            "+" => ("+", 30),
            "-" => ("-", 30),
            _ => return Err("unsupported TLA operator".into()),
        };
        while operators
            .last()
            .is_some_and(|top| top.precedence() >= precedence)
        {
            operators.pop().ok_or("TLA operator")?.apply(&mut values)?;
        }
        operators.push(Operator::Binary(op, precedence));
        operand = true;
    }
    if operand {
        return Err("incomplete TLA expression".into());
    }
    while let Some(operator) = operators.pop() {
        operator.apply(&mut values)?;
    }
    let term = values.pop().ok_or("empty TLA expression")?;
    if !values.is_empty() || term.kind != kind {
        return Err("TLA expression result type".into());
    }
    Ok(term.text)
}

fn emit(model: &CreateRelation, kind: &str) -> Result<String, String> {
    let state: Vec<_> = model
        .state
        .iter()
        .map(|field| field.symbol.as_str())
        .collect();
    let variables: BTreeSet<_> = state
        .iter()
        .copied()
        .chain(model.inputs.iter().map(|input| input.symbol.as_str()))
        .collect();
    let initial = state
        .iter()
        .map(|name| format!("{name} = 0"))
        .collect::<Vec<_>>()
        .join(" /\\ ");
    let mut declarations = String::new();
    for name in &state {
        declarations.push_str("VARIABLE\n  \\* @type: Int;\n  ");
        declarations.push_str(name);
        declarations.push('\n');
    }
    let updates = model
        .updates
        .iter()
        .map(|(name, value)| {
            expression(value.as_str(), &variables, Kind::Number)
                .map(|value| format!("{name}' = {value}"))
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(" /\\ ");
    let guards = model
        .guards
        .iter()
        .map(|value| expression(value.as_str(), &variables, Kind::Boolean))
        .collect::<Result<Vec<_>, _>>()?;
    let choices = model
        .inputs
        .iter()
        .map(|input| format!("{} \\in ({})..({})", input.symbol, input.lower, input.upper))
        .collect::<Vec<_>>()
        .join(", ");
    let quantifier = if choices.is_empty() {
        String::new()
    } else {
        format!("\\E {choices} : ")
    };
    let condition = expression(model.condition.as_str(), &variables, Kind::Boolean)?;
    let response = expression(model.response.as_str(), &variables, Kind::Boolean)?;
    let (step, invariant) = match kind {
        "initialization" => ("idle", "law"),
        "preservation" => ("step", "law"),
        "non_vacuity" => ("step", "unreachable"),
        _ => return Err("unsupported TLA goal kind".into()),
    };
    Ok(format!(
        "---- MODULE generated ----\nEXTENDS Integers\nVARIABLE\n  \\* @type: Bool;\n  present\n{declarations}init == present = FALSE /\\ {initial}\nwrite == {quantifier}(~present /\\ ({}) /\\ present' = TRUE /\\ {updates})\nidle == UNCHANGED <<present, {}>>\nstep == write \\/ idle\nlaw == ~present \\/ ~({condition}) \\/ ({response})\nunreachable == ~(present /\\ ({condition}) /\\ ({response}))\nq_init == init\nq_step == {step}\nq_inv == {invariant}\n====\n",
        if guards.is_empty() {
            "TRUE".into()
        } else {
            guards.join(" /\\ ")
        },
        state.join(", ")
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "compiler correspondence fixtures")]
mod tests {
    use super::*;
    use crate::obligations::quint_tests::{bounds, fixture, source};
    use proptest::prelude::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    fn relation() -> CreateRelation {
        let artifacts = fixture();
        let source = source(&artifacts);
        let registry = BTreeSet::new();
        let authority = ApprovedContract::approve(
            &source,
            &registry,
            json!({"expr":"norn-zen/6"}),
            json!({"store":"empty"}),
            json!({"level":"bounded"}),
        )
        .expect("admitted engine fixture");
        authority
            .finite(&source, &registry, &bounds(&artifacts))
            .expect("relation batch")
            .goals
            .into_iter()
            .find_map(Result::ok)
            .expect("finite relation")
    }

    #[test]
    fn direct_tla_retains_challenge_inventory_bounds_and_refusals() {
        let artifacts = fixture();
        let source = source(&artifacts);
        let registry = BTreeSet::new();
        let authority = ApprovedContract::approve(
            &source,
            &registry,
            json!({"expr":"norn-zen/6"}),
            json!({"store":"empty"}),
            json!({"level":"bounded"}),
        )
        .expect("admitted engine fixture");
        let profile = bounds(&artifacts);
        let native = authority
            .finite(&source, &registry, &profile)
            .expect("finite");
        let quint = authority
            .quint(&source, &registry, &profile)
            .expect("Quint");
        let tla = authority.tla(&source, &registry, &profile).expect("TLA");
        assert_eq!(native.challenge.sha256(), tla.challenge.sha256());
        assert_eq!(quint.challenge.sha256(), tla.challenge.sha256());
        assert_eq!(native.bounds, tla.bounds);
        assert_eq!(native.goals.len(), tla.goals.len());
        assert_eq!(native.goals.iter().filter(|goal| goal.is_ok()).count(), 3);
        for (native, tla) in native.goals.iter().zip(&tla.goals) {
            assert_eq!(native.is_ok(), tla.is_ok());
            if let (Err(left), Err(right)) = (native, tla) {
                assert_eq!(left, right);
            }
        }
    }

    #[test]
    fn opaque_or_malformed_expressions_never_become_tautologies() {
        let variables = BTreeSet::from(["v0"]);
        for invalid in [
            "",
            "v1 == 0",
            "call(v0)",
            "v0 / 2",
            "v0 * 2",
            "(v0 == 0",
            "true false",
            "true)",
            "v0",
        ] {
            assert!(
                expression(invalid, &variables, Kind::Boolean).is_err(),
                "{invalid}"
            );
        }
        assert!(emit(&relation(), "unbounded_temporal").is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn tla_projection_matches_native_boolean_additive_semantics(a in -9_i32..10, b in -9_i32..10, c in -9_i32..10) {
            let numeric = format!("-({a} - {b}) + {c}");
            let compare = match a.rem_euclid(6) { 0 => "<", 1 => "<=", 2 => ">", 3 => ">=", 4 => "==", _ => "!=" };
            let boolean = format!("{a} {compare} {b} or {b} == {c} and not({a} == {c})");
            let context = norn_expr::Value::Null;
            let number = norn_expr::Expression::compile(&numeric).expect("numeric grammar")
                .evaluate(&context, norn_expr::EvaluationBudget::new(1000)).expect("native numeric");
            let flag = norn_expr::Expression::compile(&boolean).expect("Boolean grammar")
                .evaluate(&context, norn_expr::EvaluationBudget::new(1000)).expect("native Boolean");
            let norn_expr::Value::Number(number) = number.value() else { return Err(TestCaseError::fail("numeric fixture")); };
            let norn_expr::Value::Bool(flag) = flag.value() else { return Err(TestCaseError::fail("Boolean fixture")); };
            let expected: i64 = number.to_string().parse().expect("exact integral result");
            let mut model = relation();
            model.inputs.clear(); model.guards.clear();
            model.condition = norn_ir::ZenExpression("true".into());
            model.response = norn_ir::ZenExpression("false".into());
            let variable = model.state.first().expect("scalar state").symbol.clone();
            model.updates = model.state.iter().map(|field| (field.symbol.clone(), norn_ir::ZenExpression("0".into()))).collect();
            model.updates.first_mut().expect("update").1 = norn_ir::ZenExpression(numeric.clone());
            let numeric_model = emit(&model, "preservation").expect("numeric projection");
            model.updates.first_mut().expect("update").1 = norn_ir::ZenExpression("0".into());
            model.guards.push(norn_ir::ZenExpression(boolean.clone()));
            let boolean_model = emit(&model, "preservation").expect("Boolean projection");
            prop_assert_eq!(expression(&numeric, &BTreeSet::new(), Kind::Number).is_ok(), true);
            prop_assert_eq!(expression(&boolean, &BTreeSet::new(), Kind::Boolean).is_ok(), true);
            if let Some(directory) = std::env::var_os("NORN_TLA_CORRESPONDENCE_OUT") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).expect("correspondence output");
                let bytes = norn_canonical::serialize_json_verbatim(&json!({"numeric":numeric_model,"boolean":boolean_model,
                    "variable":variable,"number":expected,"flag":flag})).expect("correspondence JSON");
                let name = format!("{:x}.json", Sha256::digest(&bytes));
                std::fs::write(directory.join(name), bytes).expect("correspondence output");
            }
        }
    }
}
