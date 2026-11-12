//! Laws of a page's declared interaction statechart (UI-MACHINE-1, #1236, #1247).
//!
//! A `WidgetPage.machine` is the only behaviour a renderer may execute: client state is a typed
//! field record changed only by a transition's `assign`, and every effect is a closed call, read,
//! cancel or navigation the server decides. These laws hold the frontend to the standard the
//! backend's lifecycles meet (`entity_model/lifecycles.rs`):
//!
//! - nothing is absorbing: every non-terminal state has an exit, and every state can reach an
//!   `idle` or `terminal` state (`absorbing_state`, `trap_region`); every state is reachable;
//! - every `pending` state names the one call or read it awaits, has a deadline, is entered only by
//!   a transition that starts that call, is left only by its outcome, and handles every outcome the
//!   server can produce for it with an unguarded transition (`pending_outcome_uncovered`);
//! - every `error` state has a recovery a person or a deadline drives;
//! - no cycle runs on timers alone (`input_free_cycle`): a loop needs an input;
//! - for one state and one event, at most one unguarded transition fires (`nondeterministic`);
//! - guards and assignments type-check against the field record and the event payload.
//!
//! What this establishes is **structural**, like the lifecycle checks: a guarded transition can be
//! disabled at run time, so coverage counts only unguarded transitions, and reachability treats
//! every guard as satisfiable (an over-approximation). Guarded reachability is a model-checking
//! question that goes to the obligations path, not here. The laws read the document only and add no
//! client-side authority.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use norn_expr::{EnvOptions, Expression, Type, ZenTypeEnv, check_expression};
use norn_ir::{
    ArtifactEnvelope, JsonValue, WidgetCallOutcome, WidgetMachine, WidgetMachineCall,
    WidgetMachineEffect, WidgetMachineEventSource, WidgetMachineField, WidgetMachineState,
    WidgetNode, WidgetPage, WidgetRefusalCategory, WidgetStateRole, WidgetUx, WidgetValueType,
};

use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

const DUPLICATE_ID: &str = "widget.machine.duplicate_id";
const UNKNOWN_REFERENCE: &str = "widget.machine.unknown_reference";
const INVALID_EVENT: &str = "widget.machine.invalid_event";
const INVALID_FIELD: &str = "widget.machine.invalid_field";
const INVALID_EXPRESSION: &str = "widget.machine.invalid_expression";
const INITIAL_NOT_IDLE: &str = "widget.machine.initial_not_idle";
const UNREACHABLE_STATE: &str = "widget.machine.unreachable_state";
const ABSORBING_STATE: &str = "widget.machine.absorbing_state";
const TRAP_REGION: &str = "widget.machine.trap_region";
const TERMINAL_HAS_TRANSITION: &str = "widget.machine.terminal_has_transition";
const PENDING_WITHOUT_AWAIT: &str = "widget.machine.pending_without_await";
const PENDING_WITHOUT_DEADLINE: &str = "widget.machine.pending_without_deadline";
const AWAIT_OUTSIDE_PENDING: &str = "widget.machine.await_outside_pending";
const OUTCOME_OUTSIDE_PENDING: &str = "widget.machine.outcome_outside_pending";
const OUTCOME_IMPOSSIBLE: &str = "widget.machine.outcome_impossible";
const PENDING_OUTCOME_UNCOVERED: &str = "widget.machine.pending_outcome_uncovered";
const PENDING_ABANDONED: &str = "widget.machine.pending_abandoned";
const START_WITHOUT_PENDING: &str = "widget.machine.start_without_pending";
const PENDING_ENTERED_WITHOUT_START: &str = "widget.machine.pending_entered_without_start";
const CANCEL_OUTSIDE_PENDING: &str = "widget.machine.cancel_outside_pending";
const ERROR_WITHOUT_RECOVERY: &str = "widget.machine.error_without_recovery";
const INPUT_FREE_CYCLE: &str = "widget.machine.input_free_cycle";
const NONDETERMINISTIC: &str = "widget.machine.nondeterministic";

