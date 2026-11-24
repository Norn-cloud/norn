//! Baseline widget page composition, semantic ID import, and bounded overrides.

use std::collections::BTreeSet;

use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, WidgetComponent, WidgetFieldTarget,
    WidgetNode, WidgetOverrideValue, WidgetUx,
};

use norn_checker::Diagnostic;
use norn_checker::entity_scopes::ScopedView;

use super::{ActionSurface, Usage, UxChecker};

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_overrides(&mut self) {
        let overrides = self.ux.overrides.clone();
        let mut operations = BTreeSet::new();
        for (index, override_) in overrides.iter().enumerate() {
            let path = format!("/payload/ux/overrides/{index}");
            let operation = match &override_.value {
                WidgetOverrideValue::Label { .. } => "label",
                WidgetOverrideValue::Order { .. } => "order",
                WidgetOverrideValue::Grouping { .. } => "grouping",
                WidgetOverrideValue::Visibility { .. } => "visibility",
                WidgetOverrideValue::Control { .. } => "control",
                WidgetOverrideValue::ActionPlacement { .. } => "action_placement",
            };
            if !operations.insert((override_.target_semantic_id.as_str(), operation)) {
                self.error(
                    &format!("{path}/value"),
                    "a target semantic id may receive each override operation at most once",
                );
            }
            if !self
                .composed_semantic_ids
                .contains(&override_.target_semantic_id)
            {
                self.error(
                    &format!("{path}/targetSemanticId"),
                    "overrides must target a semantic id from a composed baseline page",
                );
            }
            let Some(target) = self
                .semantic_nodes
                .get(&override_.target_semantic_id)
                .cloned()
            else {
                self.error(
                    &format!("{path}/targetSemanticId"),
                    "override target does not exist in the composed component tree",
                );
                continue;
            };
            match &override_.value {
                WidgetOverrideValue::Label { value } => {
                    if value.0.is_empty() || value.0.values().any(|text| text.trim().is_empty()) {
                        self.error(
                            &format!("{path}/value"),
                            "label override must contain nonblank localized text",
                        );
                    }
                }
                WidgetOverrideValue::Order { .. } => {}
                WidgetOverrideValue::Grouping { value } => {
                    if value.trim().is_empty() {
                        self.error(
                            &format!("{path}/value"),
                            "grouping override must not be blank",
                        );
                    }
                }
                WidgetOverrideValue::Visibility { condition } => {
                    self.check_condition(condition, &format!("{path}/value/condition"));
                }
                WidgetOverrideValue::Control { control } => {
                    self.check_display_initialization(
                        *control,
                        &target.form_input_initialization,
                        &format!("{path}/value/control"),
                    );
                    let (Some(value_type), true) = (target.value_type, target.control_target)
                    else {
                        self.error(
                            &format!("{path}/value/control"),
                            "control override target must be a field semantic id",
                        );
                        continue;
                    };
                    self.check_control_type_with_option_type(
                        *control,
                        None,
                        target.option_key_type.as_ref(),
                        &value_type,
                        &path,
                    );
                }
                WidgetOverrideValue::ActionPlacement { placement_id } => {
                    if !target.action_target {
                        self.error(
                            &format!("{path}/value/placementId"),
                            "action placement override target must be an action semantic id",
                        );
                    }
                    if !self.placements.contains_key(placement_id.as_str()) {
                        self.error(
                            &format!("{path}/value/placementId"),
                            "action placement override names an unknown placement",
                        );
                    }
                }
            }
        }
    }

    pub(super) fn check_composition(
        &mut self,
        widget: &norn_ir::widget_ux::ArtifactRef,
        page_id: &str,
        host_page_id: &str,
        path: &str,
    ) {
        let Some(artifact) = self.resolve_ref(
            &widget.id,
            &widget.version,
            &ArtifactKind::Widget,
            &format!("{path}/widget"),
        ) else {
            return;
        };
        let identity = (
            widget.id.clone(),
            widget.version.clone(),
            page_id.to_owned(),
        );
        if self.composition_stack.contains(&identity) {
            self.error(path, "widget composition contains a recursive page cycle");
            return;
        }
        let ArtifactPayload::Widget(payload) = &artifact.payload else {
            return;
        };
        let Some(raw_ux) = payload.ux.as_value() else {
            self.error(path, "composed widget has no typed UX contract");
            return;
        };
        let Ok(ux) = serde_json::to_value(raw_ux).and_then(serde_json::from_value::<WidgetUx>)
        else {
            self.error(path, "composed widget root cannot be decoded");
            return;
        };
        let Some(selected_page) = ux.pages.iter().find(|page| page.page_id == page_id) else {
            self.error(
                &format!("{path}/pageId"),
                "composed widget page does not exist",
            );
            return;
        };
        if selected_page
            .context_params
            .iter()
            .any(|param| param.required)
        {
            self.error(
                &format!("{path}/pageId"),
                "composition cannot supply required context parameters to its baseline page",
            );
        }
        let mut selected_ids = BTreeSet::new();
        let mut selected_stack = BTreeSet::new();
        collect_composed_page_semantic_ids(
            self.all,
            artifact,
            &ux,
            page_id,
            &mut selected_stack,
            &mut selected_ids,
        );
        let nested_view = ScopedView::of(artifact, &[], self.all, self.registry);
        let mut nested_findings = page_findings(artifact, &ux, page_id, &nested_view);
        let mut nested = UxChecker::new(
            artifact,
            self.all,
            nested_view.catalog(),
            self.registry,
            &ux,
            &mut nested_findings,
        );
        nested.enforce_effect_root = false;
        nested.composition_stack.clone_from(&self.composition_stack);
        nested.composition_stack.insert(identity);
        nested.check();
        self.import_rendered_lists(&nested, page_id, host_page_id, path);
        self.import_composed_semantics(&nested, &selected_ids, host_page_id, path);
        drop(nested);
        if !nested_findings.is_empty() {
            self.error(
                path,
                "composed widget page or one of its referenced pages fails typed UX validation",
            );
        }
    }

    fn import_composed_semantics(
        &mut self,
        source: &UxChecker<'_, '_, '_>,
        selected_ids: &BTreeSet<String>,
        host_page_id: &str,
        path: &str,
    ) {
        for (semantic_id, target) in source
            .semantic_nodes
            .iter()
            .filter(|(id, _)| selected_ids.contains(*id))
        {
            if self.local_semantic_ids.contains(semantic_id)
                || self.local_field_ids.contains(semantic_id)
                || self.semantic_nodes.contains_key(semantic_id)
            {
                self.error(
                    path,
                    format!(
                        "composed semantic id {semantic_id:?} collides with another component id"
                    ),
                );
                continue;
            }
            self.composed_semantic_ids.insert(semantic_id.clone());
            if source.form_owned_semantics.contains(semantic_id) {
                self.form_owned_semantics.insert(semantic_id.clone());
            }
            self.page_uses
                .entry(Usage::Semantic(semantic_id.clone()))
                .or_default()
                .insert(Some(host_page_id.to_owned()));
            if let Some(surface) = source.action_surfaces.get(semantic_id) {
                self.action_surfaces
                    .insert(semantic_id.clone(), surface.clone());
            }
            if let Some(surfaces) = source
                .usage_surfaces
                .get(&Usage::Semantic(semantic_id.clone()))
            {
                for surface in surfaces {
                    let surface = match surface {
                        ActionSurface::List(id) | ActionSurface::Detail(id)
                            if source.bindings.get(id.as_str()).copied()
                                != self.bindings.get(id.as_str()).copied() =>
                        {
                            ActionSurface::Page
                        }
                        ActionSurface::Form(id)
                            if source.forms.get(id.as_str()).copied()
                                != self.forms.get(id.as_str()).copied() =>
                        {
                            ActionSurface::Page
                        }
                        ActionSurface::Page
                        | ActionSurface::List(_)
                        | ActionSurface::Detail(_)
                        | ActionSurface::Form(_) => surface.clone(),
                    };
                    self.add_surface_use(Usage::Semantic(semantic_id.clone()), &surface);
                }
            }
            self.semantic_nodes
                .insert(semantic_id.clone(), target.clone());
        }
    }

    fn import_rendered_lists(
        &mut self,
        source: &UxChecker<'_, '_, '_>,
        page_id: &str,
        host_page_id: &str,
        path: &str,
    ) {
        for (id, pages) in &source.rendered_lists {
            if !pages.contains(&Some(page_id.to_owned())) {
                continue;
            }
            let Some(binding) = source.bindings.get(id.as_str()).copied() else {
                continue;
            };
            if self.bindings.get(id.as_str()).copied() != Some(binding) {
                continue;
            }
            let Some(selection) = source.list_selections.get(id).copied() else {
                continue;
            };
            if self
                .list_selections
                .get(id)
                .is_some_and(|mode| *mode != selection)
            {
                self.error(
                    path,
                    "composed list cannot use a conflicting host selection mode",
                );
                continue;
            }
            self.list_selections.insert(id.clone(), selection);
            self.list_row_contexts.insert(id.clone());
            self.rendered_lists
                .entry(id.clone())
                .or_default()
                .insert(Some(host_page_id.to_owned()));
        }
    }
}

