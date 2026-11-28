//! Flow lowering validation & collection pass split out of [`super::lower_flow`].

use std::collections::BTreeSet;

use super::canonical::{compact_canonical_json, is_pascal_ident, is_snake_ident};
use super::flow_source_map::{
    FLOW_MAX_RECURS, FLOW_MAX_STEPS, FLOW_MAX_TIMER_MILLIS, FnRef, PerformStep, RetryPolicy,
    SourceLocation, Step, TaggedErrorDecl, retry_delay,
};
use super::lower_flow::{FlowLowering, snake_case_tag};
use super::value_schema::{ObjectSchema, ValueSchema};

impl FlowLowering<'_> {
    pub(super) fn collect(&mut self, steps: &[Step]) {
        for step in steps {
            match step {
                Step::Compose {
                    name,
                    schema,
                    body,
                    at,
                    ..
                } => {
                    self.register_binding(name, schema.clone(), at.as_ref());
                    self.collect(body);
                }
                Step::All { steps: inner, .. } => self.collect(inner),
                Step::Value { .. } | Step::Succeed { .. } => {}
                Step::Retry {
                    perform,
                    policy,
                    at,
                    ..
                } => {
                    self.check_policy(policy, at.as_ref());
                    self.collect_perform(perform);
                    if !perform.func.errors.is_empty() {
                        self.retried.insert(perform.step.clone());
                    }
                }
                Step::Perform(p) => self.collect_perform(p),
                Step::Sleep {
                    step: step_id,
                    millis,
                    at,
                    ..
                } => self.collect_sleep(step_id, *millis, at.as_ref()),
                Step::Route {
                    branches,
                    otherwise,
                    ..
                } => {
                    for b in branches {
                        self.collect(&b.then);
                    }
                    self.collect(otherwise);
                }
                Step::CatchTag {
                    body,
                    tag,
                    as_name,
                    handler,
                    at,
                    ..
                } => self.collect_catch_tag(body, tag, as_name.as_deref(), handler, at.as_ref()),
                Step::Fail { error, .. } => self.register_error(error),
            }
        }
    }

    fn collect_sleep(&mut self, step_id: &str, millis: f64, at: Option<&SourceLocation>) {
        self.register_durable(step_id, at);
        self.sleeps.insert(step_id.to_owned());
        let max_f = 604_800_000.0_f64;
        let valid = millis.is_finite()
            && millis.fract().abs().to_bits() == 0
            && millis > 0.0
            && millis <= max_f;
        if !valid {
            self.report(
                "NORN-FLOW-UNBOUNDED",
                format!(
                    "sleep {step_id} must be a whole number of milliseconds in (0, {FLOW_MAX_TIMER_MILLIS}]"
                ),
                at,
            );
        }
    }

    fn collect_catch_tag(
        &mut self,
        body: &[Step],
        tag: &str,
        as_name: Option<&str>,
        handler: &[Step],
        at: Option<&SourceLocation>,
    ) {
        if !Self::raises(body).contains(tag) {
            self.report(
                "NORN-FLOW-TAG",
                format!(
                    "catchTag(\"{tag}\") catches a tag no Function performed in its body declares and no Effect.fail there raises"
                ),
                at,
            );
        }
        self.collect(body);
        if let (Some(bind_name), Some(decl)) = (as_name, self.errors.get(tag).cloned()) {
            let schema = ValueSchema::object(Self::failure_record(&decl));
            self.register_binding(bind_name, schema, at);
        }
        self.collect(handler);
    }

    fn collect_perform(&mut self, perform: &PerformStep) {
        self.register_durable(&perform.step, perform.at.as_ref());
        if !self.performs.contains_key(&perform.step) {
            self.performs_order.push(perform.step.clone());
        }
        self.performs.insert(perform.step.clone(), perform.clone());
        self.register_function(&perform.func);
        if let Some(bind) = &perform.bind {
            self.register_binding(bind, perform.func.output.clone(), perform.at.as_ref());
        }
    }

    fn raises(steps: &[Step]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for step in steps {
            match step {
                Step::Perform(p) => {
                    for e in &p.func.errors {
                        out.insert(e.tag.clone());
                    }
                }
                Step::Retry { perform, .. } => {
                    for e in &perform.func.errors {
                        out.insert(e.tag.clone());
                    }
                }
                Step::Fail { error, .. } => {
                    out.insert(error.tag.clone());
                }
                Step::Route {
                    branches,
                    otherwise,
                    ..
                } => {
                    for b in branches {
                        out.extend(Self::raises(&b.then));
                    }
                    out.extend(Self::raises(otherwise));
                }
                Step::CatchTag {
                    body, tag, handler, ..
                } => {
                    for t in Self::raises(body) {
                        if &t != tag {
                            out.insert(t);
                        }
                    }
                    out.extend(Self::raises(handler));
                }
                Step::Compose { body, .. } => out.extend(Self::raises(body)),
                Step::All { steps, .. } => out.extend(Self::raises(steps)),
                Step::Sleep { .. } | Step::Succeed { .. } | Step::Value { .. } => {}
            }
        }
        out
    }

    fn register_durable(&mut self, id: &str, at: Option<&SourceLocation>) {
        if !is_snake_ident(id) {
            self.report(
                "NORN-FLOW-LITERAL",
                format!("step id {id:?} must match [a-z][a-z0-9_]*"),
                at,
            );
        }
        if self.steps_at.contains_key(id) {
            self.report(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                format!(
                    "step id \"{id}\" occurs more than once; durable step ids are stable and unique per flow"
                ),
                at,
            );
        } else {
            self.steps_order.push(id.to_owned());
        }
        self.steps_at.insert(id.to_owned(), at.cloned());
        if self.steps_at.len() > FLOW_MAX_STEPS {
            self.report(
                "NORN-FLOW-UNBOUNDED",
                format!("more than {FLOW_MAX_STEPS} durable steps"),
                at,
            );
        }
    }

    fn register_function(&mut self, handle: &FnRef) {
        if let Some(seen) = self.functions.get(&handle.id)
            && seen.version != handle.version
        {
            self.report(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                format!(
                    "Function {} is used at versions {} and {}",
                    handle.id, seen.version, handle.version
                ),
                handle.at.as_ref(),
            );
        }
        self.functions.insert(handle.id.clone(), handle.clone());
        for err in &handle.errors {
            self.register_error(err);
        }
    }

    fn register_error(&mut self, decl: &TaggedErrorDecl) {
        if !is_pascal_ident(&decl.tag) {
            self.report(
                "NORN-FLOW-LITERAL",
                format!("error tag {:?} must be PascalCase", decl.tag),
                decl.at.as_ref(),
            );
        }
        let state = snake_case_tag(&decl.tag);
        if let Some(owner) = self.tag_states.get(&state)
            && owner != &decl.tag
        {
            let mut pair = [owner.as_str(), decl.tag.as_str()];
            pair.sort_unstable();
            self.report(
                "NORN-FLOW-TAG",
                format!("tags {} both lower to failed_{state}", pair.join(" and ")),
                decl.at.as_ref(),
            );
        }
        self.tag_states.insert(state, decl.tag.clone());
        if let Some(seen) = self.errors.get(&decl.tag) {
            let a =
                compact_canonical_json(&ValueSchema::object(seen.fields.clone()).to_json()).ok();
            let b =
                compact_canonical_json(&ValueSchema::object(decl.fields.clone()).to_json()).ok();
            if a != b {
                self.report(
                    "NORN-FLOW-TAG",
                    format!(
                        "error tag {} is registered twice with different fields",
                        decl.tag
                    ),
                    decl.at.as_ref(),
                );
            }
        }
        if decl.fields.properties.contains_key("_tag") {
            self.report(
                "NORN-FLOW-TAG",
                format!("error {} declares a field named _tag", decl.tag),
                decl.at.as_ref(),
            );
        }
        self.errors.insert(decl.tag.clone(), decl.clone());
    }

    fn register_binding(&mut self, name: &str, schema: ValueSchema, at: Option<&SourceLocation>) {
        if !is_snake_ident(name) {
            self.report(
                "NORN-FLOW-LITERAL",
                format!("binding name {name:?} must match [a-z][a-z0-9_]*"),
                at,
            );
        }
        if self.binding_schemas.contains_key(name) {
            self.report(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                format!("binding \"{name}\" is declared twice; v1 bindings are unique per flow"),
                at,
            );
        } else {
            self.binding_order.push(name.to_owned());
        }
        self.binding_schemas.insert(name.to_owned(), schema);
    }

    fn check_policy(&mut self, policy: &RetryPolicy, at: Option<&SourceLocation>) {
        let Some(n) = policy.recurs else {
            self.report(
                "NORN-FLOW-UNBOUNDED",
                "the retry policy has no finite Schedule.recurs(n); an unbounded retry is never admitted",
                at,
            );
            return;
        };
        if !(1..=FLOW_MAX_RECURS).contains(&n) {
            self.report(
                "NORN-FLOW-UNBOUNDED",
                format!("Schedule.recurs({n}) must be an integer in [1, {FLOW_MAX_RECURS}]"),
                at,
            );
            return;
        }
        let max_f = 604_800_000.0_f64;
        for attempt in 1..=n {
            let delay = retry_delay(policy, attempt);
            let ok = delay.is_finite()
                && delay.fract().abs().to_bits() == 0
                && delay >= 0.0
                && delay <= max_f;
            if !ok {
                self.report(
                    "NORN-FLOW-UNBOUNDED",
                    format!(
                        "retry delay after attempt {attempt} is {delay} ms, outside [0, {FLOW_MAX_TIMER_MILLIS}]"
                    ),
                    at,
                );
                return;
            }
        }
    }

    pub(super) fn failure_record(decl: &TaggedErrorDecl) -> ObjectSchema {
        let mut ordered = Vec::with_capacity(decl.fields.properties.len().saturating_add(1));
        ordered.push(("_tag".to_owned(), ValueSchema::text()));
        let mut optional = Vec::new();
        for (k, v) in &decl.fields.properties {
            ordered.push((k.clone(), v.clone()));
            if !decl.fields.required.iter().any(|r| r == k) {
                optional.push(k.clone());
            }
        }
        ObjectSchema::record_ordered(ordered, &optional)
    }
}
