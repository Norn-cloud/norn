//! Laws of catalog components (UI-CATALOG-1): custom UI in any framework, joined to Norn only by
//! typed contracts.
//!
//! A catalog entry declares typed props, its output events, its own machine (none for an opaque
//! island), the accessibility behaviours it implements, its density tier and, optionally, the one
//! field type it renders. A page places an entry with a `catalog` node whose props are ZEN
//! expressions over the page machine's fields, and raises the entry's events as its own `user`
//! events with `emits`. The laws keep every one of those joins exact: no undeclared prop, event or
//! entry; every required prop bound and typed; every raised event's payload the entry's own; an
//! island without a machine; at most one renderer per field type. What the implementation does
//! inside its contract is the runtime's to enforce (refused and logged), not this pass's.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{Expression, Type, check_expression};
use norn_ir::{
    ArtifactEnvelope, WidgetCatalogEntry, WidgetCatalogRef, WidgetComponent,
    WidgetMachineEventSource, WidgetMachineField, WidgetNode, WidgetTrust, WidgetUx,
};

use crate::ui_interaction::ComposedUx;
use crate::ui_machine::{field_type, machine_env};
use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

const DUPLICATE_ENTRY: &str = "widget.catalog.duplicate_entry";
const UNKNOWN_ENTRY: &str = "widget.catalog.unknown_entry";
const UNKNOWN_PROP: &str = "widget.catalog.unknown_prop";
const UNBOUND_PROP: &str = "widget.catalog.unbound_prop";
const INVALID_PROP: &str = "widget.catalog.invalid_prop";
const UNKNOWN_EVENT: &str = "widget.catalog.unknown_event";
const EVENT_PAYLOAD_MISMATCH: &str = "widget.catalog.event_payload_mismatch";
const ISLAND_MACHINE: &str = "widget.catalog.island_machine";
const DUPLICATE_RENDERER: &str = "widget.catalog.duplicate_renderer";

/// Every code these laws can emit.
pub const CODES: [&str; 9] = [
    DUPLICATE_ENTRY,
    UNKNOWN_ENTRY,
    UNKNOWN_PROP,
    UNBOUND_PROP,
    INVALID_PROP,
    UNKNOWN_EVENT,
    EVENT_PAYLOAD_MISMATCH,
    ISLAND_MACHINE,
    DUPLICATE_RENDERER,
];

struct Finding {
    code: &'static str,
    pointer: String,
    message: String,
}

