//! Laws of a page's place in the UI ontology (UI-VIEW-1).
//!
//! A `WidgetPage.view` names a semantic view kind and the domain concept it presents. The laws keep
//! the ontology honest: the kind fits the subject (a collection presents an object type, a form an
//! action), an object-type or action subject is one the contract actually reads or calls, and each
//! accessibility behaviour is a UI SDK foundation behaviour (the generated catalog,
//! [`crate::ui_sdk`]) that the node's component is built from: a UI SDK primitive's own, a custom
//! catalog entry's declared ones, or those the Norn renderer draws a native component with.
//! Rendering stays with renderers; nothing here is visual.

use std::collections::BTreeSet;

use norn_ir::{
    ArtifactEnvelope, WidgetComponent, WidgetNode, WidgetUx, WidgetViewKind, WidgetViewSubject,
};

use crate::ui_sdk;

use norn_checker::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

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
        behaviors(ux, &page.root, &format!("{base}/root"), &mut findings);
    }
    // A custom catalog entry implements behaviours of the pinned UI SDK, by name.
    let known = crate::ui_sdk::catalog().map(|catalog| &catalog.behaviors);
    for (index, entry) in ux.catalog.iter().enumerate() {
        for (at, behavior) in entry.behaviors.iter().enumerate() {
            if !known.is_some_and(|known| known.contains(&behavior.0)) {
                findings.push((
                    BEHAVIOR_UNFIT,
                    format!("/payload/ux/catalog/{index}/behaviors/{at}"),
                    format!(
                        "catalog entry `{}` names `{}`, which is not a foundation behaviour of the pinned UI SDK",
                        entry.entry_id, behavior.0
                    ),
                ));
            }
        }
    }
    findings
        .into_iter()
        .map(|(code, pointer, message)| Diagnostic {
            witness: None,
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

/// The foundation behaviours a node's component can carry: a UI SDK primitive's own, a custom
/// catalog entry's declared ones (an entry of another widget is judged there), or the native
/// renderer's. `None` when every catalog behaviour fits (an entry this contract cannot see).
fn carried(ux: &WidgetUx, component: &WidgetComponent) -> Option<BTreeSet<String>> {
    if let WidgetComponent::Catalog { entry, .. } = component {
        if entry.widget.is_some() {
            return None;
        }
        if ui_sdk::component(&entry.entry_id).is_some() {
            return Some(
                ui_sdk::entry(&entry.entry_id)
                    .map_or_default(|entry| entry.behaviors.into_iter().map(|b| b.0).collect()),
            );
        }
        return Some(
            ux.catalog
                .iter()
                .filter(|declared| declared.entry_id == entry.entry_id)
                .flat_map(|declared| declared.behaviors.iter().map(|b| b.0.clone()))
                .collect(),
        );
    }
    Some(ui_sdk::native_behaviors(component))
}

fn behaviors(
    ux: &WidgetUx,
    node: &WidgetNode,
    pointer: &str,
    findings: &mut Vec<(&'static str, String, String)>,
) {
    let known = ui_sdk::catalog().map(|catalog| &catalog.behaviors);
    let fits = carried(ux, &node.component);
    for (index, behavior) in node.behaviors.iter().enumerate() {
        let name = behavior.0.as_str();
        let problem = if !known.is_some_and(|known| known.contains(name)) {
            Some("is not a foundation behaviour of the pinned UI SDK")
        } else if fits.as_ref().is_some_and(|fits| !fits.contains(name)) {
            Some("is not one its component is built from")
        } else if node
            .behaviors
            .iter()
            .take(index)
            .any(|earlier| earlier == behavior)
        {
            Some("is listed twice")
        } else {
            None
        };
        if let Some(problem) = problem {
            findings.push((
                BEHAVIOR_UNFIT,
                format!("{pointer}/behaviors/{index}"),
                format!("behaviour `{name}` of node `{}` {problem}", node.node_id),
            ));
        }
    }
    for (index, child) in node.children.iter().enumerate() {
        behaviors(ux, child, &format!("{pointer}/children/{index}"), findings);
    }
}
