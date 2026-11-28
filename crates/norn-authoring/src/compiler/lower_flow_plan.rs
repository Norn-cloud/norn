//! Continuation stepping (`proceed`, `proceed_step`, `wait`) split out of
//! [`super::lower_flow`].

use std::collections::BTreeSet;

use serde_json::Value;

use super::expr::{
    Binding, Expr, ExprError, Scope, assert_expression_budget, assignment_error, render, type_of,
    zen_string,
};
use super::flow_source_map::{
    FlowSourceMapAnnotation, PerformStep, RetryPolicy, RouteBranch, SourceLocation, Step,
    TaggedErrorDecl,
};
use super::lower_flow::{
    AwaitTask, FlowLowering, Frame, LowerBreak, Path, ResumeTask, format_whole_f64,
};
use super::lower_flow_resume::EmitRow;
use super::value_schema::{BudgetExceeded, ValueSchema};

impl FlowLowering<'_> {
    pub(super) fn proceed(&mut self, path: Path, cont: Vec<Frame>) -> Result<(), LowerBreak> {
        self.budget.check()?;
        self.proceed_depth = self.proceed_depth.saturating_add(1);
        if self.proceed_depth > 512 {
            self.proceed_depth = self.proceed_depth.saturating_sub(1);
            self.report(
                "NORN-FLOW-UNBOUNDED",
                "composed flow exceeds lowering continuation depth bound",
                self.def.at.as_ref(),
            );
            return Err(LowerBreak::Bound);
        }
        let res = self.proceed_inner(path, cont);
        self.proceed_depth = self.proceed_depth.saturating_sub(1);
        res
    }

    fn proceed_inner(&mut self, path: Path, mut cont: Vec<Frame>) -> Result<(), LowerBreak> {
        let Some(top) = cont.pop() else {
            self.report(
                "NORN-FLOW-RESULT",
                "the flow can complete without an explicit result; end every path with a return",
                self.def.at.as_ref(),
            );
            return Ok(());
        };
        match top {
            Frame::Catch {
                source_map_context, ..
            } => {
                let mut next_path = path;
                next_path.source_map_context = source_map_context;
                self.proceed(next_path, cont)
            }
            Frame::Compose { at, .. } => {
                self.report(
                    "NORN-FLOW-RESULT",
                    "a composed effect can complete without returning a value",
                    at.as_ref(),
                );
                Ok(())
            }
            Frame::Rest {
                steps,
                index,
                outer,
                source_map_context,
            } => {
                let Some(step) = steps.get(index).cloned() else {
                    let scope = path
                        .scope
                        .into_iter()
                        .filter(|(name, _)| outer.contains(name))
                        .collect();
                    let next_path = Path {
                        scope,
                        source_map_context,
                        ..path
                    };
                    return self.proceed(next_path, cont);
                };
                cont.push(Frame::Rest {
                    steps,
                    index: index.saturating_add(1),
                    outer,
                    source_map_context,
                });
                self.proceed_step(path, step, cont)
            }
        }
    }

    fn proceed_step(&mut self, path: Path, step: Step, next: Vec<Frame>) -> Result<(), LowerBreak> {
        let names: BTreeSet<String> = path.scope.keys().cloned().collect();
        match step {
            Step::Compose {
                name,
                schema,
                body,
                at,
                source_map,
            } => self.step_compose(
                path,
                ComposeStepData {
                    name,
                    schema,
                    body,
                    at,
                    source_map,
                },
                names,
                next,
            ),
            Step::All {
                steps: inner,
                source_map,
                ..
            } => self.step_all(path, inner, source_map, names, next),
            Step::Value { value, at, .. } => self.step_value(path, value, at.as_ref(), next),
            Step::Perform(p) => self.step_perform(&path, *p, next),
            Step::Retry {
                perform,
                policy,
                source_map,
                ..
            } => self.step_retry(&path, *perform, policy, source_map, next),
            Step::Sleep {
                step,
                millis,
                at,
                source_map,
                ..
            } => self.step_sleep(path, (&step, millis), at.as_ref(), source_map, names, next),
            Step::Route {
                branches,
                otherwise,
                at,
                source_map,
            } => self.step_route(
                path,
                (branches, otherwise),
                at.as_ref(),
                source_map.as_ref(),
                names,
                next,
            ),
            Step::CatchTag {
                body,
                tag,
                as_name,
                handler,
                source_map,
                ..
            } => {
                let data = CatchTagStepData {
                    body,
                    tag,
                    as_name,
                    handler,
                    source_map,
                };
                self.step_catch_tag(path, data, names, next)
            }
            Step::Fail {
                error,
                fields,
                at,
                source_map,
            } => self.step_fail(
                path,
                &error,
                &fields,
                at.as_ref(),
                source_map.as_ref(),
                &next,
            ),
            Step::Succeed {
                value,
                at,
                source_map,
            } => self.step_succeed(&path, &value, at.as_ref(), source_map),
        }
    }

    fn step_perform(
        &mut self,
        path: &Path,
        perform: PerformStep,
        next: Vec<Frame>,
    ) -> Result<(), LowerBreak> {
        let owners = perform
            .source_map
            .as_ref()
            .map(|s| s.id.clone())
            .into_iter()
            .collect();
        self.wait(path, perform, next, None, owners)
    }

    fn step_retry(
        &mut self,
        path: &Path,
        perform: PerformStep,
        policy: RetryPolicy,
        source_map: Option<FlowSourceMapAnnotation>,
        next: Vec<Frame>,
    ) -> Result<(), LowerBreak> {
        let mut owners = Vec::new();
        if let Some(sm) = source_map {
            owners.push(sm.id);
        }
        if let Some(sm) = perform.source_map.as_ref() {
            owners.push(sm.id.clone());
        }
        self.wait(path, perform, next, Some(policy), owners)
    }

    fn step_compose(
        &mut self,
        mut path: Path,
        data: ComposeStepData,
        names: BTreeSet<String>,
        mut next: Vec<Frame>,
    ) -> Result<(), LowerBreak> {
        let context = Self::with_source_map(
            &path.source_map_context,
            data.source_map.as_ref().map(|s| s.id.as_str()),
        );
        let outer_ctx = path.source_map_context.clone();
        next.push(Frame::Compose {
            name: data.name,
            schema: data.schema,
            outer: names.clone(),
            at: data.at,
            source_map_context: outer_ctx,
        });
        next.push(Frame::Rest {
            steps: data.body,
            index: 0,
            outer: names,
            source_map_context: context.clone(),
        });
        path.source_map_context = context;
        self.proceed(path, next)
    }

    fn step_all(
        &mut self,
        mut path: Path,
        inner: Vec<Step>,
        source_map: Option<FlowSourceMapAnnotation>,
        names: BTreeSet<String>,
        mut next: Vec<Frame>,
    ) -> Result<(), LowerBreak> {
        let context = Self::with_source_map(
            &path.source_map_context,
            source_map.as_ref().map(|s| s.id.as_str()),
        );
        let outer = Self::bindings_in(&inner, &names);
        let outer_ctx = path.source_map_context.clone();
        next.push(Frame::Rest {
            steps: inner,
            index: 0,
            outer,
            source_map_context: outer_ctx,
        });
        path.source_map_context = context;
        self.proceed(path, next)
    }

    fn step_catch_tag(
        &mut self,
        mut path: Path,
        data: CatchTagStepData,
        names: BTreeSet<String>,
        mut next: Vec<Frame>,
    ) -> Result<(), LowerBreak> {
        let sm_id = data.source_map.map(|s| s.id);
        let context = Self::with_source_map(&path.source_map_context, sm_id.as_deref());
        let outer_ctx = path.source_map_context.clone();
        next.push(Frame::Catch {
            tag: data.tag,
            as_name: data.as_name,
            handler: data.handler,
            node_source_map_id: sm_id,
            source_map_context: outer_ctx,
        });
        next.push(Frame::Rest {
            steps: data.body,
            index: 0,
            outer: names,
            source_map_context: context.clone(),
        });
        path.source_map_context = context;
        self.proceed(path, next)
    }

    fn step_value(
        &mut self,
        path: Path,
        value: Expr,
        at: Option<&SourceLocation>,
        mut next: Vec<Frame>,
    ) -> Result<(), LowerBreak> {
        let Some(idx) = next
            .iter()
            .rposition(|f| matches!(f, Frame::Compose { .. }))
        else {
            self.report("NORN-FLOW-RESULT", "a Value must occur inside Compose", at);
            return Ok(());
        };
        let Some(Frame::Compose {
            name,
            schema,
            outer,
            source_map_context,
            ..
        }) = next.get(idx).cloned()
        else {
            return Ok(());
        };
        assert_expression_budget(self.budget, &value, &path.scope)?;
        match assignment_error(&value, &schema, &path.scope, "$") {
            Ok(None) => {}
            Ok(Some(problem)) => {
                self.report("NORN-FLOW-VALUE", problem, at);
                return Ok(());
            }
            Err(err) => {
                self.report(&err.code, err.message, at);
                return Ok(());
            }
        }
        let mut new_scope: Scope = path
            .scope
            .iter()
            .filter(|(k, _)| outer.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        new_scope.insert(
            name,
            Binding::Inline {
                inline: value,
                scope: path.scope,
                schema,
            },
        );
        next.truncate(idx);
        let next_path = Path {
            scope: new_scope,
            source_map_context,
            ..path
        };
        self.proceed(next_path, next)
    }

    fn step_sleep(
        &mut self,
        path: Path,
        sleep: (&str, f64),
        at: Option<&SourceLocation>,
        source_map: Option<FlowSourceMapAnnotation>,
        names: BTreeSet<String>,
        next: Vec<Frame>,
    ) -> Result<(), LowerBreak> {
        let (step_id, millis) = sleep;
        let state = format!("sleeping_{step_id}");
        let timer_key = Self::step_key(&path.key, step_id);
        let millis_str = format_whole_f64(millis);
        let timer = format!(
            "{{kind: 'timer', runId: {}, step: '{step_id}', attempt: 0, timerId: {timer_key}, afterMillis: {millis_str}}}",
            path.key
        );
        let owners: Vec<String> = source_map.map(|s| s.id).into_iter().collect();
        self.emit(EmitRow {
            path: &path,
            to: &state,
            commands: Vec::new(),
            timers: vec![timer],
            what: &format!("sleep {millis_str} ms"),
            at,
            explicit_slots: None,
            extra_owners: &owners,
        })?;
        self.enqueue_state(
            &state,
            at,
            ResumeTask::Sleep {
                step: step_id.to_owned(),
                state: state.clone(),
                names,
                source_map_context: path.source_map_context,
                owners,
                next,
            },
        );
        Ok(())
    }

    fn step_route(
        &mut self,
        path: Path,
        route: (Vec<RouteBranch>, Vec<Step>),
        at: Option<&SourceLocation>,
        source_map: Option<&FlowSourceMapAnnotation>,
        names: BTreeSet<String>,
        next: Vec<Frame>,
    ) -> Result<(), LowerBreak> {
        let (branches, otherwise) = route;
        let context =
            Self::with_source_map(&path.source_map_context, source_map.map(|s| s.id.as_str()));
        let mut rendered = Vec::with_capacity(branches.len());
        for b in &branches {
            match self.render_condition(&b.when, &path.scope) {
                Ok(cond) => rendered.push(cond),
                Err(err) => {
                    self.report(&err.code, err.message, at);
                    return Ok(());
                }
            }
        }
        for (i, branch) in branches.into_iter().enumerate() {
            let mut guards = path.guards.clone();
            if let Some(prefix) = rendered.get(..i) {
                for prev in prefix {
                    guards.push(format!("not ({prev})"));
                }
            }
            if let Some(cur) = rendered.get(i) {
                guards.push(format!("({cur})"));
            }
            let mut branch_cont = next.clone();
            branch_cont.push(Frame::Rest {
                steps: branch.then,
                index: 0,
                outer: names.clone(),
                source_map_context: path.source_map_context.clone(),
            });
            let branch_path = Path {
                guards,
                source_map_context: context.clone(),
                ..path.clone()
            };
            self.proceed(branch_path, branch_cont)?;
        }
        let mut otherwise_guards = path.guards.clone();
        for prev in &rendered {
            otherwise_guards.push(format!("not ({prev})"));
        }
        let mut otherwise_cont = next;
        otherwise_cont.push(Frame::Rest {
            steps: otherwise,
            index: 0,
            outer: names,
            source_map_context: path.source_map_context.clone(),
        });
        let otherwise_path = Path {
            guards: otherwise_guards,
            source_map_context: context,
            ..path
        };
        self.proceed(otherwise_path, otherwise_cont)
    }

    fn step_fail(
        &mut self,
        path: Path,
        error: &TaggedErrorDecl,
        fields: &Expr,
        at: Option<&SourceLocation>,
        source_map: Option<&FlowSourceMapAnnotation>,
        next: &[Frame],
    ) -> Result<(), LowerBreak> {
        let Expr::Record(record_fields) = fields else {
            self.report(
                "NORN-FLOW-VALUE",
                "Effect.fail takes the error's fields as a record literal",
                at,
            );
            return Ok(());
        };
        assert_expression_budget(self.budget, fields, &path.scope)?;
        let expected = ValueSchema::object(error.fields.clone());
        match assignment_error(fields, &expected, &path.scope, "$") {
            Ok(None) => {}
            Ok(Some(problem)) => {
                self.report(
                    "NORN-FLOW-VALUE",
                    format!("fields of {}: {problem}", error.tag),
                    at,
                );
                return Ok(());
            }
            Err(err) => {
                self.report(&err.code, err.message, at);
                return Ok(());
            }
        }
        let mut failure_fields = Vec::with_capacity(record_fields.len().saturating_add(1));
        failure_fields.push((
            "_tag".to_owned(),
            Expr::lit(Value::String(error.tag.clone())),
        ));
        failure_fields.extend(record_fields.iter().cloned());
        let failure_expr = Expr::Record(failure_fields);
        let schema = ValueSchema::object(Self::failure_record(error));
        let mut raise_path = path;
        raise_path.source_map_owners = Self::with_source_map(
            &raise_path.source_map_owners,
            source_map.map(|s| s.id.as_str()),
        );
        let failure = Binding::Inline {
            inline: failure_expr,
            scope: raise_path.scope.clone(),
            schema,
        };
        self.raise(raise_path, next, &error.tag, failure, at)
    }

    fn step_succeed(
        &mut self,
        path: &Path,
        value: &Expr,
        at: Option<&SourceLocation>,
        source_map: Option<FlowSourceMapAnnotation>,
    ) -> Result<(), LowerBreak> {
        assert_expression_budget(self.budget, value, &path.scope)?;
        let schema = match type_of(value, &path.scope) {
            Ok(s) => s,
            Err(err) => {
                self.report(&err.code, err.message, at);
                return Ok(());
            }
        };
        self.budget.assert_schema(&schema)?;
        let rendered = match render(value, &path.scope) {
            Ok(r) => r,
            Err(err) => {
                self.report(&err.code, err.message, at);
                return Ok(());
            }
        };
        let merged =
            self.merge_optional_schema(self.result_schema.clone(), schema, at, "flow result")?;
        self.result_schema = Some(merged);
        let owners: Vec<String> = source_map.map(|s| s.id).into_iter().collect();
        self.emit(EmitRow {
            path,
            to: "completed",
            commands: Vec::new(),
            timers: Vec::new(),
            what: "complete",
            at,
            explicit_slots: Some(format!("{{result: {rendered}}}")),
            extra_owners: &owners,
        })?;
        self.terminal.insert("completed".to_owned());
        Ok(())
    }

    fn render_condition(&mut self, when: &Expr, scope: &Scope) -> Result<String, ExprError> {
        if let Err(BudgetExceeded(msg)) = assert_expression_budget(self.budget, when, scope) {
            return Err(ExprError::new("NORN-FLOW-UNBOUNDED", msg));
        }
        let ty = type_of(when, scope)?;
        if let Err(BudgetExceeded(msg)) = self.budget.assert_schema(&ty) {
            return Err(ExprError::new("NORN-FLOW-UNBOUNDED", msg));
        }
        if !matches!(ty, ValueSchema::Boolean { .. }) {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                format!("a route condition must be boolean, not {}", ty.type_label()),
            ));
        }
        render(when, scope)
    }

    pub(super) fn step_key(run_key: &str, step: &str) -> String {
        format!("{run_key} + ':{step}'")
    }

    pub(super) fn retry_timer_key(run_key: &str, step: &str, attempt: u32) -> String {
        format!("{run_key} + ':{step}:attempt:{attempt}'")
    }

    pub(super) fn awaiting_state(
        node: &PerformStep,
        attempt: u32,
        policy: Option<&RetryPolicy>,
    ) -> String {
        if policy.is_some() {
            format!("awaiting_retry_{}_attempt_{attempt}", node.step)
        } else {
            format!("awaiting_perform_{}", node.step)
        }
    }

    pub(super) fn retrying_state(node: &PerformStep, attempt: u32) -> String {
        format!("retrying_{}_attempt_{attempt}", node.step)
    }

    pub(super) fn perform_command(
        run_key: &str,
        node: &PerformStep,
        attempt: u32,
        input: &str,
    ) -> Result<String, ExprError> {
        Ok(format!(
            "{{kind: 'perform', runId: {run_key}, step: '{}', function: {}, functionVersion: {}, idempotencyKey: {}, attempt: {attempt}, input: {{{}: {input}}}}}",
            node.step,
            zen_string(&node.func.id)?,
            zen_string(&node.func.version)?,
            Self::step_key(run_key, &node.step),
            node.step,
        ))
    }

    fn wait(
        &mut self,
        path: &Path,
        node: PerformStep,
        next: Vec<Frame>,
        policy: Option<RetryPolicy>,
        owners: Vec<String>,
    ) -> Result<(), LowerBreak> {
        assert_expression_budget(self.budget, &node.input, &path.scope)?;
        let expected_input = ValueSchema::object(node.func.input.clone());
        match assignment_error(&node.input, &expected_input, &path.scope, "$") {
            Ok(None) => {}
            Ok(Some(problem)) => {
                self.report(
                    "NORN-FLOW-VALUE",
                    format!("input of {}: {problem}", node.func.id),
                    node.at.as_ref(),
                );
                return Ok(());
            }
            Err(err) => {
                self.report(&err.code, err.message, node.at.as_ref());
                return Ok(());
            }
        }
        let input = match render(&node.input, &path.scope) {
            Ok(r) => r,
            Err(err) => {
                self.report(&err.code, err.message, node.at.as_ref());
                return Ok(());
            }
        };
        let cmd = match Self::perform_command(&path.key, &node, 1, &input) {
            Ok(c) => c,
            Err(err) => {
                self.report(&err.code, err.message, node.at.as_ref());
                return Ok(());
            }
        };
        let state = Self::awaiting_state(&node, 1, policy.as_ref());
        self.emit(EmitRow {
            path,
            to: &state,
            commands: vec![cmd],
            timers: Vec::new(),
            what: &format!("perform {}", node.func.id),
            at: node.at.as_ref(),
            explicit_slots: None,
            extra_owners: &owners,
        })?;
        let names: BTreeSet<String> = path.scope.keys().cloned().collect();
        let node_at = node.at.clone();
        self.enqueue_state(
            &state,
            node_at.as_ref(),
            ResumeTask::Awaiting(Box::new(AwaitTask {
                node: Box::new(node),
                next,
                policy,
                names,
                attempt: 1,
                source_map_context: path.source_map_context.clone(),
                owners,
            })),
        );
        Ok(())
    }
}

struct ComposeStepData {
    name: String,
    schema: ValueSchema,
    body: Vec<Step>,
    at: Option<SourceLocation>,
    source_map: Option<FlowSourceMapAnnotation>,
}

struct CatchTagStepData {
    body: Vec<Step>,
    tag: String,
    as_name: Option<String>,
    handler: Vec<Step>,
    source_map: Option<FlowSourceMapAnnotation>,
}