/// Check the catalog of one widget's UX contract and every catalog node its pages place.
#[must_use]
pub(crate) fn check(
    owner: &ArtifactEnvelope,
    ux: &WidgetUx,
    composed: &ComposedUx<'_>,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    check_entries(ux, &mut findings);
    for (index, page) in ux.pages.iter().enumerate() {
        let base = format!("/payload/ux/pages/{index}");
        let mut placed: BTreeMap<String, (String, WidgetCatalogEntry)> = BTreeMap::new();
        place(
            ux,
            composed,
            &page.root,
            &format!("{base}/root"),
            page.machine
                .as_ref()
                .map(|m| m.fields.as_slice())
                .unwrap_or_default(),
            &mut placed,
            &mut findings,
        );
        let Some(machine) = &page.machine else {
            continue;
        };
        for (event_index, event) in machine.events.iter().enumerate() {
            let WidgetMachineEventSource::User { node_id, emits } = &event.source else {
                continue;
            };
            let pointer = format!("{base}/machine/events/{event_index}/source");
            match (placed.get(node_id), emits) {
                (None, None) => {}
                (None, Some(_)) => findings.push(Finding {
                    code: UNKNOWN_EVENT,
                    pointer: format!("{pointer}/emits"),
                    message: format!(
                        "`{node_id}` is not a catalog component, so it emits no named event"
                    ),
                }),
                (Some((entry_id, _)), None) => findings.push(Finding {
                    code: UNKNOWN_EVENT,
                    pointer,
                    message: format!(
                        "an event from catalog component `{node_id}` names which of `{entry_id}`'s events it is (`emits`)"
                    ),
                }),
                (Some((entry_id, entry)), Some(emitted)) => {
                    match entry.events.iter().find(|declared| &declared.event_id == emitted) {
                        None => findings.push(Finding {
                            code: UNKNOWN_EVENT,
                            pointer: format!("{pointer}/emits"),
                            message: format!("`{entry_id}` declares no event `{emitted}`"),
                        }),
                        Some(declared) if !same_payload(&declared.payload, &event.payload) => {
                            findings.push(Finding {
                                code: EVENT_PAYLOAD_MISMATCH,
                                pointer: format!("{base}/machine/events/{event_index}/payload"),
                                message: format!(
                                    "the payload of `{}` is not `{entry_id}.{emitted}`'s declared payload",
                                    event.event_id
                                ),
                            });
                        }
                        Some(_) => {}
                    }
                }
            }
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

/// Payloads match member for member: same ids, types and requiredness, in any order.
fn same_payload(declared: &[WidgetMachineField], raised: &[WidgetMachineField]) -> bool {
    let shape = |fields: &[WidgetMachineField]| -> BTreeSet<(String, String, bool)> {
        fields
            .iter()
            .map(|field| {
                (
                    field.field_id.clone(),
                    format!("{:?}", field.value_type),
                    field.required,
                )
            })
            .collect()
    };
    declared.len() == raised.len() && shape(declared) == shape(raised)
}

fn check_entries(ux: &WidgetUx, findings: &mut Vec<Finding>) {
    let mut seen = BTreeSet::new();
    let mut renderers = BTreeMap::new();
    for (index, entry) in ux.catalog.iter().enumerate() {
        let pointer = format!("/payload/ux/catalog/{index}");
        if !seen.insert(entry.entry_id.as_str()) {
            findings.push(Finding {
                code: DUPLICATE_ENTRY,
                pointer: format!("{pointer}/entryId"),
                message: format!("catalog entry `{}` is declared twice", entry.entry_id),
            });
        }
        if entry.trust == WidgetTrust::OpaqueIsland && entry.machine.is_some() {
            findings.push(Finding {
                code: ISLAND_MACHINE,
                pointer: format!("{pointer}/machine"),
                message: format!(
                    "`{}` is an opaque island: only its typed props and declared events take part in the checked machine",
                    entry.entry_id
                ),
            });
        }
        if let Some(value_type) = entry.renders
            && let Some(first) =
                renderers.insert(format!("{value_type:?}"), entry.entry_id.as_str())
        {
            let type_name = format!("{value_type:?}").to_lowercase();
            findings.push(Finding {
                code: DUPLICATE_RENDERER,
                pointer: format!("{pointer}/renders"),
                message: format!(
                    "`{}` and `{first}` both render {type_name}: one renderer per field type",
                    entry.entry_id
                ),
            });
        }
    }
}

fn resolve(
    ux: &WidgetUx,
    composed: &ComposedUx<'_>,
    reference: &WidgetCatalogRef,
) -> Option<WidgetCatalogEntry> {
    let find = |catalog: &[WidgetCatalogEntry]| {
        catalog
            .iter()
            .find(|entry| entry.entry_id == reference.entry_id)
            .cloned()
    };
    match &reference.widget {
        None => find(&ux.catalog),
        Some(widget) => composed(widget).and_then(|other| find(&other.catalog)),
    }
}

fn place(
    ux: &WidgetUx,
    composed: &ComposedUx<'_>,
    node: &WidgetNode,
    pointer: &str,
    fields: &[WidgetMachineField],
    placed: &mut BTreeMap<String, (String, WidgetCatalogEntry)>,
    findings: &mut Vec<Finding>,
) {
    if let WidgetComponent::Catalog { entry, props } = &node.component {
        match resolve(ux, composed, entry) {
            None => findings.push(Finding {
                code: UNKNOWN_ENTRY,
                pointer: format!("{pointer}/component/entry"),
                message: format!("catalog entry `{}` is not declared", entry.entry_id),
            }),
            Some(declared) => {
                check_props(&declared, props, fields, pointer, findings);
                placed.insert(node.node_id.clone(), (declared.entry_id.clone(), declared));
            }
        }
    }
    for (index, child) in node.children.iter().enumerate() {
        place(
            ux,
            composed,
            child,
            &format!("{pointer}/children/{index}"),
            fields,
            placed,
            findings,
        );
    }
}

fn check_props(
    entry: &WidgetCatalogEntry,
    props: &BTreeMap<String, norn_ir::ZenExpression>,
    fields: &[WidgetMachineField],
    pointer: &str,
    findings: &mut Vec<Finding>,
) {
    let field_types: BTreeMap<String, Type> = fields
        .iter()
        .map(|field| (field.field_id.clone(), field_type(field)))
        .collect();
    let event = BTreeMap::new();
    for declared in &entry.props {
        if declared.required && !props.contains_key(&declared.field_id) {
            findings.push(Finding {
                code: UNBOUND_PROP,
                pointer: format!("{pointer}/component/props"),
                message: format!(
                    "`{}` needs its required prop `{}`",
                    entry.entry_id, declared.field_id
                ),
            });
        }
    }
    for (name, expression) in props {
        let at = format!("{pointer}/component/props/{name}");
        let Some(declared) = entry.props.iter().find(|prop| &prop.field_id == name) else {
            findings.push(Finding {
                code: UNKNOWN_PROP,
                pointer: at,
                message: format!("`{}` declares no prop `{name}`", entry.entry_id),
            });
            continue;
        };
        let Ok(env) = machine_env((&field_types, &event)) else {
            continue;
        };
        let checked = Expression::compile(expression.as_str())
            .map_err(|error| vec![error.to_string()])
            .and_then(|compiled| {
                check_expression(&compiled, &env, &field_type(declared))
                    .map(|_| ())
                    .map_err(|errors| errors.into_iter().map(|error| error.message).collect())
            });
        if let Err(errors) = checked {
            for error in errors {
                findings.push(Finding {
                    code: INVALID_PROP,
                    pointer: at.clone(),
                    message: format!("prop `{name}` of `{}`: {error}", entry.entry_id),
                });
            }
        }
    }
}