/// Every code these laws can emit, for registry tests.
pub const CODES: [&str; 23] = [
    DUPLICATE_ID,
    UNKNOWN_REFERENCE,
    INVALID_EVENT,
    INVALID_FIELD,
    INVALID_EXPRESSION,
    INITIAL_NOT_IDLE,
    UNREACHABLE_STATE,
    ABSORBING_STATE,
    TRAP_REGION,
    TERMINAL_HAS_TRANSITION,
    PENDING_WITHOUT_AWAIT,
    PENDING_WITHOUT_DEADLINE,
    AWAIT_OUTSIDE_PENDING,
    OUTCOME_OUTSIDE_PENDING,
    OUTCOME_IMPOSSIBLE,
    PENDING_OUTCOME_UNCOVERED,
    PENDING_ABANDONED,
    START_WITHOUT_PENDING,
    PENDING_ENTERED_WITHOUT_START,
    CANCEL_OUTSIDE_PENDING,
    ERROR_WITHOUT_RECOVERY,
    INPUT_FREE_CYCLE,
    NONDETERMINISTIC,
];

const REFUSAL_CATEGORIES: [WidgetRefusalCategory; 7] = [
    WidgetRefusalCategory::Visibility,
    WidgetRefusalCategory::Permission,
    WidgetRefusalCategory::Precondition,
    WidgetRefusalCategory::Conflict,
    WidgetRefusalCategory::Invalid,
    WidgetRefusalCategory::Limit,
    WidgetRefusalCategory::Internal,
];

/// One concrete way an awaited call ends: an outcome, and for `refused` its category.
type OutcomeValue = (WidgetCallOutcome, Option<WidgetRefusalCategory>);

struct Finding {
    code: &'static str,
    pointer: String,
    message: String,
}

/// Check the declared machine of every page in one widget's UX contract.
#[must_use]
pub(crate) fn check(owner: &ArtifactEnvelope, ux: &WidgetUx) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for (index, page) in ux.pages.iter().enumerate() {
        if let Some(machine) = &page.machine {
            let base = format!("/payload/ux/pages/{index}/machine");
            findings.extend(check_machine(ux, page, machine, &base));
        }
    }
    findings
        .into_iter()
        .map(|finding| Diagnostic {
            code: DiagnosticCode::new(finding.code),
            severity: DiagnosticSeverity::Error,
            message: finding.message,
            span: Some(Span::pointer(finding.pointer)),
            artifact_id: Some(owner.artifact_id.clone()),
            remediation: Some(DiagnosticRemediation::FixDefinition),
        })
        .collect()
}

/// Every outcome value the server can produce for `call`. Approval gates only calls; only reads
/// can see their answer move. `cancelled` is possible only where the page offers a cancel.
fn possible_outcomes(call: &WidgetMachineCall, cancellable: bool) -> BTreeSet<OutcomeValue> {
    let mut values: BTreeSet<OutcomeValue> = [
        (WidgetCallOutcome::Accepted, None),
        (WidgetCallOutcome::NetworkFailure, None),
        (WidgetCallOutcome::Timeout, None),
    ]
    .into_iter()
    .collect();
    values.extend(
        REFUSAL_CATEGORIES
            .iter()
            .map(|category| (WidgetCallOutcome::Refused, Some(*category))),
    );
    match call {
        WidgetMachineCall::Action { .. } | WidgetMachineCall::Operation { .. } => {
            values.insert((WidgetCallOutcome::PendingApproval, None));
        }
        WidgetMachineCall::Read { .. } => {
            values.insert((WidgetCallOutcome::AnswerMoved, None));
        }
    }
    if cancellable {
        values.insert((WidgetCallOutcome::Cancelled, None));
    }
    values
}

/// The outcome values one `outcome` event source matches.
fn matched_outcomes(
    outcome: WidgetCallOutcome,
    category: Option<WidgetRefusalCategory>,
) -> Vec<OutcomeValue> {
    match (outcome, category) {
        (WidgetCallOutcome::Refused, None) => REFUSAL_CATEGORIES
            .iter()
            .map(|category| (WidgetCallOutcome::Refused, Some(*category)))
            .collect(),
        (WidgetCallOutcome::Refused, Some(category)) => {
            vec![(WidgetCallOutcome::Refused, Some(category))]
        }
        (outcome, _) => vec![(outcome, None)],
    }
}

fn outcome_name(outcome: WidgetCallOutcome) -> &'static str {
    match outcome {
        WidgetCallOutcome::Accepted => "accepted",
        WidgetCallOutcome::Refused => "refused",
        WidgetCallOutcome::PendingApproval => "pending_approval",
        WidgetCallOutcome::AnswerMoved => "answer_moved",
        WidgetCallOutcome::NetworkFailure => "network_failure",
        WidgetCallOutcome::Timeout => "timeout",
        WidgetCallOutcome::Cancelled => "cancelled",
    }
}

