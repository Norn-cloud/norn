//! Resume handlers, exception raising, and decision-row emission split out of
//! [`super::lower_flow`].

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::expr::{Binding, Scope, render, zen_string};
use super::flow_source_map::{FLOW_MAX_ROWS, RetryPolicy, SourceLocation, Step, retry_delay};
use super::lower_flow::{
    AwaitTask, FlowLowering, Frame, LowerBreak, Path, ResumeTask, RetryTask, format_whole_f64,
    snake_case_tag,
};
use super::value_schema::{ValueSchema, merge_schemas};

pub(super) struct EmitRow<'a> {
    pub(super) path: &'a Path,
    pub(super) to: &'a str,
    pub(super) commands: Vec<String>,
    pub(super) timers: Vec<String>,
    pub(super) what: &'a str,
    pub(super) at: Option<&'a SourceLocation>,
    pub(super) explicit_slots: Option<String>,
    pub(super) extra_owners: &'a [String],
}

pub(super) struct ResumePath<'a> {
    pub(super) state: &'a str,
    pub(super) event: &'a str,
    pub(super) extra_guards: Vec<String>,
    pub(super) names: &'a BTreeSet<String>,
    pub(super) source_map_context: Vec<String>,
    pub(super) source_map_owners: Vec<String>,
}

