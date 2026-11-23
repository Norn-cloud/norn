//! ZEN expression environments, property/value type conversion, and `ActionIR` schema matching.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::schema::Schema;
use norn_expr::{
    ConstructionSource, EnvOptions, Expression, InferredType, Type, ZenTypeEnv, check_expression,
};
use norn_ir::{
    ArtifactKind, JsonValue, PropertyType, WidgetComponent, WidgetExpressionInputs,
    WidgetFieldTarget, WidgetFilterOperator, WidgetFilterValue, WidgetSelectionMode, ZenCondition,
};

use crate::entity_instance::{EffectiveKind, EffectiveProperty};

use super::child_grid::widget_node_by_id;
use super::{UxChecker, path_index};

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_optional_condition(
        &mut self,
        condition: Option<&ZenCondition>,
        path: &str,
    ) {
        if let Some(condition) = condition {
            self.check_condition(condition, path);
        }
    }

    pub(super) fn check_condition(&mut self, condition: &ZenCondition, path: &str) {
        let Some(env) = self.expression_env(&condition.inputs, path) else {
            return;
        };
        let expression = match Expression::compile(condition.expression.as_str()) {
            Ok(expression) => expression,
            Err(error) => {
                self.error(path, format!("ZEN condition does not compile: {error}"));
                return;
            }
        };
        if let Err(errors) = check_expression(&expression, &env, &Type::Bool) {
            for error in errors {
                self.error(
                    path,
                    format!(
                        "ZEN condition must type-check to Boolean: {}",
                        error.message
                    ),
                );
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This method assembles the closed, explicitly source-scoped ZEN input environment."
    )]
    pub(super) fn expression_env(
        &mut self,
        inputs: &WidgetExpressionInputs,
        path: &str,
    ) -> Option<ZenTypeEnv> {
        let action_binding = path_index(path, "/payload/ux/actionBindings/").is_some();
        let mut valid = true;
        let mut records = BTreeMap::new();
        let mut input = BTreeMap::new();
        if let Some(grid_id) = &inputs.row_grid_id {
            let row_object = path_index(path, "/payload/ux/fields/")
                .and_then(|index| self.ux.fields.get(index))
                .and_then(|field| {
                    let WidgetFieldTarget::ChildRowProperty {
                        form_id,
                        collection_id,
                        ..
                    } = &field.target
                    else {
                        return None;
                    };
                    let grid = widget_node_by_id(self.ux, grid_id)?;
                    let matches_grid = matches!(
                        &grid.component,
                        WidgetComponent::ChildGrid {
                            form_id: grid_form,
                            collection_id: grid_collection,
                            field_ids,
                            ..
                        } if grid_form == form_id
                            && grid_collection == collection_id
                            && field_ids.contains(&field.field_id)
                    );
                    if !matches_grid {
                        return None;
                    }
                    let form = self.forms.get(form_id.as_str()).copied()?;
                    let action = self.action_for_form(form)?;
                    self.child_object(action, collection_id)
                });
            if let Some(row_object) = row_object {
                let name = self.unique_record_name();
                let row_fields = row_object
                    .properties
                    .iter()
                    .filter(|property| !property.computed)
                    .filter_map(|property| {
                        property_type(property)
                            .map(|type_| (property.id.as_str().to_owned(), type_))
                    })
                    .collect();
                records.insert(name.clone(), row_fields);
                input.insert("row".to_owned(), Type::Record(name));
            } else {
                self.error(
                    &format!("{path}/inputs/rowGridId"),
                    "input.row is available only to a child-row field condition naming its exact enclosing ChildGrid",
                );
                valid = false;
            }
        }
        if let Some(binding_id) = &inputs.record_binding_id {
            if !self.record_source_available_at(binding_id, path) {
                self.error(
                    &format!("{path}/inputs/recordBindingId"),
                    "record input requires a detail record, list row, or single-selection context",
                );
                valid = false;
            } else if let Some(record) =
                self.binding_record(binding_id, &format!("{path}/inputs/recordBindingId"))
            {
                let name = self.unique_record_name();
                records.insert(name.clone(), record);
                input.insert("record".to_owned(), Type::Record(name));
                input.insert("recordKey".to_owned(), Type::text());
                if action_binding {
                    input.insert("recordVersion".to_owned(), Type::text());
                }
            } else {
                valid = false;
            }
        }
        if let Some(form_id) = &inputs.draft_form_id {
            if !self.draft_source_available_at(form_id, path) {
                self.error(
                    &format!("{path}/inputs/draftFormId"),
                    if action_binding {
                        "input.draft requires a matching form opener, submit, or form footer at every invocation"
                    } else {
                        "input.draft must be available from the matching form at every use"
                    },
                );
                valid = false;
            } else if let Some(form) = self.forms.get(form_id.as_str()).copied().cloned() {
                if let Some(binding) = self
                    .action_bindings
                    .get(form.submit_binding_id.as_str())
                    .copied()
                    .cloned()
                {
                    if let Some(action) = self.actions.get(binding.binding_id.as_str()).cloned() {
                        let fields = action
                            .fields
                            .into_iter()
                            .filter(|(name, _)| form.input_names.contains(name))
                            .map(|(name, (schema, required))| {
                                let mut type_ = self.schema_type(&schema, &mut records);
                                if !required {
                                    type_ = type_.optional();
                                }
                                (name, type_)
                            })
                            .collect();
                        if form.input_names.iter().any(|name| {
                            !self
                                .actions
                                .get(binding.binding_id.as_str())
                                .is_some_and(|action| action.fields.contains_key(name))
                        }) {
                            self.error(
                                &format!("{path}/inputs/draftFormId"),
                                "draft form includes an input not declared by its ActionIR",
                            );
                            valid = false;
                        }
                        let name = self.unique_record_name();
                        records.insert(name.clone(), fields);
                        input.insert("draft".to_owned(), Type::Record(name));
                    } else {
                        self.error(
                            &format!("{path}/inputs/draftFormId"),
                            "draft form ActionIR schema is unavailable",
                        );
                        valid = false;
                    }
                } else {
                    self.error(
                        &format!("{path}/inputs/draftFormId"),
                        "draft form submit binding does not exist",
                    );
                    valid = false;
                }
            } else {
                self.error(
                    &format!("{path}/inputs/draftFormId"),
                    "draft input names an unknown form",
                );
                valid = false;
            }
        }
        if let Some(binding_id) = &inputs.selection_binding_id {
            if !self.selection_available_at(binding_id, path) {
                self.error(
                    &format!("{path}/inputs/selectionBindingId"),
                    if action_binding {
                        "input.selection must be available at every invocation; forms do not capture selection"
                    } else {
                        "input.selection must be available from the same list at every invocation; forms do not capture selection"
                    },
                );
                valid = false;
            } else if !self.is_bounded_list_binding(binding_id) {
                self.error(
                    &format!("{path}/inputs/selectionBindingId"),
                    "selection input must be a bounded list or traversal binding",
                );
                valid = false;
            } else if self
                .list_selections
                .get(binding_id)
                .is_none_or(|mode| *mode == WidgetSelectionMode::None)
            {
                self.error(
                    &format!("{path}/inputs/selectionBindingId"),
                    "selection input requires a list with single or multiple selection enabled",
                );
                valid = false;
            } else if let Some(record) =
                self.binding_record(binding_id, &format!("{path}/inputs/selectionBindingId"))
            {
                let name = self.unique_record_name();
                records.insert(name.clone(), record);
                input.insert("selection".to_owned(), Type::Record(name).list());
            } else {
                valid = false;
            }
        }
        if let Some(page_id) = &inputs.context_page_id {
            if !self.check_context_page_at(page_id, &format!("{path}/inputs/contextPageId")) {
                valid = false;
            }
            if let Some(page) = self.pages.get(page_id.as_str()).copied().cloned() {
                let name = self.unique_record_name();
                let fields = page
                    .context_params
                    .iter()
                    .map(|param| {
                        (
                            param.param_id.clone(),
                            value_type(param.value_type, param.required),
                        )
                    })
                    .collect();
                records.insert(name.clone(), fields);
                input.insert("context".to_owned(), Type::Record(name));
            } else {
                self.error(
                    &format!("{path}/inputs/contextPageId"),
                    "context input names an unknown page",
                );
                valid = false;
            }
        }
        match ZenTypeEnv::input_only_with_records(records, input, EnvOptions { max_link_depth: 0 })
        {
            Ok(env) if valid => Some(env),
            Ok(_) => None,
            Err(error) => {
                self.error(
                    path,
                    format!("could not construct explicit-input ZEN environment: {error}"),
                );
                None
            }
        }
    }

    fn binding_record(&mut self, binding_id: &str, path: &str) -> Option<BTreeMap<String, Type>> {
        let Some(binding) = self.bindings.get(binding_id).copied().cloned() else {
            self.error(path, "expression input names an unknown data binding");
            return None;
        };
        let entity = self.resolve_ref(
            &binding.object.id,
            &binding.object.version,
            &ArtifactKind::Entity,
            path,
        )?;
        let Some(Ok(object)) = self.catalog.object_type(&entity.artifact_id) else {
            self.error(path, "expression input object type is unavailable");
            return None;
        };
        Some(
            object
                .properties
                .iter()
                .map(|property| {
                    let name = property.id.as_str().to_owned();
                    let ty = property_type(property).unwrap_or(Type::Json);
                    (name, ty)
                })
                .collect(),
        )
    }

    pub(super) fn schema_type(
        &mut self,
        schema: &Schema,
        records: &mut BTreeMap<String, BTreeMap<String, Type>>,
    ) -> Type {
        match schema {
            Schema::Object {
                properties,
                required,
            } => {
                let mut fields = BTreeMap::new();
                for (name, schema) in properties {
                    let mut ty = self.schema_type(schema, records);
                    if !required.contains(name) {
                        ty = ty.optional();
                    }
                    fields.insert(name.clone(), ty);
                }
                let name = self.unique_record_name();
                records.insert(name.clone(), fields);
                Type::Record(name)
            }
            Schema::Array(items) => self.schema_type(items, records).list(),
            Schema::String(None) => Type::text(),
            Schema::String(Some(values)) => Type::enumeration(values.iter().cloned()),
            Schema::Integer => Type::Integer,
            Schema::Number => Type::Number,
            Schema::Decimal(ty) => ty.clone(),
            Schema::Boolean => Type::Bool,
            Schema::Null | Schema::Json => Type::Json,
            Schema::Nullable(inner) => self.schema_type(inner, records).optional(),
        }
    }

    fn unique_record_name(&mut self) -> String {
        self.next_record = self.next_record.saturating_add(1);
        format!("norn:WidgetUx:{}", self.next_record)
    }

    pub(super) fn filter_value_type(
        &mut self,
        value: &WidgetFilterValue,
        record_contexts: &BTreeSet<String>,
        path: &str,
    ) -> Option<Type> {
        match value {
            WidgetFilterValue::Literal { value } => Some(json_value_type(value)),
            WidgetFilterValue::ContextParameter {
                page_id,
                parameter_id,
            } => {
                self.check_context_page_at(page_id, path);
                let Some(page) = self.pages.get(page_id.as_str()) else {
                    self.error(path, "context value names an unknown page");
                    return None;
                };
                let Some(param) = page
                    .context_params
                    .iter()
                    .find(|param| param.param_id == *parameter_id)
                else {
                    self.error(path, "context value names an unknown page parameter");
                    return None;
                };
                Some(value_type(param.value_type, param.required))
            }
            WidgetFilterValue::Record { property } if property.property_id == "$key" => {
                if !record_contexts.contains(&property.binding_id) {
                    self.error(
                        path,
                        "runtime-key navigation value is not available in this list row, detail, or single-selection context",
                    );
                    return None;
                }
                if !self.bindings.contains_key(property.binding_id.as_str()) {
                    self.error(path, "runtime-key value names an unknown data binding");
                    return None;
                }
                Some(Type::text())
            }
            WidgetFilterValue::Record { property } => {
                if !record_contexts.contains(&property.binding_id) {
                    self.error(path, "record-derived navigation value is not available in this list row or detail context");
                    return None;
                }
                self.resolve_property(property, path)
                    .and_then(property_type)
            }
        }
    }
}