fn category_name(category: WidgetRefusalCategory) -> &'static str {
    match category {
        WidgetRefusalCategory::Visibility => "visibility",
        WidgetRefusalCategory::Permission => "permission",
        WidgetRefusalCategory::Precondition => "precondition",
        WidgetRefusalCategory::Conflict => "conflict",
        WidgetRefusalCategory::Invalid => "invalid",
        WidgetRefusalCategory::Limit => "limit",
        WidgetRefusalCategory::Internal => "internal",
    }
}

fn show_outcome((outcome, category): &OutcomeValue) -> String {
    match category {
        Some(category) => format!("{}:{}", outcome_name(*outcome), category_name(*category)),
        None => outcome_name(*outcome).to_owned(),
    }
}

/// The fixed payload an outcome event carries.
fn outcome_payload(outcome: WidgetCallOutcome) -> BTreeMap<String, Type> {
    let text = |name: &str| (name.to_owned(), Type::text());
    match outcome {
        WidgetCallOutcome::Refused => {
            BTreeMap::from([text("code"), ("retryable".to_owned(), Type::Bool)])
        }
        WidgetCallOutcome::PendingApproval => BTreeMap::from([text("decisionId")]),
        WidgetCallOutcome::AnswerMoved => BTreeMap::from([text("code")]),
        WidgetCallOutcome::Accepted
        | WidgetCallOutcome::NetworkFailure
        | WidgetCallOutcome::Timeout
        | WidgetCallOutcome::Cancelled => BTreeMap::new(),
    }
}

fn field_type(field: &WidgetMachineField) -> Type {
    crate::widget_ux::value_type(field.value_type, field.required)
}

fn initial_fits(value: &JsonValue, value_type: WidgetValueType, required: bool) -> bool {
    match (value, value_type) {
        (JsonValue::Null, _) => !required,
        (_, WidgetValueType::Json) => true,
        (JsonValue::Bool(_), WidgetValueType::Boolean) => true,
        (JsonValue::Number(number), WidgetValueType::Integer) => number.fract() == 0.0,
        (JsonValue::Number(_), WidgetValueType::Number) => true,
        (
            JsonValue::String(_),
            WidgetValueType::Text
            | WidgetValueType::Date
            | WidgetValueType::Timestamp
            | WidgetValueType::Duration,
        ) => true,
        _ => false,
    }
}

fn collect_nodes<'n>(node: &'n WidgetNode, out: &mut BTreeSet<&'n str>) {
    out.insert(node.node_id.as_str());
    for child in &node.children {
        collect_nodes(child, out);
    }
}

fn reachable(seeds: impl IntoIterator<Item = usize>, edges: &[BTreeSet<usize>]) -> Vec<bool> {
    let mut seen = vec![false; edges.len()];
    let mut queue: VecDeque<usize> = VecDeque::new();
    for seed in seeds {
        if !seen[seed] {
            seen[seed] = true;
            queue.push_back(seed);
        }
    }
    while let Some(state) = queue.pop_front() {
        for next in &edges[state] {
            if !seen[*next] {
                seen[*next] = true;
                queue.push_back(*next);
            }
        }
    }
    seen
}

/// States that lie on a cycle of `edges` (Tarjan's strongly connected components; a singleton
/// counts only with a self-loop).
fn on_cycle(edges: &[BTreeSet<usize>]) -> BTreeSet<usize> {
    struct Tarjan<'e> {
        edges: &'e [BTreeSet<usize>],
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        stack: Vec<usize>,
        on_stack: Vec<bool>,
        next: usize,
        cyclic: BTreeSet<usize>,
    }
    impl Tarjan<'_> {
        fn visit(&mut self, state: usize) {
            self.index[state] = Some(self.next);
            self.low[state] = self.next;
            self.next += 1;
            self.stack.push(state);
            self.on_stack[state] = true;
            for &next in self.edges[state].iter() {
                match self.index[next] {
                    None => {
                        self.visit(next);
                        self.low[state] = self.low[state].min(self.low[next]);
                    }
                    Some(index) if self.on_stack[next] => {
                        self.low[state] = self.low[state].min(index);
                    }
                    Some(_) => {}
                }
            }
            if Some(self.low[state]) == self.index[state] {
                let mut component = Vec::new();
                while let Some(member) = self.stack.pop() {
                    self.on_stack[member] = false;
                    component.push(member);
                    if member == state {
                        break;
                    }
                }
                if component.len() > 1 || self.edges[state].contains(&state) {
                    self.cyclic.extend(component);
                }
            }
        }
    }
    let mut tarjan = Tarjan {
        edges,
        index: vec![None; edges.len()],
        low: vec![0; edges.len()],
        stack: Vec::new(),
        on_stack: vec![false; edges.len()],
        next: 0,
        cyclic: BTreeSet::new(),
    };
    for state in 0..edges.len() {
        if tarjan.index[state].is_none() {
            tarjan.visit(state);
        }
    }
    tarjan.cyclic
}

