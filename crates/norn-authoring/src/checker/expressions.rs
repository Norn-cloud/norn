use std::collections::HashSet;

use tsrs_ast::{Kind, Node, NodeFlags, is_assignment_operator, is_const_type_reference};
use tsrs_checker::TypeFlags;
use tsrs_core::P;

use crate::SymbolUseKind;

use super::resolve::{ResolvedSymbol, every_type, node_text, parse_js_number};
use super::{Ctx, FileWalk};

impl FileWalk<'_> {
    pub(super) fn expr(&mut self, expr: P<Node>, mut ctx: Ctx) {
        let kind = expr.kind();
        // Only calls (and transparent wrappers) can occupy an effect position;
        // the operands of every other expression are data.
        if ctx.effect()
            && kind != Kind::CallExpression
            && kind != Kind::ParenthesizedExpression
            && kind != Kind::SatisfiesExpression
            && kind != Kind::AsExpression
            && kind != Kind::YieldExpression
        {
            ctx = ctx.with_effect(false);
        }

        if self.expr_primary(expr, kind, ctx) || self.expr_compound(expr, kind, ctx) {
            return;
        }
        self.report(
            expr,
            "NORN-FLOW-UNSUPPORTED-SYNTAX",
            &format!("{kind:?} is outside the expression grammar"),
        );
    }

    fn expr_primary(&mut self, expr: P<Node>, kind: Kind, ctx: Ctx) -> bool {
        if kind == Kind::NumericLiteral {
            if !parse_js_number(expr.text()).is_finite() {
                self.report(
                    expr,
                    "NORN-FLOW-EXPRESSION",
                    "non-finite numeric literal (JSON would serialize it as null)",
                );
            }
            return true;
        }
        if kind == Kind::StringLiteral
            || kind == Kind::NoSubstitutionTemplateLiteral
            || kind == Kind::TrueKeyword
            || kind == Kind::FalseKeyword
            || kind == Kind::NullKeyword
        {
            return true;
        }
        if kind == Kind::Identifier {
            self.identifier(expr, ctx);
            return true;
        }
        if kind == Kind::PropertyAccessExpression {
            self.property_access(expr, ctx);
            return true;
        }
        if kind == Kind::ElementAccessExpression {
            let ea = expr.as_element_access_expression();
            let arg_kind = ea.argument_expression().kind();
            if ea.question_dot_token().is_some()
                || (arg_kind != Kind::StringLiteral && arg_kind != Kind::NumericLiteral)
            {
                self.report(
                    expr,
                    "NORN-FLOW-EXPRESSION",
                    "dynamic indexing is not admitted; use a declared property path",
                );
            } else {
                self.expr(ea.expression(), ctx);
            }
            return true;
        }
        if kind == Kind::CallExpression {
            self.call(expr, ctx);
            return true;
        }
        if kind == Kind::NewExpression {
            self.new_expression(expr, ctx, false);
            return true;
        }
        if kind == Kind::YieldExpression {
            let yield_expr = expr.as_yield_expression();
            if yield_expr.asterisk_token.is_none() || !ctx.generator() {
                self.report(
                    expr,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "only yield* of an admitted effect inside Effect.gen",
                );
                return true;
            }
            let Some(operand) = yield_expr.expression else {
                self.report(
                    expr,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "only yield* of an admitted effect inside Effect.gen",
                );
                return true;
            };
            self.expr(operand, ctx.with_effect(true));
            return true;
        }
        false
    }

    fn expr_compound(&mut self, expr: P<Node>, kind: Kind, ctx: Ctx) -> bool {
        if kind == Kind::ParenthesizedExpression {
            self.expr(expr.as_parenthesized_expression().expression(), ctx);
            return true;
        }
        if kind == Kind::SatisfiesExpression {
            self.expr(expr.as_satisfies_expression().expression, ctx);
            return true;
        }
        if kind == Kind::AsExpression {
            let as_expr = expr.as_as_expression();
            if is_const_type_reference(as_expr.type_) {
                self.expr(as_expr.expression, ctx);
            } else {
                self.report(
                    expr,
                    "NORN-FLOW-EXPRESSION",
                    "a type assertion is not evidence about the runtime value",
                );
            }
            return true;
        }
        if kind == Kind::TypeAssertionExpression || kind == Kind::NonNullExpression {
            self.report(
                expr,
                "NORN-FLOW-EXPRESSION",
                "a type assertion is not evidence about the runtime value",
            );
            return true;
        }
        if kind == Kind::ArrayLiteralExpression {
            for &el in expr.as_array_literal_expression().elements.nodes() {
                if el.kind() == Kind::SpreadElement {
                    self.report(el, "NORN-FLOW-EXPRESSION", "spread is not admitted");
                } else {
                    self.expr(el, ctx);
                }
            }
            return true;
        }
        if kind == Kind::ObjectLiteralExpression {
            self.object_literal(expr, ctx);
            return true;
        }
        if kind == Kind::PrefixUnaryExpression {
            self.prefix_unary(expr, ctx);
            return true;
        }
        if kind == Kind::PostfixUnaryExpression {
            self.report(expr, "NORN-FLOW-CAPTURE", "mutation is not admitted");
            return true;
        }
        if kind == Kind::BinaryExpression {
            self.binary(expr, ctx);
            return true;
        }
        self.expr_misc(expr, kind, ctx)
    }

    fn expr_misc(&mut self, expr: P<Node>, kind: Kind, ctx: Ctx) -> bool {
        if kind == Kind::TemplateExpression {
            self.report(
                expr,
                "NORN-FLOW-EXPRESSION",
                "runtime template interpolation is not admitted in v1",
            );
            return true;
        }
        if kind == Kind::ConditionalExpression {
            let cond = expr.as_conditional_expression();
            self.expr(cond.condition, ctx);
            if !self.is_boolean(cond.condition) {
                self.report(
                    cond.condition,
                    "NORN-FLOW-EXPRESSION",
                    "conditional needs a boolean; compare the value explicitly",
                );
            }
            self.expr(cond.when_true, ctx);
            self.expr(cond.when_false, ctx);
            return true;
        }
        if kind == Kind::TypeOfExpression
            || kind == Kind::VoidExpression
            || kind == Kind::DeleteExpression
        {
            self.report(
                expr,
                "NORN-FLOW-EXPRESSION",
                "reflection/coercion operators are not admitted",
            );
            return true;
        }
        if kind == Kind::AwaitExpression {
            self.report(
                expr,
                "NORN-FLOW-UNSUPPORTED-CALL",
                "await/Promises are not admitted; use Norn.perform",
            );
            return true;
        }
        if kind == Kind::TaggedTemplateExpression {
            self.report(
                expr,
                "NORN-FLOW-UNSUPPORTED-CALL",
                "tagged templates are calls the compiler cannot see",
            );
            return true;
        }
        if kind == Kind::ArrowFunction || kind == Kind::FunctionExpression {
            self.report(
                expr,
                "NORN-FLOW-CAPTURE",
                "arbitrary closures are not admitted; pass callbacks inline to an admitted combinator",
            );
            return true;
        }
        false
    }

    fn admitted_value(&mut self, node: P<Node>, resolved: &ResolvedSymbol, ctx: Ctx) {
        let Some(id) = resolved.id.as_deref() else {
            return;
        };
        let Some(entry) = self.owner.admitted_entry(id).cloned() else {
            let code = self.owner.code_for(id).to_owned();
            self.report(node, &code, &format!("{id} is not admitted by the profile"));
            return;
        };
        if entry.use_kind != SymbolUseKind::Value {
            self.report(
                node,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                &format!("{id} must be called inline, not referenced as a value"),
            );
            return;
        }
        if self.context_ok(node, &entry, ctx) {
            self.record_use(node, &entry);
        }
    }

    fn identifier(&mut self, ident: P<Node>, ctx: Ctx) {
        let text = ident.text();
        if text == "undefined" {
            self.report(
                ident,
                "NORN-FLOW-EXPRESSION",
                "undefined is not a JSON value; use null",
            );
            return;
        }
        let resolved = self.resolve(ident);
        if resolved.target.is_none() {
            return;
        }
        if resolved.id.is_some() {
            if resolved.namespace {
                self.report(
                    ident,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "a profile namespace may only qualify an admitted member",
                );
                return;
            }
            self.admitted_value(ident, &resolved, ctx);
            return;
        }
        let decl = resolved.declaration;
        if self.is_default_lib(decl) {
            if text == "NaN" || text == "Infinity" {
                self.report(
                    ident,
                    "NORN-FLOW-EXPRESSION",
                    &format!("{text} is not a JSON value"),
                );
                return;
            }
            self.report(
                ident,
                "NORN-FLOW-AMBIENT",
                &format!("{text} is a host global"),
            );
            return;
        }
        if let Some(decl_node) = decl {
            if decl_node.kind() == Kind::VariableDeclaration
                && decl_node
                    .parent()
                    .is_some_and(|parent| !parent.flags().intersects(NodeFlags::Const))
            {
                let where_decl = Self::where_declared(Some(decl_node));
                self.report(
                    ident,
                    "NORN-FLOW-CAPTURE",
                    &format!("captures mutable binding declared at {where_decl}"),
                );
                return;
            }
            if decl_node.kind() == Kind::FunctionDeclaration {
                let where_decl = Self::where_declared(Some(decl_node));
                self.report(
                    ident,
                    "NORN-FLOW-CAPTURE",
                    &format!("captures helper function declared at {where_decl}"),
                );
                return;
            }
        }
        if ctx.flow() {
            self.flow_data_ref(ident, decl);
        }
    }

    fn flow_data_ref(&mut self, node: P<Node>, decl: Option<P<Node>>) {
        let Some(decl) = decl else {
            return;
        };
        if decl.kind() == Kind::ClassDeclaration {
            let where_decl = Self::where_declared(Some(decl));
            let text = node_text(self.sf, node);
            self.report(
                node,
                "NORN-FLOW-VALUE",
                &format!("{text} is a class declared at {where_decl}, not flow data"),
            );
            return;
        }
        if is_top_level_const(decl) {
            self.json_data_depth_exceeded = false;
            let mut seen = HashSet::new();
            if !self.is_json_data_decl(decl, &mut seen) {
                let (code, detail) = if self.json_data_depth_exceeded {
                    (
                        "NORN-FLOW-UNBOUNDED",
                        "data provenance exceeds 128 referenced declarations".to_owned(),
                    )
                } else {
                    let where_decl = Self::where_declared(Some(decl));
                    let text = node_text(self.sf, node);
                    (
                        "NORN-FLOW-VALUE",
                        format!(
                            "{text} is bound at {where_decl} to a definition value, not a JSON data const"
                        ),
                    )
                };
                self.report(node, code, &detail);
            }
        }
    }

    fn is_json_data_decl(&mut self, decl: P<Node>, seen: &mut HashSet<P<Node>>) -> bool {
        if seen.contains(&decl) {
            return false;
        }
        if seen.len() >= 128 {
            self.json_data_depth_exceeded = true;
            return false;
        }
        if let Some(&known) = self.json_data.get(&decl) {
            if self.bounded_json_data.contains(&decl) {
                self.json_data_depth_exceeded = true;
            }
            return known;
        }
        seen.insert(decl);
        let result = decl
            .as_variable_declaration()
            .initializer()
            .is_some_and(|init| self.is_json_data(init, seen));
        seen.remove(&decl);
        self.json_data.insert(decl, result);
        if !result && self.json_data_depth_exceeded {
            self.bounded_json_data.insert(decl);
        }
        result
    }

    fn is_json_data(&mut self, expr: P<Node>, seen: &mut HashSet<P<Node>>) -> bool {
        let kind = expr.kind();
        if kind == Kind::ParenthesizedExpression {
            return self.is_json_data(expr.as_parenthesized_expression().expression(), seen);
        }
        if kind == Kind::SatisfiesExpression {
            return self.is_json_data(expr.as_satisfies_expression().expression, seen);
        }
        if kind == Kind::AsExpression {
            let as_expr = expr.as_as_expression();
            return is_const_type_reference(as_expr.type_)
                && self.is_json_data(as_expr.expression, seen);
        }
        if kind == Kind::NumericLiteral {
            return parse_js_number(expr.text()).is_finite();
        }
        if kind == Kind::PrefixUnaryExpression {
            let unary = expr.as_prefix_unary_expression();
            return unary.operator == Kind::MinusToken
                && unary.operand.kind() == Kind::NumericLiteral
                && self.is_json_data(unary.operand, seen);
        }
        if kind == Kind::StringLiteral
            || kind == Kind::NoSubstitutionTemplateLiteral
            || kind == Kind::TrueKeyword
            || kind == Kind::FalseKeyword
            || kind == Kind::NullKeyword
        {
            return true;
        }
        if kind == Kind::ArrayLiteralExpression {
            return expr
                .as_array_literal_expression()
                .elements
                .nodes()
                .iter()
                .all(|&el| el.kind() != Kind::SpreadElement && self.is_json_data(el, seen));
        }
        if kind == Kind::ObjectLiteralExpression {
            return expr
                .as_object_literal_expression()
                .properties
                .nodes()
                .iter()
                .all(|&prop| {
                    if prop.kind() == Kind::PropertyAssignment {
                        let pa = prop.as_property_assignment();
                        pa.name().kind() != Kind::ComputedPropertyName
                            && self.is_json_data(pa.initializer(), seen)
                    } else if prop.kind() == Kind::ShorthandPropertyAssignment {
                        let decl = self
                            .resolve(prop.as_shorthand_property_assignment().name())
                            .declaration;
                        self.is_json_data_ref(decl, seen)
                    } else {
                        false
                    }
                });
        }
        if kind == Kind::Identifier {
            let decl = self.resolve(expr).declaration;
            return self.is_json_data_ref(decl, seen);
        }
        if kind == Kind::PropertyAccessExpression {
            let pa = expr.as_property_access_expression();
            let decl = self.resolve(pa.name()).declaration;
            if let Some(decl_node) = decl
                && is_top_level_const(decl_node)
            {
                return self.is_json_data_decl(decl_node, seen);
            }
            return pa.question_dot_token().is_none() && self.is_json_data(pa.expression(), seen);
        }
        false
    }

    fn is_json_data_ref(&mut self, decl: Option<P<Node>>, seen: &mut HashSet<P<Node>>) -> bool {
        decl.is_some_and(|node| is_top_level_const(node) && self.is_json_data_decl(node, seen))
    }

    fn property_access(&mut self, expr: P<Node>, ctx: Ctx) {
        let pa = expr.as_property_access_expression();
        if pa.question_dot_token().is_some() {
            self.report(
                expr,
                "NORN-FLOW-EXPRESSION",
                "optional chaining is not admitted; null policy must be explicit",
            );
            return;
        }
        let resolved = self.resolve(pa.name());
        if resolved.id.is_some() {
            self.admitted_value(expr, &resolved, ctx);
            return;
        }
        let text = node_text(self.sf, expr);
        if self.owner.spelled_like_admitted(&text, false) {
            let where_decl = Self::where_declared(resolved.declaration);
            self.report(
                expr,
                "NORN-FLOW-SYMBOL",
                &format!("`{text}` resolves to {where_decl}, not the admitted profile symbol"),
            );
            return;
        }
        let prop_name = pa.name().text();
        if prop_name == "length" {
            let recv_ty = self.type_of(pa.expression());
            if every_type(recv_ty, |t| t.flags().intersects(TypeFlags::StringLike)) {
                self.report(
                    expr,
                    "NORN-FLOW-EXPRESSION",
                    "string.length counts UTF-16 units; use Norn.byteLength(text) for UTF-8 bytes",
                );
                return;
            }
        }
        let before = self.diagnostics.len();
        self.expr(pa.expression(), ctx);
        if self.diagnostics.len() == before
            && self.is_default_lib(resolved.declaration)
            && prop_name != "length"
        {
            self.report(
                expr,
                "NORN-FLOW-EXPRESSION",
                &format!("`.{prop_name}` is a host property, not a declared data path"),
            );
        }
        // `ns.name` through a namespace import of a local module.
        if self.diagnostics.len() == before
            && ctx.flow()
            && let Some(decl) = resolved.declaration
            && (decl.kind() == Kind::ClassDeclaration || is_top_level_const(decl))
        {
            self.flow_data_ref(expr, Some(decl));
        }
    }

    fn object_literal(&mut self, expr: P<Node>, ctx: Ctx) {
        for &prop in expr.as_object_literal_expression().properties.nodes() {
            let kind = prop.kind();
            if kind == Kind::PropertyAssignment {
                let pa = prop.as_property_assignment();
                if pa.name().kind() == Kind::ComputedPropertyName {
                    self.report(
                        pa.name(),
                        "NORN-FLOW-EXPRESSION",
                        "computed keys are not admitted",
                    );
                } else {
                    self.expr(pa.initializer(), ctx);
                }
            } else if kind == Kind::ShorthandPropertyAssignment {
                self.identifier(prop.as_shorthand_property_assignment().name(), ctx);
            } else if kind == Kind::SpreadAssignment {
                self.report(prop, "NORN-FLOW-EXPRESSION", "spread is not admitted");
            } else {
                self.report(
                    prop,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "methods and accessors are not admitted in data",
                );
            }
        }
    }

    fn prefix_unary(&mut self, expr: P<Node>, ctx: Ctx) {
        let unary = expr.as_prefix_unary_expression();
        let op = unary.operator;
        if op == Kind::ExclamationToken {
            let before = self.diagnostics.len();
            self.expr(unary.operand, ctx);
            if self.diagnostics.len() == before && !self.is_boolean(unary.operand) {
                self.report(
                    expr,
                    "NORN-FLOW-EXPRESSION",
                    "! applies only to booleans (no truthiness)",
                );
            }
            return;
        }
        if op == Kind::MinusToken {
            if unary.operand.kind() == Kind::NumericLiteral {
                self.expr(unary.operand, ctx);
                return;
            }
            self.expr(unary.operand, ctx);
            if !self.is_number(unary.operand) {
                self.report(expr, "NORN-FLOW-EXPRESSION", "unary - expects a number");
            }
            return;
        }
        if op == Kind::PlusPlusToken || op == Kind::MinusMinusToken {
            self.report(expr, "NORN-FLOW-CAPTURE", "mutation is not admitted");
            return;
        }
        self.report(
            expr,
            "NORN-FLOW-EXPRESSION",
            "coercion/arithmetic operators are not admitted",
        );
    }

    fn binary(&mut self, expr: P<Node>, ctx: Ctx) {
        let bin = expr.as_binary_expression();
        let op = bin.operator_token().kind();
        if is_assignment_operator(op) {
            self.report(
                expr,
                "NORN-FLOW-CAPTURE",
                "assignment/mutation is not admitted",
            );
            return;
        }
        let left = bin.left();
        let right = bin.right();
        if self.binary_comparison_or_logical(expr, op, left, right, ctx) {
            return;
        }
        if op == Kind::QuestionQuestionToken {
            let _ = self.walk_binary_operands(left, right, ctx);
            return;
        }
        if op == Kind::PlusToken
            || op == Kind::MinusToken
            || op == Kind::AsteriskToken
            || op == Kind::SlashToken
            || op == Kind::PercentToken
        {
            if self.walk_binary_operands(left, right, ctx)
                && !(self.is_number(left) && self.is_number(right))
                && !(op == Kind::PlusToken && self.is_string(left) && self.is_string(right))
            {
                self.report(
                    expr,
                    "NORN-FLOW-EXPRESSION",
                    "expected two numbers or two strings for +; implicit coercion is not in the Norn profile",
                );
            }
            return;
        }
        if op == Kind::CommaToken {
            self.report(
                expr,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "comma expressions are not admitted",
            );
            return;
        }
        let op_text = node_text(self.sf, bin.operator_token());
        self.report(
            expr,
            "NORN-FLOW-EXPRESSION",
            &format!("operator {op_text} (arithmetic/coercion) is not admitted in v1"),
        );
    }

    fn binary_comparison_or_logical(
        &mut self,
        expr: P<Node>,
        op: Kind,
        left: P<Node>,
        right: P<Node>,
        ctx: Ctx,
    ) -> bool {
        if op == Kind::EqualsEqualsEqualsToken || op == Kind::ExclamationEqualsEqualsToken {
            if self.walk_binary_operands(left, right, ctx)
                && !(self.is_scalar(left) && self.is_scalar(right))
                && !self.is_nullable_null_check(left, right)
            {
                self.report(
                    expr,
                    "NORN-FLOW-EXPRESSION",
                    "===/!== compares scalars only (no object equality)",
                );
            }
            return true;
        }
        if op == Kind::AmpersandAmpersandToken || op == Kind::BarBarToken {
            if self.walk_binary_operands(left, right, ctx)
                && !(self.is_boolean(left) && self.is_boolean(right))
            {
                self.report(
                    expr,
                    "NORN-FLOW-EXPRESSION",
                    "&&/|| take strict booleans (no truthiness)",
                );
            }
            return true;
        }
        if op == Kind::LessThanToken
            || op == Kind::LessThanEqualsToken
            || op == Kind::GreaterThanToken
            || op == Kind::GreaterThanEqualsToken
        {
            if self.walk_binary_operands(left, right, ctx)
                && !(self.is_number(left) && self.is_number(right))
                && !self.same_parsed_temporal(left, right)
                && !(ctx.law_subject() && self.is_string(left) && self.is_string(right))
            {
                self.report(
                    expr,
                    "NORN-FLOW-EXPRESSION",
                    "ordering comparisons are qualified for numbers and for two parsed dates or timestamps (Norn.parseDate, Norn.addDays, Norn.parseTimestamp, Norn.addSeconds, Norn.resolveLocalTimestamp)",
                );
            }
            return true;
        }
        if op == Kind::EqualsEqualsToken || op == Kind::ExclamationEqualsToken {
            self.report(
                expr,
                "NORN-FLOW-EXPRESSION",
                "loose equality coerces; use ===/!==",
            );
            return true;
        }
        false
    }

    fn walk_binary_operands(&mut self, left: P<Node>, right: P<Node>, ctx: Ctx) -> bool {
        let before = self.diagnostics.len();
        self.expr(left, ctx);
        self.expr(right, ctx);
        self.diagnostics.len() == before
    }

    pub(super) fn new_expression(&mut self, expr: P<Node>, ctx: Ctx, fail_argument: bool) {
        let new_expr = expr.as_new_expression();
        let resolved = self.resolve(new_expr.expression);
        if let Some(decl) = resolved.declaration
            && decl.kind() == Kind::ClassDeclaration
            && self.is_tagged_error_class(decl)
        {
            if !fail_argument {
                self.report(
                    expr,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "tagged errors are constructed only as the argument of Effect.fail",
                );
                return;
            }
            if let Some(args) = new_expr.arguments {
                for &arg in args.nodes() {
                    self.expr(arg, ctx.with_effect(false));
                }
            }
            return;
        }
        if self.is_ambient(new_expr.expression) {
            self.report(
                expr,
                "NORN-FLOW-AMBIENT",
                "host clock/IO constructors are not admitted",
            );
            return;
        }
        self.report(
            expr,
            "NORN-FLOW-UNSUPPORTED-CALL",
            "arbitrary class instances are not admitted; construct Data.TaggedError declarations only",
        );
    }

    fn is_tagged_error_class(&mut self, decl: P<Node>) -> bool {
        let Some(clauses) = decl.as_class_declaration().heritage_clauses() else {
            return false;
        };
        let Some(&first_clause) = clauses.nodes().first() else {
            return false;
        };
        let Some(&first_type) = first_clause.as_heritage_clause().types().nodes().first() else {
            return false;
        };
        let expr = first_type.as_expression_with_type_arguments().expression;
        expr.kind() == Kind::CallExpression
            && self.callee_id(expr).as_deref() == Some("effect/Data#TaggedError")
    }
}

pub(super) fn is_top_level_const(decl: P<Node>) -> bool {
    if decl.kind() != Kind::VariableDeclaration {
        return false;
    }
    let Some(list) = decl.parent() else {
        return false;
    };
    if !list.flags().intersects(NodeFlags::Const) {
        return false;
    }
    let Some(stmt) = list.parent() else {
        return false;
    };
    if stmt.kind() != Kind::VariableStatement {
        return false;
    }
    stmt.parent()
        .is_some_and(|parent| parent.kind() == Kind::SourceFile)
}
