use tsrs_ast::{Kind, Node};
use tsrs_core::P;

use super::calls::unwrap_expr;
use super::resolve::is_js_integer_literal;
use super::{Ctx, FileWalk};

impl FileWalk<'_> {
    pub(super) fn admitted_call(&mut self, id: &str, call: P<Node>, call_ctx: Ctx) {
        let ctx = call_ctx.with_effect(false);
        let eff = call_ctx.with_effect(true);
        if self.admitted_norn_call(id, call, ctx, eff)
            || self.admitted_effect_call(id, call, ctx, eff)
            || self.admitted_schedule_or_tool_call(id, call, ctx, eff)
        {
            return;
        }
        self.args(call, ctx);
    }

    fn admitted_norn_call(&mut self, id: &str, call: P<Node>, ctx: Ctx, eff: Ctx) -> bool {
        let args = call.as_call_expression().arguments().nodes();
        match id {
            "@norn/authoring#Norn.evaluate" => {
                if let Some(&arg1) = args.get(1) {
                    self.expr(arg1, ctx);
                }
                true
            }
            "@norn/authoring#Norn.expression" => {
                if let Some(&arg0) = args.first() {
                    self.expr(arg0, ctx);
                }
                self.callback(args.get(1).copied(), call, ctx, true);
                true
            }
            "@norn/authoring#Norn.decision" => {
                self.decision_call(call, args, ctx);
                true
            }
            "@norn/authoring#Norn.flow" => {
                self.flow_call(call, args, ctx, eff);
                true
            }
            "@norn/authoring#Norn.perform" => {
                self.perform_call(call, args, ctx);
                true
            }
            "@norn/authoring#Norn.law"
            | "@norn/authoring#Norn.obligation"
            | "@norn/authoring#Norn.prohibition"
            | "@norn/authoring#Norn.permission" => {
                self.law_call(call, ctx);
                true
            }
            _ => false,
        }
    }

    fn decision_call(&mut self, call: P<Node>, args: &[P<Node>], ctx: Ctx) {
        if let Some(&arg0) = args.first() {
            self.expr(arg0, ctx);
        }
        let Some(&arg1) = args.get(1) else {
            return;
        };
        let rows = unwrap_expr(arg1);
        if rows.kind() != Kind::ArrayLiteralExpression {
            self.callback(Some(arg1), call, ctx, true);
            return;
        }
        for &row in rows.as_array_literal_expression().elements.nodes() {
            let entry = unwrap_expr(row);
            if entry.kind() != Kind::ObjectLiteralExpression {
                self.report(row, "NORN-FLOW-LITERAL", "a table row is { when, then }");
                continue;
            }
            for &prop in entry.as_object_literal_expression().properties.nodes() {
                if prop.kind() == Kind::PropertyAssignment {
                    let pa = prop.as_property_assignment();
                    if pa.name().kind() == Kind::Identifier
                        && matches!(pa.name().text(), "when" | "then")
                    {
                        self.callback(Some(pa.initializer()), call, ctx, true);
                        continue;
                    }
                }
                self.report(
                    prop,
                    "NORN-FLOW-LITERAL",
                    "a table row takes when and then callbacks",
                );
            }
        }
    }

    fn flow_call(&mut self, call: P<Node>, args: &[P<Node>], ctx: Ctx, eff: Ctx) {
        if let Some(&arg0) = args.first() {
            self.expr(arg0, ctx);
        }
        let body = args.get(1).copied();
        let before = self.diagnostics.len();
        if let Some(body_node) = body
            && args.len() == 2
            && body_node.kind() == Kind::CallExpression
        {
            self.expr(body_node, eff.with_flow(true));
        }
        if self.diagnostics.len() > before {
            return;
        }
        let ok = body.is_some_and(|body_node| {
            args.len() == 2
                && body_node.kind() == Kind::CallExpression
                && self.flow_root(body_node).as_deref() == Some("effect/Effect#gen")
        });
        if !ok {
            self.report(
                body.unwrap_or(call),
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "the flow body must be an inline Effect.gen(function* () { ... }) (optionally .pipe'd)",
            );
        }
    }

    fn perform_call(&mut self, call: P<Node>, args: &[P<Node>], ctx: Ctx) {
        let step = args.first().copied();
        if step.is_none_or(|s| s.kind() != Kind::StringLiteral) {
            self.report(
                step.unwrap_or(call),
                "NORN-FLOW-LITERAL",
                "the step id must be a string literal",
            );
        }
        let handle = args.get(1).copied();
        let valid_handle =
            handle.is_some_and(|h| h.kind() == Kind::Identifier && self.is_fn_handle(h));
        if !valid_handle {
            self.report(
                handle.unwrap_or(call),
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "the Function must be a top-level const declared with Norn.fn",
            );
        }
        if let Some(&arg2) = args.get(2) {
            self.expr(arg2, ctx);
        }
    }

    fn law_call(&mut self, call: P<Node>, ctx: Ctx) {
        let args = call.as_call_expression().arguments().nodes();
        let Some(&arg0) = args.first() else {
            self.args(call, ctx);
            return;
        };
        let Some(&arg1) = args.get(1) else {
            self.args(call, ctx);
            return;
        };
        let spec = unwrap_expr(arg1);
        if spec.kind() != Kind::ObjectLiteralExpression {
            self.args(call, ctx);
            return;
        }
        self.expr(arg0, ctx);
        for &prop in spec.as_object_literal_expression().properties.nodes() {
            self.law_spec_property(prop, call, ctx);
        }
    }

    fn law_spec_property(&mut self, prop: P<Node>, call: P<Node>, ctx: Ctx) {
        if prop.kind() != Kind::PropertyAssignment {
            self.report(
                prop,
                "NORN-FLOW-LITERAL",
                "a law spec takes plain name: value members",
            );
            return;
        }
        let pa = prop.as_property_assignment();
        let key_kind = pa.name().kind();
        if key_kind != Kind::Identifier && key_kind != Kind::StringLiteral {
            self.report(
                prop,
                "NORN-FLOW-LITERAL",
                "a law spec takes plain name: value members",
            );
            return;
        }
        let key = pa.name().text();
        let init = pa.initializer();
        let value = unwrap_expr(init);
        if matches!(key, "when" | "must" | "never" | "eventually" | "may") {
            self.callback(Some(init), call, ctx.with_law_subject(true), true);
        } else if key == "unless" {
            self.law_exceptions(init, call, ctx);
        } else if key == "within" && value.kind() == Kind::ObjectLiteralExpression {
            for &within_prop in value.as_object_literal_expression().properties.nodes() {
                if within_prop.kind() == Kind::PropertyAssignment {
                    let wpa = within_prop.as_property_assignment();
                    if wpa.name().kind() == Kind::Identifier && wpa.name().text() == "before" {
                        self.callback(Some(wpa.initializer()), call, ctx, true);
                    } else {
                        self.expr(wpa.initializer(), ctx);
                    }
                } else {
                    self.report(
                        within_prop,
                        "NORN-FLOW-LITERAL",
                        "within takes days, hours or before",
                    );
                }
            }
        } else {
            self.expr(init, ctx);
        }
    }

    fn law_exceptions(&mut self, node: P<Node>, call: P<Node>, ctx: Ctx) {
        let tree = unwrap_expr(node);
        if tree.kind() != Kind::ObjectLiteralExpression {
            self.report(
                node,
                "NORN-FLOW-LITERAL",
                "unless is an object literal of labelled exceptions",
            );
            return;
        }
        for &entry in tree.as_object_literal_expression().properties.nodes() {
            if entry.kind() != Kind::PropertyAssignment {
                self.report(
                    entry,
                    "NORN-FLOW-LITERAL",
                    "an exception is label: { when, because, unless? }",
                );
                continue;
            }
            let body = unwrap_expr(entry.as_property_assignment().initializer());
            if body.kind() != Kind::ObjectLiteralExpression {
                self.report(
                    entry,
                    "NORN-FLOW-LITERAL",
                    "an exception is label: { when, because, unless? }",
                );
                continue;
            }
            for &prop in body.as_object_literal_expression().properties.nodes() {
                if prop.kind() != Kind::PropertyAssignment {
                    self.report(
                        prop,
                        "NORN-FLOW-LITERAL",
                        "an exception takes plain name: value members",
                    );
                    continue;
                }
                let pa = prop.as_property_assignment();
                let key_kind = pa.name().kind();
                if key_kind != Kind::Identifier && key_kind != Kind::StringLiteral {
                    self.report(
                        prop,
                        "NORN-FLOW-LITERAL",
                        "an exception takes plain name: value members",
                    );
                    continue;
                }
                let key = pa.name().text();
                if key == "when" {
                    self.callback(
                        Some(pa.initializer()),
                        call,
                        ctx.with_law_subject(true),
                        true,
                    );
                } else if key == "unless" {
                    self.law_exceptions(pa.initializer(), call, ctx);
                } else {
                    self.expr(pa.initializer(), ctx);
                }
            }
        }
    }

    fn admitted_effect_call(&mut self, id: &str, call: P<Node>, ctx: Ctx, eff: Ctx) -> bool {
        let args = call.as_call_expression().arguments().nodes();
        match id {
            "effect/Effect#gen" => {
                self.effect_gen_call(call, args, ctx);
                true
            }
            "effect/Effect#map" | "effect/Effect#flatMap" => {
                if args.len() == 2
                    && let Some(&arg0) = args.first()
                {
                    self.expr(arg0, eff);
                }
                self.callback(args.last().copied(), call, ctx, id == "effect/Effect#map");
                true
            }
            "effect/Effect#catchTag" => {
                let tag_index = usize::from(args.len() == 3);
                if args.len() == 3
                    && let Some(&arg0) = args.first()
                {
                    self.expr(arg0, eff);
                }
                let tag = args.get(tag_index).copied();
                if tag.is_none_or(|t| t.kind() != Kind::StringLiteral) {
                    self.report(
                        tag.unwrap_or(call),
                        "NORN-FLOW-LITERAL",
                        "catchTag takes one literal registered tag",
                    );
                }
                let cb_index = tag_index.saturating_add(1);
                self.callback(args.get(cb_index).copied(), call, ctx, false);
                true
            }
            "effect/Effect#fail" => {
                let err = args.first().copied();
                if let Some(err_node) = err
                    && err_node.kind() == Kind::NewExpression
                {
                    self.new_expression(err_node, ctx, true);
                } else {
                    self.report(
                        err.unwrap_or(call),
                        "NORN-FLOW-UNSUPPORTED-SYNTAX",
                        "Effect.fail takes new <Data.TaggedError class>({ ...fields })",
                    );
                }
                true
            }
            "effect/Effect#all" => {
                self.effect_all_call(call, args, eff);
                true
            }
            "effect/Effect#retry" => {
                self.effect_retry_call(call, args, ctx, eff);
                true
            }
            "effect/Effect#sleep" => {
                self.require_literal(args.first().copied(), call, "the sleep duration", false);
                true
            }
            _ => false,
        }
    }

    fn effect_gen_call(&mut self, call: P<Node>, args: &[P<Node>], ctx: Ctx) {
        let fn_node = args.first().copied();
        let valid = args.len() == 1
            && fn_node.is_some_and(|node| {
                if node.kind() != Kind::FunctionExpression {
                    return false;
                }
                let func = node.as_function_expression();
                func.asterisk_token().is_some()
                    && func.parameters().is_none_or(|list| list.nodes().is_empty())
            });
        if !valid {
            self.report(
                fn_node.unwrap_or(call),
                "NORN-FLOW-CAPTURE",
                "Effect.gen takes one inline function* literal",
            );
            return;
        }
        if let Some(node) = fn_node
            && let Some(body) = node.as_function_expression().body()
            && body.kind() == Kind::Block
        {
            let stmts = body.as_block().statements.nodes();
            self.block(stmts, ctx.for_generator());
            if stmts
                .last()
                .is_none_or(|&last| last.kind() != Kind::ReturnStatement)
            {
                self.report(
                    node,
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "the generator must end with an explicit return of its result",
                );
            }
        }
    }

    fn effect_all_call(&mut self, call: P<Node>, args: &[P<Node>], eff: Ctx) {
        let tuple = args.first().copied();
        if let Some(tuple_node) = tuple
            && tuple_node.kind() == Kind::ArrayLiteralExpression
        {
            for &el in tuple_node.as_array_literal_expression().elements.nodes() {
                if el.kind() == Kind::SpreadElement {
                    self.report(el, "NORN-FLOW-EXPRESSION", "spread is not admitted");
                } else {
                    self.expr(el, eff);
                }
            }
        } else {
            self.report(
                tuple.unwrap_or(call),
                "NORN-FLOW-UNBOUNDED",
                "Effect.all takes a finite literal tuple",
            );
        }
        if let Some(&arg1) = args.get(1) {
            self.report(
                arg1,
                "NORN-FLOW-CONCURRENCY",
                "Effect.all options (concurrency/mode) are not admitted; v1 is sequential",
            );
        }
    }

    fn effect_retry_call(&mut self, call: P<Node>, args: &[P<Node>], ctx: Ctx, eff: Ctx) {
        if args.len() == 2
            && let Some(&arg0) = args.first()
        {
            self.expr(arg0, eff);
        }
        let policy = args.last().copied();
        let Some(policy_node) = policy else {
            self.report(
                call,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "use a Schedule policy: Schedule.recurs(n) or Schedule.max([delay, Schedule.recurs(n)])",
            );
            return;
        };
        if policy_node.kind() == Kind::ObjectLiteralExpression {
            self.report(
                policy_node,
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "use a Schedule policy: Schedule.recurs(n) or Schedule.max([delay, Schedule.recurs(n)])",
            );
            return;
        }
        let before = self.diagnostics.len();
        self.expr(policy_node, ctx.with_retry_policy(true));
        if self.diagnostics.len() == before && !self.finite_policy(policy_node) {
            self.report(
                policy_node,
                "NORN-FLOW-UNBOUNDED",
                "the retry policy must intersect a finite Schedule.recurs(n) (via Schedule.max)",
            );
        }
    }

    fn admitted_schedule_or_tool_call(
        &mut self,
        id: &str,
        call: P<Node>,
        ctx: Ctx,
        eff: Ctx,
    ) -> bool {
        let args = call.as_call_expression().arguments().nodes();
        match id {
            "effect/Schedule#recurs" => {
                let n = args.first().copied();
                let ok = n.is_some_and(|node| {
                    node.kind() == Kind::NumericLiteral && is_js_integer_literal(node.text())
                });
                if !ok {
                    self.report(
                        n.unwrap_or(call),
                        "NORN-FLOW-LITERAL",
                        "Schedule.recurs takes a non-negative integer literal",
                    );
                }
                true
            }
            "effect/Schedule#spaced" => {
                self.require_literal(args.first().copied(), call, "the schedule duration", false);
                true
            }
            "effect/Schedule#exponential" => {
                self.require_literal(args.first().copied(), call, "the base duration", false);
                if let Some(&arg1) = args.get(1) {
                    self.require_literal(Some(arg1), call, "the growth factor", true);
                }
                true
            }
            "effect/Schedule#max" => {
                let list = args.first().copied();
                if let Some(list_node) = list
                    && list_node.kind() == Kind::ArrayLiteralExpression
                    && !list_node
                        .as_array_literal_expression()
                        .elements
                        .nodes()
                        .is_empty()
                {
                    self.expr(list_node, ctx);
                } else {
                    self.report(
                        list.unwrap_or(call),
                        "NORN-FLOW-LITERAL",
                        "Schedule.max takes a literal non-empty array of schedules",
                    );
                }
                true
            }
            "effect/unstable/ai/Tool#make" => {
                let name = args.first().copied();
                if name.is_none_or(|n| n.kind() != Kind::StringLiteral) {
                    self.report(
                        name.unwrap_or(call),
                        "NORN-FLOW-LITERAL",
                        "the tool name must be a string literal",
                    );
                }
                if let Some(rest) = args.get(1..) {
                    for &arg in rest {
                        self.expr(arg, ctx);
                    }
                }
                true
            }
            "effect/Function#pipe" => {
                self.args(call, eff);
                true
            }
            _ => false,
        }
    }
}
