//! Interaction laws over UI SDK layers, notification surfaces and the authority split (#1762).
//!
//! A page places a UI SDK primitive as a `catalog` node naming `ui-sdk:<component>`; its props are
//! the primitive's options as ZEN over the page machine's fields, and `heldIn` names the machine
//! states it is open in. The laws read the pinned catalog ([`crate::ui_sdk`]) and the page machine:
//!
//! - **L1, dismissal** (`widget.layer.dismissal_refused_without_cause`). A layer refuses dismissal
//!   at one of two levels. *Total*: Escape cannot close it (`dismissible: false`, or a primitive
//!   without Escape) and the state offers no Cancel and no `onDismissAttempt` question; it is legal
//!   only in a `pending` state, whose call has a deadline and an exit on every outcome. *Lock*: an
//!   outside press or a swipe cannot close it (an alert dialog, `closeOnOutsidePress: false`,
//!   `dismissible: false` with a way out) but Escape, a Cancel control or the question still works;
//!   it is legal in `pending`, in an `idle` state that asks through `onDismissAttempt` (a dirty
//!   form's "Discard changes?"), or in an `idle` confirmation that offers Cancel. A layer that
//!   refuses dismissal with no `heldIn` state refuses it whenever a person opens it: refused.
//! - **L3, notifications** (`widget.layer.notification_intrusive`). A toast never holds the page:
//!   it is never held open or modal, it is announced politely, and it is dismissible or expires. A
//!   native node that announces changes never also traps focus.
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
        let id = &self.node.node_id;
        if !locks {
            return;
        }
        let at = format!("{}/component/heldIn", self.pointer);
        let Some(machine) = machine.filter(|_| !self.held_in.is_empty()) else {
            findings.push(Finding {
                code: DISMISSAL_REFUSED,
                pointer: at,
                message: format!(
                    "layer `{id}` refuses a dismissal whenever it is open: hold it (`heldIn`) only in a pending state, an idle state that asks through `onDismissAttempt`, or an idle confirmation that offers Cancel"
                ),
            });
            return;
        };
        let mut inside = BTreeSet::new();
        descendants(self.node, &mut inside);
        for (index, state_id) in self.held_in.iter().enumerate() {
            let Some(state) = machine
                .states
                .iter()
                .find(|state| &state.state_id == state_id)
            else {
                findings.push(Finding {
                    code: DISMISSAL_REFUSED,
                    pointer: format!("{at}/{index}"),
                    message: format!("layer `{id}` is held in `{state_id}`, which is not declared"),
                });
                continue;
            };
            // An unguarded transition out of this state on a person's event from `raised`.
            let on = |raised: &dyn Fn(&str, Option<&str>, &str) -> bool| {
                machine.transitions.iter().any(|transition| {
                    transition.from == *state_id
                        && transition.guard.is_none()
                        && transition.component_guard.is_none()
                        && machine.events.iter().any(|event| {
                            event.event_id == transition.on
                                && matches!(&event.source, WidgetMachineEventSource::User { node_id, emits }
                                    if raised(node_id, emits.as_deref(), &transition.target))
                        })
                })
            };
            // The layer asks ("Discard changes?") on a refused dismissal.
            let asks = on(&|node_id, emits, _| node_id == id && emits == Some(DISMISS_ATTEMPT));
            // A control inside the layer closes it: Cancel.
            let cancel = on(&|node_id, _, target| {
                inside.contains(node_id) && !self.held_in.iter().any(|held| held == target)
            });
            let leave = escape_closes || asks || cancel;
            let legal = match state.role {
                WidgetStateRole::Pending => true,
                WidgetStateRole::Idle => leave && (asks || cancel),
                WidgetStateRole::Error | WidgetStateRole::Terminal => false,
            };
            if !legal {
                let level = if leave {
                    "locks out an outside press or swipe"
                } else {
                    "cannot be dismissed at all"
                };
                findings.push(Finding {
                    code: DISMISSAL_REFUSED,
                    pointer: format!("{at}/{index}"),
                    message: format!(
                        "layer `{id}` {level} in `{state_id}`: that is legal only while a call is pending{}",
                        if leave {
                            ", or in an idle state that asks through `onDismissAttempt` or offers a Cancel control inside the layer"
                        } else {
                            ""
                        }
                    ),
                });
            }
        }
    }
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
        let polite = match literal(props, "priority") {
            Prop::Absent => kind_known && !kind_is("error"),
            prop @ (Prop::Computed | Prop::Literal(_)) => {
                prop.is(&Value::String("polite".to_owned()))
            }
        };
        if !polite {
            problems.push("it is announced assertively (set `priority` to \"polite\")".to_owned());
        }
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
