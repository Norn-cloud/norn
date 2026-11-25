//! Interaction laws over UI SDK layers, notification surfaces and the authority split (#1762).
//!
//! A page places a UI SDK primitive as a `catalog` node naming `ui-sdk:<component>`; its props are
//! the primitive's options as ZEN over the page machine's fields, and `heldIn` names the machine
//! states it is open in. The laws read the pinned catalog ([`crate::ui_sdk`]) and the page machine:
//!
//! - **L1, dismissal** (`widget.layer.dismissal_refused_without_cause`). A layer refuses dismissal
//!   at one of two levels. *Total*: Escape cannot close it (`dismissible: false`, or a primitive
//!   without Escape). *Lock*: Escape still can, but an outside press or a swipe cannot (an alert
//!   dialog, `closeOnOutsidePress: false`). Either is legal in a `pending` state, whose call has a
//!   deadline and an exit on every outcome, and in an `idle` state with a *way out*: a path of
//!   unguarded transitions on the person's own events to a state the layer is not held in, where no
//!   step starts a call or enters a pending state. The person's events are Escape, which every
//!   person can press (the layer's `onOpenChange` where Escape closes it; its `onDismissAttempt`,
//!   as a dirty form's "Discard changes?", where it refuses Escape), and a control inside the
//!   layer, as Cancel. Asking again into the same state, or between held states, is no way out, and
//!   neither is a Confirm control that sends the command, nor an `onDismissAttempt` that only an
//!   outside press or a swipe raises. A layer that refuses dismissal with no `heldIn` state refuses
//!   it whenever a person opens it: refused.
//!
//!   `heldIn` is the one source of whether a layer is open. A prop reads only the machine's fields,
//!   never its state, and UI SDK keeps a layer whose `open` is set as `open` says whatever Escape,
//!   an outside press, a Close part or Cancel asks (its `.d.ts`: "Pass it to control the dialog";
//!   a controlled component ignores `cancel()`, "since the next value comes from you"). So `open`
//!   is refused on every layer with dismissal attempts, and `defaultOpen`, which opens it on load,
//!   on such a layer that refuses a dismissal or names `heldIn`. A held layer is controlled the
//!   same way: it stays open as long as its state lasts, whatever Escape asks. So a layer that
//!   refuses nothing also needs a way out of every state it is held in that is not pending.
//! - **L3, notifications** (`widget.layer.notification_intrusive`). A toast never holds the page:
//!   it is never held open or modal, never traps focus, and is dismissible or expires. How it is
//!   announced is UI SDK's call (assertive is its default for an error). A native node that
//!   announces changes never also traps focus.
//! - **L5, authority** (`widget.authority.action_outside_binding`). A Norn action is reached only
//!   through its action binding: no machine calls the host's action-call or MCP operation directly.
//!   UI SDK's page agent fills and presses the page; pressing a bound control raises the same
//!   machine event, so an agent's press goes through the same server call. Authority stays the
//!   server's.
//!
//! What a prop is set to when it is not a literal is unknown here and read as its least
//! permissive value (a dynamic `dismissible` may be `false`).

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, WidgetComponent, WidgetMachine, WidgetMachineCall, WidgetMachineEffect,
    WidgetMachineEventSource, WidgetNode, WidgetStateRole, WidgetUx, ZenExpression,
};
use serde_json::Value;

use crate::ui_sdk::{OptionKind, Primitive, primitive_of};
use norn_checker::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

const DISMISSAL_REFUSED: &str = "widget.layer.dismissal_refused_without_cause";
const NOTIFICATION_INTRUSIVE: &str = "widget.layer.notification_intrusive";
const ACTION_OUTSIDE_BINDING: &str = "widget.authority.action_outside_binding";
const INVALID_PROP: &str = "widget.catalog.invalid_prop";

/// Every code these laws can emit.
pub const CODES: [&str; 3] = [
    DISMISSAL_REFUSED,
    NOTIFICATION_INTRUSIVE,
    ACTION_OUTSIDE_BINDING,
];

/// Host operations that invoke a Norn action (`POST /v1/actions/{actionId}`, and `tools/call`
/// over `POST /v1/mcp`). A page reaches an action only through its binding. This crate's tests
/// tie the list to the checked-in `OpenAPI` document.
pub const ACTION_OPERATIONS: [&str; 2] = ["call_action", "post_message"];

