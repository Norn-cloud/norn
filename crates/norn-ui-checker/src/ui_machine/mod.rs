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
//! - every `error` state, and every state a failure outcome enters, offers a person Retry or Close
//!   (a timer alone is no recovery);
//! - a refusal handler that names an error code names one the awaited action declares; it refines
//!   its category and never covers it (coverage stays at the seven refusal categories);
//! - no cycle runs on timers alone (`input_free_cycle`): a loop needs an input;
//! - for one state and one event, at most one unguarded transition fires (`nondeterministic`);
//! - guards, assignments and request members type-check against the field record and the event
//!   payload.
//!
//! What this establishes is **structural**, like the lifecycle checks, and it runs on the same core
//! (`soundness`, LIFE-SOUND-1): an `idle` state is a resting ending, a `terminal` one a good ending,
//! and a guarded transition (the page's guard or a catalog component's) is **may**. Reachability
//! counts may edges (an over-approximation, so nothing unreachable is missed); option to complete
//! counts only unguarded ones, so a state that settles only through guards is reported Unknown
//! (`completion_unknown`, information), never passed. Coverage likewise counts only unguarded
//! transitions. The laws read the document only and add no client-side authority.

mod passes;

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::soundness::{Ending, Net, NetEdge};
use norn_checker::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};
use norn_expr::{EnvOptions, Expression, Type, ZenTypeEnv, check_expression};
use norn_ir::{
    ArtifactEnvelope, JsonValue, RuntimeValue, WidgetCallOutcome, WidgetComponent, WidgetMachine,
    WidgetMachineCall, WidgetMachineEvent, WidgetMachineEventSource, WidgetMachineField,
    WidgetMachineState, WidgetNode, WidgetRefusalCategory, WidgetStateRole, WidgetUx,
    WidgetValueType,
};

pub(super) const DUPLICATE_ID: &str = "widget.machine.duplicate_id";
pub(super) const UNKNOWN_REFERENCE: &str = "widget.machine.unknown_reference";
pub(super) const INVALID_EVENT: &str = "widget.machine.invalid_event";
pub(super) const INVALID_FIELD: &str = "widget.machine.invalid_field";
pub(super) const INVALID_EXPRESSION: &str = "widget.machine.invalid_expression";
pub(super) const INITIAL_NOT_IDLE: &str = "widget.machine.initial_not_idle";
pub(super) const UNREACHABLE_STATE: &str = "widget.machine.unreachable_state";
pub(super) const ABSORBING_STATE: &str = "widget.machine.absorbing_state";
pub(super) const TRAP_REGION: &str = "widget.machine.trap_region";
pub(super) const COMPLETION_UNKNOWN: &str = "widget.machine.completion_unknown";
pub(super) const TERMINAL_HAS_TRANSITION: &str = "widget.machine.terminal_has_transition";
pub(super) const PENDING_WITHOUT_AWAIT: &str = "widget.machine.pending_without_await";
pub(super) const PENDING_WITHOUT_DEADLINE: &str = "widget.machine.pending_without_deadline";
pub(super) const AWAIT_OUTSIDE_PENDING: &str = "widget.machine.await_outside_pending";
pub(super) const OUTCOME_OUTSIDE_PENDING: &str = "widget.machine.outcome_outside_pending";
pub(super) const OUTCOME_IMPOSSIBLE: &str = "widget.machine.outcome_impossible";
pub(super) const PENDING_OUTCOME_UNCOVERED: &str = "widget.machine.pending_outcome_uncovered";
pub(super) const PENDING_ABANDONED: &str = "widget.machine.pending_abandoned";
pub(super) const START_WITHOUT_PENDING: &str = "widget.machine.start_without_pending";
pub(super) const PENDING_ENTERED_WITHOUT_START: &str =
    "widget.machine.pending_entered_without_start";
pub(super) const CANCEL_OUTSIDE_PENDING: &str = "widget.machine.cancel_outside_pending";
pub(super) const ERROR_WITHOUT_RECOVERY: &str = "widget.machine.error_without_recovery";
pub(super) const INPUT_FREE_CYCLE: &str = "widget.machine.input_free_cycle";
pub(super) const NONDETERMINISTIC: &str = "widget.machine.nondeterministic";
pub(super) const CONTROL_WITHOUT_EVENT: &str = "widget.machine.control_without_event";
pub(super) const COMPONENT_GUARD_OUTSIDE_CATALOG: &str =
    "widget.machine.component_guard_outside_catalog";
