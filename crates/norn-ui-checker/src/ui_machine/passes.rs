//! Law-checking passes over a page or catalog component statechart.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::soundness::{self, Completion};
use norn_expr::Type;
use norn_ir::{
    WidgetCallOutcome, WidgetComponent, WidgetControlInput, WidgetMachineCall, WidgetMachineEffect,
    WidgetMachineEvent, WidgetMachineEventSource, WidgetMachineState, WidgetMachineTransition,
    WidgetPage, WidgetStateRole, WidgetValueType,
};

use super::{
    ABSORBING_STATE, AWAIT_OUTSIDE_PENDING, CANCEL_OUTSIDE_PENDING, COMPLETION_UNKNOWN,
    COMPONENT_GUARD_OUTSIDE_CATALOG, CONTROL_WITHOUT_EVENT, ERROR_WITHOUT_RECOVERY, Env,
    FAILURE_WITHOUT_RETRY_OR_CLOSE, Facts, Finding, INITIAL_NOT_IDLE, INPUT_FREE_CYCLE,
    INVALID_EVENT, INVALID_EXPRESSION, MachineChecker, NONDETERMINISTIC, OUTCOME_CODE_UNDECLARED,
    OUTCOME_IMPOSSIBLE, OUTCOME_OUTSIDE_PENDING, OutcomeValue, PENDING_ABANDONED,
    PENDING_ENTERED_WITHOUT_START, PENDING_OUTCOME_UNCOVERED, PENDING_WITHOUT_AWAIT,
    PENDING_WITHOUT_DEADLINE, START_WITHOUT_PENDING, TERMINAL_HAS_TRANSITION, TRAP_REGION,
    UNKNOWN_REFERENCE, UNREACHABLE_STATE, collect_nodes, event_payload, field_type, is_failure,
    matched_outcomes, outcome_name, outcome_of, page_net, possible_outcomes, show_outcome,
    subscribed,
};

struct TransitionCheck<'m, 'a> {
    index: usize,
    transition: &'m WidgetMachineTransition,
    from_state: &'m WidgetMachineState,
    target_state: &'m WidgetMachineState,
    event: &'m WidgetMachineEvent,
    outcome: Option<OutcomeValue>,
    pointer: &'a str,
}