/// The `onDismissAttempt` callback: a refused dismissal raised as a machine event.
const DISMISS_ATTEMPT: &str = "onDismissAttempt";
/// The `onOpenChange` callback: a dismissal the layer does not refuse, raised as a machine event.
const OPEN_CHANGE: &str = "onOpenChange";
/// How a finding names a way out (see [`way_out`]).
const WAY_OUT: &str = "a way out: Escape (`onOpenChange` where Escape closes the layer, `onDismissAttempt` where it refuses Escape) or a control inside the layer, reaching a state it is not held in without starting a call or entering a pending state";
const ANNOUNCE: &str = "platform/announce";
const MODAL: &str = "popups/modal";

struct Finding {
    code: &'static str,
    pointer: String,
    message: String,
}

/// Check the layers, notification surfaces and action paths of one widget's UX contract.
#[must_use]
pub(crate) fn check(owner: &ArtifactEnvelope, ux: &WidgetUx) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for (index, page) in ux.pages.iter().enumerate() {
        let base = format!("/payload/ux/pages/{index}");
        let mut nodes = Vec::new();
        collect(&page.root, &format!("{base}/root"), &mut nodes);
        for (node, pointer) in &nodes {
            notification(node, pointer, &mut findings);
            let Some((component, primitive)) = primitive_of(node) else {
                continue;
            };
            let WidgetComponent::Catalog { props, held_in, .. } = &node.component else {
                continue;
            };
            literals(component, primitive, props, pointer, &mut findings);
            let layer = Layer {
                node,
                primitive,
                props,
                held_in,
                pointer,
            };
            layer.check(page.machine.as_ref(), &mut findings);
        }
        if let Some(machine) = &page.machine {
            authority(machine, &format!("{base}/machine"), &mut findings);
        }
    }
    for (index, entry) in ux.catalog.iter().enumerate() {
        if let Some(machine) = &entry.machine {
            authority(
                machine,
                &format!("/payload/ux/catalog/{index}/machine"),
                &mut findings,
            );
        }
    }
    findings
        .into_iter()
        .map(|finding| Diagnostic {
            witness: None,
            code: DiagnosticCode::new(finding.code),
            severity: DiagnosticSeverity::Error,
            message: finding.message,
            span: Some(Span::pointer(finding.pointer)),
            artifact_id: Some(owner.artifact_id.clone()),
            remediation: Some(DiagnosticRemediation::FixDefinition),
        })
        .collect()
}

fn collect<'n>(node: &'n WidgetNode, pointer: &str, out: &mut Vec<(&'n WidgetNode, String)>) {
    out.push((node, pointer.to_owned()));
    for (index, child) in node.children.iter().enumerate() {
        collect(child, &format!("{pointer}/children/{index}"), out);
    }
}

fn descendants<'n>(node: &'n WidgetNode, out: &mut BTreeSet<&'n str>) {
    for child in &node.children {
        out.insert(child.node_id.as_str());
        descendants(child, out);
    }
}

/// What a page sets one option to.
#[derive(Debug, PartialEq)]
enum Prop {
    /// Not set: the option's default applies.
    Absent,
    /// A ZEN expression over the machine's fields, unknown here.
    Computed,
    /// A literal value.
    Literal(Value),
}

impl Prop {
    fn is(&self, value: &Value) -> bool {
        matches!(self, Self::Literal(literal) if literal == value)
    }
}

fn literal(props: &BTreeMap<String, ZenExpression>, name: &str) -> Prop {
    let Some(source) = props.get(name).map(|source| source.as_str().trim()) else {
        return Prop::Absent;
    };
    let single = source
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .filter(|inner| !inner.contains('\''));
    let value = match single {
        Some(inner) => Some(Value::String(inner.to_owned())),
        None => serde_json::from_str::<Value>(source)
            .ok()
            .filter(|value| !value.is_array() && !value.is_object()),
    };
    value.map_or(Prop::Computed, Prop::Literal)
}

/// Whether a boolean option surely holds: absent and defaulting to `default`, or the literal `true`.
fn surely(props: &BTreeMap<String, ZenExpression>, name: &str, default: bool) -> bool {
    match literal(props, name) {
        Prop::Absent => default,
        prop @ (Prop::Computed | Prop::Literal(_)) => prop.is(&Value::Bool(true)),
    }
}