pub(super) const OUTCOME_CODE_UNDECLARED: &str = "widget.machine.outcome_code_undeclared";
pub(super) const FAILURE_WITHOUT_RETRY_OR_CLOSE: &str =
    "widget.machine.failure_without_retry_or_close";

/// Every code these laws can emit.
pub const CODES: [&str; 28] = [
    DUPLICATE_ID,
    UNKNOWN_REFERENCE,
    INVALID_EVENT,
    INVALID_FIELD,
    INVALID_EXPRESSION,
    INITIAL_NOT_IDLE,
    UNREACHABLE_STATE,
    ABSORBING_STATE,
    TRAP_REGION,
    COMPLETION_UNKNOWN,
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
    CONTROL_WITHOUT_EVENT,
    COMPONENT_GUARD_OUTSIDE_CATALOG,
    OUTCOME_CODE_UNDECLARED,
    FAILURE_WITHOUT_RETRY_OR_CLOSE,
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
pub(super) type OutcomeValue = (WidgetCallOutcome, Option<WidgetRefusalCategory>);

/// Outcome value to the unguarded transitions that take it: `(index, specific)`.
pub(super) type Covered = BTreeMap<OutcomeValue, Vec<(usize, bool)>>;

/// Action binding id to the error codes its action declares (`ActionUiEffect::Error`).
pub(crate) type DeclaredCodes = BTreeMap<String, BTreeSet<String>>;

/// The field record and the event payload an expression sees.
pub(crate) type Env<'e> = (&'e BTreeMap<String, Type>, &'e BTreeMap<String, Type>);

pub(super) struct Finding {
    code: &'static str,
    pointer: String,
    message: String,
}

/// Codes reported as information, not errors: an Unknown verdict is never a pass, but the guards
/// that decide it are the server's to evaluate, not this pass's.
const INFO: [&str; 1] = [COMPLETION_UNKNOWN];

/// Check the declared machine of every page in one widget's UX contract, without the declared
/// error codes of its actions (a code-specific handler is then not judged).
#[must_use]
pub(crate) fn check(owner: &ArtifactEnvelope, ux: &WidgetUx) -> Vec<Diagnostic> {
    check_with_codes(owner, ux, None)
}

/// Check the declared machine of every page in one widget's UX contract against the error codes
/// its action bindings' actions declare.
#[must_use]
pub(crate) fn check_with_codes(
    owner: &ArtifactEnvelope,
    ux: &WidgetUx,
    codes: Option<&DeclaredCodes>,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for (index, page) in ux.pages.iter().enumerate() {
        if let Some(machine) = &page.machine {
            let base = format!("/payload/ux/pages/{index}/machine");
            let mut held = BTreeMap::new();
            collect_held(&page.root, &[], &mut held);
            let checker = MachineChecker {
                ux,
                codes,
                in_catalog: false,
                machine,
                held,
                base: &base,
                findings: Vec::new(),
            };
            findings.extend(checker.run(Some(page)));
        }
    }
    // A catalog component's own machine (UI-CATALOG-1) meets the same laws; its user events come
    // from the component's own controls, which the contract does not enumerate.
    for (index, entry) in ux.catalog.iter().enumerate() {
        if let Some(machine) = &entry.machine {
            let base = format!("/payload/ux/catalog/{index}/machine");
            let checker = MachineChecker {
                ux,
                codes,
                in_catalog: true,
                machine,
                held: BTreeMap::new(),
                base: &base,
                findings: Vec::new(),
            };
            findings.extend(checker.run(None));
        }
    }
    findings
        .into_iter()
        .map(|finding| Diagnostic {
            witness: None,
            code: DiagnosticCode::new(finding.code),
            severity: if INFO.contains(&finding.code) {
                DiagnosticSeverity::Info
            } else {
                DiagnosticSeverity::Error
            },
            message: finding.message,
            span: Some(Span::pointer(finding.pointer)),
            artifact_id: Some(owner.artifact_id.clone()),
            remediation: Some(DiagnosticRemediation::FixDefinition),
        })
        .collect()
}

/// Every outcome value the server can produce for `call`. Approval gates only calls; only reads
/// can see their answer move, and only a read of a binding the machine subscribes to can find its
/// live feed lapsed (`stale`) or its subscription refused. `cancelled` is possible only where the
/// page offers a cancel.
pub(super) fn possible_outcomes(
    call: &WidgetMachineCall,
    cancellable: bool,
    subscribed: bool,
) -> BTreeSet<OutcomeValue> {
    let mut values: BTreeSet<OutcomeValue> = [
        (WidgetCallOutcome::Accepted, None),
        (WidgetCallOutcome::NetworkFailure, None),
        (WidgetCallOutcome::NotServed, None),
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
            if subscribed {
                values.insert((WidgetCallOutcome::Stale, None));
                values.insert((WidgetCallOutcome::SubscriptionRefused, None));
            }
        }
    }
    if cancellable {
        values.insert((WidgetCallOutcome::Cancelled, None));
    }
    values
}