pub(super) fn property_type(property: &EffectiveProperty) -> Option<Type> {
    let ty = match &property.kind {
        EffectiveKind::Base(PropertyType::Text { format }) => Type::Text { format: *format },
        EffectiveKind::Base(PropertyType::Integer { .. }) => Type::Integer,
        EffectiveKind::Base(PropertyType::Decimal {
            scale,
            unit,
            rounding,
            currency_source,
            per_unit,
            ..
        }) => Type::Decimal {
            scale: scale.0,
            unit: unit.clone(),
            rounding: norn_ir::Rounding::effective(*rounding),
            currency_source: currency_source.clone(),
            per_unit: per_unit.clone(),
        },
        EffectiveKind::Base(PropertyType::Boolean {}) => Type::Bool,
        EffectiveKind::Base(PropertyType::Timestamp { timezone }) => Type::Timestamp {
            timezone: *timezone,
        },
        EffectiveKind::Base(PropertyType::Date {}) => Type::Date,
        EffectiveKind::Base(PropertyType::Duration {}) => Type::Duration,
        EffectiveKind::Base(PropertyType::Enum { values }) => {
            Type::enumeration(values.iter().map(|value| value.value.clone()))
        }
        EffectiveKind::Base(
            PropertyType::Record { .. } | PropertyType::JsonExt { .. } | PropertyType::Ref { .. },
        )
        | EffectiveKind::Registry(_) => Type::Json,
        EffectiveKind::Base(PropertyType::DecimalBinding { .. }) | EffectiveKind::Unresolved => {
            return None;
        }
    };
    Some(if property.nullable { ty.optional() } else { ty })
}