/// A literal prop is one of the values its option's type admits.
fn literals(
    component: &str,
    primitive: &Primitive,
    props: &BTreeMap<String, ZenExpression>,
    pointer: &str,
    findings: &mut Vec<Finding>,
) {
    for name in props.keys() {
        let Prop::Literal(value) = literal(props, name) else {
            continue;
        };
        let admitted = match primitive.options.get(name) {
            Some(OptionKind::Enum(values)) => values.contains(&value),
            Some(OptionKind::Union(kinds, values)) => {
                values.contains(&value)
                    || match &value {
                        Value::Bool(_) => kinds.contains("boolean"),
                        Value::Number(_) => kinds.contains("number"),
                        Value::String(_) => kinds.contains("string"),
                        Value::Null | Value::Array(_) | Value::Object(_) => false,
                    }
            }
            Some(
                OptionKind::Boolean
                | OptionKind::Text
                | OptionKind::Number
                | OptionKind::Callback(_)
                | OptionKind::Other,
            )
            | None => true,
        };
        if !admitted {
            findings.push(Finding {
                code: INVALID_PROP,
                pointer: format!("{pointer}/component/props/{name}"),
                message: format!("`ui-sdk:{component}` option `{name}` does not admit {value}"),
            });
        }
    }
}

struct Layer<'a> {
    node: &'a WidgetNode,
    primitive: &'a Primitive,
    props: &'a BTreeMap<String, ZenExpression>,
    held_in: &'a [String],
    pointer: &'a str,
}

impl Layer<'_> {
    fn check(&self, machine: Option<&WidgetMachine>, findings: &mut Vec<Finding>) {
        let primitive = self.primitive;
        if primitive.dismiss_attempts.is_empty() {
            return;
        }
        let dismissible = surely(self.props, "dismissible", true);
        let escape_closes = dismissible && primitive.dismiss_reasons.contains("escape-key");
        let outside_closes = dismissible
            && primitive.dismiss_reasons.contains("outside-press")
            && surely(self.props, "closeOnOutsidePress", true);
        let locks = !escape_closes
            || (!outside_closes && primitive.dismiss_attempts.contains("outside-press"));
        self.opened(locks, findings);
        let id = &self.node.node_id;
        let at = format!("{}/component/heldIn", self.pointer);
        if self.held_in.is_empty() {
            if locks {
                findings.push(Finding {
                    code: DISMISSAL_REFUSED,
                    pointer: at,
                    message: format!(
                        "layer `{id}` refuses a dismissal whenever it is open: hold it (`heldIn`) only in a pending state, or in an idle state with {WAY_OUT}"
                    ),
                });
            }
            return;
        }
        let mut inside = BTreeSet::new();
        descendants(self.node, &mut inside);
        // Escape is the dismissal every person can make: it closes the layer and raises
        // `onOpenChange`, or it is refused and raises `onDismissAttempt`.
        let escape = if escape_closes {
            Some(OPEN_CHANGE)
        } else {
            primitive
                .dismiss_attempts
                .contains("escape-key")
                .then_some(DISMISS_ATTEMPT)
        };
        for (index, state_id) in self.held_in.iter().enumerate() {
            let held = machine.and_then(|machine| {
                let state = machine
                    .states
                    .iter()
                    .find(|state| &state.state_id == state_id)?;
                Some((machine, state))
            });
            let Some((machine, state)) = held else {
                findings.push(Finding {
                    code: DISMISSAL_REFUSED,
                    pointer: format!("{at}/{index}"),
                    message: format!("layer `{id}` is held in `{state_id}`, which is not declared"),
                });
                continue;
            };
            let out = way_out(machine, id, escape, &inside, self.held_in, state_id);
            let legal = match state.role {
                WidgetStateRole::Pending => true,
                WidgetStateRole::Idle => out,
                WidgetStateRole::Error | WidgetStateRole::Terminal => !locks && out,
            };
            if legal {
                continue;
            }
            let message = if locks {
                let level = if escape_closes || out {
                    "locks out an outside press or swipe"
                } else {
                    "cannot be dismissed at all"
                };
                format!(
                    "layer `{id}` {level} in `{state_id}`: that is legal only while a call is pending, or in an idle state with {WAY_OUT}"
                )
            } else {
                format!(
                    "layer `{id}` is held open in `{state_id}`, so Escape or Close leaves it open unless a transition hears it: give it {WAY_OUT}"
                )
            };
            findings.push(Finding {
                code: DISMISSAL_REFUSED,
                pointer: format!("{at}/{index}"),
                message,
            });
        }
    }

    /// `heldIn` is the one source of whether the layer is open (see the module docs).
    fn opened(&self, locks: bool, findings: &mut Vec<Finding>) {
        let id = &self.node.node_id;
        let at = |name: &str| format!("{}/component/props/{name}", self.pointer);
        if self.props.contains_key("open") {
            findings.push(Finding {
                code: DISMISSAL_REFUSED,
                pointer: at("open"),
                message: format!(
                    "layer `{id}` sets `open`, so it stays as `open` says whatever Escape, an outside press or Cancel asks, in every state: name the states it is open in with `heldIn` instead"
                ),
            });
        }
        let opens_on_load = self.props.contains_key("defaultOpen")
            && !literal(self.props, "defaultOpen").is(&Value::Bool(false));
        if opens_on_load && (locks || !self.held_in.is_empty()) {
            findings.push(Finding {
                code: DISMISSAL_REFUSED,
                pointer: at("defaultOpen"),
                message: format!(
                    "layer `{id}` sets `defaultOpen`, so it opens on load outside its `heldIn` states: a layer that refuses a dismissal or names `heldIn` is open only in its `heldIn` states"
                ),
            });
        }
    }
}