/// Whether `call` reads a binding `machine` subscribes to (it has a `subscription` event for it).
pub(super) fn subscribed(machine: &WidgetMachine, call: &WidgetMachineCall) -> bool {
    let WidgetMachineCall::Read { binding_id } = call else {
        return false;
    };
    machine.events.iter().any(|event| {
        matches!(&event.source, WidgetMachineEventSource::Subscription { binding_id: read } if read == binding_id)
    })
}

/// Outcomes that end a call without its answer: a person needs Retry or Close after one.
pub(super) const fn is_failure(outcome: WidgetCallOutcome) -> bool {
    match outcome {
        WidgetCallOutcome::Refused
        | WidgetCallOutcome::NetworkFailure
        | WidgetCallOutcome::NotServed
        | WidgetCallOutcome::Timeout
        | WidgetCallOutcome::SubscriptionRefused => true,
        WidgetCallOutcome::Accepted
        | WidgetCallOutcome::PendingApproval
        | WidgetCallOutcome::AnswerMoved
        | WidgetCallOutcome::Stale
        | WidgetCallOutcome::Cancelled => false,
    }
}

/// The outcome values one `outcome` event source matches.
pub(super) fn matched_outcomes(
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

pub(super) fn outcome_name(outcome: WidgetCallOutcome) -> &'static str {
    match outcome {
        WidgetCallOutcome::Accepted => "accepted",
        WidgetCallOutcome::Refused => "refused",
        WidgetCallOutcome::PendingApproval => "pending_approval",
        WidgetCallOutcome::AnswerMoved => "answer_moved",
        WidgetCallOutcome::NetworkFailure => "network_failure",
        WidgetCallOutcome::NotServed => "not_served",
        WidgetCallOutcome::Stale => "stale",
        WidgetCallOutcome::SubscriptionRefused => "subscription_refused",
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

pub(super) fn show_outcome((outcome, category): OutcomeValue) -> String {
    match category {
        Some(category) => format!("{}:{}", outcome_name(outcome), category_name(category)),
        None => outcome_name(outcome).to_owned(),
    }
}

/// The fixed payload an outcome event carries.
fn outcome_payload(outcome: WidgetCallOutcome) -> BTreeMap<String, Type> {
    match outcome {
        WidgetCallOutcome::Refused => BTreeMap::from([
            ("code".to_owned(), Type::text()),
            ("retryable".to_owned(), Type::Bool),
        ]),
        WidgetCallOutcome::SubscriptionRefused => {
            BTreeMap::from([("code".to_owned(), Type::text())])
        }
        WidgetCallOutcome::PendingApproval => {
            BTreeMap::from([("decisionId".to_owned(), Type::text())])
        }
        WidgetCallOutcome::AnswerMoved => BTreeMap::from([("code".to_owned(), Type::text())]),
        WidgetCallOutcome::Accepted
        | WidgetCallOutcome::NetworkFailure
        | WidgetCallOutcome::NotServed
        | WidgetCallOutcome::Stale
        | WidgetCallOutcome::Timeout
        | WidgetCallOutcome::Cancelled => BTreeMap::new(),
    }
}

/// The payload record an event's guards and assignments see.
pub(super) fn event_payload(event: &WidgetMachineEvent) -> BTreeMap<String, Type> {
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

pub(super) fn outcome_of(event: &WidgetMachineEvent) -> Option<OutcomeValue> {
    match &event.source {
        WidgetMachineEventSource::Outcome {
            outcome, category, ..
        } => Some((*outcome, *category)),
        WidgetMachineEventSource::PageOpened
        | WidgetMachineEventSource::User { .. }
        | WidgetMachineEventSource::Subscription { .. }
        | WidgetMachineEventSource::Timer { .. } => None,
    }
}

pub(crate) fn field_type(field: &WidgetMachineField) -> Type {
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

/// Each node of `node`'s tree to the `heldIn` lists of it and of the catalog nodes above it: the
/// renderer draws a node in a state only when every one of them names that state (a node held in
/// no state is drawn in all of them), so only there can a person raise its events.
pub(super) fn collect_held<'n>(
    node: &'n WidgetNode,
    above: &[&'n [String]],
    out: &mut BTreeMap<&'n str, Vec<&'n [String]>>,
) {
    let mut lists = above.to_vec();
    if let WidgetComponent::Catalog { held_in, .. } = &node.component
        && !held_in.is_empty()
    {
        lists.push(held_in);
    }
    for child in &node.children {
        collect_held(child, &lists, out);
    }
    out.insert(node.node_id.as_str(), lists);
}

pub(super) fn collect_nodes<'n>(
    node: &'n WidgetNode,
    out: &mut BTreeMap<&'n str, &'n WidgetComponent>,
) {
    out.insert(node.node_id.as_str(), &node.component);
    for child in &node.children {
        collect_nodes(child, out);
    }
}

pub(crate) fn machine_env(env: Env<'_>) -> Result<ZenTypeEnv, String> {
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

/// A page machine as a [`Net`] (LIFE-SOUND-1's core, shared with the backend lifecycles): states
/// by declaration index, an `idle` state a resting ending and a `terminal` one a good ending, and
/// one edge per transition between declared states (its label is the transition's index), may when
/// a guard (the page's or a catalog component's) decides it, or when it handles one refusal code. Transitions that name an undeclared
/// state or event, or leave a terminal state, are reported by the laws and left out.
pub(crate) fn page_net(machine: &WidgetMachine) -> Net {
    let index = |id: &str| machine.states.iter().position(|state| state.state_id == id);
    let edges = machine
        .transitions
        .iter()
        .enumerate()
        .filter_map(|(label, transition)| {
            let from = index(&transition.from)?;
            let to = index(&transition.target)?;
            let event = machine
                .events
                .iter()
                .find(|event| event.event_id == transition.on)?;
            if machine.states.get(from)?.role == WidgetStateRole::Terminal {
                return None;
            }
            let input = !matches!(event.source, WidgetMachineEventSource::Timer { .. });
            let coded = matches!(
                &event.source,
                WidgetMachineEventSource::Outcome { code: Some(_), .. }
            );
            Some(
                if transition.guard.is_some() || transition.component_guard.is_some() || coded {
                    NetEdge::guarded(from, to, label, input)
                } else {
                    NetEdge::unguarded(from, to, label, input)
                },
            )
        })
        .collect();
    Net {
        states: machine.states.len(),
        initial: index(&machine.initial_state_id).into_iter().collect(),
        endings: machine
            .states
            .iter()
            .map(|state| match state.role {
                WidgetStateRole::Idle => Some(Ending::Resting),
                WidgetStateRole::Terminal => Some(Ending::Good),
                WidgetStateRole::Pending | WidgetStateRole::Error => None,
            })
            .collect(),
        edges,
    }
}

#[derive(Default)]
pub(super) struct Facts<'m> {
    /// States with an unguarded exit a person drives (Retry or Close).
    pub(super) recovers: BTreeSet<&'m str>,
    /// Transitions on a user event whose source node is not drawn in the state they leave: no
    /// person can take them, so they are no edge of the page's net.
    pub(super) held_out: BTreeSet<usize>,
    /// `(state, refusal code)` to the code-specific transitions that handle it.
    pub(super) coded: BTreeMap<(&'m str, &'m str), Vec<usize>>,
    /// States a failure outcome enters, with the transition that enters each.
    pub(super) failures: BTreeMap<&'m str, usize>,
    /// Pending states that offer a cancel.
    pub(super) cancellable: BTreeSet<&'m str>,
    /// `(state, event)` to unguarded transition indexes, for non-outcome events.
    pub(super) unguarded: BTreeMap<(&'m str, &'m str), Vec<usize>>,
    /// `state` to outcome value to unguarded transitions: `(index, specific)`, where `specific`
    /// marks an event that names the refusal category. A specific transition takes precedence over
    /// a catch-all one, so only two of the same specificity conflict.
    pub(super) covered: BTreeMap<&'m str, Covered>,
}

pub(super) struct MachineChecker<'m> {
    pub(super) ux: &'m WidgetUx,
    /// Error codes each action binding's action declares; `None` when they are not known here.
    pub(super) codes: Option<&'m DeclaredCodes>,
    /// Checking a catalog entry's own machine (component guards allowed).
    pub(super) in_catalog: bool,
    pub(super) machine: &'m WidgetMachine,
    /// Each node of the page's tree to the `heldIn` lists that decide where it is drawn
    /// ([`collect_held`]); empty for a catalog entry's machine, whose nodes the contract does
    /// not enumerate.
    pub(super) held: BTreeMap<&'m str, Vec<&'m [String]>>,
    pub(super) base: &'m str,
    pub(super) findings: Vec<Finding>,
}

impl<'m> MachineChecker<'m> {
    pub(super) fn push(&mut self, code: &'static str, pointer: String, message: String) {
        self.findings.push(Finding {
            code,
            pointer,
            message,
        });
    }

    pub(super) fn unique<'i>(
        &mut self,
        ids: impl IntoIterator<Item = &'i str>,
        list: &str,
        member: &str,
    ) {
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

    pub(super) fn state(&self, id: &str) -> Option<&'m WidgetMachineState> {
        self.machine
            .states
            .iter()
            .find(|state| state.state_id == id)
    }

    pub(super) fn event(&self, id: &str) -> Option<&'m WidgetMachineEvent> {
        self.machine
            .events
            .iter()
            .find(|event| event.event_id == id)
    }

    pub(super) fn check_call(&mut self, call: &WidgetMachineCall, pointer: &str) {
        let known = match call {
            WidgetMachineCall::Action { binding_id } => self
                .ux
                .action_bindings
                .iter()
                .any(|binding| &binding.binding_id == binding_id),
            WidgetMachineCall::Read { binding_id } => {
                self.ux
                    .data_bindings
                    .iter()
                    .any(|binding| &binding.binding_id == binding_id)
                    || self
                        .ux
                        .context_reads
                        .iter()
                        .any(|read| &read.binding_id == binding_id)
            }
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

    pub(super) fn check_fields(
        &mut self,
        fields: &[WidgetMachineField],
        list: &str,
        payload: bool,
    ) {
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

    pub(super) fn check_expression(
        &mut self,
        source: &str,
        env: Env<'_>,
        expected: &Type,
        pointer: String,
    ) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_without_category_matches_every_category() {
        assert_eq!(matched_outcomes(WidgetCallOutcome::Refused, None).len(), 7);
        let read = WidgetMachineCall::Read {
            binding_id: "b".into(),
        };
        let possible = possible_outcomes(&read, false, false);
        assert!(possible.contains(&(WidgetCallOutcome::AnswerMoved, None)));
        assert!(!possible.contains(&(WidgetCallOutcome::PendingApproval, None)));
        assert!(!possible.contains(&(WidgetCallOutcome::Cancelled, None)));
        assert!(possible.contains(&(WidgetCallOutcome::NotServed, None)));
        assert!(!possible.contains(&(WidgetCallOutcome::Stale, None)));
        let live = possible_outcomes(&read, false, true);
        assert!(live.contains(&(WidgetCallOutcome::Stale, None)));
        assert!(live.contains(&(WidgetCallOutcome::SubscriptionRefused, None)));
        let call = WidgetMachineCall::Action {
            binding_id: "a".into(),
        };
        assert!(!possible_outcomes(&call, false, true).contains(&(WidgetCallOutcome::Stale, None)));
    }
}
