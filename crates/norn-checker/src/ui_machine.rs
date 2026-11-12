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
//! - guards, assignments and request members type-check against the field record and the event
//!   payload.
//!
//! What this establishes is **structural**, like the lifecycle checks: a guarded transition can be
//! disabled at run time, so coverage counts only unguarded transitions, and reachability treats
//! every guard as satisfiable (an over-approximation). Guarded reachability is a model-checking
//! question for the obligations path, not this pass. The laws read the document only and add no
//! client-side authority.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use norn_expr::{EnvOptions, Expression, Type, ZenTypeEnv, check_expression};
use norn_ir::{
    ArtifactEnvelope, JsonValue, RuntimeValue, WidgetCallOutcome, WidgetMachine, WidgetMachineCall,
    WidgetMachineEffect, WidgetMachineEvent, WidgetMachineEventSource, WidgetMachineField,
    WidgetMachineState, WidgetNode, WidgetPage, WidgetRefusalCategory, WidgetStateRole, WidgetUx,
    WidgetValueType,
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

/// Every code these laws can emit.
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

/// State graph keyed by state id.
type Graph<'m> = BTreeMap<&'m str, BTreeSet<&'m str>>;

/// The field record and the event payload an expression sees.
type Env<'e> = (&'e BTreeMap<String, Type>, &'e BTreeMap<String, Type>);

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
            let checker = MachineChecker {
                ux,
                machine,
                base: &base,
                findings: Vec::new(),
            };
            findings.extend(checker.run(page));
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
        (outcome, category) => vec![(
            outcome,
            category.filter(|_| outcome == WidgetCallOutcome::Refused),
        )],
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
    match outcome {
        WidgetCallOutcome::Refused => BTreeMap::from([
            ("code".to_owned(), Type::text()),
            ("retryable".to_owned(), Type::Bool),
        ]),
        WidgetCallOutcome::PendingApproval => {
            BTreeMap::from([("decisionId".to_owned(), Type::text())])
        }
        WidgetCallOutcome::AnswerMoved => BTreeMap::from([("code".to_owned(), Type::text())]),
        WidgetCallOutcome::Accepted
        | WidgetCallOutcome::NetworkFailure
        | WidgetCallOutcome::Timeout
        | WidgetCallOutcome::Cancelled => BTreeMap::new(),
    }
}

/// The payload record an event's guards and assignments see.
fn event_payload(event: &WidgetMachineEvent) -> BTreeMap<String, Type> {
    match &event.source {
        WidgetMachineEventSource::Outcome { outcome, .. } => outcome_payload(*outcome),
        WidgetMachineEventSource::PageOpened
        | WidgetMachineEventSource::User { .. }
        | WidgetMachineEventSource::Subscription { .. }
        | WidgetMachineEventSource::Timer { .. } => event
            .payload
            .iter()
            .map(|member| (member.field_id.clone(), field_type(member)))
            .collect(),
    }
}

fn outcome_of(event: &WidgetMachineEvent) -> Option<OutcomeValue> {
    match &event.source {
        WidgetMachineEventSource::Outcome { outcome, category } => Some((*outcome, *category)),
        WidgetMachineEventSource::PageOpened
        | WidgetMachineEventSource::User { .. }
        | WidgetMachineEventSource::Subscription { .. }
        | WidgetMachineEventSource::Timer { .. } => None,
    }
}

fn field_type(field: &WidgetMachineField) -> Type {
    crate::widget_ux::value_type(field.value_type, field.required)
}

fn initial_fits(value: &JsonValue, value_type: WidgetValueType, required: bool) -> bool {
    match (value, value_type) {
        (JsonValue::Null, _) => !required,
        (_, WidgetValueType::Json)
        | (JsonValue::Bool(_), WidgetValueType::Boolean)
        | (JsonValue::Number(_), WidgetValueType::Number)
        | (
            JsonValue::String(_),
            WidgetValueType::Text
            | WidgetValueType::Date
            | WidgetValueType::Timestamp
            | WidgetValueType::Duration,
        ) => true,
        (JsonValue::Number(_), WidgetValueType::Integer) => matches!(
            RuntimeValue::from_ir(value),
            Ok(RuntimeValue::Number(number)) if number.is_integer()
        ),
        _ => false,
    }
}

