//! Lifecycle, state-machine, transition, and timer-duration lowering split out
//! of [`super::def_entities`].

use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::compact_canonical_json;
use super::def_helpers::{DefinitionCompiler, Members};
use super::types::CompileError;
use super::value_schema::ExpansionBudget;

type DeclaredEvent = (String, Option<String>);
type MachineEvents = (Vec<DeclaredEvent>, HashSet<String>);

struct MachineMove {
    id: String,
    from: Vec<String>,
    body: Map<String, Value>,
    key: String,
}

#[derive(Default)]
struct MachineAcc {
    terminal: Vec<String>,
    immutable: Vec<String>,
    endings: Vec<Value>,
    states: Vec<String>,
    moves: Vec<MachineMove>,
}

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_lifecycle(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        if self.record(checker, input)?.has("states") {
            return self.compile_machine(checker, expansion, input);
        }
        let m = self.closed_record(
            checker,
            input,
            &[
                "property",
                "description",
                "initial",
                "terminal",
                "immutable",
                "transitions",
                "endings",
                "events",
            ],
            &["property", "initial", "transitions"],
        )?;
        let mut obj = Map::new();
        obj.insert(
            "property".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "property", input)?)?),
        );
        if let Some(desc) = m.get("description") {
            obj.insert(
                "description".to_owned(),
                Value::String(self.string(checker, desc)?),
            );
        }
        obj.insert(
            "initial".to_owned(),
            Value::Array(
                self.strings(checker, Self::member(&m, "initial", input)?)?
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
        self.insert_optional_strings(checker, &m, "terminal", &mut obj)?;
        self.insert_optional_strings(checker, &m, "immutable", &mut obj)?;
        let mut transitions = Vec::new();
        for t in self.list(checker, Self::member(&m, "transitions", input)?)? {
            transitions.push(self.compile_transition(checker, expansion, t)?);
        }
        if !transitions.is_empty() {
            obj.insert("transitions".to_owned(), Value::Array(transitions));
        }
        self.append_lifecycle_endings_and_events(checker, &m, &mut obj)?;
        Ok(Value::Object(obj))
    }

    fn insert_optional_strings(
        &self,
        checker: &mut Checker,
        m: &Members,
        key: &str,
        obj: &mut Map<String, Value>,
    ) -> Result<(), CompileError> {
        if let Some(node) = m.get(key) {
            let items = self.strings(checker, node)?;
            if !items.is_empty() {
                obj.insert(
                    key.to_owned(),
                    Value::Array(items.into_iter().map(Value::String).collect()),
                );
            }
        }
        Ok(())
    }

    fn append_lifecycle_endings_and_events(
        &self,
        checker: &mut Checker,
        m: &Members,
        obj: &mut Map<String, Value>,
    ) -> Result<(), CompileError> {
        if let Some(endings_node) = m.get("endings") {
            let mut endings = Vec::new();
            for (state, outcome_node) in self.record(checker, endings_node)?.iter() {
                let mut e = Map::new();
                e.insert("state".to_owned(), Value::String(state.to_owned()));
                e.insert(
                    "outcome".to_owned(),
                    Value::String(self.read_outcome(checker, outcome_node)?),
                );
                endings.push(Value::Object(e));
            }
            if !endings.is_empty() {
                obj.insert("endings".to_owned(), Value::Array(endings));
            }
        }
        if let Some(events_node) = m.get("events") {
            let mut events = Vec::new();
            for (id, spec) in self.record(checker, events_node)?.iter() {
                events.push(self.compile_lifecycle_event(checker, id, spec)?);
            }
            if !events.is_empty() {
                obj.insert("events".to_owned(), Value::Array(events));
            }
        }
        Ok(())
    }

    fn compile_machine(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &["property", "description", "initial", "states", "events"],
            &["property", "initial", "states"],
        )?;
        let initial_node = Self::member(&m, "initial", input)?;
        let initial = if self.deref(checker, initial_node)?.kind() == Kind::ArrayLiteralExpression {
            self.strings(checker, initial_node)?
        } else {
            vec![self.string(checker, initial_node)?]
        };
        let (declared, declared_names) = self.read_machine_events(checker, &m)?;
        let mut acc = MachineAcc::default();
        for (state, node_input) in self
            .record(checker, Self::member(&m, "states", input)?)?
            .iter()
        {
            self.compile_machine_state(
                checker,
                expansion,
                state,
                node_input,
                &declared_names,
                &mut acc,
            )?;
        }
        let events_json = build_machine_events(declared, &acc);
        let transitions: Vec<Value> = acc
            .moves
            .into_iter()
            .map(|mv| {
                let mut t = mv.body;
                t.insert("id".to_owned(), Value::String(mv.id));
                t.insert(
                    "from".to_owned(),
                    Value::Array(mv.from.into_iter().map(Value::String).collect()),
                );
                Value::Object(t)
            })
            .collect();

        let mut obj = Map::new();
        obj.insert(
            "property".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "property", input)?)?),
        );
        if let Some(desc) = m.get("description") {
            obj.insert(
                "description".to_owned(),
                Value::String(self.string(checker, desc)?),
            );
        }
        obj.insert(
            "initial".to_owned(),
            Value::Array(initial.into_iter().map(Value::String).collect()),
        );
        if !acc.terminal.is_empty() {
            obj.insert(
                "terminal".to_owned(),
                Value::Array(acc.terminal.into_iter().map(Value::String).collect()),
            );
        }
        if !acc.immutable.is_empty() {
            obj.insert(
                "immutable".to_owned(),
                Value::Array(acc.immutable.into_iter().map(Value::String).collect()),
            );
        }
        if !transitions.is_empty() {
            obj.insert("transitions".to_owned(), Value::Array(transitions));
        }
        if !acc.endings.is_empty() {
            obj.insert("endings".to_owned(), Value::Array(acc.endings));
        }
        if !events_json.is_empty() {
            obj.insert("events".to_owned(), Value::Array(events_json));
        }
        Ok(Value::Object(obj))
    }

    fn read_machine_events(
        &self,
        checker: &mut Checker,
        m: &Members,
    ) -> Result<MachineEvents, CompileError> {
        let mut declared = Vec::new();
        let mut declared_names = HashSet::new();
        if let Some(ev_node) = m.get("events") {
            for (id, spec) in self.record(checker, ev_node)?.iter() {
                let e = self.closed_record(checker, spec, &["ignore"], &[])?;
                let reason = match e.get("ignore") {
                    Some(r) => Some(self.string(checker, r)?),
                    None => None,
                };
                declared_names.insert(id.to_owned());
                declared.push((id.to_owned(), reason));
            }
        }
        Ok((declared, declared_names))
    }

    fn compile_machine_state(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        state: &str,
        node_input: P<Node>,
        declared_names: &HashSet<String>,
        acc: &mut MachineAcc,
    ) -> Result<(), CompileError> {
        acc.states.push(state.to_owned());
        let node = self.closed_record(
            checker,
            node_input,
            &[
                "description",
                "on",
                "after",
                "final",
                "resting",
                "immutable",
            ],
            &[],
        )?;
        if node.has("immutable") {
            acc.immutable.push(state.to_owned());
        }
        if let Some(final_node) = node.get("final") {
            if node.has("on") || node.has("after") || node.has("resting") {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("final state {state:?} takes no moves and is not resting"),
                    node_input,
                ));
            }
            let outcome = self.read_outcome(checker, final_node)?;
            if outcome == "resting" {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "a final state's outcome is completed, compensated, cancelled or breached_visible; use resting: true for a perpetual state",
                    final_node,
                ));
            }
            acc.terminal.push(state.to_owned());
            let mut e = Map::new();
            e.insert("state".to_owned(), Value::String(state.to_owned()));
            e.insert("outcome".to_owned(), Value::String(outcome));
            acc.endings.push(Value::Object(e));
            return Ok(());
        }
        if node.has("resting") {
            let mut e = Map::new();
            e.insert("state".to_owned(), Value::String(state.to_owned()));
            e.insert("outcome".to_owned(), Value::String("resting".to_owned()));
            acc.endings.push(Value::Object(e));
        }
        if let Some(on_node) = node.get("on") {
            for (event, move_input) in self.record(checker, on_node)?.iter() {
                let body = self.compile_machine_move(
                    checker,
                    expansion,
                    event,
                    move_input,
                    declared_names.contains(event),
                )?;
                add_machine_move(&mut acc.moves, event, state, body, move_input)?;
            }
        }
        if let Some(after_node) = node.get("after") {
            self.compile_machine_after(checker, state, after_node, &mut acc.moves)?;
        }
        Ok(())
    }

    fn compile_machine_after(
        &self,
        checker: &mut Checker,
        state: &str,
        after_node: P<Node>,
        moves: &mut Vec<MachineMove>,
    ) -> Result<(), CompileError> {
        let a = self.closed_record(
            checker,
            after_node,
            &["days", "hours", "minutes", "to", "id"],
            &["to"],
        )?;
        let mut parts = Members::default();
        for key in ["days", "hours", "minutes"] {
            if let Some(v) = a.get(key) {
                parts.insert(key.to_owned(), v);
            }
        }
        let id = match a.get("id") {
            Some(n) => self.string(checker, n)?,
            None => format!("{state}_timeout"),
        };
        let mut trig = Map::new();
        trig.insert("kind".to_owned(), Value::String("timer".to_owned()));
        trig.insert(
            "millis".to_owned(),
            Value::String(self.millis_of(checker, &parts, after_node)?),
        );
        let mut body = Map::new();
        body.insert(
            "to".to_owned(),
            Value::String(self.string(checker, Self::member(&a, "to", after_node)?)?),
        );
        body.insert("trigger".to_owned(), Value::Object(trig));
        add_machine_move(moves, &id, state, body, after_node)
    }

    fn compile_machine_move(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        event: &str,
        move_input: P<Node>,
        is_declared_event: bool,
    ) -> Result<Map<String, Value>, CompileError> {
        let deref = self.deref(checker, move_input)?;
        let fields = if deref.kind() == Kind::ObjectLiteralExpression {
            self.closed_record(
                checker,
                deref,
                &[
                    "to",
                    "guards",
                    "guardRefusalCode",
                    "mode",
                    "compensatedBy",
                    "description",
                ],
                &["to"],
            )?
        } else {
            let mut single = Members::default();
            single.insert("to".to_owned(), move_input);
            single
        };
        let mut body = Map::new();
        if let Some(desc) = fields.get("description") {
            body.insert(
                "description".to_owned(),
                Value::String(self.string(checker, desc)?),
            );
        }
        body.insert(
            "to".to_owned(),
            Value::String(self.string(checker, Self::member(&fields, "to", move_input)?)?),
        );
        self.append_transition_guards_and_mode(checker, expansion, &fields, &mut body)?;
        if is_declared_event {
            let mut trig = Map::new();
            trig.insert("kind".to_owned(), Value::String("event".to_owned()));
            trig.insert("event".to_owned(), Value::String(event.to_owned()));
            body.insert("trigger".to_owned(), Value::Object(trig));
        }
        Ok(body)
    }

    fn compile_transition(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &[
                "id",
                "description",
                "from",
                "to",
                "guards",
                "guardRefusalCode",
                "after",
                "on",
                "mode",
                "compensatedBy",
            ],
            &["id", "from", "to"],
        )?;
        let after = m.get("after");
        let on = m.get("on");
        if after.is_some() && on.is_some() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a transition fires on a timer (after) or an event (on), not both",
                input,
            ));
        }
        let mut obj = Map::new();
        obj.insert(
            "id".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "id", input)?)?),
        );
        if let Some(desc) = m.get("description") {
            obj.insert(
                "description".to_owned(),
                Value::String(self.string(checker, desc)?),
            );
        }
        obj.insert(
            "from".to_owned(),
            Value::Array(
                self.strings(checker, Self::member(&m, "from", input)?)?
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
        obj.insert(
            "to".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "to", input)?)?),
        );
        self.append_transition_guards_and_mode(checker, expansion, &m, &mut obj)?;
        if let Some(after_node) = after {
            let parts =
                self.closed_record(checker, after_node, &["days", "hours", "minutes"], &[])?;
            let mut trig = Map::new();
            trig.insert("kind".to_owned(), Value::String("timer".to_owned()));
            trig.insert(
                "millis".to_owned(),
                Value::String(self.millis_of(checker, &parts, after_node)?),
            );
            obj.insert("trigger".to_owned(), Value::Object(trig));
        }
        if let Some(on_node) = on {
            let mut trig = Map::new();
            trig.insert("kind".to_owned(), Value::String("event".to_owned()));
            trig.insert(
                "event".to_owned(),
                Value::String(self.string(checker, on_node)?),
            );
            obj.insert("trigger".to_owned(), Value::Object(trig));
        }
        Ok(Value::Object(obj))
    }

    fn append_transition_guards_and_mode(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        fields: &Members,
        obj: &mut Map<String, Value>,
    ) -> Result<(), CompileError> {
        if let Some(guards_node) = fields.get("guards") {
            let mut gs = Vec::new();
            for g in self.list(checker, guards_node)? {
                gs.push(Value::String(self.zen(checker, expansion, g, "self")?));
            }
            if !gs.is_empty() {
                obj.insert("guards".to_owned(), Value::Array(gs));
            }
        }
        if let Some(code) = fields.get("guardRefusalCode") {
            obj.insert(
                "guardRefusalCode".to_owned(),
                Value::String(self.string(checker, code)?),
            );
        }
        if let Some(mode_node) = fields.get("mode") {
            let mode = self.string(checker, mode_node)?;
            if mode != "fact" && mode != "request" {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "mode is \"fact\" or \"request\"",
                    mode_node,
                ));
            }
            obj.insert("mode".to_owned(), Value::String(mode));
        }
        if let Some(comp) = fields.get("compensatedBy") {
            obj.insert(
                "compensatedBy".to_owned(),
                Value::String(self.string(checker, comp)?),
            );
        }
        Ok(())
    }

    fn read_outcome(&self, checker: &mut Checker, input: P<Node>) -> Result<String, CompileError> {
        let v = self.string(checker, input)?;
        if !matches!(
            v.as_str(),
            "completed" | "compensated" | "cancelled" | "breached_visible" | "resting"
        ) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "an ending outcome is completed, compensated, cancelled, breached_visible or resting",
                input,
            ));
        }
        Ok(v)
    }

    fn compile_lifecycle_event(
        &self,
        checker: &mut Checker,
        id: &str,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(checker, input, &["ignoredIn"], &[])?;
        let mut obj = Map::new();
        obj.insert("id".to_owned(), Value::String(id.to_owned()));
        if let Some(ignored_node) = m.get("ignoredIn") {
            let mut items = Vec::new();
            for item in self.list(checker, ignored_node)? {
                let entry = self.closed_record(
                    checker,
                    item,
                    &["states", "because"],
                    &["states", "because"],
                )?;
                let mut ig = Map::new();
                ig.insert(
                    "states".to_owned(),
                    Value::Array(
                        self.strings(checker, Self::member(&entry, "states", item)?)?
                            .into_iter()
                            .map(Value::String)
                            .collect(),
                    ),
                );
                ig.insert(
                    "reason".to_owned(),
                    Value::String(self.string(checker, Self::member(&entry, "because", item)?)?),
                );
                items.push(Value::Object(ig));
            }
            if !items.is_empty() {
                obj.insert("ignoredIn".to_owned(), Value::Array(items));
            }
        }
        Ok(Value::Object(obj))
    }

    fn millis_of(
        &self,
        checker: &mut Checker,
        m: &Members,
        input: P<Node>,
    ) -> Result<String, CompileError> {
        let mut total: u128 = 0;
        for (key, unit) in [
            ("days", 86_400_000_u128),
            ("hours", 3_600_000_u128),
            ("minutes", 60_000_u128),
        ] {
            if let Some(node) = m.get(key) {
                let val = self.deref(checker, node)?;
                let text = val.text();
                if val.kind() != Kind::NumericLiteral
                    || text.is_empty()
                    || !text.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("{key} is a whole number literal"),
                        node,
                    ));
                }
                let n = text.parse::<u128>().map_err(|_err| {
                    CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "the duration is longer than u64 milliseconds",
                        input,
                    )
                })?;
                total = total.saturating_add(n.saturating_mul(unit));
            }
        }
        if total == 0 {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a duration needs a positive days, hours or minutes",
                input,
            ));
        }
        if total > u128::from(u64::MAX) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "the duration is longer than u64 milliseconds",
                input,
            ));
        }
        Ok(total.to_string())
    }
}