impl<'m> MachineChecker<'m> {
    pub(super) fn run(mut self, page: Option<&WidgetPage>) -> Vec<Finding> {
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
    fn check_events(&mut self, page: Option<&WidgetPage>) {
        let mut nodes = BTreeMap::new();
        if let Some(page) = page {
            collect_nodes(&page.root, &mut nodes);
        }
        let mut raised: BTreeSet<&str> = BTreeSet::new();
        for (index, event) in self.machine.events.iter().enumerate() {
            let pointer = format!("{}/events/{index}", self.base);
            let carries_payload = match &event.source {
                WidgetMachineEventSource::User { node_id, .. } => {
                    match nodes.get(node_id.as_str()) {
                        None if page.is_none() => {}
                        None => self.push(
                            UNKNOWN_REFERENCE,
                            format!("{pointer}/source/nodeId"),
                            format!("node `{node_id}` is not in this page's tree"),
                        ),
                        Some(component) => {
                            if let WidgetComponent::Control { input, .. } = component {
                                raised.insert(node_id.as_str());
                                self.check_control_event(event, *input, node_id, &pointer);
                            }
                        }
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
                WidgetMachineEventSource::Outcome {
                    outcome,
                    category,
                    code,
                } => {
                    for (member, present) in
                        [("category", category.is_some()), ("code", code.is_some())]
                    {
                        if present && *outcome != WidgetCallOutcome::Refused {
                            self.push(
                                INVALID_EVENT,
                                format!("{pointer}/source/{member}"),
                                format!("only a refused outcome has a {member}"),
                            );
                        }
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
        // A control does nothing but raise events: one that raises none is dead.
        for (node_id, component) in &nodes {
            if matches!(component, WidgetComponent::Control { .. }) && !raised.contains(node_id) {
                self.push(
                    CONTROL_WITHOUT_EVENT,
                    format!("{}/events", self.base),
                    format!("control `{node_id}` raises no event of this page's machine"),
                );
            }
        }
    }

    /// An event from a choice or text control carries the control's value as `value: text`.
    fn check_control_event(
        &mut self,
        event: &WidgetMachineEvent,
        input: WidgetControlInput,
        node_id: &str,
        pointer: &str,
    ) {
        let carries_value = event.payload.iter().any(|member| {
            member.field_id == "value"
                && member.value_type == WidgetValueType::Text
                && member.required
        });
        if input != WidgetControlInput::Trigger && !carries_value {
            self.push(
                INVALID_EVENT,
                format!("{pointer}/payload"),
                format!(
                    "an event from the choice or text control `{node_id}` declares the required text payload member `value`"
                ),
            );
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

    fn check_transitions(&mut self) -> Facts<'m> {
        let field_types: BTreeMap<String, Type> = self
            .machine
            .fields
            .iter()
            .map(|field| (field.field_id.clone(), field_type(field)))
            .collect();
        let mut facts = Facts::default();
        for (index, transition) in self.machine.transitions.iter().enumerate() {
            self.check_transition(index, transition, &field_types, &mut facts);
        }
        facts
    }

    fn check_transition(
        &mut self,
        index: usize,
        transition: &'m WidgetMachineTransition,
        field_types: &BTreeMap<String, Type>,
        facts: &mut Facts<'m>,
    ) {
        let pointer = format!("{}/transitions/{index}", self.base);
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
        // A page's guards are ZEN the checker reads; only a catalog component's own machine may
        // leave a guard to the component's code.
        if transition.component_guard.is_some() && !self.in_catalog {
            self.push(
                COMPONENT_GUARD_OUTSIDE_CATALOG,
                format!("{pointer}/componentGuard"),
                "a page machine's guards are ZEN expressions; only a catalog entry's machine leaves a guard to its component".to_owned(),
            );
        }
        let payload = event.map_or_default(event_payload);
        let env = (field_types, &payload);
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
        let (Some(from_state), Some(target_state), Some(event)) = (from_state, target_state, event)
        else {
            return;
        };
        let from = from_state.state_id.as_str();
        if from_state.role == WidgetStateRole::Terminal {
            self.push(
                TERMINAL_HAS_TRANSITION,
                format!("{pointer}/from"),
                format!("terminal state `{from}` cannot be left"),
            );
            return;
        }
        let outcome = outcome_of(event);
        let check = TransitionCheck {
            index,
            transition,
            from_state,
            target_state,
            event,
            outcome,
            pointer: &pointer,
        };
        match &event.source {
            WidgetMachineEventSource::User { node_id, .. } => {
                self.check_user_transition(&check, node_id, facts);
            }
            WidgetMachineEventSource::PageOpened if from != self.machine.initial_state_id => {
                self.push(
                    INVALID_EVENT,
                    format!("{pointer}/on"),
                    "page_opened fires once, in the initial state".to_owned(),
                );
            }
            WidgetMachineEventSource::PageOpened
            | WidgetMachineEventSource::Timer { .. }
            | WidgetMachineEventSource::Outcome { .. }
            | WidgetMachineEventSource::Subscription { .. } => {}
        }
        self.check_transition_outcome(&check, facts);
        self.check_transition_pending_effects(&check, facts);
    }

    /// A person raises a node's event only where the node is drawn: a transition from a state that
    /// holds its source out is none they can take, so it is refused and is no Retry or Close of
    /// that state (L4), nor an edge of its net. Otherwise an unguarded one is a person's way on.
    fn check_user_transition(
        &mut self,
        check: &TransitionCheck<'m, '_>,
        node_id: &str,
        facts: &mut Facts<'m>,
    ) {
        let &TransitionCheck {
            index,
            transition,
            from_state,
            event,
            pointer,
            ..
        } = check;
        let from = from_state.state_id.as_str();
        let held_out = self.held.get(node_id).is_some_and(|lists| {
            lists
                .iter()
                .any(|states| !states.iter().any(|state| state == from))
        });
        if held_out {
            facts.held_out.insert(index);
            self.push(
                INVALID_EVENT,
                format!("{pointer}/from"),
                format!(
                    "`{}` comes from node `{node_id}`, which is not drawn in `{from}` (it or a node above it is held in other states): no person can raise it there",
                    event.event_id
                ),
            );
        } else if transition.guard.is_none() && transition.component_guard.is_none() {
            facts.recovers.insert(from);
        }
    }

    fn check_transition_outcome(&mut self, check: &TransitionCheck<'m, '_>, facts: &mut Facts<'m>) {
        let &TransitionCheck {
            index,
            transition,
            from_state,
            event,
            outcome,
            pointer,
            ..
        } = check;
        let from = from_state.state_id.as_str();
        let code = match &event.source {
            WidgetMachineEventSource::Outcome { code, .. } => code.as_deref(),
            WidgetMachineEventSource::PageOpened
            | WidgetMachineEventSource::User { .. }
            | WidgetMachineEventSource::Subscription { .. }
            | WidgetMachineEventSource::Timer { .. } => None,
        };
        // A failure leaves the page somewhere a person can Retry or Close (L4).
        if let Some((outcome, _)) = outcome
            && is_failure(outcome)
            && check.target_state.role != WidgetStateRole::Pending
        {
            facts
                .failures
                .entry(check.target_state.state_id.as_str())
                .or_insert(index);
        }
        // Outcome events belong to pending states, and only to outcomes their call can have.
        if let Some((outcome, category)) = outcome {
            match (from_state.role, &from_state.awaits) {
                (WidgetStateRole::Pending, Some(call)) => {
                    let possible = possible_outcomes(call, true, subscribed(self.machine, call));
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
                    } else if let Some(code) = code {
                        self.check_outcome_code(call, code, check, facts);
                    } else if transition.guard.is_none() && transition.component_guard.is_none() {
                        let slot = facts.covered.entry(from).or_default();
                        for value in matched {
                            slot.entry(value)
                                .or_default()
                                .push((index, category.is_some()));
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
        } else if transition.guard.is_none() && transition.component_guard.is_none() {
            facts
                .unguarded
                .entry((from, event.event_id.as_str()))
                .or_default()
                .push(index);
        }
    }

    /// A refusal handler for one code names a code the awaited action declares (L2). It refines the
    /// code's category and never covers it.
    fn check_outcome_code(
        &mut self,
        call: &WidgetMachineCall,
        code: &'m str,
        check: &TransitionCheck<'m, '_>,
        facts: &mut Facts<'m>,
    ) {
        let from = check.from_state.state_id.as_str();
        if let Some(codes) = self.codes {
            let declared = match call {
                WidgetMachineCall::Action { binding_id } => codes
                    .get(binding_id)
                    .is_some_and(|declared| declared.contains(code)),
                WidgetMachineCall::Read { .. } | WidgetMachineCall::Operation { .. } => false,
            };
            if !declared {
                self.push(
                    OUTCOME_CODE_UNDECLARED,
                    format!("{}/on", check.pointer),
                    format!(
                        "the call `{from}` awaits cannot refuse with `{code}`: handle a code only when the bound action declares it"
                    ),
                );
            }
        }
        if check.transition.guard.is_none() && check.transition.component_guard.is_none() {
            facts
                .coded
                .entry((from, code))
                .or_default()
                .push(check.index);
        }
    }

    fn check_transition_pending_effects(
        &mut self,
        check: &TransitionCheck<'m, '_>,
        facts: &mut Facts<'m>,
    ) {
        let &TransitionCheck {
            transition,
            from_state,
            target_state,
            event,
            outcome,
            pointer,
            ..
        } = check;
        let from = from_state.state_id.as_str();
        let target = target_state.state_id.as_str();
        // A call in flight is left only by its outcome.
        if from_state.role == WidgetStateRole::Pending && outcome.is_none() && from != target {
            self.push(
                PENDING_ABANDONED,
                pointer.to_owned(),
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
        let enters_pending =
            target_state.role == WidgetStateRole::Pending && (from != target || outcome.is_some());
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
            if from_state.role == WidgetStateRole::Pending && from == target && outcome.is_none() {
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
        for ((from, code), indexes) in &facts.coded {
            if let Some(second) = indexes.get(1) {
                self.push(
                    NONDETERMINISTIC,
                    format!("{base}/transitions/{second}"),
                    format!(
                        "state `{from}` has {} unguarded transitions on refusal code `{code}`",
                        indexes.len()
                    ),
                );
            }
        }
        for (from, values) in &facts.covered {
            for (value, entries) in values {
                let specific = entries.iter().any(|(_, specific)| *specific);
                let indexes: Vec<usize> = entries
                    .iter()
                    .filter(|(_, is_specific)| *is_specific == specific)
                    .map(|(index, _)| *index)
                    .collect();
                if let Some(second) = indexes.get(1) {
                    self.push(
                        NONDETERMINISTIC,
                        format!("{base}/transitions/{second}"),
                        format!(
                            "state `{from}` has {} unguarded transitions on outcome `{}`",
                            indexes.len(),
                            show_outcome(*value)
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
            let live = subscribed(self.machine, call);
            let missing: Vec<String> =
                possible_outcomes(call, facts.cancellable.contains(id), live)
                    .iter()
                    .filter(|value| have.is_none_or(|have| !have.contains_key(*value)))
                    .map(|value| show_outcome(*value))
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

    /// Option to complete, reachability and timer cycles over [`page_net`].
    fn check_graph(&mut self, facts: &Facts<'m>) {
        let machine = self.machine;
        let base = self.base;
        let mut net = page_net(machine);
        net.edges
            .retain(|edge| !facts.held_out.contains(&edge.label));
        let verdict = soundness::analyse(&net);
        let seeded = !net.initial.is_empty();
        let cycles = soundness::input_free_cycles(&net);
        for (index, state) in machine.states.iter().enumerate() {
            let id = state.state_id.as_str();
            let pointer = format!("{base}/states/{index}");
            let Some(found) = verdict.states.get(index) else {
                continue;
            };
            if seeded && !found.reachable {
                self.push(
                    UNREACHABLE_STATE,
                    pointer.clone(),
                    format!("state `{id}` is unreachable from the initial state"),
                );
            }
            if cycles.get(index).copied().unwrap_or(false) {
                self.push(
                    INPUT_FREE_CYCLE,
                    pointer.clone(),
                    format!("state `{id}` is on a cycle that runs on timers alone"),
                );
            }
            if state.role == WidgetStateRole::Terminal {
                continue;
            }
            if found.deadlock {
                self.push(
                    ABSORBING_STATE,
                    pointer,
                    format!("non-terminal state `{id}` has no exit"),
                );
                continue;
            }
            match found.completion {
                Completion::None => self.push(
                    TRAP_REGION,
                    pointer,
                    format!("no idle or terminal state is reachable from `{id}`"),
                ),
                Completion::Unknown => self.push(
                    COMPLETION_UNKNOWN,
                    pointer,
                    format!(
                        "an idle or terminal state is reachable from `{id}` only through guarded transitions"
                    ),
                ),
                Completion::Must => {
                    if state.role == WidgetStateRole::Error && !facts.recovers.contains(id) {
                        self.push(
                            ERROR_WITHOUT_RECOVERY,
                            pointer,
                            format!(
                                "error state `{id}` offers no Retry or Close: no unguarded transition a person drives (a timer alone is no recovery)"
                            ),
                        );
                    }
                }
            }
        }
        for (state, transition) in &facts.failures {
            // An error state answers to its own law; a terminal one is the page's declared end,
            // which the host closes.
            let judged_elsewhere = machine.states.iter().any(|declared| {
                declared.state_id == *state
                    && matches!(
                        declared.role,
                        WidgetStateRole::Error | WidgetStateRole::Terminal
                    )
            });
            if !judged_elsewhere && !facts.recovers.contains(state) {
                self.push(
                    FAILURE_WITHOUT_RETRY_OR_CLOSE,
                    format!("{base}/transitions/{transition}/target"),
                    format!(
                        "state `{state}` follows a failed call but offers no Retry or Close: give it an unguarded transition a person drives"
                    ),
                );
            }
        }
    }
}