fn collect_nodes<'n>(node: &'n WidgetNode, out: &mut BTreeSet<&'n str>) {
    out.insert(node.node_id.as_str());
    for child in &node.children {
        collect_nodes(child, out);
    }
}

/// Every state reachable from `seeds` along `graph`, the seeds included.
fn reachable<'m>(seeds: impl IntoIterator<Item = &'m str>, graph: &Graph<'m>) -> BTreeSet<&'m str> {
    let mut seen: BTreeSet<&'m str> = BTreeSet::new();
    let mut queue: VecDeque<&'m str> = VecDeque::new();
    for seed in seeds {
        if seen.insert(seed) {
            queue.push_back(seed);
        }
    }
    while let Some(state) = queue.pop_front() {
        for next in graph.get(state).into_iter().flatten() {
            if seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    seen
}

/// States that lie on a cycle of `graph`: a state is on one when its successors reach it again.
fn on_cycle<'m>(graph: &Graph<'m>) -> BTreeSet<&'m str> {
    graph
        .iter()
        .filter(|(state, successors)| {
            reachable(successors.iter().copied(), graph).contains(**state)
        })
        .map(|(state, _)| *state)
        .collect()
}

fn reverse<'m>(graph: &Graph<'m>) -> Graph<'m> {
    let mut reversed: Graph<'m> = BTreeMap::new();
    for (from, targets) in graph {
        for target in targets {
            reversed.entry(target).or_default().insert(from);
        }
    }
    reversed
}

fn machine_env(env: Env<'_>) -> Result<ZenTypeEnv, String> {
    ZenTypeEnv::input_only_with_records(
        BTreeMap::from([
            ("machine.fields".to_owned(), env.0.clone()),
            ("machine.event".to_owned(), env.1.clone()),
        ]),
        BTreeMap::from([
            (
                "fields".to_owned(),
                Type::Record("machine.fields".to_owned()),
            ),
            ("event".to_owned(), Type::Record("machine.event".to_owned())),
        ]),
        EnvOptions::default(),
    )
    .map_err(|error| format!("{error:?}"))
}

/// Per-transition facts gathered for the graph laws.
#[derive(Default)]
struct Facts<'m> {
    edges: Graph<'m>,
    timer_edges: Graph<'m>,
    /// States with an exit a person or a timer drives.
    recovers: BTreeSet<&'m str>,
    /// Pending states that offer a cancel.
    cancellable: BTreeSet<&'m str>,
    /// `(state, event)` to unguarded transition indexes, for non-outcome events.
    unguarded: BTreeMap<(&'m str, &'m str), Vec<usize>>,
    /// `state` to outcome value to unguarded transition indexes.
    covered: BTreeMap<&'m str, BTreeMap<OutcomeValue, Vec<usize>>>,
}

struct MachineChecker<'m> {
    ux: &'m WidgetUx,
    machine: &'m WidgetMachine,
    base: &'m str,
    findings: Vec<Finding>,
}

impl<'m> MachineChecker<'m> {
    fn push(&mut self, code: &'static str, pointer: String, message: String) {
        self.findings.push(Finding {
            code,
            pointer,
            message,
        });
    }

