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

use crate::ui_interaction::ComposedUx;
use crate::ui_sdk;

use norn_checker::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

const SUBJECT_MISMATCH: &str = "widget.view.subject_mismatch";
const SUBJECT_UNBOUND: &str = "widget.view.subject_unbound";
const BEHAVIOR_UNFIT: &str = "widget.view.behavior_unfit";

/// Every code these laws can emit, for registry tests.
pub const CODES: [&str; 3] = [SUBJECT_MISMATCH, SUBJECT_UNBOUND, BEHAVIOR_UNFIT];

/// Check the view and behaviours of every page in one widget's UX contract.
#[must_use]
pub(crate) fn check(
    owner: &ArtifactEnvelope,
    ux: &WidgetUx,
    composed: &ComposedUx<'_>,
) -> Vec<Diagnostic> {
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
        behaviors(
            ux,
            composed,
            &page.root,
            &format!("{base}/root"),
            &mut findings,
        );
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
            WidgetViewKind::Collection
                | WidgetViewKind::Record
                | WidgetViewKind::Lineage
                | WidgetViewKind::Lifecycle,
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
/// catalog entry's declared ones (local or composed from another widget), or the native
/// renderer's. An unresolvable catalog entry carries none (fails closed).
fn carried(
    ux: &WidgetUx,
    composed: &ComposedUx<'_>,
    component: &WidgetComponent,
) -> BTreeSet<String> {
    if let WidgetComponent::Catalog { entry, .. } = component {
        if let Some(widget) = &entry.widget {
            return composed(widget).map_or_default(|other| {
                other
                    .catalog
                    .iter()
                    .filter(|declared| declared.entry_id == entry.entry_id)
                    .flat_map(|declared| declared.behaviors.iter().map(|b| b.0.clone()))
                    .collect()
            });
        }
        if let Some(name) = ui_sdk::component(&entry.entry_id) {
            return ui_sdk::catalog()
                .and_then(|catalog| catalog.primitives.get(name))
                .map_or_default(|primitive| primitive.behaviors.clone());
        }
        return ux
            .catalog
            .iter()
            .filter(|declared| declared.entry_id == entry.entry_id)
            .flat_map(|declared| declared.behaviors.iter().map(|b| b.0.clone()))
            .collect();
    }
    ui_sdk::native_behaviors(component)
}

fn behaviors(
    ux: &WidgetUx,
    composed: &ComposedUx<'_>,
    root: &WidgetNode,
    root_pointer: &str,
    findings: &mut Vec<(&'static str, String, String)>,
) {
    let known = ui_sdk::catalog().map(|catalog| &catalog.behaviors);
    let mut stack = vec![(root, root_pointer.to_owned())];
    while let Some((node, pointer)) = stack.pop() {
        if !node.behaviors.is_empty() {
            let fits = carried(ux, composed, &node.component);
            for (index, behavior) in node.behaviors.iter().enumerate() {
                let name = behavior.0.as_str();
                let problem = if !known.is_some_and(|known| known.contains(name)) {
                    Some("is not a foundation behaviour of the pinned UI SDK")
                } else if !fits.contains(name) {
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
        }
        for (index, child) in node.children.iter().enumerate().rev() {
            stack.push((child, format!("{pointer}/children/{index}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_view_kind_fits_object_type_subject() {
        let subject = WidgetViewSubject::ObjectType {
            object: norn_ir::widget_ux::ArtifactRef {
                id: "commerce.Order.entity".to_owned(),
                version: "1.0.0".to_owned(),
            },
        };
        assert!(kind_fits_subject(WidgetViewKind::Lifecycle, &subject));
        assert!(!kind_fits_subject(WidgetViewKind::Form, &subject));
    }
}
