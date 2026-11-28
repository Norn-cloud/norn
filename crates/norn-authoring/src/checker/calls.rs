use tsrs_ast::{Kind, ModifierFlags, Node, NodeFlags};
use tsrs_checker::TypeFlags;
use tsrs_core::P;

use crate::{AdmittedSymbol, SymbolContext, SymbolUseKind};

use super::cfg::returns;
use super::resolve::{every_type, list_nodes, node_text};
use super::{Ctx, FileWalk};

impl FileWalk<'_> {
    pub(super) fn context_ok(&mut self, node: P<Node>, entry: &AdmittedSymbol, ctx: Ctx) -> bool {
        match entry.context {
            SymbolContext::Flow if !ctx.flow() => {
                self.report(
                    node,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    &format!(
                        "{} is admitted only inline inside a Norn.flow body (no generator escape)",
                        entry.symbol
                    ),
                );
                false
            }
            SymbolContext::Definition if ctx.flow() => {
                self.report(
                    node,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    &format!(
                        "{} is a top-level definition constructor, not a flow step",
                        entry.symbol
                    ),
                );
                false
            }
            SymbolContext::RetryPolicy if !ctx.retry_policy() => {
                self.report(
                    node,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    &format!(
                        "{} is admitted only inline as an Effect.retry policy",
                        entry.symbol
                    ),
                );
                false
            }
            SymbolContext::Flow
            | SymbolContext::Definition
            | SymbolContext::RetryPolicy
            | SymbolContext::Any => true,
        }
    }

    pub(super) fn call(&mut self, call: P<Node>, ctx: Ctx) {
        let call_expr = call.as_call_expression();
        let callee = call_expr.expression();
        if callee.kind() == Kind::ImportKeyword {
            self.report(
                call,
                "NORN-FLOW-UNSUPPORTED-CALL",
                "dynamic import is not admitted",
            );
            return;
        }
        if call_expr.question_dot_token().is_some() {
            self.report(
                call,
                "NORN-FLOW-EXPRESSION",
                "optional calls are not admitted",
            );
            return;
        }
        if callee.kind() == Kind::CallExpression {
            // Curried constructors (`Context.Service<...>()("id")`) report through their inner call;
            // the admitted curried forms appear only in class heritage.
            let before = self.diagnostics.len();
            self.call(callee, ctx);
            if self.diagnostics.len() == before {
                self.report(
                    callee,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "curried calls are admitted only in class heritage",
                );
            }
            return;
        }
        let name = if callee.kind() == Kind::PropertyAccessExpression {
            callee.as_property_access_expression().name()
        } else if callee.kind() == Kind::Identifier {
            callee
        } else {
            self.report(
                callee,
                "NORN-FLOW-UNSUPPORTED-CALL",
                "callee is not a statically resolved symbol",
            );
            return;
        };
        let resolved = self.resolve(name);
        if let Some(id) = resolved.id {
            self.call_profile_symbol(call, callee, name, &id, ctx);
            return;
        }
        if callee.kind() == Kind::PropertyAccessExpression
            && resolved.declaration.is_some()
            && self.is_default_lib(resolved.declaration)
            && self.try_default_lib_method_call(call, callee, ctx)
        {
            return;
        }
        self.report_unknown_call(callee, resolved.declaration);
    }

    fn report_unknown_call(&mut self, callee: P<Node>, declaration: Option<P<Node>>) {
        let text = node_text(self.sf, callee).to_owned();
        if self
            .owner
            .spelled_like_admitted(&text, callee.kind() == Kind::Identifier)
        {
            let where_decl = self.where_declared(declaration);
            self.report(
                callee,
                "NORN-FLOW-SYMBOL",
                &format!("`{text}` resolves to {where_decl}, not the admitted profile symbol"),
            );
            return;
        }
        if self.is_ambient(callee) {
            self.report(
                callee,
                "NORN-FLOW-AMBIENT",
                &format!("`{text}` reads host time/entropy/IO"),
            );
            return;
        }
        self.report(
            callee,
            "NORN-FLOW-UNSUPPORTED-CALL",
            &format!("`{text}` is a call the compiler cannot see"),
        );
    }

    fn call_profile_symbol(
        &mut self,
        call: P<Node>,
        callee: P<Node>,
        name: P<Node>,
        id: &str,
        ctx: Ctx,
    ) {
        let Some(entry) = self.owner.admitted_entry(id).cloned() else {
            let code = self.owner.code_for(id).to_owned();
            self.report(
                callee,
                &code,
                &format!("{id} is not admitted by the profile"),
            );
            return;
        };
        if entry.use_kind == SymbolUseKind::Method {
            if self.effect_as_data(call, ctx) {
                return;
            }
            self.record_use(name, &entry);
            let eff = ctx.with_effect(true);
            if callee.kind() == Kind::PropertyAccessExpression {
                self.expr(callee.as_property_access_expression().expression(), eff);
            }
            for &arg in call.as_call_expression().arguments().nodes() {
                self.expr(arg, eff);
            }
            return;
        }
        if entry.use_kind != SymbolUseKind::Call {
            self.report(
                callee,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                &format!("{id} is admitted only as {:?}", entry.use_kind),
            );
            return;
        }
        if ctx.pure() && entry.context == SymbolContext::Flow {
            self.report(
                callee,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "Effect.map callbacks are pure value expressions; use Effect.flatMap to sequence an effect",
            );
            return;
        }
        if entry.context == SymbolContext::Flow && ctx.flow() && !ctx.effect() {
            self.report(
                call,
                "NORN-FLOW-VALUE",
                "an effect is not a data value; run it with yield* and use the result",
            );
            return;
        }
        if id == "effect/Function#pipe" && self.effect_as_data(call, ctx) {
            return;
        }
        if !self.context_ok(callee, &entry, ctx) {
            return;
        }
        self.record_use(callee, &entry);
        self.admitted_call(id, call, ctx);
    }

    fn try_default_lib_method_call(&mut self, call: P<Node>, callee: P<Node>, ctx: Ctx) -> bool {
        let pa = callee.as_property_access_expression();
        let receiver = self.type_of(pa.expression());
        let method = pa.name().text();
        let is_string = every_type(receiver, |t| t.flags().intersects(TypeFlags::StringLike));
        let is_array = self.is_array_or_tuple(receiver);
        let args = call.as_call_expression().arguments().nodes();
        let arity = usize::from(!matches!(method, "toLowerCase" | "toUpperCase" | "trim"));
        let string_ok = is_string
            && matches!(
                method,
                "toLowerCase" | "toUpperCase" | "trim" | "includes" | "startsWith" | "endsWith"
            );
        let array_ok = is_array
            && (method == "includes"
                || (method == "concat"
                    && args.first().is_some_and(|&arg0| {
                        let arg_ty = self.type_of(arg0);
                        self.checker.is_array_type_exported(arg_ty)
                    })));
        if (string_ok || array_ok) && args.len() == arity {
            self.expr(pa.expression(), ctx);
            for &arg in args {
                self.expr(arg, ctx);
            }
            return true;
        }
        if is_array && matches!(method, "some" | "every" | "filter" | "map") {
            self.array_closure(call, callee, ctx);
            return true;
        }
        false
    }

    fn array_closure(&mut self, call: P<Node>, callee: P<Node>, ctx: Ctx) {
        let pa = callee.as_property_access_expression();
        let method = pa.name().text();
        if !ctx.pure() {
            self.report(
                callee,
                "NORN-FLOW-EXPRESSION",
                &format!(
                    "`.{method}` is a closure; closures are admitted in expression callbacks (Norn.expression, laws, guards, derivations), not in flow data"
                ),
            );
            return;
        }
        self.expr(pa.expression(), ctx);
        let args = call.as_call_expression().arguments().nodes();
        let Some(&fn_node) = args.first().filter(|_| args.len() == 1) else {
            self.report(
                call,
                "NORN-FLOW-EXPRESSION",
                &format!("`.{method}` takes exactly one inline callback"),
            );
            return;
        };
        if fn_node.kind() != Kind::ArrowFunction {
            self.report(
                fn_node,
                "NORN-FLOW-CAPTURE",
                &format!(
                    "pass `.{method}` an inline arrow callback; a function value is opaque to the compiler"
                ),
            );
            return;
        }
        let arrow = fn_node.as_arrow_function();
        if has_async_modifier(arrow.modifiers()) {
            self.report(
                fn_node,
                "NORN-FLOW-UNSUPPORTED-CALL",
                "async callbacks are not admitted",
            );
            return;
        }
        let params = list_nodes(arrow.parameters());
        let valid_single_param = params.len() == 1
            && params.first().is_some_and(|&param| {
                let p = param.as_parameter_declaration();
                p.name().kind() == Kind::Identifier
                    && p.initializer().is_none()
                    && p.dot_dot_dot_token().is_none()
            });
        if !valid_single_param {
            self.report(
                fn_node,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                &format!(
                    "a `.{method}` callback takes one plain parameter (the element); the index and array arguments are not admitted"
                ),
            );
            return;
        }
        let Some(body) = arrow.body() else {
            return;
        };
        if body.kind() == Kind::Block {
            self.report(
                body,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                &format!(
                    "a `.{method}` callback body is one expression; write `x => ...` without braces"
                ),
            );
            return;
        }
        self.expr(body, ctx);
    }

    pub(super) fn args(&mut self, call: P<Node>, ctx: Ctx) {
        for &arg in call.as_call_expression().arguments().nodes() {
            self.expr(arg, ctx);
        }
    }

    fn effect_as_data(&mut self, call: P<Node>, ctx: Ctx) -> bool {
        if !ctx.flow() || ctx.effect() || ctx.pure() || !self.is_effect_value(call) {
            return false;
        }
        self.report(
            call,
            "NORN-FLOW-VALUE",
            "an effect is not a data value; run it with yield* and use the result",
        );
        true
    }

    pub(super) fn require_literal(
        &mut self,
        node: Option<P<Node>>,
        call: P<Node>,
        what: &str,
        numeric: bool,
    ) {
        let ok = node.is_some_and(|expr| {
            let kind = expr.kind();
            kind == Kind::NumericLiteral
                || (!numeric
                    && (kind == Kind::StringLiteral || kind == Kind::NoSubstitutionTemplateLiteral))
        });
        if !ok {
            self.report(
                node.unwrap_or(call),
                "NORN-FLOW-LITERAL",
                &format!("{what} must be a literal"),
            );
        }
    }

    pub(super) fn callback(
        &mut self,
        fn_node: Option<P<Node>>,
        call: P<Node>,
        ctx: Ctx,
        pure: bool,
    ) {
        let Some(fn_node) = fn_node else {
            self.report(
                call,
                "NORN-FLOW-CAPTURE",
                "the continuation must be an inline callback literal (function-valued continuations are opaque)",
            );
            return;
        };
        let Some((is_generator, is_async, params, body)) = extract_callback_parts(fn_node) else {
            self.report(
                fn_node,
                "NORN-FLOW-CAPTURE",
                "the continuation must be an inline callback literal (function-valued continuations are opaque)",
            );
            return;
        };
        if is_generator {
            self.report(
                fn_node,
                "NORN-FLOW-CAPTURE",
                "the continuation must be an inline callback literal (function-valued continuations are opaque)",
            );
            return;
        }
        if is_async {
            self.report(
                fn_node,
                "NORN-FLOW-UNSUPPORTED-CALL",
                "async callbacks are not admitted",
            );
            return;
        }
        for &param in params {
            let p = param.as_parameter_declaration();
            if p.name().kind() != Kind::Identifier
                || p.initializer().is_some()
                || p.dot_dot_dot_token().is_some()
            {
                self.report(
                    param,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "callback parameters must be plain identifiers",
                );
            }
        }
        let Some(body) = body else {
            return;
        };
        let is_pure = ctx.pure() || pure;
        let inner = ctx.for_callback(is_pure);
        if body.kind() != Kind::Block {
            self.expr(body, inner.with_effect(!is_pure));
            return;
        }
        let stmts = body.as_block().statements.nodes();
        self.block(stmts, inner);
        if is_pure && !stmts.iter().any(|&stmt| returns(stmt)) {
            self.report(
                fn_node,
                "NORN-FLOW-RESULT",
                "a block-bodied Effect.map callback must return a value on every path",
            );
        }
    }

    pub(super) fn flow_root(&mut self, call: P<Node>) -> Option<String> {
        let id = self.callee_id(call)?;
        let call_expr = call.as_call_expression();
        if id == "effect/Pipeable#Pipeable.pipe"
            && call_expr.expression().kind() == Kind::PropertyAccessExpression
        {
            let receiver = unwrap_expr(
                call_expr
                    .expression()
                    .as_property_access_expression()
                    .expression(),
            );
            return if receiver.kind() == Kind::CallExpression {
                self.flow_root(receiver)
            } else {
                None
            };
        }
        if id == "effect/Function#pipe" {
            let first = call_expr
                .arguments()
                .nodes()
                .first()
                .map(|&arg| unwrap_expr(arg))?;
            return if first.kind() == Kind::CallExpression {
                self.flow_root(first)
            } else {
                None
            };
        }
        Some(id)
    }

    pub(super) fn finite_policy(&mut self, policy: P<Node>) -> bool {
        if policy.kind() != Kind::CallExpression {
            return false;
        }
        let Some(id) = self.callee_id(policy) else {
            return false;
        };
        if id == "effect/Schedule#recurs" {
            return true;
        }
        if id != "effect/Schedule#max" {
            return false;
        }
        let Some(&list) = policy.as_call_expression().arguments().nodes().first() else {
            return false;
        };
        if list.kind() != Kind::ArrayLiteralExpression {
            return false;
        }
        list.as_array_literal_expression()
            .elements
            .nodes()
            .iter()
            .any(|&el| {
                el.kind() == Kind::CallExpression
                    && self.callee_id(el).as_deref() == Some("effect/Schedule#recurs")
            })
    }

    pub(super) fn is_fn_handle(&mut self, ident: P<Node>) -> bool {
        let Some(decl) = self.resolve(ident).declaration else {
            return false;
        };
        if decl.kind() != Kind::VariableDeclaration {
            return false;
        }
        let Some(parent) = decl.parent() else {
            return false;
        };
        if !parent.flags().intersects(NodeFlags::Const) {
            return false;
        }
        let Some(init) = decl.as_variable_declaration().initializer() else {
            return false;
        };
        init.kind() == Kind::CallExpression
            && self.callee_id(init).as_deref() == Some("@norn/authoring#Norn.fn")
    }
}