pub(crate) fn value_type(value_type: norn_ir::WidgetValueType, required: bool) -> Type {
    let ty = match value_type {
        norn_ir::WidgetValueType::Text => Type::text(),
        norn_ir::WidgetValueType::Integer => Type::Integer,
        norn_ir::WidgetValueType::Number => Type::Number,
        norn_ir::WidgetValueType::Boolean => Type::Bool,
        norn_ir::WidgetValueType::Date => Type::Date,
        norn_ir::WidgetValueType::Timestamp => Type::Timestamp {
            timezone: norn_ir::TimezonePolicy::Utc,
        },
        norn_ir::WidgetValueType::Duration => Type::Duration,
        norn_ir::WidgetValueType::Json => Type::Json,
    };
    if required { ty } else { ty.optional() }
}

pub(super) fn types_compatible(source: &Type, expected: &Type) -> bool {
    if source == expected {
        return true;
    }
    match (source, expected) {
        (Type::Optional(source), Type::Optional(expected)) => types_compatible(source, expected),
        (source, Type::Optional(expected)) => types_compatible(source, expected),
        (Type::Integer | Type::Decimal { .. }, Type::Number)
        | (Type::Enum { .. }, Type::Text { .. }) => true,
        _ => false,
    }
}

pub(super) fn is_ordered_filter_operator(operator: WidgetFilterOperator) -> bool {
    matches!(
        operator,
        WidgetFilterOperator::Lt
            | WidgetFilterOperator::Lte
            | WidgetFilterOperator::Gt
            | WidgetFilterOperator::Gte
    )
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
pub(super) fn is_orderable_type(ty: &Type) -> bool {
    match ty {
        Type::Optional(inner) => is_orderable_type(inner),
        Type::Text { .. }
        | Type::Enum { .. }
        | Type::Integer
        | Type::Number
        | Type::Decimal { .. }
        | Type::Date
        | Type::Timestamp { .. } => true,
        _ => false,
    }
}

pub(super) fn type_fits_schema(source: &Type, schema: &Schema) -> bool {
    match (source, schema) {
        (Type::Optional(source), Schema::Nullable(expected)) => type_fits_schema(source, expected),
        (Type::Optional(_), _) | (Type::Json, Schema::Null) => false,
        (_, Schema::Nullable(expected)) => type_fits_schema(source, expected),
        (Type::Integer, Schema::Integer | Schema::Number)
        | (Type::Number | Type::Decimal { .. }, Schema::Number)
        | (Type::Bool, Schema::Boolean)
        | (
            Type::Text { .. } | Type::Enum { .. } | Type::Date | Type::Timestamp { .. },
            Schema::String(_),
        )
        | (Type::Json, Schema::Json) => true,
        (Type::List(item), Schema::Array(expected)) => type_fits_schema(item, expected),
        _ => false,
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
pub(super) fn type_is_integer_source(type_: &Type) -> bool {
    match type_ {
        Type::Integer => true,
        Type::Optional(inner) => type_is_integer_source(inner),
        _ => false,
    }
}

pub(super) fn schema_accepts_empty_array(schema: &Schema) -> bool {
    match schema {
        Schema::Array(_) | Schema::Json => true,
        Schema::Nullable(inner) => schema_accepts_empty_array(inner),
        Schema::Object { .. }
        | Schema::String(_)
        | Schema::Integer
        | Schema::Number
        | Schema::Decimal(_)
        | Schema::Boolean
        | Schema::Null => false,
    }
}

fn json_value_type(value: &JsonValue) -> Type {
    match value {
        JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => Type::Json,
        JsonValue::Bool(_) => Type::Bool,
        JsonValue::Number(number) if number.fract() == 0.0 => Type::Integer,
        JsonValue::Number(_) => Type::Number,
        JsonValue::String(_) => Type::text(),
    }
}

pub(super) fn inferred_matches_schema(
    inferred: &InferredType,
    schema: &Schema,
    sources: Option<&[&ConstructionSource]>,
    integer_proof: &mut impl FnMut(&[&ConstructionSource], &str) -> bool,
    path: &str,
) -> Result<(), String> {
    match (inferred, schema) {
        (InferredType::Optional(actual), Schema::Nullable(expected)) => {
            let nonnull: Option<Vec<_>> = sources.map(|sources| {
                sources
                    .iter()
                    .copied()
                    .filter(|source| {
                        !matches!(source, ConstructionSource::Scalar(source) if source.trim() == "null")
                    })
                    .collect()
            });
            inferred_matches_schema(actual, expected, nonnull.as_deref(), integer_proof, path)
        }
        (InferredType::Optional(_), _) => Err(format!(
            "{path} may be null but the ActionIR schema is non-nullable"
        )),
        (actual, Schema::Nullable(expected)) => {
            if matches!(actual, InferredType::Null) {
                Ok(())
            } else {
                inferred_matches_schema(actual, expected, sources, integer_proof, path)
            }
        }
        (
            InferredType::Record(actual),
            Schema::Object {
                properties,
                required,
            },
        ) => inferred_record_matches_schema(
            actual,
            properties,
            required,
            sources,
            integer_proof,
            path,
        ),
        (InferredType::List(actual), Schema::Array(expected)) => {
            let items = sources.and_then(list_item_sources);
            inferred_matches_schema(
                actual,
                expected,
                items.as_deref(),
                integer_proof,
                &format!("{path}/*"),
            )
        }
        (InferredType::Const(value), Schema::String(Some(allowed))) if allowed.contains(value) => {
            Ok(())
        }
        (InferredType::Enum(values), Schema::String(Some(allowed)))
            if values.iter().all(|value| allowed.contains(value)) =>
        {
            Ok(())
        }
        (InferredType::String, Schema::String(Some(_))) => Err(format!(
            "{path} is unconstrained text but ActionIR requires one of the declared string values"
        )),
        (InferredType::Const(value), Schema::String(Some(allowed))) => Err(format!(
            "{path} produces undeclared string value {value:?}; allowed values are {allowed:?}"
        )),
        (InferredType::Enum(values), Schema::String(Some(allowed))) => Err(format!(
            "{path} produces string values {values:?} outside ActionIR values {allowed:?}"
        )),
        (InferredType::Const(text), decimal @ Schema::Decimal(_)) => decimal
            .decode_action_input(norn_ir::RuntimeValue::String(text.clone()), path)
            .map(|_value| ())
            .map_err(|error| error.reason),
        (InferredType::Enum(values), decimal @ Schema::Decimal(_)) => {
            for text in values {
                decimal
                    .decode_action_input(norn_ir::RuntimeValue::String(text.clone()), path)
                    .map_err(|error| error.reason)?;
            }
            Ok(())
        }
        (InferredType::Null, Schema::Null)
        | (
            InferredType::String
            | InferredType::Const(_)
            | InferredType::Enum(_)
            | InferredType::Date
            | InferredType::Timestamp,
            Schema::String(None),
        )
        | (InferredType::Bool, Schema::Boolean)
        | (InferredType::String, Schema::Decimal(_))
        | (_, Schema::Json) => Ok(()),
        (InferredType::Number, Schema::Integer | Schema::Number)
            if matches!(schema, Schema::Number)
                || sources.is_some_and(|sources| integer_proof(sources, path)) =>
        {
            Ok(())
        }
        (InferredType::Number, Schema::Integer) => Err(format!(
            "{path} has no proof of an integer value; use a direct Integer input or exact integer literal"
        )),
        (actual, expected) => Err(format!(
            "{path} produces {actual}, incompatible with ActionIR schema {expected:?}"
        )),
    }
}

fn inferred_record_matches_schema(
    actual: &BTreeMap<String, InferredType>,
    properties: &BTreeMap<String, Schema>,
    required: &BTreeSet<String>,
    sources: Option<&[&ConstructionSource]>,
    integer_proof: &mut impl FnMut(&[&ConstructionSource], &str) -> bool,
    path: &str,
) -> Result<(), String> {
    if let Some(extra) = actual.keys().find(|name| !properties.contains_key(*name)) {
        return Err(format!(
            "{path} constructs undeclared ActionIR object member {extra:?}"
        ));
    }
    if let Some(missing) = required.iter().find(|name| !actual.contains_key(*name)) {
        return Err(format!(
            "{path} omits required ActionIR object member {missing:?}"
        ));
    }
    for (name, value) in actual {
        if let Some(expected) = properties.get(name) {
            let members = sources.and_then(|sources| record_member_sources(sources, name));
            inferred_matches_schema(
                value,
                expected,
                members.as_deref(),
                integer_proof,
                &format!("{path}/{name}"),
            )?;
        }
    }
    Ok(())
}

fn record_member_sources<'s>(
    sources: &[&'s ConstructionSource],
    name: &str,
) -> Option<Vec<&'s ConstructionSource>> {
    sources
        .iter()
        .map(|source| match source {
            ConstructionSource::Record(fields) => fields.get(name),
            ConstructionSource::Scalar(_) | ConstructionSource::List(_) => None,
        })
        .collect()
}

fn list_item_sources<'s>(
    sources: &[&'s ConstructionSource],
) -> Option<Vec<&'s ConstructionSource>> {
    let mut items = Vec::new();
    for source in sources {
        let ConstructionSource::List(children) = source else {
            return None;
        };
        items.extend(children);
    }
    Some(items)
}