    fn unique<'i>(&mut self, ids: impl IntoIterator<Item = &'i str>, list: &str, member: &str) {
        let mut seen = BTreeSet::new();
        for (index, id) in ids.into_iter().enumerate() {
            if !seen.insert(id) {
                self.push(
                    DUPLICATE_ID,
                    format!("{}/{list}/{index}/{member}", self.base),
                    format!("{list} id `{id}` is declared twice"),
                );
            }
        }
    }

    fn state(&self, id: &str) -> Option<&'m WidgetMachineState> {
        self.machine
            .states
            .iter()
            .find(|state| state.state_id == id)
    }

    fn event(&self, id: &str) -> Option<&'m WidgetMachineEvent> {
        self.machine
            .events
            .iter()
            .find(|event| event.event_id == id)
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
                (Some(_) | None, false) | (None, true) => {}
            }
        }
    }

    fn check_expression(&mut self, source: &str, env: Env<'_>, expected: &Type, pointer: String) {
        let env = match machine_env(env) {
            Ok(env) => env,
            Err(error) => {
                self.push(
                    INVALID_EXPRESSION,
                    pointer,
                    format!("expression environment is invalid: {error}"),
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

    fn run(mut self, page: &WidgetPage) -> Vec<Finding> {
        let machine = self.machine;
        self.unique(
            machine.fields.iter().map(|f| f.field_id.as_str()),
            "fields",
            "fieldId",
        );
        self.unique(
            machine.events.iter().map(|e| e.event_id.as_str()),
            "events",
            "eventId",
        );
        self.unique(
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
        self.check_initial();
        self.check_events(page);
        self.check_states();
        let facts = self.check_transitions();
        self.check_determinism(&facts);
        self.check_coverage(&facts);
        self.check_graph(&facts);
        self.findings
    }

    fn check_initial(&mut self) {
        let base = self.base;
        let initial = &self.machine.initial_state_id;
        match self.state(initial) {
            None => self.push(
                UNKNOWN_REFERENCE,
                format!("{base}/initialStateId"),
                format!("initial state `{initial}` is not declared"),
            ),
            Some(state) if state.role != WidgetStateRole::Idle => self.push(
                INITIAL_NOT_IDLE,
                format!("{base}/initialStateId"),
                format!("initial state `{initial}` must have the idle role"),
            ),
            Some(_) => {}
        }
    }

    /// Events: sources name declared things; payloads only where the source carries one.
    fn check_events(&mut self, page: &WidgetPage) {
        let mut nodes = BTreeSet::new();
        collect_nodes(&page.root, &mut nodes);
        for (index, event) in self.machine.events.iter().enumerate() {
            let pointer = format!("{}/events/{index}", self.base);
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
        }
    }

    /// States: only pending states await, and each one awaits one call under a deadline.
    fn check_states(&mut self) {
        for (index, state) in self.machine.states.iter().enumerate() {
            let pointer = format!("{}/states/{index}", self.base);
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
    }

    fn check_effects(&mut self, effects: &[WidgetMachineEffect], env: Env<'_>, pointer: &str) {
        for (index, effect) in effects.iter().enumerate() {
            let at = format!("{pointer}/effects/{index}");
            match effect {
                WidgetMachineEffect::Start { call, input } => {
                    self.check_call(call, &format!("{at}/call"));
                    if !input.is_empty() && matches!(call, WidgetMachineCall::Action { .. }) {
                        self.push(
                            INVALID_EXPRESSION,
                            format!("{at}/input"),
                            "an action binding maps its own inputs; start it without input"
                                .to_owned(),
                        );
                    }
                    for (member, expression) in input {
                        // Any type fits here; the operation's request schema judges the value and
                        // the server refuses a body that does not fit it.
                        self.check_expression(
                            expression.as_str(),
                            env,
                            &Type::Json,
                            format!("{at}/input/{member}"),
                        );
                    }
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
                            format!("{at}/itemId"),
                            format!("navigation item `{item_id}` is not declared"),
                        );
                    }
                }
                WidgetMachineEffect::Cancel => {}
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One pass over the declared transitions; each law is a short, commented block."
    )]
    fn check_transitions(&mut self) -> Facts<'m> {
        let machine = self.machine;
        let base = self.base;
        let field_types: BTreeMap<String, Type> = machine
            .fields
            .iter()
            .map(|field| (field.field_id.clone(), field_type(field)))
            .collect();
        let mut facts = Facts::default();
        for (index, transition) in machine.transitions.iter().enumerate() {
            let pointer = format!("{base}/transitions/{index}");
            let from_state = self.state(&transition.from);
            let target_state = self.state(&transition.target);
            let event = self.event(&transition.on);
            for (member, id, known) in [
                ("from", &transition.from, from_state.is_some()),
                ("target", &transition.target, target_state.is_some()),
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
            let payload = event.map(event_payload).unwrap_or_default();
            let env = (&field_types, &payload);
            if let Some(guard) = &transition.guard {
                self.check_expression(guard.as_str(), env, &Type::Bool, format!("{pointer}/guard"));
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
                        env,
                        expected,
                        format!("{pointer}/assign/{field}"),
                    ),
                }
            }
            self.check_effects(&transition.effects, env, &pointer);
            let (Some(from_state), Some(target_state), Some(event)) =
                (from_state, target_state, event)
            else {
                continue;
            };
            let from = from_state.state_id.as_str();
            let target = target_state.state_id.as_str();
            if from_state.role == WidgetStateRole::Terminal {
                self.push(
                    TERMINAL_HAS_TRANSITION,
                    format!("{pointer}/from"),
                    format!("terminal state `{from}` cannot be left"),
                );
                continue;
            }
            facts.edges.entry(from).or_default().insert(target);
            let outcome = outcome_of(event);
            match &event.source {
                WidgetMachineEventSource::Timer { .. } => {
                    facts.timer_edges.entry(from).or_default().insert(target);
                    facts.recovers.insert(from);
                }
                WidgetMachineEventSource::User { .. } => {
                    facts.recovers.insert(from);
                }
                WidgetMachineEventSource::PageOpened if from != machine.initial_state_id => {
                    self.push(
                        INVALID_EVENT,
                        format!("{pointer}/on"),
                        "page_opened fires once, in the initial state".to_owned(),
                    );
                }
                WidgetMachineEventSource::PageOpened
                | WidgetMachineEventSource::Outcome { .. }
                | WidgetMachineEventSource::Subscription { .. } => {}
            }

            // Outcome events belong to pending states, and only to outcomes their call can have.
            if let Some((outcome, category)) = outcome {
                match (from_state.role, &from_state.awaits) {
                    (WidgetStateRole::Pending, Some(call)) => {
                        let possible = possible_outcomes(call, true);
                        let matched = matched_outcomes(outcome, category);
                        if matched.iter().all(|value| !possible.contains(value)) {
                            self.push(
                                OUTCOME_IMPOSSIBLE,
                                format!("{pointer}/on"),
                                format!(
                                    "the call `{from}` awaits cannot end `{}`",
                                    outcome_name(outcome)
                                ),
                            );
                        } else if transition.guard.is_none() {
                            let slot = facts.covered.entry(from).or_default();
                            for value in matched {
                                slot.entry(value).or_default().push(index);
                            }
                        }
                    }
                    (WidgetStateRole::Pending, None) => {}
                    (
                        WidgetStateRole::Idle | WidgetStateRole::Error | WidgetStateRole::Terminal,
                        _,
                    ) => self.push(
                        OUTCOME_OUTSIDE_PENDING,
                        format!("{pointer}/on"),
                        format!(
                            "outcome event `{}` can only fire in a pending state; `{from}` is not pending",
                            event.event_id
                        ),
                    ),
                }
            } else if transition.guard.is_none() {
                facts
                    .unguarded
                    .entry((from, event.event_id.as_str()))
                    .or_default()
                    .push(index);
            }

            // A call in flight is left only by its outcome.
            if from_state.role == WidgetStateRole::Pending && outcome.is_none() && from != target {
                self.push(
                    PENDING_ABANDONED,
                    pointer.clone(),
                    format!(
                        "leaving pending state `{from}` on `{}` abandons the call in flight; cancel it and handle the outcome",
                        event.event_id
                    ),
                );
            }

            // Calls start exactly when a pending state is entered, and only the call it awaits.
            let starts: Vec<&WidgetMachineCall> = transition
                .effects
                .iter()
                .filter_map(|effect| match effect {
                    WidgetMachineEffect::Start { call, .. } => Some(call),
                    WidgetMachineEffect::Cancel | WidgetMachineEffect::Navigate { .. } => None,
                })
                .collect();
            let enters_pending = target_state.role == WidgetStateRole::Pending
                && (from != target || outcome.is_some());
            if enters_pending {
                let starts_awaited = match (starts.as_slice(), &target_state.awaits) {
                    ([started], Some(awaited)) => *started == awaited,
                    _ => false,
                };
                if !starts_awaited {
                    self.push(
                        PENDING_ENTERED_WITHOUT_START,
                        format!("{pointer}/effects"),
                        format!(
                            "entering pending state `{target}` must start exactly the call it awaits"
                        ),
                    );
                }
            } else if !starts.is_empty() {
                self.push(
                    START_WITHOUT_PENDING,
                    format!("{pointer}/effects"),
                    if target_state.role == WidgetStateRole::Pending {
                        format!("pending state `{target}` already has its call in flight")
                    } else {
                        format!("a started call must be awaited, but `{target}` is not pending")
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
                    facts.cancellable.insert(from);
                } else {
                    self.push(
                        CANCEL_OUTSIDE_PENDING,
                        format!("{pointer}/effects"),
                        "cancel requests the end of the call in flight: it fires in a pending state, stays there, and the outcome follows".to_owned(),
                    );
                }
            }
        }
        facts
    }

    /// One unguarded transition per state and event or outcome value.
    fn check_determinism(&mut self, facts: &Facts<'m>) {
        let base = self.base;
        for ((from, event), indexes) in &facts.unguarded {
            if let Some(second) = indexes.get(1) {
                self.push(
                    NONDETERMINISTIC,
                    format!("{base}/transitions/{second}"),
                    format!(
                        "state `{from}` has {} unguarded transitions on `{event}`",
                        indexes.len()
                    ),
                );
            }
        }
        for (from, values) in &facts.covered {
            for (value, indexes) in values {
                if let Some(second) = indexes.get(1) {
                    self.push(
                        NONDETERMINISTIC,
                        format!("{base}/transitions/{second}"),
                        format!(
                            "state `{from}` has {} unguarded transitions on outcome `{}`",
                            indexes.len(),
                            show_outcome(value)
                        ),
                    );
                }
            }
        }
    }

    /// Every outcome the awaited call can have has an unguarded exit.
    fn check_coverage(&mut self, facts: &Facts<'m>) {
        for (index, state) in self.machine.states.iter().enumerate() {
            let (WidgetStateRole::Pending, Some(call)) = (state.role, &state.awaits) else {
                continue;
            };
            let id = state.state_id.as_str();
            let have = facts.covered.get(id);
            let missing: Vec<String> = possible_outcomes(call, facts.cancellable.contains(id))
                .iter()
                .filter(|value| have.is_none_or(|have| !have.contains_key(*value)))
                .map(show_outcome)
                .collect();
            if !missing.is_empty() {
                self.push(
                    PENDING_OUTCOME_UNCOVERED,
                    format!("{}/states/{index}", self.base),
                    format!(
                        "pending state `{id}` has no unguarded transition for: {}",
                        missing.join(", ")
                    ),
                );
            }
        }
    }

    fn check_graph(&mut self, facts: &Facts<'m>) {
        let machine = self.machine;
        let base = self.base;
        if self.state(&machine.initial_state_id).is_some() {
            let seen = reachable([machine.initial_state_id.as_str()], &facts.edges);
            for (index, state) in machine.states.iter().enumerate() {
                if !seen.contains(state.state_id.as_str()) {
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
        let settled = reachable(
            machine
                .states
                .iter()
                .filter(|state| {
                    matches!(
                        state.role,
                        WidgetStateRole::Idle | WidgetStateRole::Terminal
                    )
                })
                .map(|state| state.state_id.as_str()),
            &reverse(&facts.edges),
        );
        for (index, state) in machine.states.iter().enumerate() {
            if state.role == WidgetStateRole::Terminal {
                continue;
            }
            let id = state.state_id.as_str();
            let pointer = format!("{base}/states/{index}");
            let exits = facts
                .edges
                .get(id)
                .is_some_and(|targets| targets.iter().any(|target| *target != id));
            if !exits {
                self.push(
                    ABSORBING_STATE,
                    pointer,
                    format!("non-terminal state `{id}` has no exit"),
                );
            } else if !settled.contains(id) {
                self.push(
                    TRAP_REGION,
                    pointer,
                    format!("no idle or terminal state is reachable from `{id}`"),
                );
            } else if state.role == WidgetStateRole::Error && !facts.recovers.contains(id) {
                self.push(
                    ERROR_WITHOUT_RECOVERY,
                    pointer,
                    format!("error state `{id}` has no transition a person or a timer drives"),
                );
            }
        }
        for id in on_cycle(&facts.timer_edges) {
            let index = machine
                .states
                .iter()
                .position(|state| state.state_id == id)
                .unwrap_or_default();
            self.push(
                INPUT_FREE_CYCLE,
                format!("{base}/states/{index}"),
                format!("state `{id}` is on a cycle that runs on timers alone"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cycle_needs_two_states_or_a_self_loop() {
        let graph: Graph<'_> = BTreeMap::from([
            ("a", BTreeSet::from(["b"])),
            ("b", BTreeSet::from(["a"])),
            ("c", BTreeSet::from(["c"])),
            ("d", BTreeSet::from(["a"])),
        ]);
        assert_eq!(on_cycle(&graph), BTreeSet::from(["a", "b", "c"]));
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
