//! Laws of a page's place in the UI ontology (UI-VIEW-1).
//!
//! A `WidgetPage.view` names a semantic view kind and the domain concept it presents. The laws keep
//! the ontology honest: the kind fits the subject (a collection presents an object type, a form an
//! action), an object-type or action subject is one the contract actually reads or calls, and each
//! accessibility behaviour sits on a component that can carry it. Rendering stays with renderers;
//! nothing here is visual.

use norn_ir::{
    ArtifactEnvelope, WidgetBehavior, WidgetComponent, WidgetNode, WidgetUx, WidgetViewKind,
    WidgetViewSubject,
};

use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

const SUBJECT_MISMATCH: &str = "widget.view.subject_mismatch";
const SUBJECT_UNBOUND: &str = "widget.view.subject_unbound";
const BEHAVIOR_UNFIT: &str = "widget.view.behavior_unfit";

/// Every code these laws can emit, for registry tests.
pub const CODES: [&str; 3] = [SUBJECT_MISMATCH, SUBJECT_UNBOUND, BEHAVIOR_UNFIT];

/// Check the view and behaviours of every page in one widget's UX contract.
#[must_use]
pub(crate) fn check(owner: &ArtifactEnvelope, ux: &WidgetUx) -> Vec<Diagnostic> {
    let mut findings: Vec<(&'static str, String, String)> = Vec::new();
    for (index, page) in ux.pages.iter().enumerate() {
        let base = format!("/payload/ux/pages/{index}");
        if let Some(view) = &page.view {
            if !kind_fits_subject(view.kind, &view.subject) {
                findings.push((
                    SUBJECT_MISMATCH,
                    format!("{base}/view"),
                    format!(
                        "a {:?} view cannot present a {:?} subject",
                        view.kind, view.subject
                    ),
                ));
            }
            let bound = match &view.subject {
                WidgetViewSubject::ObjectType { object } => ux
                    .data_bindings
                    .iter()
                    .any(|binding| &binding.object == object),
                WidgetViewSubject::Action { action } => ux
                    .action_bindings
                    .iter()
                    .any(|binding| &binding.action == action),
                WidgetViewSubject::Question { .. }
                | WidgetViewSubject::Transform { .. }
                | WidgetViewSubject::Dashboard { .. }
                | WidgetViewSubject::Flow { .. }
                | WidgetViewSubject::Decisions => true,
            };
            if !bound {
                findings.push((
                    SUBJECT_UNBOUND,
                    format!("{base}/view/subject"),
                    "the view's subject is neither read by a data binding nor called by an action binding of this contract".to_owned(),
                ));
            }
        }
        behaviors(&page.root, &format!("{base}/root"), &mut findings);
    }
    findings
        .into_iter()
        .map(|(code, pointer, message)| Diagnostic {
            code: DiagnosticCode::new(code),
            severity: DiagnosticSeverity::Error,
            message,
            span: Some(Span::pointer(pointer)),
            artifact_id: Some(owner.artifact_id.clone()),
            remediation: Some(DiagnosticRemediation::FixDefinition),
        })
        .collect()
}

/// Which subject each view kind presents.
fn kind_fits_subject(kind: WidgetViewKind, subject: &WidgetViewSubject) -> bool {
    matches!(
        (kind, subject),
        (
            WidgetViewKind::Collection | WidgetViewKind::Record | WidgetViewKind::Lineage,
            WidgetViewSubject::ObjectType { .. }
        ) | (WidgetViewKind::Form, WidgetViewSubject::Action { .. })
            | (
                WidgetViewKind::Question,
                WidgetViewSubject::Question { .. } | WidgetViewSubject::Transform { .. }
            )
            | (
                WidgetViewKind::Dashboard,
                WidgetViewSubject::Dashboard { .. }
            )
            | (WidgetViewKind::Run, WidgetViewSubject::Flow { .. })
            | (WidgetViewKind::DecisionInbox, WidgetViewSubject::Decisions)
    )
}

/// Which components can carry a behaviour: item behaviours need items, overlay behaviours need an
/// overlay, announcements need content that changes.
fn behavior_fits(behavior: WidgetBehavior, component: &WidgetComponent) -> bool {
    match behavior {
        WidgetBehavior::RovingFocus | WidgetBehavior::Typeahead | WidgetBehavior::Selection => {
            matches!(
                component,
                WidgetComponent::List { .. }
                    | WidgetComponent::ChildGrid { .. }
                    | WidgetComponent::Navigation { .. }
            )
        }
        WidgetBehavior::DragAndDrop => matches!(
            component,
            WidgetComponent::List { .. } | WidgetComponent::ChildGrid { .. }
        ),
        WidgetBehavior::FocusScope | WidgetBehavior::Popup | WidgetBehavior::Dismiss => {
            matches!(component, WidgetComponent::Dialog { .. })
        }
        WidgetBehavior::LiveRegion => matches!(
            component,
            WidgetComponent::Section { .. }
                | WidgetComponent::Detail { .. }
                | WidgetComponent::List { .. }
        ),
    }
}

fn behaviors(node: &WidgetNode, pointer: &str, findings: &mut Vec<(&'static str, String, String)>) {
    for (index, behavior) in node.behaviors.iter().enumerate() {
        if !behavior_fits(*behavior, &node.component)
            || node
                .behaviors
                .iter()
                .take(index)
                .any(|earlier| earlier == behavior)
        {
            findings.push((
                BEHAVIOR_UNFIT,
                format!("{pointer}/behaviors/{index}"),
                format!(
                    "node `{}` cannot carry behaviour {behavior:?} (wrong component, or listed twice)",
                    node.node_id
                ),
            ));
        }
    }
    for (index, child) in node.children.iter().enumerate() {
        behaviors(child, &format!("{pointer}/children/{index}"), findings);
    }
}
