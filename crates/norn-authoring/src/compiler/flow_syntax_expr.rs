//! Pure data-expression lowering methods for [`SyntaxCompiler`].
//!
//! Split out of `flow_syntax.rs` to keep every compiler module under 800 LOC.

use std::collections::BTreeMap;

use serde_json::Value;
use tsrs_ast::{Kind, Node, get_source_file_of_node};
use tsrs_checker::{Checker, TypeFlags, is_tuple_type_exported};
use tsrs_core::P;

use crate::checker::resolve::resolve_symbol;

use super::expr::{BinaryOp, CallFn, ClosureFn, CompareOp, Expr};
use super::flow_syntax::{DecisionHooks, Env, SyntaxCompiler, json_scalar_to_string};
use super::types::{CompileError, exact_number, is_string_literal_like};
use super::value_schema::{BudgetExceeded, ExpansionBudget};

impl SyntaxCompiler<'_> {
    pub(crate) fn scalar(
        &mut self,
        expr: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Value, CompileError> {
        let empty = Env::new();
        let lowered = self.data(expr, &empty, checker, expansion, None)?;
        match lowered {
            Expr::Literal(v) => Ok(v),
            Expr::Binary { .. }
            | Expr::Call { .. }
            | Expr::Closure { .. }
            | Expr::Conditional { .. }
            | Expr::Coalesce { .. }
            | Expr::Bound { .. }
            | Expr::Input { .. }
            | Expr::Compare { .. }
            | Expr::Logical { .. }
            | Expr::Not(_)
            | Expr::List(_)
            | Expr::Record(_) => Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "expected scalar metadata literal",
                expr,
            )),
        }
    }

    pub(crate) fn record_literal(
        &mut self,
        expr: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<BTreeMap<String, Expr>, CompileError> {
        let empty = Env::new();
        let lowered = self.data(expr, &empty, checker, expansion, None)?;
        match lowered {
            Expr::Record(fields) => Ok(fields.into_iter().collect()),
            Expr::Binary { .. }
            | Expr::Call { .. }
            | Expr::Closure { .. }
            | Expr::Conditional { .. }
            | Expr::Coalesce { .. }
            | Expr::Bound { .. }
            | Expr::Literal(_)
            | Expr::Input { .. }
            | Expr::Compare { .. }
            | Expr::Logical { .. }
            | Expr::Not(_)
            | Expr::List(_) => Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "metadata must be a record literal",
                expr,
            )),
        }
    }

    pub(crate) fn data(
        &mut self,
        input: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Expr, CompileError> {
        let value = self.data_inner(input, env, checker, expansion, hooks)?;
        self.record_expression_source(&value, input);
        expansion
            .assert_value(&value.to_json())
            .map_err(|BudgetExceeded(msg)| CompileError::new("NORN-FLOW-UNBOUNDED", msg, input))?;
        Ok(value)
    }

    fn data_inner(
        &mut self,
        input: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Expr, CompileError> {
        self.check_budget(input)?;
        self.depth = self.depth.saturating_add(1);
        let result = self.data_dispatch(input, env, checker, expansion, hooks);
        self.depth = self.depth.saturating_sub(1);
        result
    }

    fn data_dispatch(
        &mut self,
        input: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        mut hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Expr, CompileError> {
        let e = Self::unwrap(input);
        let kind = e.kind();
        if is_string_literal_like(e) {
            return Ok(Expr::lit(Value::String(e.text().to_owned())));
        }
        if kind == Kind::NumericLiteral {
            let sf = get_source_file_of_node(e).unwrap_or(self.source);
            let n = exact_number(sf, e, self.started, self.limits.max_millis, false)?;
            return Ok(Expr::lit(Value::Number(n)));
        }
        if kind == Kind::TrueKeyword || kind == Kind::FalseKeyword {
            return Ok(Expr::lit(Value::Bool(kind == Kind::TrueKeyword)));
        }
        if kind == Kind::NullKeyword {
            return Ok(Expr::lit(Value::Null));
        }
        if kind == Kind::Identifier {
            return self.data_identifier(e, env, checker, expansion, hooks);
        }
        if kind == Kind::PropertyAccessExpression || kind == Kind::ElementAccessExpression {
            return self.data_member_access(e, env, checker, expansion, hooks);
        }
        if kind == Kind::ArrayLiteralExpression {
            let mut items = Vec::new();
            for &el in e.as_array_literal_expression().elements.nodes() {
                items.push(self.data(el, env, checker, expansion, hooks.as_deref_mut())?);
            }
            return Ok(Expr::List(items));
        }
        if kind == Kind::ObjectLiteralExpression {
            return self.data_object_literal(e, env, checker, expansion, hooks);
        }
        if kind == Kind::PrefixUnaryExpression {
            return self.data_prefix_unary(e, env, checker, expansion, hooks);
        }
        if kind == Kind::ConditionalExpression {
            let ce = e.as_conditional_expression();
            let cond = self.data(ce.condition, env, checker, expansion, hooks.as_deref_mut())?;
            let yes = self.data(ce.when_true, env, checker, expansion, hooks.as_deref_mut())?;
            let no = self.data(ce.when_false, env, checker, expansion, hooks)?;
            return Ok(Expr::conditional(cond, yes, no));
        }
        if kind == Kind::CallExpression {
            return self.data_call(e, env, checker, expansion, hooks);
        }
        if kind == Kind::BinaryExpression {
            return self.data_binary(e, env, checker, expansion, hooks);
        }
        Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "expression has no first-order lowering",
            e,
        ))
    }

    fn data_identifier(
        &mut self,
        e: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Expr, CompileError> {
        let sym = self.symbol(checker, e)?;
        if let Some(bound) = env.get(&sym).or_else(|| self.constants.get(&sym)) {
            return Ok(bound.clone());
        }
        if self.resolving.contains(&sym) {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "cyclic data constant",
                e,
            ));
        }
        let Some(decl) = sym.value_declaration() else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "unbound pure value",
                e,
            ));
        };
        if decl.kind() != Kind::VariableDeclaration {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "unbound pure value",
                e,
            ));
        }
        let Some(init) = decl.as_variable_declaration().initializer() else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "unbound pure value",
                e,
            ));
        };
        self.resolving.insert(sym);
        let value = self.data(init, env, checker, expansion, hooks);
        self.resolving.remove(&sym);
        let val = value?;
        self.constants.insert(sym, val.clone());
        Ok(val)
    }

    fn data_member_access(
        &mut self,
        e: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        mut hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Expr, CompileError> {
        if e.kind() == Kind::PropertyAccessExpression
            && let Some(special) =
                self.data_special_property(e, env, checker, expansion, hooks.as_deref_mut())?
        {
            return Ok(special);
        }
        let (base_node, key) = if e.kind() == Kind::PropertyAccessExpression {
            let pa = e.as_property_access_expression();
            (pa.expression(), pa.name().text().to_owned())
        } else {
            let ea = e.as_element_access_expression();
            let key_val = self.scalar(ea.argument_expression(), checker, expansion)?;
            (ea.expression(), json_scalar_to_string(&key_val))
        };
        let base = self.data(base_node, env, checker, expansion, hooks)?;
        Self::data_select_member(e, base, key)
    }

    fn data_special_property(
        &mut self,
        e: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Option<Expr>, CompileError> {
        let pa = e.as_property_access_expression();
        let resolved = resolve_symbol(
            checker,
            self.intrinsics_module,
            self.effect_dist_prefix,
            self.vfs,
            pa.name(),
        );
        if let Some(decl) = resolved.target.and_then(|s| s.value_declaration())
            && decl.kind() == Kind::VariableDeclaration
            && let Some(init) = decl.as_variable_declaration().initializer()
            && get_source_file_of_node(decl) != get_source_file_of_node(e)
        {
            return self.data(init, env, checker, expansion, hooks).map(Some);
        }
        if pa.name().text() != "length" {
            return Ok(None);
        }
        let ty = checker.get_type_at_location(pa.expression());
        if checker.is_array_type_exported(ty) || is_tuple_type_exported(ty) {
            let list = self.data(pa.expression(), env, checker, expansion, hooks)?;
            if let Expr::Closure {
                func: ClosureFn::Filter,
                list: inner,
                binder,
                body,
            } = list
            {
                return Ok(Some(Expr::Closure {
                    func: ClosureFn::Count,
                    list: inner,
                    binder,
                    body,
                }));
            }
            return Ok(Some(Expr::call(CallFn::Len, vec![list])));
        }
        if ty.flags().intersects(TypeFlags::StringLike) {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "string.length counts UTF-16 units in JavaScript; use Norn.byteLength(text) for the canonical UTF-8 byte count",
                e,
            ));
        }
        Ok(None)
    }

    fn data_select_member(e: P<Node>, base: Expr, key: String) -> Result<Expr, CompileError> {
        match base {
            Expr::Input { .. } | Expr::Bound { .. } => base
                .field(key)
                .map_err(|err| CompileError::new(err.code, err.message, e)),
            Expr::Record(fields) => fields
                .into_iter()
                .find(|(name, _)| name == &key)
                .map(|(_, v)| v)
                .ok_or_else(|| {
                    CompileError::new(
                        "NORN-FLOW-SLICE",
                        "property is not present in compiled data",
                        e,
                    )
                }),
            Expr::List(items) => {
                if key.bytes().all(|b| b.is_ascii_digit())
                    && !key.is_empty()
                    && let Ok(idx) = key.parse::<usize>()
                    && let Some(item) = items.get(idx)
                {
                    return Ok(item.clone());
                }
                Err(CompileError::new(
                    "NORN-FLOW-SLICE",
                    "property is not present in compiled data",
                    e,
                ))
            }
            Expr::Binary { .. }
            | Expr::Call { .. }
            | Expr::Closure { .. }
            | Expr::Conditional { .. }
            | Expr::Coalesce { .. }
            | Expr::Literal(_)
            | Expr::Compare { .. }
            | Expr::Logical { .. }
            | Expr::Not(_) => Err(CompileError::new(
                "NORN-FLOW-SLICE",
                "property is not present in compiled data",
                e,
            )),
        }
    }

    fn data_object_literal(
        &mut self,
        e: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        mut hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Expr, CompileError> {
        let mut fields = Vec::new();
        for &prop in e.as_object_literal_expression().properties.nodes() {
            let pk = prop.kind();
            if pk == Kind::ShorthandPropertyAssignment {
                let name_node = prop.as_shorthand_property_assignment().name();
                let val = self.data(name_node, env, checker, expansion, hooks.as_deref_mut())?;
                fields.push((name_node.text().to_owned(), val));
            } else if pk == Kind::PropertyAssignment {
                let pa = prop.as_property_assignment();
                let nk = pa.name().kind();
                if nk != Kind::Identifier && nk != Kind::StringLiteral && nk != Kind::NumericLiteral
                {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "unsupported record field",
                        prop,
                    ));
                }
                let val = self.data(
                    pa.initializer(),
                    env,
                    checker,
                    expansion,
                    hooks.as_deref_mut(),
                )?;
                fields.push((pa.name().text().to_owned(), val));
            } else {
                return Err(CompileError::new(
                    "NORN-FLOW-EXPRESSION",
                    "unsupported record field",
                    prop,
                ));
            }
        }
        Ok(Expr::Record(fields))
    }

    fn data_prefix_unary(
        &mut self,
        e: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Expr, CompileError> {
        let pu = e.as_prefix_unary_expression();
        if pu.operator == Kind::ExclamationToken {
            return Ok(Expr::not(
                self.data(pu.operand, env, checker, expansion, hooks)?,
            ));
        }
        if pu.operator == Kind::MinusToken {
            if pu.operand.kind() == Kind::NumericLiteral {
                let sf = get_source_file_of_node(pu.operand).unwrap_or(self.source);
                let n = exact_number(sf, pu.operand, self.started, self.limits.max_millis, true)?;
                return Ok(Expr::lit(Value::Number(n)));
            }
            let rhs = self.data(pu.operand, env, checker, expansion, hooks)?;
            return Ok(Expr::binary(BinaryOp::Sub, Expr::lit(Value::from(0)), rhs));
        }
        Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "expression has no first-order lowering",
            e,
        ))
    }

    fn data_binary(
        &mut self,
        e: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        mut hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Expr, CompileError> {
        let be = e.as_binary_expression();
        let op_kind = be.operator_token().kind();
        let left = self.data(be.left(), env, checker, expansion, hooks.as_deref_mut())?;
        let right = self.data(be.right(), env, checker, expansion, hooks)?;
        if op_kind == Kind::EqualsEqualsToken || op_kind == Kind::EqualsEqualsEqualsToken {
            return Ok(Expr::compare(CompareOp::Eq, left, right));
        }
        if op_kind == Kind::ExclamationEqualsToken || op_kind == Kind::ExclamationEqualsEqualsToken
        {
            return Ok(Expr::compare(CompareOp::Ne, left, right));
        }
        if op_kind == Kind::LessThanToken {
            return Ok(Expr::compare(CompareOp::Lt, left, right));
        }
        if op_kind == Kind::LessThanEqualsToken {
            return Ok(Expr::compare(CompareOp::Le, left, right));
        }
        if op_kind == Kind::GreaterThanToken {
            return Ok(Expr::compare(CompareOp::Gt, left, right));
        }
        if op_kind == Kind::GreaterThanEqualsToken {
            return Ok(Expr::compare(CompareOp::Ge, left, right));
        }
        if op_kind == Kind::PlusToken {
            return Ok(Expr::binary(BinaryOp::Add, left, right));
        }
        if op_kind == Kind::MinusToken {
            return Ok(Expr::binary(BinaryOp::Sub, left, right));
        }
        if op_kind == Kind::AsteriskToken {
            return Ok(Expr::binary(BinaryOp::Mul, left, right));
        }
        if op_kind == Kind::SlashToken {
            return Ok(Expr::binary(BinaryOp::Div, left, right));
        }
        if op_kind == Kind::PercentToken {
            return Ok(Expr::binary(BinaryOp::Rem, left, right));
        }
        if op_kind == Kind::QuestionQuestionToken {
            return Ok(Expr::coalesce(left, right));
        }
        if op_kind == Kind::AmpersandAmpersandToken {
            return Ok(Expr::and(left, right));
        }
        if op_kind == Kind::BarBarToken {
            return Ok(Expr::or(left, right));
        }
        Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "expression has no first-order lowering",
            e,
        ))
    }

    pub(super) fn lower_args(
        &mut self,
        args: &[P<Node>],
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        mut hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Vec<Expr>, CompileError> {
        let mut out = Vec::with_capacity(args.len());
        for &a in args {
            out.push(self.data(a, env, checker, expansion, hooks.as_deref_mut())?);
        }
        Ok(out)
    }
}