struct MachineChecker<'m> {
    ux: &'m WidgetUx,
    machine: &'m WidgetMachine,
    base: &'m str,
    findings: Vec<Finding>,
}

impl MachineChecker<'_> {
    fn push(&mut self, code: &'static str, pointer: String, message: String) {
        self.findings.push(Finding {
            code,
            pointer,
            message,
        });
    }

    fn unique<'i>(
        &mut self,
        ids: impl IntoIterator<Item = &'i str>,
        list: &str,
        member: &str,
    ) -> BTreeMap<&'i str, usize> {
        let mut seen = BTreeMap::new();
        for (index, id) in ids.into_iter().enumerate() {
            if seen.contains_key(id) {
                self.push(
                    DUPLICATE_ID,
                    format!("{}/{list}/{index}/{member}", self.base),
                    format!("{list} id `{id}` is declared twice"),
                );
            } else {
                seen.insert(id, index);
            }
        }
        seen
    }

    fn check_call(&mut self, call: &WidgetMachineCall, pointer: &str) {
        let known = match call {
            WidgetMachineCall::Action { binding_id } => self
                .ux
                .action_bindings
                .iter()
                .any(|binding| &binding.binding_id == binding_id),
            WidgetMachineCall::Read { binding_id } => self
                .ux
                .data_bindings
                .iter()
                .any(|binding| &binding.binding_id == binding_id),
            WidgetMachineCall::Operation { operation_id } => {
                !operation_id.is_empty()
                    && operation_id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            }
        };
        if !known {
            self.push(
                UNKNOWN_REFERENCE,
                pointer.to_owned(),
                format!("{call:?} names no declared binding or valid operation id"),
            );
        }
    }

    fn check_fields(&mut self, fields: &[WidgetMachineField], list: &str, payload: bool) {
        for (index, field) in fields.iter().enumerate() {
            let pointer = format!("{}/{list}/{index}", self.base);
            match (&field.initial, payload) {
                (Some(_), true) => self.push(
                    INVALID_FIELD,
                    format!("{pointer}/initial"),
                    format!(
                        "payload member `{}` cannot carry an initial value",
                        field.field_id
                    ),
                ),
                (None, false) if field.required => self.push(
                    INVALID_FIELD,
                    pointer,
                    format!("required field `{}` needs an initial value", field.field_id),
                ),
                (Some(value), false) if !initial_fits(value, field.value_type, field.required) => {
                    self.push(
                        INVALID_FIELD,
                        format!("{pointer}/initial"),
                        format!(
                            "initial value of `{}` does not fit its declared type",
                            field.field_id
                        ),
                    );
                }
                _ => {}
            }
        }
    }

    fn check_expression(
        &mut self,
        source: &str,
        fields: &BTreeMap<String, Type>,
        event: &BTreeMap<String, Type>,
        expected: &Type,
        pointer: String,
    ) {
        let env = ZenTypeEnv::input_only_with_records(
            BTreeMap::from([
                ("machine.fields".to_owned(), fields.clone()),
                ("machine.event".to_owned(), event.clone()),
            ]),
            BTreeMap::from([
                (
                    "fields".to_owned(),
                    Type::Record("machine.fields".to_owned()),
                ),
                ("event".to_owned(), Type::Record("machine.event".to_owned())),
            ]),
            EnvOptions::default(),
        );
        let env = match env {
            Ok(env) => env,
            Err(error) => {
                self.push(
                    INVALID_EXPRESSION,
                    pointer,
                    format!("expression environment is invalid: {error:?}"),
                );
                return;
            }
        };
        let expression = match Expression::compile(source) {
            Ok(expression) => expression,
            Err(error) => {
                self.push(
                    INVALID_EXPRESSION,
                    pointer,
                    format!("expression does not compile: {error}"),
                );
                return;
            }
        };
        if let Err(errors) = check_expression(&expression, &env, expected) {
            for error in errors {
                self.push(
                    INVALID_EXPRESSION,
                    pointer.clone(),
                    format!("expression does not type-check: {}", error.message),
                );
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One pass over the declared machine; each law is a short, commented block."
    )]
    fn run(mut self, page: &WidgetPage) -> Vec<Finding> {
        let machine = self.machine;
        let base = self.base;
        self.unique(
            machine.fields.iter().map(|f| f.field_id.as_str()),
            "fields",
            "fieldId",
        );
        let events = self.unique(
            machine.events.iter().map(|e| e.event_id.as_str()),
            "events",
            "eventId",
        );
        let states = self.unique(
            machine.states.iter().map(|s| s.state_id.as_str()),
            "states",
            "stateId",
        );
        self.unique(
            machine.transitions.iter().map(|t| t.transition_id.as_str()),
            "transitions",
            "transitionId",
        );
        self.check_fields(&machine.fields, "fields", false);
        let field_types: BTreeMap<String, Type> = machine
            .fields
            .iter()
            .map(|field| (field.field_id.clone(), field_type(field)))
            .collect();

        // Initial state: known and idle.
        match states.get(machine.initial_state_id.as_str()) {
            None => self.push(
                UNKNOWN_REFERENCE,
                format!("{base}/initialStateId"),
                format!(
                    "initial state `{}` is not declared",
                    machine.initial_state_id
                ),
            ),
            Some(index) if machine.states[*index].role != WidgetStateRole::Idle => self.push(
                INITIAL_NOT_IDLE,
                format!("{base}/initialStateId"),
                format!(
                    "initial state `{}` must have the idle role",
                    machine.initial_state_id
                ),
            ),
            Some(_) => {}
        }

        // Events: sources name declared things; payloads only where the source carries one.
        let mut nodes = BTreeSet::new();
        collect_nodes(&page.root, &mut nodes);
        let mut event_payloads: BTreeMap<&str, BTreeMap<String, Type>> = BTreeMap::new();
        for (index, event) in machine.events.iter().enumerate() {
            let pointer = format!("{base}/events/{index}");
            let carries_payload = match &event.source {
                WidgetMachineEventSource::User { node_id } => {
                    if !nodes.contains(node_id.as_str()) {
                        self.push(
                            UNKNOWN_REFERENCE,
                            format!("{pointer}/source/nodeId"),
                            format!("node `{node_id}` is not in this page's tree"),
                        );
                    }
                    true
                }
                WidgetMachineEventSource::Subscription { binding_id } => {
                    if !self
                        .ux
                        .data_bindings
                        .iter()
                        .any(|binding| &binding.binding_id == binding_id)
                    {
                        self.push(
                            UNKNOWN_REFERENCE,
                            format!("{pointer}/source/bindingId"),
                            format!("data binding `{binding_id}` is not declared"),
                        );
                    }
                    true
                }
                WidgetMachineEventSource::Timer { after_ms } => {
                    if after_ms.0 == 0 {
                        self.push(
                            INVALID_EVENT,
                            format!("{pointer}/source/afterMs"),
                            "a timer must wait a positive number of milliseconds".to_owned(),
                        );
                    }
                    false
                }
                WidgetMachineEventSource::Outcome { outcome, category } => {
                    if category.is_some() && *outcome != WidgetCallOutcome::Refused {
                        self.push(
                            INVALID_EVENT,
                            format!("{pointer}/source/category"),
                            "only a refused outcome has a category".to_owned(),
                        );
                    }
                    false
                }
                WidgetMachineEventSource::PageOpened => false,
            };
            if !carries_payload && !event.payload.is_empty() {
                self.push(
                    INVALID_EVENT,
                    format!("{pointer}/payload"),
                    "only user and subscription events declare a payload".to_owned(),
                );
            }
            self.check_fields(&event.payload, &format!("events/{index}/payload"), true);
            let payload = match &event.source {
                WidgetMachineEventSource::Outcome { outcome, .. } => outcome_payload(*outcome),
                _ => event
                    .payload
                    .iter()
                    .map(|member| (member.field_id.clone(), field_type(member)))
                    .collect(),
            };
            event_payloads.insert(event.event_id.as_str(), payload);
        }

        // States: only pending states await, and each one awaits one call under a deadline.
        for (index, state) in machine.states.iter().enumerate() {
            let pointer = format!("{base}/states/{index}");
            if state.role == WidgetStateRole::Pending {
                match &state.awaits {
                    None => self.push(
                        PENDING_WITHOUT_AWAIT,
                        pointer.clone(),
                        format!(
                            "pending state `{}` does not name the call it awaits",
                            state.state_id
                        ),
                    ),
                    Some(call) => self.check_call(call, &format!("{pointer}/awaits")),
                }
                if state.deadline_ms.is_none_or(|deadline| deadline.0 == 0) {
                    self.push(
                        PENDING_WITHOUT_DEADLINE,
                        pointer,
                        format!(
                            "pending state `{}` needs a positive deadline",
                            state.state_id
                        ),
                    );
                }
            } else if state.awaits.is_some() || state.deadline_ms.is_some() {
                self.push(
                    AWAIT_OUTSIDE_PENDING,
                    pointer,
                    format!(
                        "only a pending state awaits a call; `{}` is not pending",
                        state.state_id
                    ),
                );
            }
        }

        // Transitions.
        let count = machine.states.len();
        let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); count];
        let mut timer_edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); count];
        let mut recovers: BTreeSet<usize> = BTreeSet::new();
        let mut cancellable: BTreeSet<usize> = BTreeSet::new();
        // (state, event) and (state, outcome value) -> unguarded transition indexes.
        let mut unguarded_events: BTreeMap<(usize, &str), Vec<usize>> = BTreeMap::new();
        let mut covered: BTreeMap<usize, BTreeMap<OutcomeValue, Vec<usize>>> = BTreeMap::new();
        for (index, transition) in machine.transitions.iter().enumerate() {
            let pointer = format!("{base}/transitions/{index}");
            let from = states.get(transition.from.as_str()).copied();
            let target = states.get(transition.target.as_str()).copied();
            let event = events
                .get(transition.on.as_str())
                .map(|event| &machine.events[*event]);
            for (member, id, known) in [
                ("from", &transition.from, from.is_some()),
                ("target", &transition.target, target.is_some()),
                ("on", &transition.on, event.is_some()),
            ] {
                if !known {
                    self.push(
                        UNKNOWN_REFERENCE,
                        format!("{pointer}/{member}"),
                        format!("`{id}` is not declared"),
                    );
                }
            }
            let empty = BTreeMap::new();
            let payload = event_payloads
                .get(transition.on.as_str())
                .unwrap_or(&empty)
                .clone();
            if let Some(guard) = &transition.guard {
                self.check_expression(
                    guard.as_str(),
                    &field_types,
                    &payload,
                    &Type::Bool,
                    format!("{pointer}/guard"),
                );
            }
            for (field, expression) in &transition.assign {
                match field_types.get(field) {
                    None => self.push(
                        UNKNOWN_REFERENCE,
                        format!("{pointer}/assign/{field}"),
                        format!("field `{field}` is not declared"),
                    ),
                    Some(expected) => self.check_expression(
                        expression.as_str(),
                        &field_types,
                        &payload,
                        expected,
                        format!("{pointer}/assign/{field}"),
                    ),
                }
            }
            for (effect_index, effect) in transition.effects.iter().enumerate() {
                match effect {
                    WidgetMachineEffect::Start { call } => {
                        self.check_call(call, &format!("{pointer}/effects/{effect_index}/call"))
                    }
                    WidgetMachineEffect::Navigate { item_id } => {
                        if !self
                            .ux
                            .navigation_items
                            .iter()
                            .any(|item| &item.item_id == item_id)
                        {
                            self.push(
                                UNKNOWN_REFERENCE,
                                format!("{pointer}/effects/{effect_index}/itemId"),
                                format!("navigation item `{item_id}` is not declared"),
                            );
                        }
                    }
                    WidgetMachineEffect::Cancel => {}
                }
            }
            let (Some(from), Some(target), Some(event)) = (from, target, event) else {
                continue;
            };
            let from_state: &WidgetMachineState = &machine.states[from];
            let target_state: &WidgetMachineState = &machine.states[target];
            if from_state.role == WidgetStateRole::Terminal {
                self.push(
                    TERMINAL_HAS_TRANSITION,
                    format!("{pointer}/from"),
                    format!("terminal state `{}` cannot be left", from_state.state_id),
                );
                continue;
            }
            edges[from].insert(target);
            let outcome = match &event.source {
                WidgetMachineEventSource::Outcome { outcome, category } => {
                    Some((*outcome, *category))
                }
                _ => None,
            };
            match &event.source {
                WidgetMachineEventSource::Timer { .. } => {
                    timer_edges[from].insert(target);
                    recovers.insert(from);
                }
                WidgetMachineEventSource::User { .. } => {
                    recovers.insert(from);
                }
                WidgetMachineEventSource::PageOpened
                    if transition.from != machine.initial_state_id =>
                {
                    self.push(
                        INVALID_EVENT,
                        format!("{pointer}/on"),
                        "page_opened fires once, in the initial state".to_owned(),
                    );
                }
                _ => {}
            }

            // Outcome events belong to pending states, and only to outcomes their call can have.
            if let Some((outcome, category)) = outcome {
                match (&from_state.role, &from_state.awaits) {
                    (WidgetStateRole::Pending, Some(call)) => {
                        let possible = possible_outcomes(call, true);
                        if matched_outcomes(outcome, category)
                            .iter()
                            .all(|value| !possible.contains(value))
                        {
                            self.push(
                                OUTCOME_IMPOSSIBLE,
                                format!("{pointer}/on"),
                                format!(
                                    "the call `{}` awaits cannot end `{}`",
                                    from_state.state_id,
                                    outcome_name(outcome)
                                ),
                            );
                        } else if transition.guard.is_none() {
                            let slot = covered.entry(from).or_default();
                            for value in matched_outcomes(outcome, category) {
                                slot.entry(value).or_default().push(index);
                            }
                        }
                    }
                    (WidgetStateRole::Pending, None) => {}
                    _ => self.push(
                        OUTCOME_OUTSIDE_PENDING,
                        format!("{pointer}/on"),
                        format!(
                            "outcome event `{}` can only fire in a pending state; `{}` is not pending",
                            event.event_id, from_state.state_id
                        ),
                    ),
                }
            } else if transition.guard.is_none() {
                unguarded_events
                    .entry((from, transition.on.as_str()))
                    .or_default()
                    .push(index);
            }

            // A call in flight is left only by its outcome.
            if from_state.role == WidgetStateRole::Pending && outcome.is_none() && from != target {
                self.push(
                    PENDING_ABANDONED,
                    pointer.clone(),
                    format!(
                        "leaving pending state `{}` on `{}` abandons the call in flight; cancel it and handle the outcome",
                        from_state.state_id, event.event_id
                    ),
                );
            }

            // Calls start exactly when a pending state is entered, and only the call it awaits.
            let starts: Vec<&WidgetMachineCall> = transition
                .effects
                .iter()
                .filter_map(|effect| match effect {
                    WidgetMachineEffect::Start { call } => Some(call),
                    _ => None,
                })
                .collect();
            let enters_pending = target_state.role == WidgetStateRole::Pending
                && (from != target || outcome.is_some());
            if enters_pending {
                let awaited = target_state.awaits.as_ref();
                if starts.len() != 1 || awaited.is_none_or(|call| starts[0] != call) {
                    self.push(
                        PENDING_ENTERED_WITHOUT_START,
                        format!("{pointer}/effects"),
                        format!(
                            "entering pending state `{}` must start exactly the call it awaits",
                            target_state.state_id
                        ),
                    );
                }
            } else if !starts.is_empty() {
                self.push(
                    START_WITHOUT_PENDING,
                    format!("{pointer}/effects"),
                    if target_state.role == WidgetStateRole::Pending {
                        format!(
                            "pending state `{}` already has its call in flight",
                            target_state.state_id
                        )
                    } else {
                        format!(
                            "a started call must be awaited, but `{}` is not pending",
                            target_state.state_id
                        )
                    },
                );
            }
            if transition
                .effects
                .iter()
                .any(|effect| matches!(effect, WidgetMachineEffect::Cancel))
            {
                if from_state.role == WidgetStateRole::Pending
                    && from == target
                    && outcome.is_none()
                {
                    cancellable.insert(from);
                } else {
                    self.push(
                        CANCEL_OUTSIDE_PENDING,
                        format!("{pointer}/effects"),
                        "cancel requests the end of the call in flight: it fires in a pending state, stays there, and the outcome follows".to_owned(),
                    );
                }
            }
        }

        // Determinism: one unguarded transition per state and event or outcome value.
        for ((from, event), indexes) in &unguarded_events {
            if indexes.len() > 1 {
                self.push(
                    NONDETERMINISTIC,
                    format!("{base}/transitions/{}", indexes[1]),
                    format!(
                        "state `{}` has {} unguarded transitions on `{event}`",
                        machine.states[*from].state_id,
                        indexes.len()
                    ),
                );
            }
        }
        for (from, values) in &covered {
            for (value, indexes) in values {
                if indexes.len() > 1 {
                    self.push(
                        NONDETERMINISTIC,
                        format!("{base}/transitions/{}", indexes[1]),
                        format!(
                            "state `{}` has {} unguarded transitions on outcome `{}`",
                            machine.states[*from].state_id,
                            indexes.len(),
                            show_outcome(value)
                        ),
                    );
                }
            }
        }

        // Coverage: every outcome the awaited call can have has an unguarded exit.
        for (index, state) in machine.states.iter().enumerate() {
            let (WidgetStateRole::Pending, Some(call)) = (state.role, &state.awaits) else {
                continue;
            };
            let have = covered.get(&index);
            let missing: Vec<String> = possible_outcomes(call, cancellable.contains(&index))
                .iter()
                .filter(|value| have.is_none_or(|have| !have.contains_key(*value)))
                .map(show_outcome)
                .collect();
            if !missing.is_empty() {
                self.push(
                    PENDING_OUTCOME_UNCOVERED,
                    format!("{base}/states/{index}"),
                    format!(
                        "pending state `{}` has no unguarded transition for: {}",
                        state.state_id,
                        missing.join(", ")
                    ),
                );
            }
        }

        // Graph laws.
        if let Some(&initial) = states.get(machine.initial_state_id.as_str()) {
            let seen = reachable([initial], &edges);
            for (index, state) in machine.states.iter().enumerate() {
                if !seen[index] && states.get(state.state_id.as_str()) == Some(&index) {
                    self.push(
                        UNREACHABLE_STATE,
                        format!("{base}/states/{index}"),
                        format!(
                            "state `{}` is unreachable from the initial state",
                            state.state_id
                        ),
                    );
                }
            }
        }
        let mut reverse: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); count];
        for (from, targets) in edges.iter().enumerate() {
            for target in targets {
                reverse[*target].insert(from);
            }
        }
        let settled = reachable(
            machine
                .states
                .iter()
                .enumerate()
                .filter(|(_, state)| {
                    matches!(
                        state.role,
                        WidgetStateRole::Idle | WidgetStateRole::Terminal
                    )
                })
                .map(|(index, _)| index),
            &reverse,
        );
        for (index, state) in machine.states.iter().enumerate() {
            if state.role == WidgetStateRole::Terminal {
                continue;
            }
            let pointer = format!("{base}/states/{index}");
            if edges[index].iter().all(|target| *target == index) {
                self.push(
                    ABSORBING_STATE,
                    pointer,
                    format!("non-terminal state `{}` has no exit", state.state_id),
                );
            } else if !settled[index] {
                self.push(
                    TRAP_REGION,
                    pointer,
                    format!(
                        "no idle or terminal state is reachable from `{}`",
                        state.state_id
                    ),
                );
            } else if state.role == WidgetStateRole::Error && !recovers.contains(&index) {
                self.push(
                    ERROR_WITHOUT_RECOVERY,
                    pointer,
                    format!(
                        "error state `{}` has no transition a person or a timer drives",
                        state.state_id
                    ),
                );
            }
        }
        for index in on_cycle(&timer_edges) {
            self.push(
                INPUT_FREE_CYCLE,
                format!("{base}/states/{index}"),
                format!(
                    "state `{}` is on a cycle that runs on timers alone",
                    machine.states[index].state_id
                ),
            );
        }
        self.findings
    }
}

