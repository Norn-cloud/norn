use std::collections::HashSet;

use tsrs_ast::{Kind, Node, NodeFlags, is_iteration_statement};
use tsrs_checker::{Type, TypeFlags};
use tsrs_core::P;

use crate::{SymbolContext, SymbolUseKind};

use super::calls::unwrap_expr;
use super::cfg::{Terminator, terminator};
use super::resolve::{list_nodes, regular_literal_type};
use super::{Ctx, FileWalk, ReturnMode};

impl FileWalk<'_> {
    pub(super) fn top_level(&mut self, stmt: P<Node>) {
        let kind = stmt.kind();
        if kind == Kind::ImportDeclaration || kind == Kind::ExportDeclaration {
            self.module_import(stmt);
            return;
        }
        if kind == Kind::InterfaceDeclaration || kind == Kind::TypeAliasDeclaration {
            return;
        }
        if kind == Kind::VariableStatement {
            self.variable_statement(stmt, Ctx::TOP);
            return;
        }
        if kind == Kind::ClassDeclaration {
            self.class_declaration(stmt);
            return;
        }
        if kind == Kind::ExportAssignment {
            self.expr(stmt.as_export_assignment().expression(), Ctx::TOP);
            return;
        }
        if kind == Kind::FunctionDeclaration {
            self.report(
                stmt,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "helper function declarations are not visible to the compiler",
            );
            return;
        }
        if kind == Kind::ExpressionStatement {
            let before = self.diagnostics.len();
            self.expr(stmt.as_expression_statement().expression, Ctx::TOP);
            if self.diagnostics.len() == before {
                self.report(
                    stmt,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "top-level expression statements are side effects",
                );
            }
            return;
        }
        if !self.control_statement(stmt) {
            self.report(
                stmt,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                &format!("{kind:?} is outside the definition grammar"),
            );
        }
    }

    /// Statements that carry a specific code wherever they appear.
    pub(super) fn control_statement(&mut self, stmt: P<Node>) -> bool {
        let kind = stmt.kind();
        if kind == Kind::TryStatement {
            self.report(
                stmt,
                "NORN-FLOW-TYPED-FAILURE-CATCH",
                "try/catch/finally cannot observe typed Effect failures",
            );
            return true;
        }
        if kind == Kind::ThrowStatement {
            self.report(
                stmt,
                "NORN-FLOW-TYPED-FAILURE-CATCH",
                "throw is not a typed failure",
            );
            return true;
        }
        if is_iteration_statement(stmt, false) {
            self.report(
                stmt,
                "NORN-FLOW-LOOP",
                "general loops are not admitted in v1",
            );
            return true;
        }
        if kind == Kind::ContinueStatement {
            self.report(stmt, "NORN-FLOW-LOOP", "loop control is not admitted");
            return true;
        }
        false
    }

    pub(super) fn variable_statement(&mut self, stmt: P<Node>, ctx: Ctx) {
        let list = stmt.as_variable_statement().declaration_list;
        if !list.flags().intersects(NodeFlags::Const) {
            self.report(
                list,
                "NORN-FLOW-CAPTURE",
                "only const bindings are admitted (no let/var)",
            );
            return;
        }
        let decls = list.as_variable_declaration_list().declarations.nodes();
        for &decl in decls {
            let var_decl = decl.as_variable_declaration();
            if var_decl.name().kind() != Kind::Identifier {
                self.report(
                    var_decl.name(),
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "destructuring bindings are not admitted",
                );
                continue;
            }
            let Some(init) = var_decl.initializer() else {
                self.report(
                    decl,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "bindings need an initializer (ambient `declare` values have nothing to extract)",
                );
                continue;
            };
            if ctx.flow() && self.is_effect_value(init) {
                self.report(
                    init,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "effect values may not be stored; bind the result with yield*",
                );
                continue;
            }
            self.expr(init, ctx.with_effect(false));
        }
    }

    /// An un-yielded call producing an effect (a flow-context admitted call or a
    /// `.pipe` / `pipe` chain over one).
    pub(super) fn is_effect_value(&mut self, expr: P<Node>) -> bool {
        let inner = unwrap_expr(expr);
        if inner.kind() != Kind::CallExpression {
            return false;
        }
        let Some(id) = self.flow_root(inner) else {
            return false;
        };
        self.owner
            .admitted_entry(&id)
            .is_some_and(|entry| entry.context == SymbolContext::Flow)
    }

    fn class_declaration(&mut self, stmt: P<Node>) {
        let class_decl = stmt.as_class_declaration();
        let clauses = list_nodes(class_decl.heritage_clauses());
        let first_types: &[P<Node>] = clauses
            .first()
            .map_or(&[], |clause| clause.as_heritage_clause().types().nodes());
        let Some(&heritage) = first_types.first() else {
            self.report(
                stmt,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "classes are admitted only as Data.TaggedError / Schema.Class declarations",
            );
            return;
        };
        if clauses.len() != 1 || first_types.len() != 1 {
            self.report(
                stmt,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "classes are admitted only as Data.TaggedError / Schema.Class declarations",
            );
            return;
        }
        let before = self.diagnostics.len();
        let ok = self.heritage(heritage.as_expression_with_type_arguments().expression);
        if self.diagnostics.len() > before {
            return;
        }
        if !ok {
            self.report(
                heritage,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "class must extend Data.TaggedError(tag) or Schema.Class<Self>(id)(fields)",
            );
            return;
        }
        if let Some(&first_member) = class_decl.members().nodes().first() {
            self.report(
                first_member,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "tagged error / schema classes carry no members, methods, or getters",
            );
        }
    }

    fn heritage(&mut self, expr: P<Node>) -> bool {
        if expr.kind() != Kind::CallExpression {
            self.expr(expr, Ctx::TOP);
            return false;
        }
        let outer_call = expr.as_call_expression();
        let inner = if outer_call.expression().kind() == Kind::CallExpression {
            outer_call.expression()
        } else {
            expr
        };
        let id = self.callee_id(inner);
        let entry = id
            .as_deref()
            .and_then(|sym_id| self.owner.admitted_entry(sym_id))
            .cloned();
        let Some(entry) = entry.filter(|e| e.use_kind == SymbolUseKind::ClassHeritage) else {
            self.expr(expr, Ctx::TOP);
            return false;
        };
        let inner_call = inner.as_call_expression();
        self.record_use(inner_call.expression(), &entry);
        let inner_args = inner_call.arguments().nodes();
        if inner_args.len() != 1
            || inner_args
                .first()
                .is_none_or(|arg| arg.kind() != Kind::StringLiteral)
        {
            self.report(
                inner,
                "NORN-FLOW-LITERAL",
                "the tag / identifier must be one string literal",
            );
        }
        if inner != expr {
            for &arg in outer_call.arguments().nodes() {
                self.expr(arg, Ctx::TOP);
            }
        } else if id.as_deref() == Some("effect/Schema#Class") {
            self.report(
                expr,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "Schema.Class requires its field struct: Schema.Class<Self>(id)(fields)",
            );
        }
        true
    }

    pub(super) fn block(&mut self, statements: &[P<Node>], ctx: Ctx) {
        for &stmt in statements {
            self.statement(stmt, ctx, false);
        }
    }

    pub(super) fn statement(&mut self, stmt: P<Node>, ctx: Ctx, last_in_clause: bool) {
        if self.control_statement(stmt) {
            return;
        }
        let kind = stmt.kind();
        if kind == Kind::VariableStatement {
            self.variable_statement(stmt, ctx);
            return;
        }
        if kind == Kind::Block {
            self.block(stmt.as_block().statements.nodes(), ctx);
            return;
        }
        if kind == Kind::EmptyStatement {
            return;
        }
        if kind == Kind::ReturnStatement {
            self.return_statement(stmt, ctx);
            return;
        }
        if kind == Kind::BreakStatement {
            if !last_in_clause || stmt.as_break_statement().label().is_some() {
                self.report(
                    stmt,
                    "NORN-FLOW-LOOP",
                    "break is admitted only to end a switch clause",
                );
            }
            return;
        }
        if kind == Kind::ExpressionStatement {
            let before = self.diagnostics.len();
            let expr = stmt.as_expression_statement().expression;
            // Walked as an effect position so a discarded effect reports as discarded, not as data.
            self.expr(expr, ctx.with_effect(true));
            let is_yield_star = expr.kind() == Kind::YieldExpression
                && expr.as_yield_expression().asterisk_token.is_some();
            if self.diagnostics.len() == before && !is_yield_star {
                self.report(
                    stmt,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "expression statement discards its value; yield* the effect",
                );
            }
            return;
        }
        if kind == Kind::IfStatement {
            let if_stmt = stmt.as_if_statement();
            self.condition(if_stmt.expression, ctx);
            self.statement(if_stmt.then_statement, ctx, false);
            if let Some(else_stmt) = if_stmt.else_statement {
                self.statement(else_stmt, ctx, false);
            }
            return;
        }
        if kind == Kind::SwitchStatement {
            self.switch_statement(stmt, ctx);
            return;
        }
        self.report(
            stmt,
            "NORN-FLOW-UNSUPPORTED-SYNTAX",
            &format!("{kind:?} is outside the flow grammar"),
        );
    }

    fn return_statement(&mut self, stmt: P<Node>, ctx: Ctx) {
        let Some(ret_expr) = stmt.as_return_statement().expression() else {
            if ctx.ret == ReturnMode::FlowResult || ctx.ret == ReturnMode::MapResult {
                self.report(
                    stmt,
                    "NORN-FLOW-RESULT",
                    "a bare `return` yields undefined; return an explicit JSON result",
                );
            }
            return;
        };
        if ctx.generator() && self.is_effect_value(ret_expr) {
            self.report(
                ret_expr,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "a flow result is a value; run the effect with `return yield* ...`",
            );
            return;
        }
        self.expr(ret_expr, ctx.with_effect(ctx.ret == ReturnMode::Effect));
    }

    fn condition(&mut self, cond: P<Node>, ctx: Ctx) {
        let before = self.diagnostics.len();
        self.expr(cond, ctx.with_effect(false));
        if self.diagnostics.len() == before && !self.is_boolean(cond) {
            let ty = self.type_of(cond);
            let ty_str = self.checker.type_to_string_exported(ty);
            self.report(
                cond,
                "NORN-FLOW-EXPRESSION",
                &format!("condition has type {ty_str}, not boolean (no truthiness)"),
            );
        }
    }

    fn switch_statement(&mut self, stmt: P<Node>, ctx: Ctx) {
        let switch_stmt = stmt.as_switch_statement();
        let disc_type = self.type_of(switch_stmt.expression);
        let members: Vec<P<Type>> = if disc_type.is_union() {
            disc_type.types().to_vec()
        } else {
            vec![disc_type]
        };
        let literal_mask = TypeFlags::Literal | TypeFlags::BooleanLiteral | TypeFlags::EnumLiteral;
        if !members
            .iter()
            .all(|&member| member.flags().intersects(literal_mask))
        {
            let ty_str = self.checker.type_to_string_exported(disc_type);
            self.report(
                switch_stmt.expression,
                "NORN-FLOW-ROUTE",
                &format!("discriminant has type {ty_str}, not a literal union"),
            );
            return;
        }

        self.expr(switch_stmt.expression, ctx.with_effect(false));
        let mut covered: HashSet<P<Type>> = HashSet::new();
        let mut has_default = false;
        let clauses = switch_stmt.case_block.as_case_block().clauses.nodes();
        let clause_len = clauses.len();
        for (idx, &clause) in clauses.iter().enumerate() {
            if clause.kind() == Kind::DefaultClause {
                has_default = true;
            } else if let Some(case_expr) = clause.as_case_or_default_clause().expression() {
                if is_switch_literal_expr(case_expr) {
                    let label = regular_literal_type(self.type_of(case_expr));
                    if !covered.insert(label) {
                        self.report(case_expr, "NORN-FLOW-ROUTE", "duplicate switch label");
                    }
                } else {
                    self.report(case_expr, "NORN-FLOW-ROUTE", "case labels must be literals");
                }
            }
            let stmts = clause.as_case_or_default_clause().statements.nodes();
            let is_last_clause = idx.saturating_add(1) == clause_len;
            if !stmts.is_empty() && terminator(clause, is_last_clause) == Terminator::Fallthrough {
                self.report(
                    clause,
                    "NORN-FLOW-ROUTE",
                    "clause falls through; end it with return or break",
                );
            }
            let stmt_len = stmts.len();
            for (stmt_idx, &inner_stmt) in stmts.iter().enumerate() {
                self.statement(inner_stmt, ctx, stmt_idx.saturating_add(1) == stmt_len);
            }
        }
        if !has_default {
            let missing: Vec<String> = members
                .into_iter()
                .filter(|&member| !covered.contains(&regular_literal_type(member)))
                .map(|member| self.checker.type_to_string_exported(member))
                .collect();
            if !missing.is_empty() {
                self.report(
                    switch_stmt.expression,
                    "NORN-FLOW-ROUTE",
                    &format!("switch is not exhaustive; missing {}", missing.join(", ")),
                );
            }
        }
    }
}

fn is_switch_literal_expr(expr: P<Node>) -> bool {
    let kind = expr.kind();
    kind == Kind::StringLiteral
        || kind == Kind::NumericLiteral
        || kind == Kind::TrueKeyword
        || kind == Kind::FalseKeyword
}
