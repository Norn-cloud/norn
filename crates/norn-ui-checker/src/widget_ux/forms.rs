//! Curated form contracts, version-conflict bindings, and parent-and-rows submission rules.

use std::collections::BTreeSet;

use norn_expr::Type;
use norn_expr::schema::Schema;
use norn_ir::{
    ActionEffectProgram, WidgetDataQuery, WidgetFieldControl, WidgetFieldTarget, WidgetForm,
    WidgetFormMode, WidgetUxActionBinding,
};

use norn_checker::entity_instance::EffectiveObjectType;

use super::actions::direct_draft_mapping;
use super::types::{property_type, type_fits_schema};
use super::{ParsedAction, UxChecker, has_initial_lifecycle_default};

impl UxChecker<'_, '_, '_> {
    pub(super) fn action_for_form(&self, form: &WidgetForm) -> Option<&ParsedAction> {
        let binding = self.action_bindings.get(form.submit_binding_id.as_str())?;
        self.actions.get(binding.binding_id.as_str())
    }

    pub(super) fn parent_object(&self, action: &ParsedAction) -> Option<EffectiveObjectType> {
        let ActionEffectProgram::MutateParentWithRows {
            entity_artifact_id, ..
        } = action.root.effect_program.as_ref()?
        else {
            return None;
        };
        self.catalog.object_type(entity_artifact_id)?.ok().cloned()
    }

    pub(super) fn child_object(
        &self,
        action: &ParsedAction,
        collection_id: &str,
    ) -> Option<EffectiveObjectType> {
        let parent = self.parent_object(action)?;
        let collection = parent
            .owned_collections
            .iter()
            .find(|candidate| candidate.id.as_str() == collection_id)?;
        self.catalog
            .object_type_named(collection.child_type.as_str())?
            .ok()
            .cloned()
    }

    pub(super) fn check_forms(&mut self) {
        let forms = self.ux.forms.clone();
        for (index, form) in forms.iter().enumerate() {
            let path = format!("/payload/ux/forms/{index}");
            self.check_generated_transition_condition(&form.submit_binding_id, &path);
            if form.submit.label.0.is_empty()
                || form
                    .submit
                    .label
                    .0
                    .values()
                    .any(|label| label.trim().is_empty())
            {
                self.error(
                    &format!("{path}/submit/label"),
                    "form submit label must contain nonblank localized text",
                );
            }
            self.check_confirmation(
                &form.submit.confirmation,
                &format!("{path}/submit/confirmation"),
            );
            let Some(binding) = self
                .action_bindings
                .get(form.submit_binding_id.as_str())
                .copied()
            else {
                self.error(
                    &format!("{path}/submitBindingId"),
                    "form submit action binding does not exist",
                );
                continue;
            };
            let input_mapping = binding.input_mapping.clone();
            let draft_form_id = binding.inputs.draft_form_id.clone();
            let record_binding_id = binding.inputs.record_binding_id.clone();
            let Some(action) = self.actions.get(binding.binding_id.as_str()).cloned() else {
                self.error(
                    &format!("{path}/submitBindingId"),
                    "form submit ActionIR schema could not be resolved",
                );
                continue;
            };
            self.check_parent_rows_form_contract(form, binding, &action, &path);
            for (name, (_, required)) in &action.fields {
                if *required && !input_mapping.contains_key(name) {
                    self.error(
                        &format!("{path}/submitBindingId"),
                        format!("required submit input {name:?} has no action binding mapping"),
                    );
                }
            }
            let Ok(form_binding) = self.check_form_data_binding(form, &path) else {
                continue;
            };
            self.check_form_input_names(
                form,
                &action,
                &input_mapping,
                draft_form_id.as_deref(),
                &path,
            );
            if let Some(version) = &form.version_conflict_binding {
                self.check_form_version_conflict(
                    form,
                    version,
                    form_binding,
                    &action,
                    &input_mapping,
                    record_binding_id.as_deref(),
                    &path,
                );
            }
        }
    }

    fn check_form_data_binding<'a>(
        &self,
        form: &'a WidgetForm,
        path: &str,
    ) -> Result<Option<&'a str>, ()> {
        match form.mode {
            WidgetFormMode::Edit => {
                let Some(data_binding_id) = &form.data_binding_id else {
                    self.error(
                        &format!("{path}/dataBindingId"),
                        "edit form requires an object-detail binding",
                    );
                    return Err(());
                };
                if !self
                    .bindings
                    .get(data_binding_id.as_str())
                    .is_some_and(|data| {
                        matches!(
                            &data.query,
                            WidgetDataQuery::ObjectDetail { .. }
                                | WidgetDataQuery::ObjectRuntimeDetail { .. }
                        )
                    })
                {
                    self.error(
                        &format!("{path}/dataBindingId"),
                        "edit form must use an object-detail binding",
                    );
                }
                Ok(Some(data_binding_id.as_str()))
            }
            WidgetFormMode::Create | WidgetFormMode::ActionInput => {
                if let Some(data_binding_id) = form.data_binding_id.as_deref() {
                    if !self.record_source_available_for_form(form, data_binding_id) {
                        self.error(&format!("{path}/dataBindingId"), "create and action_input form prefill requires a declared record or list-row source");
                    }
                    Ok(Some(data_binding_id))
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn check_form_input_names(
        &self,
        form: &WidgetForm,
        action: &ParsedAction,
        input_mapping: &std::collections::BTreeMap<String, norn_ir::ZenExpression>,
        draft_form_id: Option<&str>,
        path: &str,
    ) {
        let mut names = BTreeSet::new();
        for (name_index, name) in form.input_names.iter().enumerate() {
            if name.trim().is_empty() || !names.insert(name.as_str()) {
                self.error(
                    &format!("{path}/inputNames/{name_index}"),
                    "form input names must be nonblank and unique",
                );
            }
            if !action.fields.contains_key(name) {
                self.error(
                    &format!("{path}/inputNames/{name_index}"),
                    "form input name is not declared by its submit ActionIR",
                );
            }
            if input_mapping.contains_key(name) {
                let expected = format!("input.draft.{name}");
                if input_mapping
                    .get(name)
                    .is_none_or(|expression| expression.as_str().trim() != expected.as_str())
                {
                    self.error(
                        &format!("{path}/inputNames/{name_index}"),
                        "form input must map directly from its matching input.draft member",
                    );
                }
            } else {
                self.error(
                    &format!("{path}/inputNames/{name_index}"),
                    "form input has no action input mapping",
                );
            }
        }
        if (!form.input_names.is_empty() || draft_form_id.is_some())
            && draft_form_id != Some(form.form_id.as_str())
        {
            self.error(
                &format!("{path}/submitBindingId"),
                "submit action binding must explicitly declare this form as input.draft",
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "version conflict validation connects the form, conflict binding, submit action, and input mappings"
    )]
    fn check_form_version_conflict(
        &mut self,
        form: &WidgetForm,
        version: &norn_ir::WidgetVersionConflictBinding,
        form_binding: Option<&str>,
        action: &ParsedAction,
        input_mapping: &std::collections::BTreeMap<String, norn_ir::ZenExpression>,
        record_binding_id: Option<&str>,
        path: &str,
    ) {
        if form.mode != WidgetFormMode::Edit {
            self.error(
                &format!("{path}/versionConflictBinding"),
                "version conflict binding is only valid on an edit form",
            );
        }
        let Some(data_binding_id) = form_binding else {
            return;
        };
        if version.property.binding_id != data_binding_id {
            self.error(
                &format!("{path}/versionConflictBinding/property"),
                "version property must belong to the edited object binding",
            );
        }
        let Some(property) = self.resolve_property(
            &version.property,
            &format!("{path}/versionConflictBinding/property"),
        ) else {
            return;
        };
        let property_type_ = property_type(property);
        if property.nullable
            || property.computed
            || !property_type_
                .as_ref()
                .is_some_and(|ty| matches!(ty, Type::Integer | Type::Text { .. }))
        {
            self.error(
                &format!("{path}/versionConflictBinding/property"),
                "version property must be a required, stored integer or text property",
            );
        }
        let expected_source = format!("input.record.{}", version.property.property_id);
        if record_binding_id != Some(data_binding_id)
            || input_mapping
                .get(&version.input_name)
                .is_none_or(|expr| expr.as_str().trim() != expected_source)
        {
            self.error(
                &format!("{path}/versionConflictBinding/inputName"),
                "version input must map directly from the matching edited record property",
            );
        }
        if !action.fields.contains_key(&version.input_name) {
            self.error(
                &format!("{path}/versionConflictBinding/inputName"),
                "version input is not declared by the submit ActionIR",
            );
        } else if form.input_names.contains(&version.input_name) {
            self.error(
                &format!("{path}/versionConflictBinding/inputName"),
                "version input is internal and must not be included as an editable form field",
            );
        } else if let Some((schema, required)) = action.fields.get(&version.input_name)
            && (!required
                || property_type_
                    .as_ref()
                    .is_none_or(|ty| !type_fits_schema(ty, schema)))
        {
            self.error(
                &format!("{path}/versionConflictBinding/inputName"),
                "version input must be required and match the edited record's version type",
            );
        }
    }

    fn check_parent_rows_form_contract(
        &mut self,
        form: &WidgetForm,
        binding: &WidgetUxActionBinding,
        action: &ParsedAction,
        path: &str,
    ) {
        let Some(ActionEffectProgram::MutateParentWithRows {
            object_id_field,
            entity_artifact_id,
            entity_artifact_version,
            parent_values_field,
            row_ops_field,
            parent_version_field,
            ..
        }) = action.root.effect_program.as_ref()
        else {
            return;
        };

        if !matches!(form.mode, WidgetFormMode::Create | WidgetFormMode::Edit) {
            self.error(
                &format!("{path}/mode"),
                "parent-and-rows submit requires a create or edit form",
            );
        }
        for name in [parent_values_field, row_ops_field] {
            if !form.input_names.contains(name) {
                self.error(
                    &format!("{path}/inputNames"),
                    format!("parent-and-rows form must include action input {name:?}"),
                );
            }
        }
        if !direct_draft_mapping(binding, parent_values_field)
            || !direct_draft_mapping(binding, row_ops_field)
        {
            self.error(
                &format!("{path}/submitBindingId"),
                "parent values and row operations must map directly from their matching form draft members",
            );
        }
        if !action
            .fields
            .get(parent_values_field)
            .is_some_and(|(schema, _)| matches!(schema, Schema::Json))
        {
            self.error(
                &format!("{path}/submitBindingId"),
                "parent-values action input must use the bounded JSON leaf schema; the kernel validates its object shape",
            );
        }
        if !action
            .fields
            .get(row_ops_field)
            .is_some_and(|(schema, _)| matches!(schema, Schema::Json))
        {
            self.error(
                &format!("{path}/submitBindingId"),
                "row-operations action input must use the bounded JSON leaf schema; the kernel validates its operation array",
            );
        }

        match form.mode {
            WidgetFormMode::Edit => self.check_parent_rows_edit_contract(
                form,
                binding,
                object_id_field,
                parent_version_field,
                entity_artifact_id,
                entity_artifact_version,
                path,
            ),
            WidgetFormMode::Create => self.check_parent_rows_create_contract(
                form,
                binding,
                action,
                object_id_field,
                parent_version_field,
                parent_values_field,
                path,
            ),
            WidgetFormMode::ActionInput => {}
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "edit contract validation checks the parent record snapshot and version mapping fields"
    )]
    fn check_parent_rows_edit_contract(
        &self,
        form: &WidgetForm,
        binding: &WidgetUxActionBinding,
        object_id_field: &str,
        parent_version_field: &str,
        entity_artifact_id: &str,
        entity_artifact_version: &str,
        path: &str,
    ) {
        let binding_matches =
            form.data_binding_id.as_deref() == binding.inputs.record_binding_id.as_deref();
        if !binding_matches
            || binding
                .input_mapping
                .get(object_id_field)
                .is_none_or(|expression| expression.as_str().trim() != "input.recordKey")
            || binding
                .input_mapping
                .get(parent_version_field)
                .is_none_or(|expression| expression.as_str().trim() != "input.recordVersion")
        {
            self.error(
                &format!("{path}/submitBindingId"),
                "edit submit must use its bound parent record key and authoritative record version",
            );
        }
        let parent_binding_matches = form
            .data_binding_id
            .as_deref()
            .and_then(|id| self.bindings.get(id))
            .is_some_and(|parent_binding| {
                parent_binding.object.id == entity_artifact_id
                    && parent_binding.object.version == entity_artifact_version
                    && matches!(
                        parent_binding.query,
                        WidgetDataQuery::ObjectDetail { .. }
                            | WidgetDataQuery::ObjectRuntimeDetail { .. }
                    )
            });
        if !parent_binding_matches {
            self.error(
                &format!("{path}/dataBindingId"),
                "edit form must bind the exact parent entity detail whose version protects the mutation",
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "create contract validation checks the parent id draft mapping, initial version zero, and property coverage"
    )]
    fn check_parent_rows_create_contract(
        &mut self,
        form: &WidgetForm,
        binding: &WidgetUxActionBinding,
        action: &ParsedAction,
        object_id_field: &str,
        parent_version_field: &str,
        parent_values_field: &str,
        path: &str,
    ) {
        let id_is_draft = binding
            .input_mapping
            .get(object_id_field)
            .is_some_and(|expression| {
                let expected_prefix = "input.draft.";
                expression
                    .as_str()
                    .trim()
                    .strip_prefix(expected_prefix)
                    .is_some_and(|name| {
                        name.bytes().enumerate().all(|(index, byte)| {
                            byte == b'_'
                                || byte.is_ascii_alphanumeric()
                                    && (index > 0 || !byte.is_ascii_digit())
                        }) && form.input_names.iter().any(|input| input == name)
                            && self.ux.fields.iter().any(|field| {
                                matches!(&field.target, WidgetFieldTarget::FormInput { form_id, input_name, .. }
                                    if form_id == &form.form_id && input_name == name)
                            })
                    })
            });
        let version_is_zero =
            binding
                .input_mapping
                .get(parent_version_field)
                .is_some_and(|expression| {
                    serde_json::from_str::<String>(expression.as_str().trim())
                        .is_ok_and(|value| value == "0")
                });
        if !id_is_draft || !version_is_zero {
            self.error(
                &format!("{path}/submitBindingId"),
                "create submit must use an explicit parent-id form input and parent version \"0\"",
            );
        }
        self.check_parent_create_property_coverage(form, action, parent_values_field, path);
    }

    fn check_parent_create_property_coverage(
        &mut self,
        form: &WidgetForm,
        action: &ParsedAction,
        parent_values_field: &str,
        path: &str,
    ) {
        let Some(parent) = self.parent_object(action) else {
            self.error(
                &format!("{path}/submitBindingId"),
                "create parent property coverage cannot be checked because the pinned parent entity is unavailable",
            );
            return;
        };
        for lifecycle in &parent.lifecycles {
            let property_id = lifecycle.lifecycle.property.as_str();
            let Some(property) = parent.property(property_id) else {
                continue;
            };
            if !property.nullable
                && !has_initial_lifecycle_default(property, &lifecycle.lifecycle.initial)
            {
                self.error(
                    &format!("{path}/submitBindingId"),
                    format!(
                        "create form requires lifecycle property {property_id:?} to declare a default in one of its initial states because the lifecycle field is read-only"
                    ),
                );
            }
        }
        for property in parent.properties.iter().filter(|property| {
            !property.nullable
                && !property.computed
                && property.default.is_none()
                && parent.lifecycle(property.id.as_str()).is_none()
        }) {
            let presented = self.ux.fields.iter().any(|field| {
                matches!(
                    &field.target,
                    WidgetFieldTarget::ParentProperty {
                        form_id,
                        input_name,
                        property_id,
                    } if form_id == &form.form_id
                        && input_name == parent_values_field
                        && property_id == property.id.as_str()
                ) && self.used_field_ids.contains(&field.field_id)
                    && field.control != WidgetFieldControl::Display
                    && field.visible_when.is_none()
                    && field.read_only_when.is_none()
            });
            if !presented {
                self.error(
                    &format!("{path}/submitBindingId"),
                    format!(
                        "create form must expose required parent property {:?} as an unconditional editable field or supply a declared default",
                        property.id.as_str()
                    ),
                );
            }
        }
    }
}