/// Whether a person can leave a layer held in `held_in` from `start`: a path of unguarded
/// transitions on the person's own events (the layer's `escape` event, or a control inside the
/// layer) to a state the layer is not held in, where no step starts a call or enters a pending
/// state. A Confirm control that sends the command is not Cancel, and asking again into the same
/// state, or between held states, never ends.
fn way_out(
    machine: &WidgetMachine,
    layer: &str,
    escape: Option<&str>,
    inside: &BTreeSet<&str>,
    held_in: &[String],
    start: &str,
) -> bool {
    let pending: BTreeSet<&str> = machine
        .states
        .iter()
        .filter(|state| state.role == WidgetStateRole::Pending)
        .map(|state| state.state_id.as_str())
        .collect();
    let by_person = |on: &str| {
        machine.events.iter().any(|event| {
            event.event_id == on
                && matches!(&event.source, WidgetMachineEventSource::User { node_id, emits }
                    if (node_id == layer && emits.is_some() && emits.as_deref() == escape)
                        || inside.contains(node_id.as_str()))
        })
    };
    let mut seen = BTreeSet::from([start]);
    let mut queue = vec![start];
    while let Some(from) = queue.pop() {
        for transition in &machine.transitions {
            let target = transition.target.as_str();
            let step = transition.from == from
                && transition.guard.is_none()
                && transition.component_guard.is_none()
                && by_person(&transition.on)
                && !transition
                    .effects
                    .iter()
                    .any(|effect| matches!(effect, WidgetMachineEffect::Start { .. }))
                && !pending.contains(target);
            if !step {
                continue;
            }
            if !held_in.iter().any(|held| held == target) {
                return true;
            }
            if seen.insert(target) {
                queue.push(target);
            }
        }
    }
    false
}

