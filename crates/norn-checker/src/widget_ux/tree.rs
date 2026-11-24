//! Component tree walks, field presentations, controls, and semantic node registration.

use std::collections::BTreeMap;

use norn_expr::Type;
use norn_expr::schema::Schema;
use norn_ir::{
    ActionEffectProgram, ArtifactKind, PropertyType, WidgetActionLocation, WidgetActionPlacement,
    WidgetComponent, WidgetDataQuery, WidgetFieldControl, WidgetFieldFormat, WidgetFieldTarget,
    WidgetNode,
};

use crate::entity_instance::EffectiveKind;

use super::child_grid::child_grid_component;
use super::types::{property_type, type_fits_schema, types_compatible};
use super::{ActionSurface, FormInputInitialization, SemanticNode, Usage, UxChecker};

impl UxChecker<'_, '_, '_> {
    #[expect(
        clippy::too_many_lines,
        reason = "This pass connects each UX field presentation to its typed source and rendered tree."
    )]
    pub(super) fn check_tree_and_fields(&mut self) {
        let fields = self.ux.fields.clone();
        for (index, field) in fields.iter().enumerate() {
            let path = format!("/payload/ux/fields/{index}");
            if field.label.0.is_empty()
                || field.label.0.values().any(|value| value.trim().is_empty())
            {
                self.error(
                    &format!("{path}/label"),
                    "field label must contain nonblank localized text",
                );
            }
            if let Some(help) = &field.help
                && help.0.values().any(|value| value.trim().is_empty())
            {
                self.error(
                    &format!("{path}/help"),
                    "help text values must not be blank",
                );
            }
            if field.format == Some(WidgetFieldFormat::MetricBuckets)
                && !matches!(&field.target, WidgetFieldTarget::Property { .. })
            {
                self.error(
                    &format!("{path}/format"),
                    "metric_buckets requires a display presentation bound to a json-ext property, not an input",
                );
            }
            match &field.target {
                WidgetFieldTarget::Property { property } => {
                    if field.format == Some(WidgetFieldFormat::MetricBuckets)
                        && let Some(resolved) =
                            self.resolve_property(property, &format!("{path}/target/property"))
                        && !matches!(
                            &resolved.kind,
                            EffectiveKind::Base(PropertyType::JsonExt { .. })
                        )
                    {
                        self.error(
                            &format!("{path}/format"),
                            "metric_buckets requires a display presentation bound to a json-ext property",
                        );
                    }
                    if let Some(resolved_property) =
                        self.resolve_property(property, &format!("{path}/target/property"))
                        && let Some(type_) = property_type(resolved_property)
                    {
                        if !self.value_source_available_at(&property.binding_id, &path, None)
                            && self
                                .usage_surfaces
                                .contains_key(&Usage::Field(field.field_id.clone()))
                        {
                            self.error(
                                &path,
                                "field value requires an available detail, row, or single-selection record at every rendering site",
                            );
                        }
                        self.check_control_type(
                            field.control,
                            field.format,
                            field.option_query_binding_id.as_deref(),
                            &type_,
                            &path,
                        );
                    }
                    if let Some(binding_id) = &field.option_query_binding_id
                        && !self.is_bounded_list_binding(binding_id)
                    {
                        self.error(
                            &format!("{path}/optionQueryBindingId"),
                            "option query must be a bounded list or traversal binding",
                        );
                    }
                }
                WidgetFieldTarget::FormInput {
                    form_id,
                    input_name,
                    initial,
                } => {
                    let Some(form) = self.forms.get(form_id.as_str()).copied() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "form input target names an unknown form",
                        );
                        continue;
                    };
                    if !form.input_names.contains(input_name) {
                        self.error(
                            &format!("{path}/target/inputName"),
                            "form input target is not included by its form",
                        );
                    }
                    let Some(action_binding) = self
                        .action_bindings
                        .get(form.submit_binding_id.as_str())
                        .copied()
                    else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "form input submit binding does not exist",
                        );
                        continue;
                    };
                    let Some((schema, required)) = self
                        .actions
                        .get(action_binding.binding_id.as_str())
                        .and_then(|action| action.fields.get(input_name))
                        .cloned()
                    else {
                        self.error(
                            &format!("{path}/target/inputName"),
                            "form input does not resolve in the submit ActionIR schema",
                        );
                        continue;
                    };
                    if self.action_for_form(form).is_some_and(|action| {
                        matches!(
                            action.root.effect_program.as_ref(),
                            Some(ActionEffectProgram::MutateParentWithRows {
                                parent_values_field,
                                row_ops_field,
                                ..
                            }) if input_name == parent_values_field || input_name == row_ops_field
                        )
                    }) {
                        self.error(&format!("{path}/target/inputName"), "parent values and row operations are composed by nested fields and ChildGrid, not scalar form inputs");
                    }
                    let mut initialization = FormInputInitialization {
                        required,
                        source_available: false,
                    };
                    let input_type = self.schema_control_type(&schema);
                    self.check_control_type(
                        field.control,
                        field.format,
                        field.option_query_binding_id.as_deref(),
                        &input_type,
                        &path,
                    );
                    if let Some(binding_id) = &field.option_query_binding_id
                        && !self.is_bounded_list_binding(binding_id)
                    {
                        self.error(
                            &format!("{path}/optionQueryBindingId"),
                            "option query must be a bounded list or traversal binding",
                        );
                    }
                    if let Some(initial) = initial {
                        let mut source_available = true;
                        let binding_matches_form =
                            form.data_binding_id.as_deref() == Some(initial.binding_id.as_str());
                        if !binding_matches_form {
                            source_available = false;
                            if form.data_binding_id.is_none() {
                                self.error(
                                    &format!("{path}/target/initial"),
                                    "form input prefill requires a form record binding",
                                );
                            } else {
                                self.error(&format!("{path}/target/initial/bindingId"), "prefill property must belong to the form's declared record binding");
                            }
                        }
                        if binding_matches_form
                            && !self.value_source_available_at(
                                &initial.binding_id,
                                &path,
                                Some(form),
                            )
                        {
                            source_available = false;
                            self.error(
                                &format!("{path}/target/initial"),
                                "prefill record value is unavailable from the form's record source",
                            );
                        }
                        if let Some(property) =
                            self.resolve_property(initial, &format!("{path}/target/initial"))
                        {
                            if let Some(source_type) = property_type(property) {
                                if !type_fits_schema(&source_type, &schema) {
                                    source_available = false;
                                    self.error(
                                        &format!("{path}/target/initial"),
                                        "prefill property type does not fit the ActionIR form input",
                                    );
                                }
                            } else {
                                source_available = false;
                            }
                        } else {
                            source_available = false;
                        }
                        initialization.source_available = source_available;
                    }
                    self.check_display_initialization(
                        field.control,
                        &initialization,
                        &format!("{path}/target/initial"),
                    );
                    self.form_input_initialization
                        .insert(field.field_id.clone(), initialization);
                    if !self
                        .presented_form_inputs
                        .insert((form_id.clone(), input_name.clone()))
                    {
                        self.error(
                            &format!("{path}/target"),
                            "a form input must have exactly one field presentation",
                        );
                    }
                }
                WidgetFieldTarget::ParentProperty {
                    form_id,
                    input_name,
                    property_id,
                } => {
                    let Some(form) = self.forms.get(form_id.as_str()).copied() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "parent property target names an unknown form",
                        );
                        continue;
                    };
                    if !matches!(self.action_for_form(form), Some(action) if matches!(
                        action.root.effect_program.as_ref(),
                        Some(ActionEffectProgram::MutateParentWithRows { parent_values_field, .. })
                            if parent_values_field == input_name
                    )) {
                        self.error(&format!("{path}/target/inputName"), "parent property must belong to the parent-values object of a parent-and-rows action");
                    }
                    if !form.input_names.contains(input_name) {
                        self.error(
                            &format!("{path}/target/inputName"),
                            "parent-values field is not included by its form",
                        );
                    }
                    let Some(action) = self.action_for_form(form).cloned() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "parent property form submit action is unavailable",
                        );
                        continue;
                    };
                    let Some(property) = self
                        .parent_object(&action)
                        .and_then(|object| object.property(property_id).cloned())
                    else {
                        self.error(
                            &format!("{path}/target/propertyId"),
                            "parent property does not exist on the action's pinned entity",
                        );
                        continue;
                    };
                    let parent_schema = action.fields.get(input_name).map(|(schema, _)| schema);
                    let Some(type_) = property_type(&property) else {
                        self.error(
                            &format!("{path}/target/propertyId"),
                            "parent property type is unresolved",
                        );
                        continue;
                    };
                    if parent_schema.is_none_or(|schema| !matches!(schema, Schema::Json)) {
                        self.error(&format!("{path}/target/propertyId"), "parent property must be written through the bounded JSON parent-values input");
                    }
                    let lifecycle_managed = self
                        .parent_object(&action)
                        .is_some_and(|object| object.lifecycle(property_id).is_some());
                    let generated = self.parent_object(&action).is_some_and(|object| {
                        object.keys.iter().any(|key| {
                            key.naming_series.is_some()
                                && key.components.iter().any(|id| id.as_str() == property_id)
                        })
                    });
                    if (property.computed || lifecycle_managed || generated)
                        && field.control != WidgetFieldControl::Display
                    {
                        self.error(
                            &format!("{path}/control"),
                            "computed or lifecycle-managed parent properties are read-only; generated parent properties are also read-only",
                        );
                    }
                    self.check_control_type(
                        field.control,
                        field.format,
                        field.option_query_binding_id.as_deref(),
                        &type_,
                        &path,
                    );
                    if !self
                        .presented_form_inputs
                        .insert((form_id.clone(), input_name.clone()))
                    {
                        // Multiple nested properties intentionally share one top-level form input.
                    }
                }
                WidgetFieldTarget::ChildRowProperty {
                    form_id,
                    collection_id,
                    property_id,
                } => {
                    let Some(form) = self.forms.get(form_id.as_str()).copied() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "child-row property target names an unknown form",
                        );
                        continue;
                    };
                    let Some(action) = self.action_for_form(form).cloned() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "child-row property form submit action is unavailable",
                        );
                        continue;
                    };
                    if child_grid_component(self.ux, form_id, collection_id).is_none() {
                        self.error(
                            &format!("{path}/target/collectionId"),
                            "child-row property must be included by its matching child grid",
                        );
                        continue;
                    }
                    let Some(property) = self
                        .child_object(&action, collection_id)
                        .and_then(|object| object.property(property_id).cloned())
                    else {
                        self.error(
                            &format!("{path}/target/propertyId"),
                            "child-row property does not exist on the owned child entity",
                        );
                        continue;
                    };
                    let Some(type_) = property_type(&property) else {
                        self.error(
                            &format!("{path}/target/propertyId"),
                            "child-row property type is unresolved",
                        );
                        continue;
                    };
                    let Some(collection) = self.parent_object(&action).and_then(|object| {
                        object
                            .owned_collections
                            .iter()
                            .find(|candidate| candidate.id.as_str() == collection_id)
                            .cloned()
                    }) else {
                        self.error(
                            &format!("{path}/target/collectionId"),
                            "child-row property names an undeclared owned collection",
                        );
                        continue;
                    };
                    let reserved = property_id == collection.row_id_property.as_str()
                        || property_id == collection.ordering_key.as_str();
                    let lifecycle_managed = self
                        .child_object(&action, collection_id)
                        .is_some_and(|object| object.lifecycle(property_id).is_some());
                    let action_derived = matches!(
                        action.root.effect_program.as_ref(),
                        Some(ActionEffectProgram::MutateParentWithRows { collections, .. })
                            if collections.iter().any(|declared| {
                                declared.id.as_str() == collection_id
                                    && declared.derive.contains_key(&property.id)
                            })
                    );
                    if (reserved || property.computed || lifecycle_managed || action_derived)
                        && field.control != WidgetFieldControl::Display
                    {
                        self.error(&format!("{path}/control"), "row identity, ordering, computed, lifecycle-managed properties are read-only in a child grid; action-derived properties must also be display-only");
                    }
                    self.check_control_type(
                        field.control,
                        field.format,
                        field.option_query_binding_id.as_deref(),
                        &type_,
                        &path,
                    );
                }
            }
            self.check_optional_condition(
                field.visible_when.as_ref(),
                &format!("{path}/visibleWhen"),
            );
            self.check_optional_condition(
                field.required_when.as_ref(),
                &format!("{path}/requiredWhen"),
            );
            self.check_optional_condition(
                field.read_only_when.as_ref(),
                &format!("{path}/readOnlyWhen"),
            );
        }
        let pages = self.ux.pages.clone();
        for (page_index, page) in pages.iter().enumerate() {
            self.check_node(
                &page.root,
                &format!("/payload/ux/pages/{page_index}/root"),
                ActionSurface::Page,
            );
        }
        for (index, field) in fields.iter().enumerate() {
            let path = format!("/payload/ux/fields/{index}");
            match &field.target {
                WidgetFieldTarget::Property { property }
                    if !self
                        .included_fields
                        .contains(&(property.binding_id.clone(), property.property_id.clone())) =>
                {
                    self.error(&path, "property presentation is not explicitly included by a section, list, or field component");
                }
                WidgetFieldTarget::FormInput {
                    form_id,
                    input_name,
                    ..
                } if !self
                    .included_form_inputs
                    .contains(&(form_id.clone(), input_name.clone())) =>
                {
                    self.error(
                        &path,
                        "form input presentation is not included by its form component",
                    );
                }
                WidgetFieldTarget::ParentProperty {
                    form_id,
                    input_name,
                    ..
                } if !self.included_form_inputs.contains(&(form_id.clone(), input_name.clone())) => {
                    self.error(&path, "parent property presentation is not included by its matching form");
                }
                WidgetFieldTarget::ChildRowProperty {
                    form_id,
                    collection_id,
                    ..
                } if !child_grid_component(self.ux, form_id, collection_id).is_some_and(|component| {
                    matches!(component, WidgetComponent::ChildGrid { field_ids, .. } if field_ids.contains(&field.field_id))
                }) => {
                    self.error(&path, "child-row property presentation is not included by its matching ChildGrid.fieldIds");
                }
                WidgetFieldTarget::Property { .. } | WidgetFieldTarget::FormInput { .. } | WidgetFieldTarget::ParentProperty { .. } | WidgetFieldTarget::ChildRowProperty { .. } => {}
            }
            if let WidgetFieldTarget::Property { property } = &field.target {
                let presentations = fields.iter().filter(|candidate| {
                    matches!(&candidate.target, WidgetFieldTarget::Property { property: other }
                        if other.binding_id == property.binding_id && other.property_id == property.property_id)
                }).count();
                if presentations != 1 {
                    self.error(
                        &path,
                        "each included property must have exactly one field presentation",
                    );
                }
            }
            if let WidgetFieldTarget::ParentProperty {
                form_id,
                input_name,
                property_id,
            } = &field.target
            {
                let presentations = fields.iter().filter(|candidate| {
                    matches!(&candidate.target, WidgetFieldTarget::ParentProperty {
                        form_id: other_form,
                        input_name: other_input,
                        property_id: other_property,
                    } if other_form == form_id && other_input == input_name && other_property == property_id)
                }).count();
                if presentations != 1 {
                    self.error(
                        &path,
                        "each nested parent property must have exactly one field presentation",
                    );
                }
            }
            if let WidgetFieldTarget::ChildRowProperty {
                form_id,
                collection_id,
                property_id,
            } = &field.target
            {
                let presentations = fields.iter().filter(|candidate| {
                    matches!(&candidate.target, WidgetFieldTarget::ChildRowProperty {
                        form_id: other_form,
                        collection_id: other_collection,
                        property_id: other_property,
                    } if other_form == form_id && other_collection == collection_id && other_property == property_id)
                }).count();
                if presentations != 1 {
                    self.error(
                        &path,
                        "each child-row property must have exactly one field presentation",
                    );
                }
            }
        }
        for (index, form) in self.ux.forms.iter().enumerate() {
            for (name_index, name) in form.input_names.iter().enumerate() {
                if !self
                    .presented_form_inputs
                    .contains(&(form.form_id.clone(), name.clone()))
                    || !self
                        .included_form_inputs
                        .contains(&(form.form_id.clone(), name.clone()))
                {
                    self.error(
                        &format!("/payload/ux/forms/{index}/inputNames/{name_index}"),
                        "each included form input needs exactly one field presentation",
                    );
                }
            }
        }
        for (binding_id, property_id) in self.included_fields.clone() {
            let presentations = fields
                .iter()
                .filter(|field| {
                    matches!(&field.target, WidgetFieldTarget::Property { property }
                    if property.binding_id == binding_id && property.property_id == property_id)
                })
                .count();
            if presentations != 1 {
                self.error("/payload/ux/fields", format!("included property {binding_id}.{property_id} requires exactly one field presentation"));
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This recursive case walk validates one closed Widget UX component and its child surface."
    )]
    fn check_node(&mut self, node: &WidgetNode, path: &str, surface: ActionSurface) {
        match &node.component {
            WidgetComponent::Section { section } => {
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
            WidgetComponent::List {
                data_binding_id,
                fields,
                ..
            } => {
                if !self
                    .bindings
                    .get(data_binding_id.as_str())
                    .is_some_and(|binding| {
                        matches!(
                            &binding.query,
                            WidgetDataQuery::ObjectList { .. }
                                | WidgetDataQuery::LinkTraversal { .. }
                        )
                    })
                {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "list component requires an existing bounded list or traversal binding",
                    );
                }
                for (index, property) in fields.iter().enumerate() {
                    if property.binding_id != *data_binding_id {
                        self.error(
                            &format!("{path}/component/fields/{index}/bindingId"),
                            "list field must belong to the list binding",
                        );
                    }
                    self.include_property(property, &format!("{path}/component/fields/{index}"));
                }
            }
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
                &surface,
                path,
            ),
            WidgetComponent::Field { field_id } => {
                let Some(field) = self
                    .ux
                    .fields
                    .iter()
                    .find(|field| field.field_id == *field_id)
                    .cloned()
                else {
                    self.error(
                        &format!("{path}/component/fieldId"),
                        "field component names an unknown presentation",
                    );
                    return;
                };
                if !self.used_field_ids.insert(field_id.clone()) {
                    self.error(
                        &format!("{path}/component/fieldId"),
                        "field presentation may appear only once in the component tree",
                    );
                }
                match &field.target {
                    WidgetFieldTarget::Property { property } => {
                        if matches!(&surface, ActionSurface::Form(_)) {
                            self.error(&format!("{path}/component/fieldId"), "form children must present form inputs, not arbitrary object properties");
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
                    } => {
                        if !matches!(&surface, ActionSurface::Form(parent_form) if parent_form == form_id)
                        {
                            self.error(
                                &format!("{path}/component/fieldId"),
                                "form input field must be a child of its matching form component",
                            );
                        }
                        if !self
                            .included_form_inputs
                            .insert((form_id.clone(), input_name.clone()))
                        {
                            self.error(
                                &format!("{path}/component/fieldId"),
                                "form input may be presented only once in its form",
                            );
                        }
                        let schema = self
                            .forms
                            .get(form_id.as_str())
                            .and_then(|form| {
                                self.action_bindings.get(form.submit_binding_id.as_str())
                            })
                            .and_then(|binding| self.actions.get(binding.binding_id.as_str()))
                            .and_then(|action| action.fields.get(input_name))
                            .map(|(schema, _)| schema.clone());
                        if let Some(schema) = schema {
                            let type_ = self.schema_control_type(&schema);
                            let option_type =
                                field.option_query_binding_id.as_deref().and_then(|id| {
                                    self.option_binding_key_type(
                                        id,
                                        &format!("{path}/component/fieldId"),
                                    )
                                });
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
                            let option_type =
                                field.option_query_binding_id.as_deref().and_then(|id| {
                                    self.option_binding_key_type(
                                        id,
                                        &format!("{path}/component/fieldId"),
                                    )
                                });
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
                    WidgetFieldTarget::ParentProperty {
                        form_id,
                        input_name,
                        property_id,
                    } => {
                        if !matches!(&surface, ActionSurface::Form(parent_form) if parent_form == form_id)
                        {
                            self.error(&format!("{path}/component/fieldId"), "parent property field must be a child of its matching form component");
                        }
                        self.included_form_inputs
                            .insert((form_id.clone(), input_name.clone()));
                        let value_type = self
                            .forms
                            .get(form_id.as_str())
                            .and_then(|form| self.action_for_form(form))
                            .and_then(|action| self.parent_object(action))
                            .and_then(|object| object.property(property_id).cloned())
                            .and_then(|property| property_type(&property));
                        let option_type = field.option_query_binding_id.as_deref().and_then(|id| {
                            self.option_binding_key_type(id, &format!("{path}/component/fieldId"))
                        });
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
                    WidgetFieldTarget::ChildRowProperty { .. } => {
                        self.error(
                            &format!("{path}/component/fieldId"),
                            "child-row property fields are rendered by ChildGrid.fieldIds, not standalone field nodes",
                        );
                    }
                }
            }
            WidgetComponent::Action { placement_id } => {
                if let Some(placement) = self.placements.get(placement_id.as_str()).copied() {
                    self.check_action_surface(
                        placement,
                        &surface,
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

    pub(super) fn check_action_surface(
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

    pub(super) fn register_semantic_node(
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

    pub(super) fn check_display_initialization(
        &self,
        control: WidgetFieldControl,
        initialization: &FormInputInitialization,
        path: &str,
    ) {
        if control == WidgetFieldControl::Display
            && initialization.required
            && !initialization.source_available
        {
            self.error(
                path,
                "required display form input must have a valid initial record value",
            );
        }
    }

    pub(super) fn check_control_type(
        &mut self,
        control: WidgetFieldControl,
        format: Option<WidgetFieldFormat>,
        option_binding: Option<&str>,
        value_type: &Type,
        path: &str,
    ) {
        let options = option_binding.and_then(|id| self.option_binding_key_type(id, path));
        self.check_control_type_with_option_type(
            control,
            format,
            options.as_ref(),
            value_type,
            path,
        );
    }

    pub(super) fn check_control_type_with_option_type(
        &self,
        control: WidgetFieldControl,
        format: Option<WidgetFieldFormat>,
        option_key_type: Option<&Type>,
        value_type: &Type,
        path: &str,
    ) {
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
        )]
        let value_type = match value_type {
            Type::Optional(inner) => inner.as_ref(),
            value_type => value_type,
        };
        let textual = matches!(value_type, Type::Text { .. } | Type::Enum { .. });
        let numeric = matches!(
            value_type,
            Type::Integer | Type::Number | Type::Decimal { .. }
        );
        let date = matches!(value_type, Type::Date);
        let datetime = matches!(value_type, Type::Timestamp { .. });
        let boolean = matches!(value_type, Type::Bool);
        let valid = match control {
            WidgetFieldControl::Text | WidgetFieldControl::TextArea => textual,
            WidgetFieldControl::Number => numeric,
            WidgetFieldControl::Date => date,
            WidgetFieldControl::DateTime => datetime,
            WidgetFieldControl::Checkbox => boolean,
            WidgetFieldControl::Select => option_key_type.map_or_else(
                || matches!(value_type, Type::Enum { .. }),
                |key| types_compatible(key, value_type) && types_compatible(value_type, key),
            ),
            WidgetFieldControl::Link => option_key_type.is_some_and(|key| {
                types_compatible(key, value_type) && types_compatible(value_type, key)
            }),
            WidgetFieldControl::Display => true,
        };
        if !valid {
            self.error(&format!("{path}/control"), "field control is incompatible with the resolved property type or missing its option query");
        }
        if let Some(format) = format {
            let compatible = match format {
                WidgetFieldFormat::Text => textual,
                WidgetFieldFormat::Integer => matches!(value_type, Type::Integer),
                WidgetFieldFormat::Decimal
                | WidgetFieldFormat::Currency
                | WidgetFieldFormat::Percent => numeric,
                WidgetFieldFormat::Date => date,
                WidgetFieldFormat::DateTime => datetime,
                WidgetFieldFormat::Boolean => boolean,
                WidgetFieldFormat::MetricBuckets => {
                    control == WidgetFieldControl::Display && matches!(value_type, Type::Json)
                }
            };
            if !compatible {
                self.error(
                    &format!("{path}/format"),
                    "field format hint is incompatible with the property type",
                );
            }
        }
    }

    fn schema_control_type(&mut self, schema: &Schema) -> Type {
        let mut records = BTreeMap::new();
        self.schema_type(schema, &mut records)
    }

    pub(super) fn option_binding_key_type(&mut self, binding_id: &str, path: &str) -> Option<Type> {
        if !self.is_bounded_list_binding(binding_id) {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option query must be a bounded list or traversal binding",
            );
            return None;
        }
        let binding = self.bindings.get(binding_id).copied()?.clone();
        let artifact = self.resolve_ref(
            &binding.object.id,
            &binding.object.version,
            &ArtifactKind::Entity,
            &format!("{path}/optionQueryBindingId/object"),
        )?;
        let Some(Ok(object)) = self.catalog.object_type(&artifact.artifact_id) else {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option binding entity type is unavailable",
            );
            return None;
        };
        let [identity_key] = object.keys.as_slice() else {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option binding requires exactly one single-property identity key",
            );
            return None;
        };
        let [property_id] = identity_key.components.as_slice() else {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option binding requires exactly one single-property identity key",
            );
            return None;
        };
        let Some(property) = object.property(property_id.as_str()) else {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option identity property does not resolve on its entity",
            );
            return None;
        };
        property_type(property)
    }
}