pub(super) fn unwrap_expr(mut expr: P<Node>) -> P<Node> {
    loop {
        let kind = expr.kind();
        if kind == Kind::ParenthesizedExpression {
            expr = expr.as_parenthesized_expression().expression();
        } else if kind == Kind::AsExpression {
            expr = expr.as_as_expression().expression;
        } else if kind == Kind::SatisfiesExpression {
            expr = expr.as_satisfies_expression().expression;
        } else if kind == Kind::TypeAssertionExpression {
            expr = expr.as_type_assertion().expression;
        } else if kind == Kind::NonNullExpression {
            expr = expr.as_non_null_expression().expression();
        } else {
            return expr;
        }
    }
}

fn extract_callback_parts(
    fn_node: P<Node>,
) -> Option<(bool, bool, &'static [P<Node>], Option<P<Node>>)> {
    if fn_node.kind() == Kind::ArrowFunction {
        let arrow = fn_node.as_arrow_function();
        return Some((
            arrow.asterisk_token().is_some(),
            has_async_modifier(arrow.modifiers()),
            list_nodes(arrow.parameters()),
            arrow.body(),
        ));
    }
    if fn_node.kind() == Kind::FunctionExpression {
        let func = fn_node.as_function_expression();
        return Some((
            func.asterisk_token().is_some(),
            has_async_modifier(func.modifiers()),
            list_nodes(func.parameters()),
            func.body(),
        ));
    }
    None
}

fn has_async_modifier(modifiers: Option<P<tsrs_ast::ModifierList>>) -> bool {
    modifiers.is_some_and(|list| list.modifier_flags.intersects(ModifierFlags::Async))
}