fn build_machine_events(declared: Vec<DeclaredEvent>, acc: &MachineAcc) -> Vec<Value> {
    let mut events_json = Vec::new();
    for (id, reason) in declared {
        let mut ev = Map::new();
        ev.insert("id".to_owned(), Value::String(id.clone()));
        if let Some(r) = reason {
            let handled: BTreeSet<&str> = acc
                .moves
                .iter()
                .find(|mv| mv.id == id)
                .map_or_default(|mv| mv.from.iter().map(String::as_str).collect());
            let ignored: Vec<Value> = acc
                .states
                .iter()
                .filter(|s| !acc.terminal.contains(s) && !handled.contains(s.as_str()))
                .cloned()
                .map(Value::String)
                .collect();
            if !ignored.is_empty() {
                let mut ig = Map::new();
                ig.insert("states".to_owned(), Value::Array(ignored));
                ig.insert("reason".to_owned(), Value::String(r));
                ev.insert(
                    "ignoredIn".to_owned(),
                    Value::Array(vec![Value::Object(ig)]),
                );
            }
        }
        events_json.push(Value::Object(ev));
    }
    events_json
}

fn add_machine_move(
    moves: &mut Vec<MachineMove>,
    id: &str,
    state: &str,
    body: Map<String, Value>,
    node: P<Node>,
) -> Result<(), CompileError> {
    let val = Value::Object(body.clone());
    let key = compact_canonical_json(&val)
        .map_err(|err| CompileError::new("NORN-FLOW-LITERAL", err.to_string(), node))?;
    if let Some(known) = moves.iter_mut().find(|mv| mv.id == id) {
        if known.key != key {
            let first = known.from.first().map_or("", String::as_str);
            return Err(CompileError::new(
                "NORN-MACHINE-EVENT-CONFLICT",
                format!(
                    "event {id:?} moves differently from {first:?} and {state:?}; a transition has one target and one guard set, so name two events"
                ),
                node,
            ));
        }
        known.from.push(state.to_owned());
        return Ok(());
    }
    moves.push(MachineMove {
        id: id.to_owned(),
        from: vec![state.to_owned()],
        body,
        key,
    });
    Ok(())
}
