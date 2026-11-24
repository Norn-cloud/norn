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

/// Every code these laws can emit.
pub const CODES: [&str; 26] = [
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

/// Check the declared machine of every page in one widget's UX contract.
#[must_use]
pub(crate) fn check(owner: &ArtifactEnvelope, ux: &WidgetUx) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for (index, page) in ux.pages.iter().enumerate() {
        if let Some(machine) = &page.machine {
            let base = format!("/payload/ux/pages/{index}/machine");
            let checker = MachineChecker {
                ux,
                in_catalog: false,
                machine,
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
                in_catalog: true,
                machine,
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
/// can see their answer move. `cancelled` is possible only where the page offers a cancel.
pub(super) fn possible_outcomes(
    call: &WidgetMachineCall,
    cancellable: bool,
) -> BTreeSet<OutcomeValue> {
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
        WidgetMachineEventSource::Outcome { outcome, category } => Some((*outcome, *category)),
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
/// a guard (the page's or a catalog component's) decides it. Transitions that name an undeclared
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
            Some(
                if transition.guard.is_some() || transition.component_guard.is_some() {
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
    /// States with an exit a person or a timer drives.
    pub(super) recovers: BTreeSet<&'m str>,
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
    /// Checking a catalog entry's own machine (component guards allowed).
    pub(super) in_catalog: bool,
    pub(super) machine: &'m WidgetMachine,
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
        let possible = possible_outcomes(&read, false);
        assert!(possible.contains(&(WidgetCallOutcome::AnswerMoved, None)));
        assert!(!possible.contains(&(WidgetCallOutcome::PendingApproval, None)));
        assert!(!possible.contains(&(WidgetCallOutcome::Cancelled, None)));
    }
}
