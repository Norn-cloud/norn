//! Component tree walks, surface placement rules, and semantic node registration.

use norn_expr::Type;
use norn_ir::{
    WidgetActionLocation, WidgetActionPlacement, WidgetComponent, WidgetDataQuery,
    WidgetFieldPresentation, WidgetFieldTarget, WidgetNode,
};

use super::super::types::property_type;
use super::super::{ActionSurface, FormInputInitialization, SemanticNode, UxChecker};

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_node(&mut self, node: &WidgetNode, path: &str, surface: ActionSurface) {
        if !self.check_node_component(node, path, &surface) {
            return;
        }
        self.check_children(node, path);
        let child_surface = match &node.component {
            WidgetComponent::List {
                data_binding_id, ..
            } => ActionSurface::List(data_binding_id.clone()),
            WidgetComponent::Detail { data_binding_id } => {
                ActionSurface::Detail(data_binding_id.clone())
            }
            WidgetComponent::Form { form_id } | WidgetComponent::ChildGrid { form_id, .. } => {
                ActionSurface::Form(form_id.clone())
            }
            WidgetComponent::Section { .. }
            | WidgetComponent::Dialog { .. }
            | WidgetComponent::Field { .. }
            | WidgetComponent::Action { .. }
            | WidgetComponent::Navigation { .. }
            | WidgetComponent::Control { .. }
            | WidgetComponent::Catalog { .. }
            | WidgetComponent::Composition { .. } => surface,
        };
        for (index, child) in node.children.iter().enumerate() {
            self.check_node(
                child,
                &format!("{path}/children/{index}"),
                child_surface.clone(),
            );
        }
    }

    fn check_node_component(
        &mut self,
        node: &WidgetNode,
        path: &str,
        surface: &ActionSurface,
    ) -> bool {
        match &node.component {
            WidgetComponent::Section { section } => self.check_section_component(section, path),
            WidgetComponent::List {
                data_binding_id,
                fields,
                ..
            } => self.check_list_component(data_binding_id, fields, path),
            WidgetComponent::Detail { data_binding_id } => {
                if !self
                    .bindings
                    .get(data_binding_id.as_str())
                    .is_some_and(|binding| {
                        matches!(
                            &binding.query,
                            WidgetDataQuery::ObjectDetail { .. }
                                | WidgetDataQuery::ObjectRuntimeDetail { .. }
                        )
                    })
                {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "detail component requires an object-detail binding",
                    );
                }
            }
            WidgetComponent::Form { form_id } => {
                if !self.forms.contains_key(form_id.as_str()) {
                    self.error(
                        &format!("{path}/component/formId"),
                        "form component names an unknown form",
                    );
                }
            }
            WidgetComponent::ChildGrid {
                form_id,
                label,
                collection_id,
                ordering_key,
                data_binding_id,
                row_ops_input_name,
                max_rows,
                field_ids,
            } => self.check_child_grid(
                form_id,
                label,
                collection_id,
                ordering_key,
                data_binding_id.as_deref(),
                row_ops_input_name,
                max_rows.0,
                field_ids,
                surface,
                path,
            ),
            WidgetComponent::Field { field_id } => {
                return self.check_field_node(node, field_id, surface, path);
            }
            WidgetComponent::Action { placement_id } => {
                if let Some(placement) = self.placements.get(placement_id.as_str()).copied() {
                    self.check_action_surface(
                        placement,
                        surface,
                        &format!("{path}/component/placementId"),
                    );
                    self.referenced_placements.insert(placement_id.clone());
                } else {
                    self.error(
                        &format!("{path}/component/placementId"),
                        "action component names an unknown placement",
                    );
                }
            }
            WidgetComponent::Navigation { item_id } => {
                if !self
                    .ux
                    .navigation_items
                    .iter()
                    .any(|item| item.item_id == *item_id)
                {
                    self.error(
                        &format!("{path}/component/itemId"),
                        "navigation component names an unknown item",
                    );
                }
            }
            WidgetComponent::Composition { .. }
            | WidgetComponent::Dialog { .. }
            | WidgetComponent::Control { .. }
            | WidgetComponent::Catalog { .. } => {}
        }
        true
    }

    fn check_section_component(&mut self, section: &norn_ir::WidgetSection, path: &str) {
        if section.label.0.is_empty()
            || section
                .label
                .0
                .values()
                .any(|value| value.trim().is_empty())
        {
            self.error(
                &format!("{path}/component/section/label"),
                "section label must contain nonblank localized text",
            );
        }
        for (index, property) in section.fields.iter().enumerate() {
            self.include_property(
                property,
                &format!("{path}/component/section/fields/{index}"),
            );
        }
    }

    fn check_list_component(
        &mut self,
        data_binding_id: &str,
        fields: &[norn_ir::PropertyRef],
        path: &str,
    ) {
        if !self.bindings.get(data_binding_id).is_some_and(|binding| {
            matches!(
                &binding.query,
                WidgetDataQuery::ObjectList { .. } | WidgetDataQuery::LinkTraversal { .. }
            )
        }) {
            self.error(
                &format!("{path}/component/dataBindingId"),
                "list component requires an existing bounded list or traversal binding",
            );
        }
        for (index, property) in fields.iter().enumerate() {
            if property.binding_id != data_binding_id {
                self.error(
                    &format!("{path}/component/fields/{index}/bindingId"),
                    "list field must belong to the list binding",
                );
            }
            self.include_property(property, &format!("{path}/component/fields/{index}"));
        }
    }

    fn check_field_node(
        &mut self,
        node: &WidgetNode,
        field_id: &str,
        surface: &ActionSurface,
        path: &str,
    ) -> bool {
        let Some(field) = self
            .ux
            .fields
            .iter()
            .find(|field| field.field_id == field_id)
            .cloned()
        else {
            self.error(
                &format!("{path}/component/fieldId"),
                "field component names an unknown presentation",
            );
            return false;
        };
        if !self.used_field_ids.insert(field_id.to_owned()) {
            self.error(
                &format!("{path}/component/fieldId"),
                "field presentation may appear only once in the component tree",
            );
        }
        match &field.target {
            WidgetFieldTarget::Property { property } => {
                if matches!(surface, ActionSurface::Form(_)) {
                    self.error(
                        &format!("{path}/component/fieldId"),
                        "form children must present form inputs, not arbitrary object properties",
                    );
                }
                self.included_fields
                    .insert((property.binding_id.clone(), property.property_id.clone()));
                let type_ = self
                    .resolve_property(property, &format!("{path}/component/fieldId"))
                    .and_then(property_type);
                let option_type = field.option_query_binding_id.as_deref().and_then(|id| {
                    self.option_binding_key_type(id, &format!("{path}/component/fieldId"))
                });
                self.register_semantic_node(
                    &field.field_id,
                    type_.clone(),
                    true,
                    false,
                    option_type.clone(),
                    &format!("{path}/component/fieldId"),
                );
                if let Some(semantic_id) = &node.semantic_id {
                    self.register_semantic_node(
                        semantic_id,
                        type_,
                        true,
                        false,
                        option_type,
                        &format!("{path}/semanticId"),
                    );
                }
            }
            WidgetFieldTarget::FormInput {
                form_id,
                input_name,
                ..
            } => self.check_form_input_field_node(node, &field, form_id, input_name, surface, path),
            WidgetFieldTarget::ParentProperty {
                form_id,
                input_name,
                property_id,
            } => self.check_parent_property_field_node(
                node,
                &field,
                form_id,
                input_name,
                property_id,
                surface,
                path,
            ),
            WidgetFieldTarget::ChildRowProperty { .. } => {
                self.error(
                    &format!("{path}/component/fieldId"),
                    "child-row property fields are rendered by ChildGrid.fieldIds, not standalone field nodes",
                );
            }
        }
        true
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "registers semantic metadata for one nested parent-property field node"
    )]
    fn check_parent_property_field_node(
        &mut self,
        node: &WidgetNode,
        field: &WidgetFieldPresentation,
        form_id: &str,
        input_name: &str,
        property_id: &str,
        surface: &ActionSurface,
        path: &str,
    ) {
        if !matches!(surface, ActionSurface::Form(parent_form) if parent_form == form_id) {
            self.error(
                &format!("{path}/component/fieldId"),
                "parent property field must be a child of its matching form component",
            );
        }
        self.included_form_inputs
            .insert((form_id.to_owned(), input_name.to_owned()));
        let value_type = self
            .forms
            .get(form_id)
            .and_then(|form| self.action_for_form(form))
            .and_then(|action| self.parent_object(action))
            .and_then(|object| object.property(property_id).cloned())
            .and_then(|property| property_type(&property));
        let option_type = field
            .option_query_binding_id
            .as_deref()
            .and_then(|id| self.option_binding_key_type(id, &format!("{path}/component/fieldId")));
        self.register_semantic_node(
            &field.field_id,
            value_type.clone(),
            true,
            false,
            option_type.clone(),
            &format!("{path}/component/fieldId"),
        );
        if let Some(semantic_id) = &node.semantic_id {
            self.register_semantic_node(
                semantic_id,
                value_type,
                true,
                false,
                option_type,
                &format!("{path}/semanticId"),
            );
        }
    }

    fn check_form_input_field_node(
        &mut self,
        node: &WidgetNode,
        field: &WidgetFieldPresentation,
        form_id: &str,
        input_name: &str,
        surface: &ActionSurface,
        path: &str,
    ) {
        if !matches!(surface, ActionSurface::Form(parent_form) if parent_form == form_id) {
            self.error(
                &format!("{path}/component/fieldId"),
                "form input field must be a child of its matching form component",
            );
        }
        if !self
            .included_form_inputs
            .insert((form_id.to_owned(), input_name.to_owned()))
        {
            self.error(
                &format!("{path}/component/fieldId"),
                "form input may be presented only once in its form",
            );
        }
        let schema = self
            .forms
            .get(form_id)
            .and_then(|form| self.action_bindings.get(form.submit_binding_id.as_str()))
            .and_then(|binding| self.actions.get(binding.binding_id.as_str()))
            .and_then(|action| action.fields.get(input_name))
            .map(|(schema, _)| schema.clone());
        let option_type = field
            .option_query_binding_id
            .as_deref()
            .and_then(|id| self.option_binding_key_type(id, &format!("{path}/component/fieldId")));
        if let Some(schema) = schema {
            let type_ = self.schema_control_type(&schema);
            self.register_semantic_node(
                &field.field_id,
                Some(type_.clone()),
                true,
                false,
                option_type.clone(),
                &format!("{path}/component/fieldId"),
            );
            let initialization = self
                .form_input_initialization
                .get(&field.field_id)
                .cloned()
                .unwrap_or_default();
            self.set_form_input_initialization(&field.field_id, &initialization);
            if let Some(semantic_id) = &node.semantic_id {
                self.register_semantic_node(
                    semantic_id,
                    Some(type_),
                    true,
                    false,
                    option_type,
                    &format!("{path}/semanticId"),
                );
                self.set_form_input_initialization(semantic_id, &initialization);
            }
        } else {
            self.register_semantic_node(
                &field.field_id,
                None,
                true,
                false,
                option_type,
                &format!("{path}/component/fieldId"),
            );
        }
    }

    pub(in super::super) fn check_action_surface(
        &self,
        placement: &WidgetActionPlacement,
        surface: &ActionSurface,
        path: &str,
    ) {
        let expected_location = match surface {
            ActionSurface::List(data_binding_id) => {
                let list_location = matches!(
                    placement.location,
                    WidgetActionLocation::Row | WidgetActionLocation::Selection
                );
                if list_location
                    && placement.selection_binding_id.as_deref() != Some(data_binding_id.as_str())
                {
                    self.error(
                        path,
                        "row and selection actions must name the containing list binding",
                    );
                }
                list_location
            }
            ActionSurface::Detail(data_binding_id) => {
                if let Some(binding) = self.action_bindings.get(placement.binding_id.as_str())
                    && binding
                        .inputs
                        .record_binding_id
                        .as_deref()
                        .is_some_and(|record| record != data_binding_id.as_str())
                {
                    self.error(
                        path,
                        "detail action input.record must match its containing detail binding",
                    );
                }
                placement.location == WidgetActionLocation::Detail
            }
            ActionSurface::Form(form_id) => {
                let footer_location = placement.location == WidgetActionLocation::FormFooter
                    && placement.form_id.is_none();
                if self
                    .forms
                    .get(form_id.as_str())
                    .is_some_and(|form| form.submit_binding_id == placement.binding_id)
                {
                    self.error(
                        path,
                        "a form cannot place its own submit action as a secondary footer action",
                    );
                }
                if let Some(binding) = self.action_bindings.get(placement.binding_id.as_str())
                    && binding
                        .inputs
                        .draft_form_id
                        .as_deref()
                        .is_some_and(|draft| draft != form_id)
                {
                    self.error(
                        path,
                        "form footer draft input must name its containing form",
                    );
                }
                footer_location
            }
            ActionSurface::Page => placement.location == WidgetActionLocation::Toolbar,
        };
        if !expected_location {
            self.error(
                path,
                "action location must match its containing page, list, detail, or form surface",
            );
        }
    }

    fn check_children(&mut self, node: &WidgetNode, path: &str) {
        let valid = match &node.component {
            WidgetComponent::Section { .. }
            | WidgetComponent::Detail { .. }
            | WidgetComponent::Dialog { .. } => true,
            WidgetComponent::List { .. } => node.children.iter().all(|child| {
                matches!(
                    &child.component,
                    WidgetComponent::Action { .. } | WidgetComponent::Navigation { .. }
                )
            }),
            WidgetComponent::Form { .. } => node.children.iter().all(|child| {
                matches!(
                    &child.component,
                    WidgetComponent::Field { .. }
                        | WidgetComponent::Action { .. }
                        | WidgetComponent::ChildGrid { .. }
                )
            }),
            WidgetComponent::Field { .. }
            | WidgetComponent::ChildGrid { .. }
            | WidgetComponent::Action { .. }
            | WidgetComponent::Navigation { .. }
            | WidgetComponent::Control { .. }
            | WidgetComponent::Catalog { .. }
            | WidgetComponent::Composition { .. } => node.children.is_empty(),
        };
        if !valid {
            self.error(
                &format!("{path}/children"),
                "component children do not match the closed tree composition rules",
            );
        }
        if matches!(&node.component, WidgetComponent::Form { .. }) {
            for child in &node.children {
                if let WidgetComponent::Action { placement_id } = &child.component
                    && !self
                        .placements
                        .get(placement_id.as_str())
                        .is_some_and(|placement| {
                            placement.location == WidgetActionLocation::FormFooter
                        })
                {
                    self.error(
                        &format!("{path}/children"),
                        "form children may contain only secondary form_footer actions",
                    );
                }
            }
        }
    }

    fn include_property(&mut self, property: &norn_ir::PropertyRef, path: &str) {
        if self.resolve_property(property, path).is_some() {
            self.included_fields
                .insert((property.binding_id.clone(), property.property_id.clone()));
            let presentations: Vec<(String, Option<String>)> = self
                .ux
                .fields
                .iter()
                .filter_map(|field| match &field.target {
                    WidgetFieldTarget::Property { property: target }
                        if target.binding_id == property.binding_id
                            && target.property_id == property.property_id =>
                    {
                        Some((
                            field.field_id.clone(),
                            field.option_query_binding_id.clone(),
                        ))
                    }
                    WidgetFieldTarget::Property { .. }
                    | WidgetFieldTarget::FormInput { .. }
                    | WidgetFieldTarget::ParentProperty { .. }
                    | WidgetFieldTarget::ChildRowProperty { .. } => None,
                })
                .collect();
            if presentations.len() != 1 {
                self.error(
                    path,
                    "explicitly included property must have exactly one field presentation",
                );
            } else if let Some((field_id, option_binding_id)) = presentations.first() {
                let value_type = self
                    .resolve_property(property, path)
                    .and_then(property_type);
                let option_type = option_binding_id
                    .as_deref()
                    .and_then(|id| self.option_binding_key_type(id, path));
                self.register_semantic_node(field_id, value_type, true, false, option_type, path);
            }
        }
    }

    pub(in super::super) fn register_semantic_node(
        &mut self,
        id: &str,
        value_type: Option<Type>,
        control_target: bool,
        action_target: bool,
        option_key_type: Option<Type>,
        path: &str,
    ) {
        if let Some(mut existing) = self.semantic_nodes.remove(id) {
            let is_local = self.local_semantic_ids.contains(id);
            if !is_local && (existing.control_target || existing.action_target) {
                let same = existing.value_type == value_type
                    && existing.control_target == control_target
                    && existing.action_target == action_target
                    && existing.option_key_type == option_key_type;
                self.semantic_nodes.insert(id.to_owned(), existing);
                if same {
                    return;
                }
                self.error(
                    path,
                    "semantic component id resolves to conflicting field definitions",
                );
                return;
            }
            if is_local && (existing.control_target || existing.action_target) {
                let same = (existing.value_type.is_none() || existing.value_type == value_type)
                    && existing.control_target == control_target
                    && existing.action_target == action_target;
                if same {
                    existing.value_type = value_type;
                    existing.option_key_type = option_key_type;
                    self.semantic_nodes.insert(id.to_owned(), existing);
                    return;
                }
                self.semantic_nodes.insert(id.to_owned(), existing);
                self.error(
                    path,
                    "semantic component id resolves to conflicting field definitions",
                );
                return;
            }
            self.semantic_nodes.insert(id.to_owned(), existing);
            self.error(
                path,
                if is_local {
                    "field id collides with a non-field semantic component id"
                } else {
                    "field id collides with another semantic component id"
                },
            );
            return;
        }
        self.semantic_nodes.insert(
            id.to_owned(),
            SemanticNode {
                value_type,
                option_key_type,
                control_target,
                action_target,
                form_input_initialization: FormInputInitialization::default(),
            },
        );
    }

    fn set_form_input_initialization(
        &mut self,
        id: &str,
        initialization: &FormInputInitialization,
    ) {
        if let Some(node) = self.semantic_nodes.get_mut(id) {
            node.form_input_initialization = initialization.clone();
        }
    }
}
