//! Stable ID uniqueness, render-use indexing, and page-use propagation.

use std::collections::BTreeSet;

use norn_ir::{
    ArtifactKind, WidgetComponent, WidgetExpressionInputs, WidgetFieldTarget, WidgetFilterValue,
    WidgetNavigationDestination, WidgetNode, WidgetOverrideValue,
};

use super::bindings::binding_record_dependencies;
use super::{
    ActionSurface, FormInputInitialization, SemanticNode, SurfaceContext, Usage, UxChecker,
    path_index,
};

impl UxChecker<'_, '_, '_> {
    pub(super) fn index_render_uses(&mut self) {
        if let Some(app) = &self.ux.app {
            for id in &app.navigation_item_ids {
                self.page_uses
                    .entry(Usage::Navigation(id.clone()))
                    .or_default()
                    .insert(None);
            }
        }
        for (index, page) in self.ux.pages.clone().iter().enumerate() {
            self.index_node_uses(
                &page.root,
                &page.page_id,
                &ActionSurface::Page,
                &format!("/payload/ux/pages/{index}/root"),
            );
        }
    }

    fn index_node_uses(
        &mut self,
        node: &WidgetNode,
        page_id: &str,
        surface: &ActionSurface,
        path: &str,
    ) {
        let form_owned = self.form_owned_node(node, surface);
        if let Some(id) = &node.semantic_id {
            self.add_page_use(Usage::Semantic(id.clone()), page_id);
            self.add_surface_use(Usage::Semantic(id.clone()), surface);
            if form_owned {
                self.form_owned_semantics.insert(id.clone());
            }
            if matches!(&node.component, WidgetComponent::Action { .. }) {
                self.action_surfaces
                    .insert(id.clone(), self.surface_context(surface));
            }
        }
        let child_surface =
            self.index_component_uses(&node.component, page_id, surface, form_owned, path);
        for (index, child) in node.children.iter().enumerate() {
            self.index_node_uses(
                child,
                page_id,
                &child_surface,
                &format!("{path}/children/{index}"),
            );
        }
    }

    fn index_component_uses(
        &mut self,
        component: &WidgetComponent,
        page_id: &str,
        surface: &ActionSurface,
        form_owned: bool,
        path: &str,
    ) -> ActionSurface {
        match component {
            WidgetComponent::Section { section } => {
                for property in &section.fields {
                    self.index_property_use(property, page_id, surface);
                }
                surface.clone()
            }
            WidgetComponent::List {
                data_binding_id,
                fields,
                ..
            } => {
                self.add_page_use(Usage::Binding(data_binding_id.clone()), page_id);
                self.rendered_lists
                    .entry(data_binding_id.clone())
                    .or_default()
                    .insert(Some(page_id.to_owned()));
                for property in fields {
                    self.index_property_use(
                        property,
                        page_id,
                        &ActionSurface::List(data_binding_id.clone()),
                    );
                }
                ActionSurface::List(data_binding_id.clone())
            }
            WidgetComponent::Detail { data_binding_id } => {
                self.add_page_use(Usage::Binding(data_binding_id.clone()), page_id);
                ActionSurface::Detail(data_binding_id.clone())
            }
            WidgetComponent::ChildGrid {
                form_id,
                data_binding_id,
                field_ids,
                ..
            } => {
                if let Some(binding_id) = data_binding_id {
                    self.add_page_use(Usage::Binding(binding_id.clone()), page_id);
                    self.rendered_lists
                        .entry(binding_id.clone())
                        .or_default()
                        .insert(Some(page_id.to_owned()));
                }
                for field_id in field_ids {
                    self.add_page_use(Usage::Field(field_id.clone()), page_id);
                    self.add_page_use(Usage::Semantic(field_id.clone()), page_id);
                    let target_surface = data_binding_id
                        .as_ref()
                        .map_or_else(|| surface.clone(), |id| ActionSurface::List(id.clone()));
                    self.add_surface_use(Usage::Field(field_id.clone()), &target_surface);
                    self.add_surface_use(Usage::Semantic(field_id.clone()), &target_surface);
                    self.form_owned_semantics.insert(field_id.clone());
                }
                ActionSurface::Form(form_id.clone())
            }
            // A control only raises page machine events; `ui_machine` checks its uses.
            WidgetComponent::Dialog { .. }
            | WidgetComponent::Control { .. }
            | WidgetComponent::Catalog { .. } => surface.clone(),
            WidgetComponent::Form { form_id } => {
                self.add_page_use(Usage::Form(form_id.clone()), page_id);
                self.rendered_forms
                    .entry(form_id.clone())
                    .or_default()
                    .insert(Some(page_id.to_owned()));
                ActionSurface::Form(form_id.clone())
            }
            WidgetComponent::Field { field_id } => {
                self.add_page_use(Usage::Field(field_id.clone()), page_id);
                self.add_page_use(Usage::Semantic(field_id.clone()), page_id);
                self.add_surface_use(Usage::Field(field_id.clone()), surface);
                self.add_surface_use(Usage::Semantic(field_id.clone()), surface);
                if form_owned {
                    self.form_owned_semantics.insert(field_id.clone());
                }
                surface.clone()
            }
            WidgetComponent::Action { placement_id } => {
                self.add_page_use(Usage::Placement(placement_id.clone()), page_id);
                self.add_surface_use(Usage::Placement(placement_id.clone()), surface);
                surface.clone()
            }
            WidgetComponent::Navigation { item_id } => {
                self.add_page_use(Usage::Navigation(item_id.clone()), page_id);
                self.add_surface_use(Usage::Navigation(item_id.clone()), surface);
                surface.clone()
            }
            WidgetComponent::Composition {
                widget,
                page_id: source_page,
            } => {
                self.check_composition(widget, source_page, page_id, &format!("{path}/component"));
                surface.clone()
            }
        }
    }

    fn surface_context(&self, surface: &ActionSurface) -> SurfaceContext {
        let binding = match surface {
            ActionSurface::List(id) | ActionSurface::Detail(id) => {
                self.bindings.get(id.as_str()).copied().cloned()
            }
            ActionSurface::Page | ActionSurface::Form(_) => None,
        };
        let form = match surface {
            ActionSurface::Form(id) => self.forms.get(id.as_str()).copied().cloned(),
            ActionSurface::Page | ActionSurface::List(_) | ActionSurface::Detail(_) => None,
        };
        let selection = match surface {
            ActionSurface::List(id) => self.list_selections.get(id).copied(),
            ActionSurface::Page | ActionSurface::Detail(_) | ActionSurface::Form(_) => None,
        };
        SurfaceContext {
            surface: surface.clone(),
            binding,
            form,
            selection,
        }
    }

    fn form_owned_node(&self, node: &WidgetNode, surface: &ActionSurface) -> bool {
        matches!(surface, ActionSurface::Form(_))
            || match &node.component {
                WidgetComponent::Form { .. } => true,
                WidgetComponent::Field { field_id } => self.ux.fields.iter().any(|field| {
                    field.field_id == *field_id
                        && matches!(
                            &field.target,
                            WidgetFieldTarget::FormInput { .. }
                                | WidgetFieldTarget::ParentProperty { .. }
                        )
                }),
                WidgetComponent::Section { .. }
                | WidgetComponent::List { .. }
                | WidgetComponent::Detail { .. }
                | WidgetComponent::Dialog { .. }
                | WidgetComponent::ChildGrid { .. }
                | WidgetComponent::Action { .. }
                | WidgetComponent::Navigation { .. }
                | WidgetComponent::Control { .. }
                | WidgetComponent::Catalog { .. }
                | WidgetComponent::Composition { .. } => false,
            }
    }

    fn add_page_use(&mut self, usage: Usage, page_id: &str) {
        self.page_uses
            .entry(usage)
            .or_default()
            .insert(Some(page_id.to_owned()));
    }

    pub(super) fn add_surface_use(&mut self, usage: Usage, surface: &ActionSurface) {
        self.usage_surfaces
            .entry(usage)
            .or_default()
            .push(surface.clone());
    }

    fn index_property_use(
        &mut self,
        property: &norn_ir::PropertyRef,
        page_id: &str,
        surface: &ActionSurface,
    ) {
        self.add_page_use(Usage::Binding(property.binding_id.clone()), page_id);
        let field_ids: Vec<String> = self
            .ux
            .fields
            .iter()
            .filter(|field| {
                matches!(&field.target, WidgetFieldTarget::Property { property: target } if target == property)
            })
            .map(|field| field.field_id.clone())
            .collect();
        for id in field_ids {
            self.add_page_use(Usage::Field(id.clone()), page_id);
            self.add_page_use(Usage::Semantic(id.clone()), page_id);
            self.add_surface_use(Usage::Field(id.clone()), surface);
            self.add_surface_use(Usage::Semantic(id), surface);
        }
    }

    pub(super) fn index_override_uses(&mut self) {
        for (index, override_) in self.ux.overrides.clone().iter().enumerate() {
            let WidgetOverrideValue::ActionPlacement { placement_id } = &override_.value else {
                continue;
            };
            if !self
                .composed_semantic_ids
                .contains(&override_.target_semantic_id)
            {
                continue;
            }
            let Some(surface) = self
                .action_surfaces
                .get(&override_.target_semantic_id)
                .cloned()
            else {
                continue;
            };
            let Some(placement) = self.placements.get(placement_id.as_str()).copied() else {
                continue;
            };
            let path = format!("/payload/ux/overrides/{index}/value/placementId");
            if let Some(binding) = &surface.binding {
                if self.bindings.get(binding.binding_id.as_str()).copied() != Some(binding) {
                    self.error(&path, "action placement override must preserve the imported surface's data binding");
                } else if let Some(selection) = surface.selection
                    && self
                        .list_selections
                        .get(&binding.binding_id)
                        .is_some_and(|existing| *existing != selection)
                {
                    self.error(
                        &path,
                        "action placement override cannot change the imported list selection mode",
                    );
                }
            }
            if let Some(form) = &surface.form
                && self.forms.get(form.form_id.as_str()).copied() != Some(form)
            {
                self.error(
                    &path,
                    "action placement override must preserve the imported surface's form",
                );
            }
            self.check_action_surface(placement, &surface.surface, &path);
            self.add_surface_use(Usage::Placement(placement_id.clone()), &surface.surface);
            self.referenced_placements.insert(placement_id.clone());
            let pages = self
                .page_uses
                .get(&Usage::Semantic(override_.target_semantic_id.clone()))
                .cloned()
                .unwrap_or_default();
            self.page_uses
                .entry(Usage::Placement(placement_id.clone()))
                .or_default()
                .extend(pages);
        }
    }

    pub(super) fn propagate_page_uses(&mut self) {
        let mut edges = Vec::new();
        self.collect_binding_and_field_use_edges(&mut edges);
        self.collect_form_action_and_nav_use_edges(&mut edges);
        loop {
            let mut changed = false;
            for (source, target) in &edges {
                let pages = self.page_uses.get(source).cloned().unwrap_or_default();
                let target_pages = self.page_uses.entry(target.clone()).or_default();
                let before = target_pages.len();
                target_pages.extend(pages);
                changed |= before != target_pages.len();
            }
            if !changed {
                break;
            }
        }
    }

    fn collect_binding_and_field_use_edges(&self, edges: &mut Vec<(Usage, Usage)>) {
        for (index, binding) in self.ux.data_bindings.iter().enumerate() {
            let usage = Usage::Binding(binding.binding_id.clone());
            for (source_id, _) in binding_record_dependencies(binding, index) {
                edges.push((usage.clone(), Usage::Binding(source_id.to_owned())));
            }
        }
        for binding in &self.ux.action_bindings {
            add_input_uses(
                edges,
                &Usage::Action(binding.binding_id.clone()),
                &binding.inputs,
            );
        }
        for field in &self.ux.fields {
            let usage = Usage::Field(field.field_id.clone());
            match &field.target {
                WidgetFieldTarget::Property { property } => {
                    edges.push((usage.clone(), Usage::Binding(property.binding_id.clone())));
                }
                WidgetFieldTarget::FormInput {
                    form_id, initial, ..
                } => {
                    edges.push((usage.clone(), Usage::Form(form_id.clone())));
                    if let Some(property) = initial {
                        edges.push((usage.clone(), Usage::Binding(property.binding_id.clone())));
                    }
                }
                WidgetFieldTarget::ParentProperty { form_id, .. }
                | WidgetFieldTarget::ChildRowProperty { form_id, .. } => {
                    edges.push((usage.clone(), Usage::Form(form_id.clone())));
                }
            }
            if let Some(id) = &field.option_query_binding_id {
                edges.push((usage.clone(), Usage::Binding(id.clone())));
            }
            for condition in [
                &field.visible_when,
                &field.required_when,
                &field.read_only_when,
            ]
            .into_iter()
            .flatten()
            {
                add_input_uses(edges, &usage, &condition.inputs);
            }
        }
    }

    fn collect_form_action_and_nav_use_edges(&self, edges: &mut Vec<(Usage, Usage)>) {
        for form in &self.ux.forms {
            let usage = Usage::Form(form.form_id.clone());
            edges.push((usage.clone(), Usage::Action(form.submit_binding_id.clone())));
            if let Some(id) = &form.data_binding_id {
                edges.push((usage, Usage::Binding(id.clone())));
            }
        }
        for placement in &self.ux.action_placements {
            let usage = Usage::Placement(placement.placement_id.clone());
            edges.push((usage.clone(), Usage::Action(placement.binding_id.clone())));
            if let Some(id) = &placement.form_id {
                edges.push((usage.clone(), Usage::Form(id.clone())));
            }
            if let Some(id) = &placement.selection_binding_id {
                edges.push((usage.clone(), Usage::Binding(id.clone())));
            }
            if let Some(condition) = &placement.enabled_when {
                add_input_uses(edges, &usage, &condition.inputs);
            }
        }
        for item in &self.ux.navigation_items {
            let usage = Usage::Navigation(item.item_id.clone());
            if let Some(condition) = &item.visible_when {
                add_input_uses(edges, &usage, &condition.inputs);
            }
            match &item.destination {
                WidgetNavigationDestination::Page { parameters, .. } => {
                    for value in parameters.values() {
                        add_value_use(edges, &usage, value);
                    }
                }
                WidgetNavigationDestination::Object {
                    data_binding_id, ..
                }
                | WidgetNavigationDestination::NativeProjection {
                    data_binding_id, ..
                } => {
                    edges.push((usage, Usage::Binding(data_binding_id.clone())));
                }
            }
        }
        for override_ in &self.ux.overrides {
            if let WidgetOverrideValue::Visibility { condition } = &override_.value {
                add_input_uses(
                    edges,
                    &Usage::Semantic(override_.target_semantic_id.clone()),
                    &condition.inputs,
                );
            }
        }
    }

    pub(super) fn usage_at(&self, path: &str) -> Option<Usage> {
        path_index(path, "/payload/ux/dataBindings/")
            .and_then(|index| self.ux.data_bindings.get(index))
            .map(|binding| Usage::Binding(binding.binding_id.clone()))
            .or_else(|| {
                path_index(path, "/payload/ux/actionBindings/")
                    .and_then(|index| self.ux.action_bindings.get(index))
                    .map(|binding| Usage::Action(binding.binding_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/fields/")
                    .and_then(|index| self.ux.fields.get(index))
                    .map(|field| Usage::Field(field.field_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/forms/")
                    .and_then(|index| self.ux.forms.get(index))
                    .map(|form| Usage::Form(form.form_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/actionPlacements/")
                    .and_then(|index| self.ux.action_placements.get(index))
                    .map(|placement| Usage::Placement(placement.placement_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/navigationItems/")
                    .and_then(|index| self.ux.navigation_items.get(index))
                    .map(|item| Usage::Navigation(item.item_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/overrides/")
                    .and_then(|index| self.ux.overrides.get(index))
                    .map(|override_| Usage::Semantic(override_.target_semantic_id.clone()))
            })
    }

    pub(super) fn pages_at(&self, path: &str) -> BTreeSet<Option<String>> {
        self.usage_at(path)
            .and_then(|usage| self.page_uses.get(&usage))
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn check_context_page_at(&self, page_id: &str, path: &str) -> bool {
        let pages = self.pages_at(path);
        let available =
            !pages.is_empty() && pages.iter().all(|page| page.as_deref() == Some(page_id));
        if !available {
            self.error(path, "contextPageId must match every rendering or invocation page; context is unavailable at this use");
        }
        available
    }

    pub(super) fn check_ids(&mut self) {
        let app = self.ux.app.clone();
        let pages = self.ux.pages.clone();
        if let Some(app) = &app {
            self.check_global_id(&mut BTreeSet::new(), &app.app_id, "/payload/ux/app/appId");
        }
        self.check_page_ids_and_params(&pages);
        self.check_collection_global_ids(app.as_ref());
        let mut semantic_ids = BTreeSet::new();
        let mut node_ids = BTreeSet::new();
        let mut section_ids = BTreeSet::new();
        if let Some(app) = &app {
            for item_id in &app.navigation_item_ids {
                self.navigation_uses
                    .entry(item_id.clone())
                    .or_default()
                    .push(None);
            }
        }
        for (page_index, page) in pages.iter().enumerate() {
            self.check_node_ids(
                &page.root,
                &format!("/payload/ux/pages/{page_index}/root"),
                &mut node_ids,
                &mut section_ids,
                &mut semantic_ids,
                None,
            );
        }
        for (index, override_) in self.ux.overrides.clone().iter().enumerate() {
            if override_.target_semantic_id.trim().is_empty() {
                self.error(
                    &format!("/payload/ux/overrides/{index}/targetSemanticId"),
                    "override target id must not be blank",
                );
            }
        }
    }

    fn check_page_ids_and_params(&mut self, pages: &[norn_ir::WidgetPage]) {
        let mut page_ids = BTreeSet::new();
        for (index, page) in pages.iter().enumerate() {
            self.check_global_id(
                &mut page_ids,
                &page.page_id,
                &format!("/payload/ux/pages/{index}/pageId"),
            );
            if page.name.trim().is_empty() {
                self.error(
                    &format!("/payload/ux/pages/{index}/name"),
                    "page name must not be blank",
                );
            }
            let mut params = BTreeSet::new();
            for (param_index, param) in page.context_params.iter().enumerate() {
                let param_path = format!("/payload/ux/pages/{index}/contextParams/{param_index}");
                if param.param_id.trim().is_empty() || !params.insert(param.param_id.as_str()) {
                    self.error(
                        &format!("{param_path}/paramId"),
                        "context parameter ids must be nonblank and unique within a page",
                    );
                }
                if let Some(runtime_key_of) = &param.runtime_key_of {
                    if !param.required
                        || param.value_type != norn_ir::widget_ux::WidgetValueType::Text
                    {
                        self.error(
                            &format!("{param_path}/runtimeKeyOf"),
                            "runtimeKeyOf requires a required text context parameter",
                        );
                    }
                    self.resolve_ref(
                        &runtime_key_of.id,
                        &runtime_key_of.version,
                        &ArtifactKind::Entity,
                        &format!("{param_path}/runtimeKeyOf"),
                    );
                }
            }
        }
    }

    fn check_collection_global_ids(&mut self, app: Option<&norn_ir::WidgetApp>) {
        let mut data_binding_ids = BTreeSet::new();
        for (index, binding) in self.ux.data_bindings.clone().iter().enumerate() {
            self.check_global_id(
                &mut data_binding_ids,
                &binding.binding_id,
                &format!("/payload/ux/dataBindings/{index}/bindingId"),
            );
        }
        let mut action_binding_ids = BTreeSet::new();
        for (index, binding) in self.ux.action_bindings.clone().iter().enumerate() {
            self.check_global_id(
                &mut action_binding_ids,
                &binding.binding_id,
                &format!("/payload/ux/actionBindings/{index}/bindingId"),
            );
        }
        let mut field_ids = BTreeSet::new();
        for (index, field) in self.ux.fields.clone().iter().enumerate() {
            self.check_global_id(
                &mut field_ids,
                &field.field_id,
                &format!("/payload/ux/fields/{index}/fieldId"),
            );
        }
        let mut form_ids = BTreeSet::new();
        for (index, form) in self.ux.forms.clone().iter().enumerate() {
            self.check_global_id(
                &mut form_ids,
                &form.form_id,
                &format!("/payload/ux/forms/{index}/formId"),
            );
        }
        let mut placement_ids = BTreeSet::new();
        for (index, placement) in self.ux.action_placements.clone().iter().enumerate() {
            self.check_global_id(
                &mut placement_ids,
                &placement.placement_id,
                &format!("/payload/ux/actionPlacements/{index}/placementId"),
            );
        }
        let mut navigation_item_ids = BTreeSet::new();
        for (index, item) in self.ux.navigation_items.clone().iter().enumerate() {
            self.check_global_id(
                &mut navigation_item_ids,
                &item.item_id,
                &format!("/payload/ux/navigationItems/{index}/itemId"),
            );
        }
        if let Some(app) = app {
            let mut route_ids = BTreeSet::new();
            for (index, route) in app.routes.iter().enumerate() {
                self.check_global_id(
                    &mut route_ids,
                    &route.route_id,
                    &format!("/payload/ux/app/routes/{index}/routeId"),
                );
            }
        }
    }

    fn check_global_id(&mut self, ids: &mut BTreeSet<String>, id: &str, path: &str) {
        if id.trim().is_empty() || !ids.insert(id.to_owned()) {
            self.error(
                path,
                "stable ids must be nonblank and unique across Widget UX",
            );
        }
    }

    fn check_node_ids(
        &mut self,
        node: &WidgetNode,
        path: &str,
        node_ids: &mut BTreeSet<String>,
        section_ids: &mut BTreeSet<String>,
        semantic_ids: &mut BTreeSet<String>,
        row_context: Option<&str>,
    ) {
        self.check_global_id(node_ids, &node.node_id, &format!("{path}/nodeId"));
        let child_row_context = match &node.component {
            WidgetComponent::List {
                data_binding_id,
                selection,
                ..
            } => {
                if let Some(existing) = self.list_selections.get(data_binding_id)
                    && existing != selection
                {
                    self.error(
                        &format!("{path}/component/selection"),
                        "one data binding cannot use conflicting selection modes",
                    );
                }
                self.list_selections
                    .insert(data_binding_id.clone(), *selection);
                self.list_row_contexts.insert(data_binding_id.clone());
                Some(data_binding_id.as_str())
            }
            WidgetComponent::Detail { data_binding_id } => Some(data_binding_id.as_str()),
            WidgetComponent::ChildGrid {
                data_binding_id: Some(data_binding_id),
                ..
            } => {
                self.list_row_contexts.insert(data_binding_id.clone());
                Some(data_binding_id.as_str())
            }
            WidgetComponent::Section { .. }
            | WidgetComponent::Dialog { .. }
            | WidgetComponent::Form { .. }
            | WidgetComponent::ChildGrid { .. }
            | WidgetComponent::Field { .. }
            | WidgetComponent::Action { .. }
            | WidgetComponent::Navigation { .. }
            | WidgetComponent::Control { .. }
            | WidgetComponent::Catalog { .. }
            | WidgetComponent::Composition { .. } => row_context,
        };
        if let WidgetComponent::Navigation { item_id } = &node.component {
            self.navigation_uses
                .entry(item_id.clone())
                .or_default()
                .push(row_context.map(str::to_owned));
        }
        if let WidgetComponent::Section { section } = &node.component {
            self.check_global_id(
                section_ids,
                &section.section_id,
                &format!("{path}/component/section/sectionId"),
            );
        }
        if let Some(semantic_id) = &node.semantic_id {
            if semantic_id.trim().is_empty()
                || !semantic_ids.insert(semantic_id.clone())
                || self.local_field_ids.contains(semantic_id)
            {
                self.error(
                    &format!("{path}/semanticId"),
                    "semantic ids must be nonblank and unique across Widget UX pages",
                );
            } else if self.semantic_nodes.contains_key(semantic_id) {
                self.error(
                    &format!("{path}/semanticId"),
                    "semantic id collides with an imported component",
                );
            } else {
                self.local_semantic_ids.insert(semantic_id.clone());
                self.semantic_nodes.insert(
                    semantic_id.clone(),
                    SemanticNode {
                        value_type: None,
                        option_key_type: None,
                        control_target: matches!(&node.component, WidgetComponent::Field { .. }),
                        action_target: matches!(&node.component, WidgetComponent::Action { .. }),
                        form_input_initialization: FormInputInitialization::default(),
                    },
                );
            }
        }
        for (index, child) in node.children.iter().enumerate() {
            self.check_node_ids(
                child,
                &format!("{path}/children/{index}"),
                node_ids,
                section_ids,
                semantic_ids,
                child_row_context,
            );
        }
    }
}

fn add_input_uses(
    edges: &mut Vec<(Usage, Usage)>,
    source: &Usage,
    inputs: &WidgetExpressionInputs,
) {
    for id in [&inputs.record_binding_id, &inputs.selection_binding_id]
        .into_iter()
        .flatten()
    {
        edges.push((source.clone(), Usage::Binding(id.clone())));
    }
    if let Some(id) = &inputs.draft_form_id {
        edges.push((source.clone(), Usage::Form(id.clone())));
    }
}

fn add_value_use(edges: &mut Vec<(Usage, Usage)>, source: &Usage, value: &WidgetFilterValue) {
    match value {
        WidgetFilterValue::Record { property } => {
            edges.push((source.clone(), Usage::Binding(property.binding_id.clone())));
        }
        WidgetFilterValue::Literal { .. } | WidgetFilterValue::ContextParameter { .. } => {}
    }
}
