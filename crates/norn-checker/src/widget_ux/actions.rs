//! Action bindings, ZEN input mappings, placements, and confirmation strength checks.

use std::collections::BTreeMap;

use norn_expr::schema::Schema;
use norn_expr::{
    ConstructionSource, Expression, InferredType, Type, ZenTypeEnv, check_expression_with_sources,
};
use norn_ir::{
    ActionEffectProgram, ArtifactKind, ArtifactPayload, WidgetActionConfirmation,
    WidgetActionLocation, WidgetActionSeverity, WidgetExpressionInputs, WidgetSelectionMode,
    WidgetUxActionBinding,
};

use super::types::{
    inferred_matches_schema, property_type, schema_accepts_empty_array, type_is_integer_source,
    value_type,
};
use super::{ParsedAction, UxChecker};

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_actions(&mut self) {
        let bindings = self.ux.action_bindings.clone();
        for (index, binding) in bindings.iter().enumerate() {
            let path = format!("/payload/ux/actionBindings/{index}");
            if binding.required_capability.trim().is_empty() {
                self.error(
                    &format!("{path}/requiredCapability"),
                    "required capability must not be blank",
                );
            }
            let Some(action_artifact) = self.resolve_ref(
                &binding.action.id,
                &binding.action.version,
                &ArtifactKind::Action,
                &format!("{path}/action"),
            ) else {
                continue;
            };
            let ArtifactPayload::Action(payload) = &action_artifact.payload else {
                continue;
            };
            let Some(root) = self.effect_action(
                self.owner,
                self.ux,
                &binding.binding_id,
                &format!("{path}/action"),
            ) else {
                continue;
            };
            let Some(raw_schema) = payload.input_schema.as_value() else {
                self.error(
                    &format!("{path}/action"),
                    "ActionIR input schema is missing",
                );
                continue;
            };
            let schema = match Schema::parse_action(raw_schema, "$.inputSchema") {
                Ok(schema) => schema,
                Err(error) => {
                    self.error(
                        &format!("{path}/action"),
                        format!("ActionIR input schema is invalid: {}", error.reason),
                    );
                    continue;
                }
            };
            let Some(fields) = schema.properties() else {
                self.error(
                    &format!("{path}/action"),
                    "ActionIR input schema must be a closed object",
                );
                continue;
            };
            let required = schema.required().cloned().unwrap_or_default();
            let parsed = ParsedAction {
                fields: fields
                    .iter()
                    .map(|(name, schema)| (name.clone(), (schema.clone(), required.contains(name))))
                    .collect(),
                root,
            };
            self.actions.insert(binding.binding_id.clone(), parsed);
        }
        for (index, binding) in bindings.iter().enumerate() {
            let path = format!("/payload/ux/actionBindings/{index}/inputMapping");
            let input_env = self.expression_env(
                &binding.inputs,
                &format!("/payload/ux/actionBindings/{index}"),
            );
            if let Some(action) = self.actions.get(binding.binding_id.as_str()).cloned() {
                self.check_action_mapping(binding, &action, input_env.as_ref(), &path);
            }
        }
    }

    fn check_action_mapping(
        &mut self,
        binding: &WidgetUxActionBinding,
        action: &ParsedAction,
        input_env: Option<&ZenTypeEnv>,
        path: &str,
    ) {
        for (name, (schema, required)) in &action.fields {
            if let Some(expression) = binding.input_mapping.get(name) {
                self.check_mapped_expression(
                    expression,
                    schema,
                    *required,
                    binding,
                    name,
                    action,
                    input_env,
                    &format!("{path}/{name}"),
                );
            }
        }
        for name in binding.input_mapping.keys() {
            if !action.fields.contains_key(name) {
                self.error(
                    &format!("{path}/{name}"),
                    "mapping names an input not declared by ActionIR",
                );
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mapping admission needs the exact action field, form binding, expected schema, expression environment, and diagnostic path"
    )]
    fn check_mapped_expression(
        &mut self,
        source: &norn_ir::ZenExpression,
        schema: &Schema,
        required: bool,
        binding: &WidgetUxActionBinding,
        action_field_name: &str,
        action: &ParsedAction,
        env: Option<&ZenTypeEnv>,
        path: &str,
    ) {
        let Some(env) = env else {
            return;
        };
        let expression = match Expression::compile(source.as_str()) {
            Ok(expression) => expression,
            Err(error) => {
                self.error(path, format!("ZEN mapping does not compile: {error}"));
                return;
            }
        };
        // Parent/row mutations intentionally carry their bounded canonical
        // payloads as opaque JSON leaves. Permit only whole-value forwarding
        // from the matching declared form draft member; member access or
        // expression composition remains subject to normal ZEN type checks.
        if self.is_parent_rows_draft_json_mapping(
            source,
            schema,
            required,
            binding,
            action_field_name,
            action,
        ) {
            return;
        }
        // Empty lists are type-correct for any array item schema, but the
        // expression checker needs an expected `List<T>` to infer their
        // element type. Their shape is exact and they have no members to
        // validate, so admit this literal for array and JSON inputs directly.
        if source.as_str().trim() == "[]" && schema_accepts_empty_array(schema) {
            return;
        }
        // Let the expression checker infer the exact constructed record shape;
        // ActionIR schemas are then checked recursively below so optional
        // object members need not be fabricated in every expression.
        match check_expression_with_sources(
            &expression,
            env,
            &Type::Json,
            &norn_ir::ResourceLimitProfile::norn_limits_v1(),
        ) {
            Ok((checked, construction)) => {
                // InferredType intentionally erases Integer versus Decimal.
                // Check every constructed integer leaf against its own source,
                // retaining all members when list inference joins their types.
                let sources = construction.as_ref().map(|source| vec![source]);
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
                )]
                let inferred = match checked.inferred() {
                    InferredType::Optional(actual)
                        if !required
                            && self.is_optional_draft_member_mapping(source, &binding.inputs) =>
                    {
                        actual.as_ref()
                    }
                    inferred => inferred,
                };
                let mut integer_proof = |sources: &[&ConstructionSource], leaf_path: &str| {
                    !sources.is_empty()
                        && sources.iter().all(|source| {
                            matches!(source, ConstructionSource::Scalar(source)
                                if self.integer_source_proven(source, &binding.inputs, leaf_path))
                        })
                };
                let schema_result = inferred_matches_schema(
                    inferred,
                    schema,
                    sources.as_deref(),
                    &mut integer_proof,
                    path,
                );
                if let Err(error) = schema_result {
                    self.error(path, error);
                }
            }
            Err(errors) => {
                for error in errors {
                    self.error(
                        path,
                        format!("ZEN mapping has incompatible type: {}", error.message),
                    );
                }
            }
        }
    }

    fn is_parent_rows_draft_json_mapping(
        &self,
        source: &norn_ir::ZenExpression,
        schema: &Schema,
        required: bool,
        binding: &WidgetUxActionBinding,
        action_field_name: &str,
        action: &ParsedAction,
    ) -> bool {
        if !required || !matches!(schema, Schema::Json) {
            return false;
        }
        let Some(ActionEffectProgram::MutateParentWithRows {
            parent_values_field,
            row_ops_field,
            ..
        }) = action.root.effect_program.as_ref()
        else {
            return false;
        };
        if action_field_name != parent_values_field && action_field_name != row_ops_field {
            return false;
        }
        if binding.input_mapping.get(action_field_name) != Some(source) {
            return false;
        }
        if source.as_str().trim() != format!("input.draft.{action_field_name}") {
            return false;
        }
        let Some(form_id) = binding.inputs.draft_form_id.as_deref() else {
            return false;
        };
        self.forms.get(form_id).is_some_and(|form| {
            form.submit_binding_id == binding.binding_id
                && form
                    .input_names
                    .iter()
                    .any(|input_name| input_name == action_field_name)
        })
    }

    fn is_optional_draft_member_mapping(
        &self,
        source: &norn_ir::ZenExpression,
        inputs: &WidgetExpressionInputs,
    ) -> bool {
        let mut parts = source.as_str().trim().split('.');
        if parts.next() != Some("input") || parts.next() != Some("draft") {
            return false;
        }
        let Some(name) = parts.next() else {
            return false;
        };
        if parts.next().is_some() {
            return false;
        }
        let Some(form_id) = inputs.draft_form_id.as_deref() else {
            return false;
        };
        let Some(form) = self.forms.get(form_id) else {
            return false;
        };
        if !form.input_names.iter().any(|input_name| input_name == name) {
            return false;
        }
        let Some(binding) = self.action_bindings.get(form.submit_binding_id.as_str()) else {
            return false;
        };
        self.actions
            .get(binding.binding_id.as_str())
            .and_then(|action| action.fields.get(name))
            .is_some_and(|(_, required)| !required)
    }

    fn integer_source_proven(
        &mut self,
        source: &str,
        inputs: &WidgetExpressionInputs,
        path: &str,
    ) -> bool {
        self.direct_input_type(source, inputs, path)
            .as_ref()
            .is_some_and(type_is_integer_source)
            || norn_ir::Decimal::parse(source.trim()).is_ok_and(|value| value.is_integer())
    }

    fn direct_input_type(
        &mut self,
        source: &str,
        inputs: &WidgetExpressionInputs,
        path: &str,
    ) -> Option<Type> {
        let trimmed = source.trim();
        let mut parts = trimmed.split('.');
        if parts.next()? != "input" {
            return None;
        }
        let source_kind = parts.next()?;
        let member = parts.next();
        if parts.next().is_some() {
            return None;
        }
        if source_kind == "recordKey" || source_kind == "recordVersion" {
            inputs.record_binding_id.as_ref()?;
            return Some(Type::text());
        }
        let member = member?;
        match source_kind {
            "record" => {
                let binding_id = inputs.record_binding_id.as_deref()?;
                let property = norn_ir::PropertyRef {
                    binding_id: binding_id.to_owned(),
                    property_id: member.to_owned(),
                };
                self.resolve_property(&property, path)
                    .and_then(property_type)
            }
            "context" => {
                let page_id = inputs.context_page_id.as_deref()?;
                let page = self.pages.get(page_id)?;
                let param = page
                    .context_params
                    .iter()
                    .find(|param| param.param_id == member)?;
                Some(value_type(param.value_type, param.required))
            }
            "draft" => {
                let form_id = inputs.draft_form_id.as_deref()?;
                let form = (*self.forms.get(form_id)?).clone();
                let binding = (*self.action_bindings.get(form.submit_binding_id.as_str())?).clone();
                let (schema, required) = self
                    .actions
                    .get(binding.binding_id.as_str())?
                    .fields
                    .get(member)?
                    .clone();
                let mut records = BTreeMap::new();
                let mut type_ = self.schema_type(&schema, &mut records);
                if !required {
                    type_ = type_.optional();
                }
                Some(type_)
            }
            _ => None,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This pass validates placement contexts, confirmation, and merged ActionIR inputs together."
    )]
    pub(super) fn check_placements(&mut self) {
        let placements = self.ux.action_placements.clone();
        for (index, placement) in placements.iter().enumerate() {
            let path = format!("/payload/ux/actionPlacements/{index}");
            self.check_generated_transition_condition(&placement.binding_id, &path);
            if placement.label.0.is_empty()
                || placement
                    .label
                    .0
                    .values()
                    .any(|label| label.trim().is_empty())
            {
                self.error(
                    &format!("{path}/label"),
                    "action placement label must contain nonblank localized text",
                );
            }
            let action_binding = self
                .action_bindings
                .get(placement.binding_id.as_str())
                .copied()
                .cloned();
            if action_binding.is_none() {
                self.error(
                    &format!("{path}/bindingId"),
                    "action placement names an unknown action binding",
                );
            }
            if let Some(form_id) = &placement.form_id {
                let pages = self.pages_at(&path);
                if pages.is_empty()
                    || self.rendered_forms.get(form_id).is_none_or(|rendered| {
                        pages
                            .iter()
                            .any(|page| page.is_none() || !rendered.contains(page))
                    })
                {
                    self.error(
                        &format!("{path}/formId"),
                        "form opener requires a matching Form node on every page where it is rendered",
                    );
                }
                if let Some(form) = self.forms.get(form_id.as_str()) {
                    if form.submit_binding_id != placement.binding_id {
                        self.error(
                            &format!("{path}/formId"),
                            "a form-opening placement must use the form's submit action binding",
                        );
                    }
                    if action_binding
                        .as_ref()
                        .and_then(|binding| binding.inputs.record_binding_id.as_deref())
                        != form.data_binding_id.as_deref()
                    {
                        self.error(&format!("{path}/formId"), "form opener record snapshot must match the form's declared data binding");
                    }
                    if placement.confirmation.severity != WidgetActionSeverity::Safe
                        || placement.confirmation.strength
                            != norn_ir::WidgetConfirmationStrength::None
                        || placement.confirmation.message.is_some()
                        || placement.confirmation.phrase.is_some()
                    {
                        self.error(&format!("{path}/confirmation"), "a form-opening placement is a safe, unconfirmed opener; confirmation belongs to form submission");
                    }
                    if placement.location == WidgetActionLocation::FormFooter {
                        self.error(
                            &format!("{path}/formId"),
                            "form_footer placements are secondary actions and cannot open a form",
                        );
                    }
                } else {
                    self.error(
                        &format!("{path}/formId"),
                        "action placement names an unknown form",
                    );
                }
            }
            if placement
                .group
                .as_ref()
                .is_some_and(|group| group.trim().is_empty())
            {
                self.error(&format!("{path}/group"), "action group must not be blank");
            }
            if !placement.selection_input_mapping.is_empty()
                && placement.location != WidgetActionLocation::Selection
            {
                self.error(
                    &format!("{path}/selectionInputMapping"),
                    "selection input mappings are only valid on selection placements",
                );
            }
            if matches!(
                placement.location,
                WidgetActionLocation::Row | WidgetActionLocation::Selection
            ) {
                let Some(selection) = placement.selection_binding_id.as_deref() else {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "row and selection actions require a list binding",
                    );
                    continue;
                };
                if !self.is_bounded_list_binding(selection) {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "action selection source must be a bounded list or traversal binding",
                    );
                }
                if placement.location == WidgetActionLocation::Selection
                    && self
                        .list_selections
                        .get(selection)
                        .is_none_or(|mode| *mode == WidgetSelectionMode::None)
                {
                    self.error(&format!("{path}/selectionBindingId"), "selection actions require a list with single or multiple selection enabled");
                }
                if placement.location == WidgetActionLocation::Row
                    && action_binding.as_ref().is_some_and(|binding| {
                        binding
                            .inputs
                            .record_binding_id
                            .as_deref()
                            .is_some_and(|record| record != selection)
                    })
                {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "row action input.record must name its containing list binding",
                    );
                }
                if placement.location == WidgetActionLocation::Selection
                    && action_binding.as_ref().is_some_and(|binding| {
                        binding
                            .inputs
                            .record_binding_id
                            .as_deref()
                            .is_some_and(|record| record != selection)
                    })
                {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "selection action input.record must name its selected list binding",
                    );
                }
                if let Some(binding_id) = action_binding
                    .as_ref()
                    .and_then(|binding| binding.inputs.record_binding_id.as_deref())
                    .filter(|binding_id| self.list_row_contexts.contains(*binding_id))
                {
                    let contextual_record = (placement.location == WidgetActionLocation::Row
                        && binding_id == selection)
                        || (placement.location == WidgetActionLocation::Selection
                            && binding_id == selection
                            && self.list_selections.get(selection)
                                == Some(&WidgetSelectionMode::Single));
                    if !contextual_record {
                        self.error(&format!("{path}/selectionBindingId"), "a list-row record input is available only to its row action or single-selection action");
                    }
                }
                if placement.location == WidgetActionLocation::Selection
                    && action_binding.as_ref().is_some_and(|binding| {
                        binding
                            .inputs
                            .selection_binding_id
                            .as_deref()
                            .is_some_and(|declared| declared != selection)
                    })
                {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "selection action input.selection must name the placement's list binding",
                    );
                }
                if placement.location != WidgetActionLocation::Selection
                    && action_binding
                        .as_ref()
                        .is_some_and(|binding| binding.inputs.selection_binding_id.is_some())
                {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "input.selection is only available to a selection placement",
                    );
                }
            } else if placement.selection_binding_id.is_some()
                || !placement.selection_input_mapping.is_empty()
            {
                self.error(
                    &format!("{path}/selectionBindingId"),
                    "selection inputs are supported only on row or selection placements",
                );
            }
            self.check_confirmation(&placement.confirmation, &format!("{path}/confirmation"));
            self.check_optional_condition(
                placement.enabled_when.as_ref(),
                &format!("{path}/enabledWhen"),
            );
            if let (Some(binding), Some(action)) = (
                action_binding.as_ref(),
                self.actions.get(placement.binding_id.as_str()).cloned(),
            ) {
                let mut inputs = binding.inputs.clone();
                if placement.selection_binding_id.is_some() {
                    inputs
                        .selection_binding_id
                        .clone_from(&placement.selection_binding_id);
                }
                let input_env = if placement.selection_input_mapping.is_empty() {
                    None
                } else {
                    self.expression_env(&inputs, &path)
                };
                let mut placement_binding = binding.clone();
                placement_binding.inputs.clone_from(&inputs);
                for (name, expression) in &placement.selection_input_mapping {
                    if binding.input_mapping.contains_key(name) {
                        self.error(
                            &format!("{path}/selectionInputMapping/{name}"),
                            "selection and action binding mappings must use disjoint input names",
                        );
                        continue;
                    }
                    if let Some((schema, required)) = action.fields.get(name) {
                        self.check_mapped_expression(
                            expression,
                            schema,
                            *required,
                            &placement_binding,
                            name,
                            &action,
                            input_env.as_ref(),
                            &format!("{path}/selectionInputMapping/{name}"),
                        );
                    } else {
                        self.error(
                            &format!("{path}/selectionInputMapping/{name}"),
                            "selection mapping names an input not declared by ActionIR",
                        );
                    }
                }
                if placement.form_id.is_none() {
                    for (name, (_, required)) in &action.fields {
                        if *required
                            && !binding.input_mapping.contains_key(name)
                            && !placement.selection_input_mapping.contains_key(name)
                        {
                            self.error(&format!("{path}/selectionInputMapping"), format!("required ActionIR input {name:?} has no mapping for this placement"));
                        }
                    }
                }
            }
        }
        let placement_ids: Vec<String> =
            self.placements.keys().map(|id| (*id).to_owned()).collect();
        for placement_id in placement_ids {
            if !self.referenced_placements.contains(&placement_id) {
                self.error(
                    "/payload/ux/actionPlacements",
                    format!("placement {placement_id:?} is not present in a page component tree"),
                );
            }
        }
    }

    pub(super) fn check_confirmation(&self, confirmation: &WidgetActionConfirmation, path: &str) {
        if confirmation.message.as_ref().is_some_and(|text| {
            text.0.is_empty() || text.0.values().any(|value| value.trim().is_empty())
        }) {
            self.error(
                &format!("{path}/message"),
                "confirmation message values must not be blank",
            );
        }
        let min_strength = match confirmation.severity {
            WidgetActionSeverity::Safe => 0,
            WidgetActionSeverity::Confirm | WidgetActionSeverity::Destructive => 1,
            WidgetActionSeverity::Irreversible => 2,
        };
        let strength = match confirmation.strength {
            norn_ir::WidgetConfirmationStrength::None => 0,
            norn_ir::WidgetConfirmationStrength::Click => 1,
            norn_ir::WidgetConfirmationStrength::Typed => 2,
        };
        if strength < min_strength {
            self.error(&format!("{path}/strength"), "confirm and destructive actions need click confirmation; irreversible actions need typed confirmation");
        }
        match (&confirmation.strength, &confirmation.phrase) {
            (norn_ir::WidgetConfirmationStrength::Typed, Some(phrase))
                if !phrase.0.is_empty()
                    && phrase.0.values().all(|value| !value.trim().is_empty()) => {}
            (norn_ir::WidgetConfirmationStrength::Typed, _) => self.error(
                &format!("{path}/phrase"),
                "typed confirmation requires nonblank localized confirmation text",
            ),
            (_, Some(_)) => self.error(
                &format!("{path}/phrase"),
                "confirmation phrase is only valid for typed confirmation",
            ),
            _ => {}
        }
    }
}

pub(super) fn direct_draft_mapping(binding: &WidgetUxActionBinding, input_name: &str) -> bool {
    let expected = format!("input.draft.{input_name}");
    binding
        .input_mapping
        .get(input_name)
        .is_some_and(|expression| expression.as_str().trim() == expected)
}
