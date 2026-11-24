//! Field presentations, control/format compatibility, and component-tree validation.

mod nodes;

use std::collections::BTreeMap;

use norn_checker::entity_instance::EffectiveKind;
use norn_expr::Type;
use norn_expr::schema::Schema;
use norn_ir::{
    ActionEffectProgram, ArtifactKind, PropertyRef, PropertyType, WidgetComponent,
    WidgetFieldControl, WidgetFieldFormat, WidgetFieldPresentation, WidgetFieldTarget,
};

use super::child_grid::child_grid_component;
use super::types::{property_type, type_fits_schema, types_compatible};
use super::{ActionSurface, FormInputInitialization, Usage, UxChecker};

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_tree_and_fields(&mut self) {
        let fields = self.ux.fields.clone();
        for (index, field) in fields.iter().enumerate() {
            self.check_field_presentation(field, &format!("/payload/ux/fields/{index}"));
        }
        let pages = self.ux.pages.clone();
        for (page_index, page) in pages.iter().enumerate() {
            self.check_node(
                &page.root,
                &format!("/payload/ux/pages/{page_index}/root"),
                ActionSurface::Page,
            );
        }
        self.check_field_inclusion_and_uniqueness(&fields);
        self.check_form_and_property_coverage(&fields);
    }

    fn check_field_presentation(&mut self, field: &WidgetFieldPresentation, path: &str) {
        if field.label.0.is_empty() || field.label.0.values().any(|value| value.trim().is_empty()) {
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
                self.check_property_field_target(field, property, path);
            }
            WidgetFieldTarget::FormInput {
                form_id,
                input_name,
                initial,
            } => {
                self.check_form_input_field_target(
                    field,
                    form_id,
                    input_name,
                    initial.as_ref(),
                    path,
                );
            }
            WidgetFieldTarget::ParentProperty {
                form_id,
                input_name,
                property_id,
            } => {
                self.check_parent_property_field_target(
                    field,
                    form_id,
                    input_name,
                    property_id,
                    path,
                );
            }
            WidgetFieldTarget::ChildRowProperty {
                form_id,
                collection_id,
                property_id,
            } => {
                self.check_child_row_property_field_target(
                    field,
                    form_id,
                    collection_id,
                    property_id,
                    path,
                );
            }
        }
        self.check_optional_condition(field.visible_when.as_ref(), &format!("{path}/visibleWhen"));
        self.check_optional_condition(
            field.required_when.as_ref(),
            &format!("{path}/requiredWhen"),
        );
        self.check_optional_condition(
            field.read_only_when.as_ref(),
            &format!("{path}/readOnlyWhen"),
        );
    }

    fn check_property_field_target(
        &mut self,
        field: &WidgetFieldPresentation,
        property: &PropertyRef,
        path: &str,
    ) {
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
            if !self.value_source_available_at(&property.binding_id, path, None)
                && self
                    .usage_surfaces
                    .contains_key(&Usage::Field(field.field_id.clone()))
            {
                self.error(
                    path,
                    "field value requires an available detail, row, or single-selection record at every rendering site",
                );
            }
            self.check_control_type(
                field.control,
                field.format,
                field.option_query_binding_id.as_deref(),
                &type_,
                path,
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

    fn check_form_input_field_target(
        &mut self,
        field: &WidgetFieldPresentation,
        form_id: &str,
        input_name: &str,
        initial: Option<&PropertyRef>,
        path: &str,
    ) {
        let Some(form) = self.forms.get(form_id).copied() else {
            self.error(
                &format!("{path}/target/formId"),
                "form input target names an unknown form",
            );
            return;
        };
        if !form.input_names.iter().any(|name| name == input_name) {
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
            return;
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
            return;
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
            path,
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
            initialization.source_available =
                self.check_form_input_initial(form, initial, &schema, path);
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
            .insert((form_id.to_owned(), input_name.to_owned()))
        {
            self.error(
                &format!("{path}/target"),
                "a form input must have exactly one field presentation",
            );
        }
    }

    fn check_form_input_initial(
        &mut self,
        form: &norn_ir::WidgetForm,
        initial: &PropertyRef,
        schema: &Schema,
        path: &str,
    ) -> bool {
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
                self.error(
                    &format!("{path}/target/initial/bindingId"),
                    "prefill property must belong to the form's declared record binding",
                );
            }
        }
        if binding_matches_form
            && !self.value_source_available_at(&initial.binding_id, path, Some(form))
        {
            source_available = false;
            self.error(
                &format!("{path}/target/initial"),
                "prefill record value is unavailable from the form's record source",
            );
        }
        if let Some(property) = self.resolve_property(initial, &format!("{path}/target/initial")) {
            if let Some(source_type) = property_type(property) {
                if !type_fits_schema(&source_type, schema) {
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
        source_available
    }

    fn check_parent_property_field_target(
        &mut self,
        field: &WidgetFieldPresentation,
        form_id: &str,
        input_name: &str,
        property_id: &str,
        path: &str,
    ) {
        let Some(form) = self.forms.get(form_id).copied() else {
            self.error(
                &format!("{path}/target/formId"),
                "parent property target names an unknown form",
            );
            return;
        };
        if !matches!(self.action_for_form(form), Some(action) if matches!(
            action.root.effect_program.as_ref(),
            Some(ActionEffectProgram::MutateParentWithRows { parent_values_field, .. })
                if parent_values_field == input_name
        )) {
            self.error(&format!("{path}/target/inputName"), "parent property must belong to the parent-values object of a parent-and-rows action");
        }
        if !form.input_names.iter().any(|name| name == input_name) {
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
            return;
        };
        let Some(property) = self
            .parent_object(&action)
            .and_then(|object| object.property(property_id).cloned())
        else {
            self.error(
                &format!("{path}/target/propertyId"),
                "parent property does not exist on the action's pinned entity",
            );
            return;
        };
        let parent_schema = action.fields.get(input_name).map(|(schema, _)| schema);
        let Some(type_) = property_type(&property) else {
            self.error(
                &format!("{path}/target/propertyId"),
                "parent property type is unresolved",
            );
            return;
        };
        if parent_schema.is_none_or(|schema| !matches!(schema, Schema::Json)) {
            self.error(
                &format!("{path}/target/propertyId"),
                "parent property must be written through the bounded JSON parent-values input",
            );
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
            path,
        );
        // Multiple nested properties intentionally share one top-level form input.
        let _ = self
            .presented_form_inputs
            .insert((form_id.to_owned(), input_name.to_owned()));
    }

    fn check_child_row_property_field_target(
        &mut self,
        field: &WidgetFieldPresentation,
        form_id: &str,
        collection_id: &str,
        property_id: &str,
        path: &str,
    ) {
        let Some(form) = self.forms.get(form_id).copied() else {
            self.error(
                &format!("{path}/target/formId"),
                "child-row property target names an unknown form",
            );
            return;
        };
        let Some(action) = self.action_for_form(form).cloned() else {
            self.error(
                &format!("{path}/target/formId"),
                "child-row property form submit action is unavailable",
            );
            return;
        };
        if child_grid_component(self.ux, form_id, collection_id).is_none() {
            self.error(
                &format!("{path}/target/collectionId"),
                "child-row property must be included by its matching child grid",
            );
            return;
        }
        let Some(property) = self
            .child_object(&action, collection_id)
            .and_then(|object| object.property(property_id).cloned())
        else {
            self.error(
                &format!("{path}/target/propertyId"),
                "child-row property does not exist on the owned child entity",
            );
            return;
        };
        let Some(type_) = property_type(&property) else {
            self.error(
                &format!("{path}/target/propertyId"),
                "child-row property type is unresolved",
            );
            return;
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
            return;
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
            path,
        );
    }

    fn check_field_inclusion_and_uniqueness(&self, fields: &[WidgetFieldPresentation]) {
        for (index, field) in fields.iter().enumerate() {
            let path = format!("/payload/ux/fields/{index}");
            self.check_field_inclusion(field, &path);
            self.check_field_uniqueness(field, fields, &path);
        }
    }

    fn check_field_inclusion(&self, field: &WidgetFieldPresentation, path: &str) {
        match &field.target {
            WidgetFieldTarget::Property { property }
                if !self
                    .included_fields
                    .contains(&(property.binding_id.clone(), property.property_id.clone())) =>
            {
                self.error(path, "property presentation is not explicitly included by a section, list, or field component");
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
                    path,
                    "form input presentation is not included by its form component",
                );
            }
            WidgetFieldTarget::ParentProperty {
                form_id,
                input_name,
                ..
            } if !self
                .included_form_inputs
                .contains(&(form_id.clone(), input_name.clone())) =>
            {
                self.error(
                    path,
                    "parent property presentation is not included by its matching form",
                );
            }
            WidgetFieldTarget::ChildRowProperty {
                form_id,
                collection_id,
                ..
            } if !child_grid_component(self.ux, form_id, collection_id).is_some_and(
                |component| {
                    matches!(component, WidgetComponent::ChildGrid { field_ids, .. } if field_ids.contains(&field.field_id))
                },
            ) =>
            {
                self.error(path, "child-row property presentation is not included by its matching ChildGrid.fieldIds");
            }
            WidgetFieldTarget::Property { .. }
            | WidgetFieldTarget::FormInput { .. }
            | WidgetFieldTarget::ParentProperty { .. }
            | WidgetFieldTarget::ChildRowProperty { .. } => {}
        }
    }

    fn check_field_uniqueness(
        &self,
        field: &WidgetFieldPresentation,
        fields: &[WidgetFieldPresentation],
        path: &str,
    ) {
        if let WidgetFieldTarget::Property { property } = &field.target {
            let presentations = fields
                .iter()
                .filter(|candidate| {
                    matches!(&candidate.target, WidgetFieldTarget::Property { property: other }
                        if other.binding_id == property.binding_id && other.property_id == property.property_id)
                })
                .count();
            if presentations != 1 {
                self.error(
                    path,
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
            let presentations = fields
                .iter()
                .filter(|candidate| {
                    matches!(&candidate.target, WidgetFieldTarget::ParentProperty {
                        form_id: other_form,
                        input_name: other_input,
                        property_id: other_property,
                    } if other_form == form_id && other_input == input_name && other_property == property_id)
                })
                .count();
            if presentations != 1 {
                self.error(
                    path,
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
            let presentations = fields
                .iter()
                .filter(|candidate| {
                    matches!(&candidate.target, WidgetFieldTarget::ChildRowProperty {
                        form_id: other_form,
                        collection_id: other_collection,
                        property_id: other_property,
                    } if other_form == form_id && other_collection == collection_id && other_property == property_id)
                })
                .count();
            if presentations != 1 {
                self.error(
                    path,
                    "each child-row property must have exactly one field presentation",
                );
            }
        }
    }

    fn check_form_and_property_coverage(&self, fields: &[WidgetFieldPresentation]) {
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
                self.error(
                    "/payload/ux/fields",
                    format!(
                        "included property {binding_id}.{property_id} requires exactly one field presentation"
                    ),
                );
            }
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

    pub(super) fn schema_control_type(&mut self, schema: &Schema) -> Type {
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