fn check_machine(
    ux: &WidgetUx,
    page: &WidgetPage,
    machine: &WidgetMachine,
    base: &str,
) -> Vec<Finding> {
    MachineChecker {
        ux,
        machine,
        base,
        findings: Vec::new(),
    }
    .run(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cycle_needs_two_states_or_a_self_loop() {
        let edges = vec![
            BTreeSet::from([1]),
            BTreeSet::from([0]),
            BTreeSet::from([2]),
            BTreeSet::from([0]),
        ];
        assert_eq!(on_cycle(&edges), BTreeSet::from([0, 1, 2]));
        assert!(on_cycle(&[BTreeSet::from([1]), BTreeSet::new()]).is_empty());
    }

    #[test]
    fn refused_without_category_matches_every_category() {
        assert_eq!(matched_outcomes(WidgetCallOutcome::Refused, None).len(), 7);
        let read = WidgetMachineCall::Read {
            binding_id: "b".into(),
        };
        let possible = possible_outcomes(&read, false);
        assert!(possible.contains(&(WidgetCallOutcome::AnswerMoved, None)));
        assert!(!possible.contains(&(WidgetCallOutcome::PendingApproval, None)));
        assert!(!possible.contains(&(WidgetCallOutcome::Cancelled, None)));
    }
}