fn collect_composed_page_semantic_ids(
    all: &[ArtifactEnvelope],
    artifact: &ArtifactEnvelope,
    ux: &WidgetUx,
    page_id: &str,
    stack: &mut BTreeSet<(String, String, String)>,
    out: &mut BTreeSet<String>,
) {
    let identity = (
        artifact.artifact_id.clone(),
        artifact.version.clone(),
        page_id.to_owned(),
    );
    if !stack.insert(identity.clone()) {
        return;
    }
    if let Some(page) = ux.pages.iter().find(|page| page.page_id == page_id) {
        collect_tree_semantic_ids(all, ux, &page.root, stack, out);
    }
    stack.remove(&identity);
}

fn collect_tree_semantic_ids(
    all: &[ArtifactEnvelope],
    ux: &WidgetUx,
    node: &WidgetNode,
    stack: &mut BTreeSet<(String, String, String)>,
    out: &mut BTreeSet<String>,
) {
    if let Some(id) = &node.semantic_id {
        out.insert(id.clone());
    }
    match &node.component {
        WidgetComponent::Section { section } => {
            for property in &section.fields {
                for field in &ux.fields {
                    if matches!(&field.target, WidgetFieldTarget::Property { property: target }
                        if target.binding_id == property.binding_id && target.property_id == property.property_id)
                    {
                        out.insert(field.field_id.clone());
                    }
                }
            }
        }
        WidgetComponent::List { fields, .. } => {
            for property in fields {
                for field in &ux.fields {
                    if matches!(&field.target, WidgetFieldTarget::Property { property: target }
                        if target.binding_id == property.binding_id && target.property_id == property.property_id)
                    {
                        out.insert(field.field_id.clone());
                    }
                }
            }
        }
        WidgetComponent::ChildGrid { field_ids, .. } => {
            out.extend(field_ids.iter().cloned());
        }
        WidgetComponent::Field { field_id } => {
            out.insert(field_id.clone());
        }
        WidgetComponent::Composition { widget, page_id } => {
            if let Some(composed) = all.iter().find(|candidate| {
                candidate.kind() == ArtifactKind::Widget
                    && candidate.artifact_id == widget.id
                    && candidate.version == widget.version
            }) && let ArtifactPayload::Widget(payload) = &composed.payload
                && let Some(raw_ux) = payload.ux.as_value()
                && let Ok(composed_ux) =
                    serde_json::to_value(raw_ux).and_then(serde_json::from_value::<WidgetUx>)
            {
                collect_composed_page_semantic_ids(
                    all,
                    composed,
                    &composed_ux,
                    page_id,
                    stack,
                    out,
                );
            }
        }
        WidgetComponent::Form { .. }
        | WidgetComponent::Action { .. }
        | WidgetComponent::Navigation { .. }
        | WidgetComponent::Control { .. }
        | WidgetComponent::Catalog { .. }
        | WidgetComponent::Detail { .. }
        | WidgetComponent::Dialog { .. } => {}
    }
    for child in &node.children {
        collect_tree_semantic_ids(all, ux, child, stack, out);
    }
}

/// The lifecycle composition and catalog findings of the composed page
/// `page_id` of `widget`, read under the composed widget's own scope and
/// dependencies.
fn page_findings(
    widget: &ArtifactEnvelope,
    ux: &WidgetUx,
    page_id: &str,
    view: &ScopedView<'_>,
) -> Vec<Diagnostic> {
    let Some(index) = ux.pages.iter().position(|page| page.page_id == page_id) else {
        return Vec::new();
    };
    let names_lifecycle = ux.pages.get(index).is_some_and(|page| {
        page.machine
            .as_ref()
            .is_some_and(|machine| machine.lifecycle.is_some())
    });
    let lifecycle = names_lifecycle
        .then(|| crate::ui_lifecycle::check(widget, ux, view.pool(), view.model()))
        .unwrap_or_default();
    let composed =
        |other: &norn_ir::WidgetArtifactRef| super::composed_ux(widget, view.pool(), other);
    let prefix = format!("/payload/ux/pages/{index}/");
    lifecycle
        .into_iter()
        .chain(crate::ui_catalog::check(widget, ux, &composed))
        .filter(|finding| {
            finding
                .span
                .as_ref()
                .is_some_and(|span| span.pointer.starts_with(&prefix))
        })
        .collect()
}