impl FlowLowering<'_> {
    pub(super) fn resume_sleep(
        &mut self,
        step: &str,
        state: &str,
        names: &BTreeSet<String>,
        source_map_context: Vec<String>,
        owners: Vec<String>,
        next: Vec<Frame>,
    ) -> Result<(), LowerBreak> {
        let timer_key = Self::step_key("slice.run.run_id", step);
        let path = self.resumed(ResumePath {
            state,
            event: "timer",
            extra_guards: vec![
                "event.runId == slice.run.run_id".to_owned(),
                format!("event.step == '{step}'"),
                "event.attempt == 0".to_owned(),
                format!("event.timerId == {timer_key}"),
            ],
            names,
            source_map_context,
            source_map_owners: owners,
        });
        self.proceed(path, next)
    }

    pub(super) fn awaiting(&mut self, task: &AwaitTask) -> Result<(), LowerBreak> {
        let state = Self::awaiting_state(&task.node, task.attempt, task.policy.as_ref());
        let on_step = format!("event.step == '{}'", task.node.step);
        let on_run = "event.runId == slice.run.run_id".to_owned();
        let on_attempt = format!("event.attempt == {}", task.attempt);
        let mut ok_path = self.resumed(ResumePath {
            state: &state,
            event: "result",
            extra_guards: vec![on_run.clone(), on_step.clone(), on_attempt.clone()],
            names: &task.names,
            source_map_context: task.source_map_context.clone(),
            source_map_owners: task.owners.clone(),
        });
        if let Some(bind) = &task.node.bind {
            ok_path.scope.insert(
                bind.clone(),
                Binding::Zen {
                    zen: format!("event.results.{}", task.node.step),
                    schema: task.node.func.output.clone(),
                },
            );
        }
        self.proceed(ok_path, task.next.clone())?;
        if task.node.func.errors.is_empty() {
            return Ok(());
        }
        let will_retry = task
            .policy
            .as_ref()
            .and_then(|p| p.recurs)
            .is_some_and(|recurs| task.attempt <= recurs);
        if will_retry && let Some(pol) = task.policy.as_ref() {
            return self.awaiting_retry(task, &state, pol, &[on_run, on_step, on_attempt]);
        }
        self.awaiting_fail(task, &state, &[on_run, on_step, on_attempt])
    }

    fn awaiting_retry(
        &mut self,
        task: &AwaitTask,
        state: &str,
        pol: &RetryPolicy,
        base_guards: &[String],
    ) -> Result<(), LowerBreak> {
        let next_attempt = task.attempt.saturating_add(1);
        let retrying = Self::retrying_state(&task.node, next_attempt);
        let delay_str = format_whole_f64(retry_delay(pol, task.attempt));
        for decl in &task.node.func.errors {
            let mut extra_guards = base_guards.to_vec();
            extra_guards.push(format!("event.failure._tag == '{}'", decl.tag));
            let fail_path = self.resumed(ResumePath {
                state,
                event: "failure",
                extra_guards,
                names: &task.names,
                source_map_context: task.source_map_context.clone(),
                source_map_owners: task.owners.clone(),
            });
            let timer = format!(
                "{{kind: 'timer', runId: {}, step: '{}', attempt: {next_attempt}, timerId: {}, afterMillis: {delay_str}}}",
                fail_path.key,
                task.node.step,
                Self::retry_timer_key(&fail_path.key, &task.node.step, next_attempt)
            );
            self.emit(EmitRow {
                path: &fail_path,
                to: &retrying,
                commands: Vec::new(),
                timers: vec![timer],
                what: &format!("retry {} after attempt {}", task.node.step, task.attempt),
                at: task.node.at.as_ref(),
                explicit_slots: None,
                extra_owners: &[],
            })?;
        }
        self.enqueue_state(
            &retrying,
            task.node.at.as_ref(),
            ResumeTask::Retrying(Box::new(RetryTask {
                node: task.node.clone(),
                next: task.next.clone(),
                policy: pol.clone(),
                names: task.names.clone(),
                next_attempt,
                retrying: retrying.clone(),
                source_map_context: task.source_map_context.clone(),
                owners: task.owners.clone(),
            })),
        );
        Ok(())
    }

    fn awaiting_fail(
        &mut self,
        task: &AwaitTask,
        state: &str,
        base_guards: &[String],
    ) -> Result<(), LowerBreak> {
        for decl in &task.node.func.errors {
            let mut extra_guards = base_guards.to_vec();
            extra_guards.push(format!("event.failure._tag == '{}'", decl.tag));
            let fail_path = self.resumed(ResumePath {
                state,
                event: "failure",
                extra_guards,
                names: &task.names,
                source_map_context: task.source_map_context.clone(),
                source_map_owners: task.owners.clone(),
            });
            let failure = Binding::Zen {
                zen: "event.failure".to_owned(),
                schema: ValueSchema::object(Self::failure_record(decl)),
            };
            self.raise(
                fail_path,
                &task.next,
                &decl.tag,
                failure,
                task.node.at.as_ref(),
            )?;
        }
        Ok(())
    }

    pub(super) fn retrying_resume(&mut self, task: &RetryTask) -> Result<(), LowerBreak> {
        let timer_key =
            Self::retry_timer_key("slice.run.run_id", &task.node.step, task.next_attempt);
        let path = self.resumed(ResumePath {
            state: &task.retrying,
            event: "timer",
            extra_guards: vec![
                "event.runId == slice.run.run_id".to_owned(),
                format!("event.step == '{}'", task.node.step),
                format!("event.attempt == {}", task.next_attempt),
                format!("event.timerId == {timer_key}"),
            ],
            names: &task.names,
            source_map_context: task.source_map_context.clone(),
            source_map_owners: task.owners.clone(),
        });
        let stored = match render(&task.node.input, &path.scope) {
            Ok(s) => s,
            Err(err) => {
                self.report(&err.code, err.message, task.node.at.as_ref());
                "null".to_owned()
            }
        };
        let cmd = match Self::perform_command(&path.key, &task.node, task.next_attempt, &stored) {
            Ok(c) => c,
            Err(err) => {
                self.report(&err.code, err.message, task.node.at.as_ref());
                return Ok(());
            }
        };
        let next_state = Self::awaiting_state(&task.node, task.next_attempt, Some(&task.policy));
        self.emit(EmitRow {
            path: &path,
            to: &next_state,
            commands: vec![cmd],
            timers: Vec::new(),
            what: &format!("perform {} again", task.node.func.id),
            at: task.node.at.as_ref(),
            explicit_slots: None,
            extra_owners: &task.owners,
        })?;
        self.enqueue_state(
            &next_state,
            task.node.at.as_ref(),
            ResumeTask::Awaiting(Box::new(AwaitTask {
                node: task.node.clone(),
                next: task.next.clone(),
                policy: Some(task.policy.clone()),
                names: task.names.clone(),
                attempt: task.next_attempt,
                source_map_context: task.source_map_context.clone(),
                owners: task.owners.clone(),
            })),
        );
        Ok(())
    }

    pub(super) fn raise(
        &mut self,
        path: Path,
        cont: &[Frame],
        tag: &str,
        failure: Binding,
        at: Option<&SourceLocation>,
    ) -> Result<(), LowerBreak> {
        for idx in (0..cont.len()).rev() {
            let Some(Frame::Catch {
                tag: catch_tag,
                as_name,
                handler,
                node_source_map_id,
                source_map_context,
            }) = cont.get(idx)
            else {
                continue;
            };
            if catch_tag != tag {
                continue;
            }
            let below = cont.get(..idx).unwrap_or(&[]);
            let mut scope = Self::scope_at(below, &path.scope);
            let kept_names: BTreeSet<String> = scope.keys().cloned().collect();
            if let Some(name) = as_name {
                scope.insert(name.clone(), failure);
            }
            let next_ctx = Self::with_source_map(source_map_context, node_source_map_id.as_deref());
            let mut next_cont = below.to_vec();
            next_cont.push(Frame::Rest {
                steps: handler.clone(),
                index: 0,
                outer: kept_names,
                source_map_context: source_map_context.clone(),
            });
            let next_path = Path {
                scope,
                source_map_context: next_ctx,
                ..path
            };
            return self.proceed(next_path, next_cont);
        }
        let state = format!("failed_{}", snake_case_tag(tag));
        let value = match &failure {
            Binding::Zen { zen, .. } => zen.clone(),
            Binding::Inline { inline, scope, .. } => match render(inline, scope) {
                Ok(r) => r,
                Err(err) => {
                    self.report(&err.code, err.message, at);
                    return Ok(());
                }
            },
        };
        let merged = self.merge_optional_schema(
            self.uncaught.clone(),
            failure.schema().clone(),
            at,
            "uncaught failure",
        )?;
        self.uncaught = Some(merged);
        self.emit(EmitRow {
            path: &path,
            to: &state,
            commands: Vec::new(),
            timers: Vec::new(),
            what: &format!("fail {tag}"),
            at,
            explicit_slots: Some(format!("{{failure: {value}}}")),
            extra_owners: &[],
        })?;
        self.terminal.insert(state);
        Ok(())
    }

    fn scope_at(below: &[Frame], scope: &Scope) -> Scope {
        let Some(Frame::Rest {
            steps,
            index,
            outer,
            ..
        }) = below.last()
        else {
            return scope.clone();
        };
        let before = steps.get(..*index).unwrap_or(&[]);
        scope
            .iter()
            .filter(|(name, _)| outer.contains(*name) || Self::declared_in(before, name))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn declared_in(steps: &[Step], name: &str) -> bool {
        steps.iter().any(|s| match s {
            Step::Perform(p) => p.bind.as_deref() == Some(name),
            Step::Retry { perform, .. } => perform.bind.as_deref() == Some(name),
            Step::Compose { name: cname, .. } => cname == name,
            Step::All { steps: inner, .. } => Self::declared_in(inner, name),
            Step::Sleep { .. }
            | Step::Route { .. }
            | Step::CatchTag { .. }
            | Step::Fail { .. }
            | Step::Succeed { .. }
            | Step::Value { .. } => false,
        })
    }

    pub(super) fn bindings_in(steps: &[Step], outer: &BTreeSet<String>) -> BTreeSet<String> {
        let mut out = outer.clone();
        for step in steps {
            match step {
                Step::Perform(p) => {
                    if let Some(b) = &p.bind {
                        out.insert(b.clone());
                    }
                }
                Step::Retry { perform, .. } => {
                    if let Some(b) = &perform.bind {
                        out.insert(b.clone());
                    }
                }
                Step::Compose { name, .. } => {
                    out.insert(name.clone());
                }
                Step::All { steps: inner, .. } => {
                    let sub = Self::bindings_in(inner, &BTreeSet::new());
                    out.extend(sub);
                }
                Step::Sleep { .. }
                | Step::Route { .. }
                | Step::CatchTag { .. }
                | Step::Fail { .. }
                | Step::Succeed { .. }
                | Step::Value { .. } => {}
            }
        }
        out
    }

    pub(super) fn merge_optional_schema(
        &mut self,
        current: Option<ValueSchema>,
        next: ValueSchema,
        at: Option<&SourceLocation>,
        what: &str,
    ) -> Result<ValueSchema, LowerBreak> {
        self.budget.assert_schema(&next)?;
        let Some(curr) = current else {
            return Ok(next);
        };
        self.budget.assert_schema(&curr)?;
        match merge_schemas(&curr, &next, "$") {
            Ok(merged) => {
                self.budget.assert_schema(&merged)?;
                Ok(merged)
            }
            Err(msg) => {
                self.report(
                    "NORN-FLOW-RESULT",
                    format!("every {what} must share one closed shape: {msg}"),
                    at,
                );
                Ok(curr)
            }
        }
    }

    pub(super) fn resumed(&self, spec: ResumePath<'_>) -> Path {
        let mut scope = BTreeMap::new();
        for name in spec.names {
            if let Some(schema) = self.binding_schemas.get(name) {
                scope.insert(
                    name.clone(),
                    Binding::Zen {
                        zen: format!("slice.run.slots.bindings.{name}"),
                        schema: schema.clone(),
                    },
                );
            }
        }
        let mut guards = vec![
            format!("slice.run.status == '{}'", spec.state),
            format!("event.kind == '{}'", spec.event),
        ];
        guards.extend(spec.extra_guards);
        Path {
            from: spec.state.to_owned(),
            event: spec.event.to_owned(),
            guards,
            scope,
            facts: Vec::new(),
            key: "slice.run.run_id".to_owned(),
            source_map_context: spec.source_map_context,
            source_map_owners: spec.source_map_owners,
        }
    }

    pub(super) fn enqueue_state(
        &mut self,
        state: &str,
        at: Option<&SourceLocation>,
        task: ResumeTask,
    ) {
        if !self.states.contains_key(state) {
            self.states_order.push(state.to_owned());
            self.states.insert(state.to_owned(), at.cloned());
        }
        if self.registered.insert(state.to_owned()) {
            self.queue.push_back(task);
        }
    }

    pub(super) fn emit(&mut self, row: EmitRow<'_>) -> Result<(), LowerBreak> {
        if self.rows.len() >= FLOW_MAX_ROWS {
            self.report(
                "NORN-FLOW-UNBOUNDED",
                format!("the flow lowers to more than {FLOW_MAX_ROWS} decision rows"),
                row.at,
            );
            return Err(LowerBreak::Bound);
        }
        if !self.states.contains_key(row.to) {
            self.states_order.push(row.to.to_owned());
            self.states.insert(row.to.to_owned(), row.at.cloned());
        }
        let mapped_ids = self.link_state_source_map(row.path, row.extra_owners, row.to);
        self.record_transition(&row.path.from, row.to);
        let Ok(run_type_zen) = zen_string(&self.run_type) else {
            return Ok(());
        };
        let mut facts = row.path.facts.clone();
        let stored = match row.explicit_slots {
            Some(s) => Some(s),
            None => self.build_slots(row.path)?,
        };
        if let Some(slot_val) = stored {
            facts.push(format!(
                "{{op: 'set', objectType: {run_type_zen}, key: {}, property: 'slots', value: {slot_val}}}",
                row.path.key
            ));
        }
        facts.push(format!(
            "{{op: 'transition', objectType: {run_type_zen}, key: {}, lifecycle: 'status', transition: 'to_{}'}}",
            row.path.key, row.to
        ));
        let base = format!("{}/{}", row.path.from, row.path.event);
        let n = self
            .row_counts
            .get(&base)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.row_counts.insert(base.clone(), n);
        let row_id = format!("{base}/{n}");
        for id in &mapped_ids {
            if let Some(links) = self.source_map_links.get_mut(id) {
                links.rows.insert(row_id.clone());
            }
        }
        let guard = row.path.guards.join(" and ");
        let description = format!(
            "{} on {}: {} → {}",
            row.path.from, row.path.event, row.what, row.to
        );
        let row_val = Value::String(format!("{row_id}:{guard}:{description}"));
        self.budget.add_output(&row_val)?;
        self.rows.push(super::lower_flow::Row {
            id: row_id,
            guard,
            facts,
            commands: row.commands,
            timers: row.timers,
            description,
            at: row.at.cloned(),
        });
        Ok(())
    }

    fn link_state_source_map(
        &mut self,
        path: &Path,
        extra_owners: &[String],
        to: &str,
    ) -> BTreeSet<String> {
        let mut mapped_ids = BTreeSet::new();
        for id in path
            .source_map_context
            .iter()
            .chain(path.source_map_owners.iter())
            .chain(extra_owners.iter())
        {
            mapped_ids.insert(id.clone());
        }
        for id in &mapped_ids {
            if let Some(links) = self.source_map_links.get_mut(id) {
                links.states.insert(to.to_owned());
            }
        }
        mapped_ids
    }

    fn record_transition(&mut self, from: &str, to: &str) {
        if !self.transitions.contains_key(to) {
            self.transitions_order.push(to.to_owned());
            self.transitions.insert(to.to_owned(), Vec::new());
        }
        if let Some(sources) = self.transitions.get_mut(to)
            && !sources.iter().any(|s| s == from)
        {
            sources.push(from.to_owned());
        }
    }

    fn build_slots(&mut self, path: &Path) -> Result<Option<String>, LowerBreak> {
        let fresh = path.scope.iter().any(|(name, binding)| match binding {
            Binding::Zen { zen, .. } => *zen != format!("slice.run.slots.bindings.{name}"),
            Binding::Inline { .. } => true,
        });
        if !fresh {
            return Ok(None);
        }
        let mut ordered_names: Vec<&str> = Vec::with_capacity(path.scope.len());
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for name in &self.binding_order {
            if let Some((key, _)) = path.scope.get_key_value(name)
                && seen.insert(key.as_str())
            {
                ordered_names.push(key.as_str());
            }
        }
        for name in path.scope.keys() {
            if seen.insert(name.as_str()) {
                ordered_names.push(name.as_str());
            }
        }
        let mut fields = Vec::with_capacity(ordered_names.len());
        for name in ordered_names {
            let Some(binding) = path.scope.get(name) else {
                continue;
            };
            let rendered = match binding {
                Binding::Zen { zen, .. } => zen.clone(),
                Binding::Inline { inline, scope, .. } => match render(inline, scope) {
                    Ok(r) => r,
                    Err(err) => {
                        self.report(&err.code, err.message, self.def.at.as_ref());
                        "null".to_owned()
                    }
                },
            };
            let field = format!("{name}: {rendered}");
            self.budget.add_output(&Value::String(field.clone()))?;
            fields.push(field);
        }
        Ok(Some(format!("{{bindings: {{{}}}}}", fields.join(", "))))
    }

    pub(super) fn with_source_map(current: &[String], id: Option<&str>) -> Vec<String> {
        let mut out = current.to_vec();
        if let Some(id) = id
            && !out.iter().any(|existing| existing == id)
        {
            out.push(id.to_owned());
        }
        out
    }
}