fn notification(node: &WidgetNode, pointer: &str, findings: &mut Vec<Finding>) {
    let behaviors: BTreeSet<&str> = node.behaviors.iter().map(|b| b.0.as_str()).collect();
    let mut problems = Vec::new();
    if let Some((component, primitive)) = primitive_of(node)
        && primitive.options.contains_key("priority")
        && let WidgetComponent::Catalog { props, held_in, .. } = &node.component
    {
        if !held_in.is_empty() {
            problems.push("it is held open".to_owned());
        }
        if props.contains_key("modal") && !literal(props, "modal").is(&Value::Bool(false)) {
            problems.push("it may be modal".to_owned());
        }
        let kind = literal(props, "type");
        let kind_is = |name: &str| kind.is(&Value::String(name.to_owned()));
        let kind_known = kind != Prop::Computed;
        let expires = match literal(props, "duration") {
            Prop::Absent => kind_known && !kind_is("loading"),
            Prop::Computed => false,
            Prop::Literal(value) => value.as_f64().is_some_and(|ms| ms > 0.0),
        };
        if !expires && !surely(props, "dismissible", true) {
            problems.push("it neither expires nor can be dismissed".to_owned());
        }
        if behaviors.contains(MODAL) {
            problems.push("it traps focus".to_owned());
        }
        if !problems.is_empty() {
            findings.push(Finding {
                code: NOTIFICATION_INTRUSIVE,
                pointer: pointer.to_owned(),
                message: format!(
                    "notification `{}` (`ui-sdk:{component}`) must not hold the page: {}",
                    node.node_id,
                    problems.join("; ")
                ),
            });
        }
        return;
    }
    if behaviors.contains(ANNOUNCE) && behaviors.contains(MODAL) {
        findings.push(Finding {
            code: NOTIFICATION_INTRUSIVE,
            pointer: format!("{pointer}/behaviors"),
            message: format!(
                "node `{}` announces changes and traps focus: a live region is never modal",
                node.node_id
            ),
        });
    }
}

/// `callAction`, `call-action` and `call.action` all name `call_action`.
fn operation_key(operation_id: &str) -> String {
    let mut out = String::new();
    for c in operation_id.chars() {
        if c.is_ascii_uppercase() {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '-' || c == '.' {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    out
}

fn authority(machine: &WidgetMachine, base: &str, findings: &mut Vec<Finding>) {
    let mut refuse = |call: &WidgetMachineCall, pointer: String| {
        if let WidgetMachineCall::Operation { operation_id } = call
            && ACTION_OPERATIONS.contains(&operation_key(operation_id).as_str())
        {
            findings.push(Finding {
                code: ACTION_OUTSIDE_BINDING,
                pointer,
                message: format!(
                    "operation `{operation_id}` calls a Norn action outside its action binding: declare an action binding and start `{{\"kind\": \"action\"}}`"
                ),
            });
        }
    };
    for (index, state) in machine.states.iter().enumerate() {
        if let Some(call) = &state.awaits {
            refuse(call, format!("{base}/states/{index}/awaits"));
        }
    }
    for (index, transition) in machine.transitions.iter().enumerate() {
        for (effect_index, effect) in transition.effects.iter().enumerate() {
            if let WidgetMachineEffect::Start { call, .. } = effect {
                refuse(
                    call,
                    format!("{base}/transitions/{index}/effects/{effect_index}/call"),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_spellings_normalize() {
        assert_eq!(operation_key("callAction"), "call_action");
        assert_eq!(operation_key("call_action"), "call_action");
        assert_eq!(operation_key("post-message"), "post_message");
        assert_eq!(operation_key("decideDecision"), "decide_decision");
    }

    #[test]
    fn action_operations_are_the_openapi_action_routes() {
        let document: Value =
            serde_json::from_str(include_str!("../../../sdks/openapi/norn-api.openapi.json"))
                .expect("openapi");
        let mut found = BTreeSet::new();
        for (path, operations) in document["paths"].as_object().expect("paths") {
            if !(path.starts_with("/v1/actions/") || path == "/v1/mcp") {
                continue;
            }
            if let Some(post) = operations.get("post") {
                found.insert(post["operationId"].as_str().expect("id").to_owned());
            }
        }
        assert_eq!(
            found,
            ACTION_OPERATIONS
                .iter()
                .map(|id| (*id).to_owned())
                .collect()
        );
    }

    #[test]
    fn literals_read_zen_constants() {
        let props = BTreeMap::from([
            ("a".to_owned(), ZenExpression("false".to_owned())),
            ("b".to_owned(), ZenExpression("'polite'".to_owned())),
            ("c".to_owned(), ZenExpression("fields.dirty".to_owned())),
        ]);
        assert_eq!(literal(&props, "a"), Prop::Literal(Value::Bool(false)));
        assert_eq!(
            literal(&props, "b"),
            Prop::Literal(Value::String("polite".into()))
        );
        assert_eq!(literal(&props, "c"), Prop::Computed);
        assert_eq!(literal(&props, "d"), Prop::Absent);
        assert!(!surely(&props, "c", true));
        assert!(surely(&props, "d", true));
    }
}
