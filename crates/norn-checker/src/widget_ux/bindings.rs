//! Data bindings, query filters/sorts, physical `SQLite` column limits, and source availability.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::Type;
use norn_ir::{
    ArtifactKind, ArtifactPayload, JsonValue, PropertyType, WidgetActionLocation,
    WidgetActionPlacement, WidgetComponent, WidgetDataBinding, WidgetDataQuery, WidgetFieldTarget,
    WidgetFilter, WidgetFilterOperator, WidgetFilterValue, WidgetForm, WidgetSelectionMode,
    WidgetSort, WidgetUxActionBinding,
};

use crate::entity_instance::{EffectiveKind, EffectiveObjectType, EffectiveProperty};
use crate::entity_lowering::entity_object_type;
use crate::graph::{cycle_groups, topological_order};

use super::child_grid::child_grid_component;
use super::types::{
    is_orderable_type, is_ordered_filter_operator, property_type, types_compatible, value_type,
};
use super::{ActionSurface, Usage, UxChecker, path_index};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingColumnError {
    Computed,
    Overflow,
    NotDeclared,
    Unavailable,
}

fn is_binding_index_sort_type(property: &EffectiveProperty) -> bool {
    matches!(
        &property.kind,
        EffectiveKind::Base(
            PropertyType::Text { .. }
                | PropertyType::Enum { .. }
                | PropertyType::Integer { .. }
                | PropertyType::Boolean {}
                | PropertyType::Date {}
        )
    )
}

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_bindings(&mut self) {
        let bindings = self.ux.data_bindings.clone();
        for (index, binding) in bindings.iter().enumerate() {
            let path = format!("/payload/ux/dataBindings/{index}");
            let Some(entity) = self.resolve_ref(
                &binding.object.id,
                &binding.object.version,
                &ArtifactKind::Entity,
                &format!("{path}/object"),
            ) else {
                continue;
            };
            let ArtifactPayload::Entity(payload) = &entity.payload else {
                continue;
            };
            let object = match entity_object_type(payload) {
                Ok(Some(object)) => object,
                Ok(None) => {
                    self.error(
                        &format!("{path}/object"),
                        "binding artifact does not define an object type",
                    );
                    continue;
                }
                Err(error) => {
                    self.error(
                        &format!("{path}/object"),
                        format!("binding entity cannot be decoded: {error}"),
                    );
                    continue;
                }
            };
            let type_name = object.root.object_type.name.as_str();
            let Some(Ok(effective)) = self.catalog.object_type(&entity.artifact_id) else {
                self.error(
                    &format!("{path}/object"),
                    "binding object type is not available in the effective entity catalog",
                );
                continue;
            };
            let effective = effective.clone();
            if effective.name.as_str() != type_name {
                self.error(
                    &format!("{path}/object"),
                    "binding artifact does not own the resolved object type",
                );
                continue;
            }
            self.check_data_query(binding, &effective, index, &path);
        }
    }

    pub(super) fn check_binding_record_cycles(&self) {
        let bindings = &self.ux.data_bindings;
        let indices: BTreeMap<&str, usize> = bindings
            .iter()
            .enumerate()
            .map(|(index, binding)| (binding.binding_id.as_str(), index))
            .collect();
        let mut requires: BTreeMap<usize, BTreeSet<usize>> = (0..bindings.len())
            .map(|index| (index, BTreeSet::new()))
            .collect();
        let mut references = Vec::new();
        for (index, binding) in bindings.iter().enumerate() {
            for (source_id, pointer) in binding_record_dependencies(binding, index) {
                if let Some(&source_index) = indices.get(source_id) {
                    requires.entry(index).or_default().insert(source_index);
                    references.push((index, source_index, pointer));
                }
            }
        }
        let (_, blocked) = topological_order(&requires, |index| index);
        for group in cycle_groups(&requires, &blocked) {
            let Some((_, _, pointer)) = references
                .iter()
                .find(|(from, to, _)| group.contains(from) && group.contains(to))
            else {
                continue;
            };
            let names: Vec<&str> = group
                .iter()
                .filter_map(|&index| bindings.get(index))
                .map(|binding| binding.binding_id.as_str())
                .collect();
            self.error(
                pointer,
                format!("data bindings {names:?} have a query record dependency cycle; their current records cannot initialize"),
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This match validates each closed query variant while keeping shared object and bounds checks together."
    )]
    fn check_data_query(
        &mut self,
        binding: &WidgetDataBinding,
        object: &EffectiveObjectType,
        index: usize,
        path: &str,
    ) {
        match &binding.query {
            WidgetDataQuery::ObjectList {
                filters,
                sort,
                page,
            } => {
                self.check_page_bounds(page, &format!("{path}/query/page"));
                for (i, filter) in filters.iter().enumerate() {
                    let filter_path = format!("{path}/query/filters/{i}");
                    self.check_filter(filter, &binding.binding_id, &filter_path);
                    self.check_filter_column(filter, &binding.binding_id, object, &filter_path);
                }
                if sort.len() > 1 {
                    self.error(
                        &format!("{path}/query/sort"),
                        "binding list sorting supports at most one sort key because indexes provide one exact property order",
                    );
                }
                for (i, key) in sort.iter().enumerate() {
                    self.check_sort(
                        key,
                        &binding.binding_id,
                        object,
                        &format!("{path}/query/sort/{i}"),
                    );
                }
            }
            WidgetDataQuery::ObjectRuntimeDetail {
                page_id,
                parameter_id,
            } => {
                self.check_context_page_at(page_id, &format!("{path}/query/pageId"));
                if !self
                    .context_parameter(page_id, parameter_id)
                    .is_some_and(|param| {
                        param.required
                            && param.value_type == norn_ir::WidgetValueType::Text
                            && param.runtime_key_of.as_ref() == Some(&binding.object)
                    })
                {
                    self.error(&format!("{path}/query/parameterId"),
                        "runtime detail requires a required text parameter marked with the exact bound entity");
                }
            }
            WidgetDataQuery::ObjectDetail { key } => {
                let supplied: Vec<&str> = key
                    .iter()
                    .map(|member| member.property_id.as_str())
                    .collect();
                if supplied.iter().collect::<BTreeSet<_>>().len() != supplied.len() {
                    self.error(
                        &format!("{path}/query/key"),
                        "object detail identity key contains a duplicate property",
                    );
                    return;
                }
                let matches = object.keys.iter().any(|identity| {
                    identity
                        .components
                        .iter()
                        .map(norn_ir::PropertyId::as_str)
                        .eq(supplied.iter().copied())
                });
                if !matches {
                    self.error(&format!("{path}/query/key"), "object detail key must exactly match one declared identity key in component order");
                }
                for (i, member) in key.iter().enumerate() {
                    let Some(property) = object.property(&member.property_id) else {
                        self.error(
                            &format!("{path}/query/key/{i}/propertyId"),
                            "identity property does not exist on the bound object",
                        );
                        continue;
                    };
                    self.check_filter_value(
                        &member.value,
                        property,
                        &format!("{path}/query/key/{i}/value"),
                        None,
                    );
                }
            }
            WidgetDataQuery::LinkTraversal {
                source_binding_id,
                link_id,
                max_depth,
                max_rows,
                page,
            } => {
                self.check_page_bounds(page, &format!("{path}/query/page"));
                if max_depth.0 == 0 || max_rows.0 == 0 || page.max_size > *max_rows {
                    self.error(&format!("{path}/query"), "link traversal requires positive depth and row bounds, with page.maxSize no greater than maxRows");
                }
                let source_index = self
                    .ux
                    .data_bindings
                    .iter()
                    .position(|candidate| candidate.binding_id == *source_binding_id);
                let Some(source_index) = source_index else {
                    self.error(
                        &format!("{path}/query/sourceBindingId"),
                        "link traversal source binding does not exist",
                    );
                    return;
                };
                if source_index >= index {
                    self.error(
                        &format!("{path}/query/sourceBindingId"),
                        "link traversal source must be declared before its target binding",
                    );
                }
                let Some(source_binding) = self.ux.data_bindings.get(source_index).cloned() else {
                    self.error(
                        &format!("{path}/query/sourceBindingId"),
                        "link traversal source binding does not exist",
                    );
                    return;
                };
                if !self.record_source_available(source_binding_id, path) {
                    self.error(
                        &format!("{path}/query/sourceBindingId"),
                        "link traversal source must be a detail record or a single-selection list",
                    );
                }
                let Some(source_entity) = self.resolve_ref(
                    &source_binding.object.id,
                    &source_binding.object.version,
                    &ArtifactKind::Entity,
                    &format!("/payload/ux/dataBindings/{source_index}/object"),
                ) else {
                    return;
                };
                let Some(Ok(source_object)) = self.catalog.object_type(&source_entity.artifact_id)
                else {
                    self.error(
                        &format!("{path}/query/linkId"),
                        "source object type is unavailable",
                    );
                    return;
                };
                let source_object = source_object.clone();
                let link = self
                    .catalog
                    .links_for_object_type(source_object.name.as_str())
                    .and_then(|links| links.iter().find(|link| link.id.as_str() == link_id))
                    .cloned();
                let Some(link) = link else {
                    self.error(
                        &format!("{path}/query/linkId"),
                        "link is not declared on the source object type",
                    );
                    return;
                };
                let target_name =
                    self.entity_object_name(&binding.object, &format!("{path}/object"));
                if let Some(target_name) = target_name {
                    let valid_targets = self.catalog.target_types(link.target.as_str());
                    if target_name != link.target.as_str()
                        && !valid_targets.is_some_and(|targets| targets.contains(&target_name))
                    {
                        self.error(
                            &format!("{path}/object"),
                            "traversal result binding does not match the declared link target",
                        );
                    }
                }
                if link.cardinality.to == norn_ir::Multiplicity::One
                    && matches!(
                        &source_binding.query,
                        WidgetDataQuery::ObjectDetail { .. }
                            | WidgetDataQuery::ObjectRuntimeDetail { .. }
                    )
                    && max_rows.0 > 1
                {
                    self.error(
                        &format!("{path}/query/maxRows"),
                        "a single-valued link cannot return more than one row per source object",
                    );
                }
                if max_depth.0 > 1
                    && (source_object.name != object.name
                        || link.target.as_str() != source_object.name.as_str())
                {
                    self.error(
                        &format!("{path}/query/maxDepth"),
                        "traversal depth greater than one requires a recursive self-target link",
                    );
                }
            }
        }
    }

    fn check_page_bounds(&mut self, bounds: &norn_ir::WidgetPageBounds, path: &str) {
        if bounds.size.0 == 0 || bounds.max_size.0 == 0 || bounds.size > bounds.max_size {
            self.error(
                path,
                "page bounds require positive size and maxSize, with size no greater than maxSize",
            );
        }
    }

    fn check_filter(&mut self, filter: &WidgetFilter, binding_id: &str, path: &str) {
        if filter.property.binding_id != binding_id {
            self.error(
                &format!("{path}/property/bindingId"),
                "filter property must belong to the queried binding",
            );
        }
        let Some(property) = self.resolve_property(&filter.property, &format!("{path}/property"))
        else {
            return;
        };
        if is_ordered_filter_operator(filter.operator)
            && property_type(property).is_none_or(|ty| !is_orderable_type(&ty))
        {
            self.error(
                &format!("{path}/operator"),
                "ordered comparison requires a text, numeric, date, or timestamp property",
            );
        }
        if filter.operator == WidgetFilterOperator::In {
            match &filter.value {
                WidgetFilterValue::Literal {
                    value: JsonValue::Array(values),
                } => {
                    for (index, value) in values.iter().enumerate() {
                        self.check_literal_value(value, property, &format!("{path}/value/{index}"));
                    }
                }
                WidgetFilterValue::Record { property } if property.property_id == "$key" => {
                    self.error(
                        &format!("{path}/value"),
                        "runtime keys cannot supply query or filter values",
                    );
                }
                WidgetFilterValue::Literal { .. }
                | WidgetFilterValue::ContextParameter { .. }
                | WidgetFilterValue::Record { .. } => self.error(
                    &format!("{path}/value"),
                    "the in operator requires a literal array of values",
                ),
            }
        } else {
            self.check_filter_value(
                &filter.value,
                property,
                &format!("{path}/value"),
                Some(filter.operator),
            );
        }
    }

    fn check_filter_column(
        &mut self,
        filter: &WidgetFilter,
        binding_id: &str,
        object: &EffectiveObjectType,
        path: &str,
    ) {
        if filter.property.binding_id != binding_id {
            return;
        }
        let property_path = format!("{path}/property");
        if object.property(&filter.property.property_id).is_none() {
            return;
        }
        match self.binding_property_column_owner(object, &filter.property.property_id) {
            Ok(_) => {}
            Err(BindingColumnError::Computed) => self.error(
                &property_path,
                "computed filter properties have no stored column for a binding index",
            ),
            Err(BindingColumnError::Overflow) => self.error(
                &property_path,
                "filter property is stored in the overflow payload, not a physical column for a binding index",
            ),
            Err(BindingColumnError::NotDeclared) => self.error(
                &property_path,
                "filter property is not declared on the object hierarchy and has no physical column for a binding index",
            ),
            Err(BindingColumnError::Unavailable) => self.error(
                &property_path,
                "filter property storage cannot be resolved for a binding index",
            ),
        }
        // The type's existing filter semantics are checked above. Physical
        // column validation does not change how the value is compared.
    }

    fn check_sort(
        &mut self,
        sort: &WidgetSort,
        binding_id: &str,
        object: &EffectiveObjectType,
        path: &str,
    ) {
        if sort.property.binding_id != binding_id {
            self.error(
                &format!("{path}/property/bindingId"),
                "sort property must belong to the queried binding",
            );
            return;
        }
        let property_path = format!("{path}/property");
        let Some(property) = self.resolve_property(&sort.property, &property_path) else {
            return;
        };
        if property.computed {
            self.error(
                &property_path,
                "computed sort properties have no stored column and cannot provide exact indexed order",
            );
            return;
        }
        if !is_binding_index_sort_type(property) {
            self.error(
                &property_path,
                "sort property type has no exact binding-index order; supported types are text, enum, integer, boolean, and date",
            );
            return;
        }
        match self.binding_property_column_owner(object, &sort.property.property_id) {
            Ok(owner) => {
                let Some(root) = object.lineage.last() else {
                    self.error(
                        &property_path,
                        "sort property storage has no hierarchy root for an exact binding index",
                    );
                    return;
                };
                if owner != root.as_str() {
                    self.error(
                        &property_path,
                        "sort property is declared below the hierarchy root and cannot provide exact order from the root binding index",
                    );
                }
            }
            Err(BindingColumnError::Computed) => self.error(
                &property_path,
                "computed sort properties have no stored column and cannot provide exact indexed order",
            ),
            Err(BindingColumnError::Overflow) => self.error(
                &property_path,
                "sort property is stored in the overflow payload, not a physical column, and cannot provide exact indexed order",
            ),
            Err(BindingColumnError::NotDeclared) => self.error(
                &property_path,
                "sort property is not declared on the object hierarchy and has no physical column for exact indexed order",
            ),
            Err(BindingColumnError::Unavailable) => self.error(
                &property_path,
                "sort property storage cannot be resolved for exact binding-index order",
            ),
        }
    }

    /// Find the table that physically stores a property and whether it is a
    /// real `SQLite` column. Inherited properties stay on their oldest
    /// declaration's table; computed and overflow properties are not columns.
    fn binding_property_column_owner(
        &self,
        object: &EffectiveObjectType,
        property_id: &str,
    ) -> Result<String, BindingColumnError> {
        let mut declaration = None;
        for (lineage_index, name) in object.lineage.iter().enumerate().rev() {
            let Some(definition) = self.object_type_definition(name.as_str()) else {
                return Err(BindingColumnError::Unavailable);
            };
            if let Some(property) = definition
                .properties
                .iter()
                .find(|property| property.id.as_str() == property_id)
            {
                let property = property.clone();
                declaration = Some((
                    lineage_index,
                    name.as_str().to_owned(),
                    definition,
                    property,
                ));
                break;
            }
        }
        let Some((lineage_index, owner, definition, property)) = declaration else {
            return Err(BindingColumnError::NotDeclared);
        };
        if property.computed.is_some() {
            return Err(BindingColumnError::Computed);
        }

        let mut inherited = BTreeSet::new();
        for ancestor in object.lineage.iter().skip(lineage_index.saturating_add(1)) {
            let Some(ancestor) = self.object_type_definition(ancestor.as_str()) else {
                return Err(BindingColumnError::Unavailable);
            };
            inherited.extend(
                ancestor
                    .properties
                    .iter()
                    .map(|property| property.id.as_str().to_owned()),
            );
        }
        let stored: Vec<_> = definition
            .properties
            .iter()
            .filter(|candidate| {
                candidate.computed.is_none() && !inherited.contains(candidate.id.as_str())
            })
            .collect();
        let Some(column_index) = stored
            .iter()
            .position(|candidate| candidate.id.as_str() == property_id)
        else {
            return Err(BindingColumnError::NotDeclared);
        };

        // The projection reserves five system columns on a hierarchy root
        // and three on a subtype table. When properties exceed that budget,
        // one property slot becomes `_overflow`; only the earlier properties
        // remain independently indexable columns.
        let system_columns = if definition.extends.is_none() { 5 } else { 3 };
        let property_budget = norn_ir::MAX_SQLITE_COLUMNS_PER_TABLE.saturating_sub(system_columns);
        let physical_columns = if stored.len() > property_budget {
            property_budget.saturating_sub(1)
        } else {
            stored.len()
        };
        if column_index >= physical_columns {
            return Err(BindingColumnError::Overflow);
        }
        Ok(owner)
    }

    fn object_type_definition(&self, name: &str) -> Option<norn_ir::ObjectTypeDef> {
        self.all.iter().find_map(|artifact| {
            let ArtifactPayload::Entity(payload) = &artifact.payload else {
                return None;
            };
            let Ok(Some(entity)) = entity_object_type(payload) else {
                return None;
            };
            if entity.root.object_type.name.as_str() == name {
                Some(entity.root.object_type)
            } else {
                None
            }
        })
    }

    fn check_filter_value(
        &mut self,
        value: &WidgetFilterValue,
        property: &EffectiveProperty,
        path: &str,
        operator: Option<WidgetFilterOperator>,
    ) {
        match value {
            WidgetFilterValue::Literal { value } => {
                if matches!(value, JsonValue::Null)
                    && operator.is_some_and(is_ordered_filter_operator)
                {
                    self.error(path, "ordered filter values cannot be null");
                } else {
                    self.check_literal_value(value, property, path);
                }
            }
            WidgetFilterValue::ContextParameter {
                page_id,
                parameter_id,
            } => {
                self.check_context_page_at(page_id, path);
                let Some(page) = self.pages.get(page_id.as_str()) else {
                    self.error(path, "context value names an unknown page");
                    return;
                };
                let Some(param) = page
                    .context_params
                    .iter()
                    .find(|param| param.param_id == *parameter_id)
                else {
                    self.error(path, "context value names an unknown page parameter");
                    return;
                };
                let actual = value_type(param.value_type, param.required);
                let Some(expected) = property_type(property) else {
                    self.error(path, "filter property has an unresolved or opaque type");
                    return;
                };
                if !types_compatible(&actual, &expected) {
                    self.error(
                        path,
                        "context parameter type is incompatible with the filter property",
                    );
                }
                if matches!(actual, Type::Optional(_)) {
                    self.error(
                        path,
                        "optional context parameters cannot supply a filter value",
                    );
                }
                if param.runtime_key_of.is_some() {
                    self.error(
                        path,
                        "runtime-key context parameters cannot supply query or filter values",
                    );
                }
                if operator.is_some_and(is_ordered_filter_operator)
                    && matches!(actual, Type::Bool | Type::Json)
                {
                    self.error(
                        path,
                        "ordered filter value must be text, numeric, date, or timestamp",
                    );
                }
            }
            WidgetFilterValue::Record { property } if property.property_id == "$key" => {
                self.error(path, "runtime keys cannot supply query or filter values");
            }
            WidgetFilterValue::Record { property: source } => {
                if !self.bindings.contains_key(source.binding_id.as_str()) {
                    self.error(path, "record value names an unknown data binding");
                    return;
                }
                if !self.record_source_available(&source.binding_id, path) {
                    self.error(path, "record value requires a detail record, list row, or single selection context");
                }
                let Some(source_property) = self.resolve_property(source, path) else {
                    return;
                };
                let (Some(actual), Some(expected)) =
                    (property_type(source_property), property_type(property))
                else {
                    self.error(
                        path,
                        "record value has an unresolved or opaque property type",
                    );
                    return;
                };
                if !types_compatible(&actual, &expected) {
                    self.error(
                        path,
                        "record value property type is incompatible with the target property",
                    );
                }
                if operator.is_some_and(is_ordered_filter_operator) && !is_orderable_type(&actual) {
                    self.error(
                        path,
                        "ordered filter value must be text, numeric, date, or timestamp",
                    );
                }
            }
        }
    }

    pub(super) fn list_rendered_at(&self, binding_id: &str, path: &str) -> bool {
        let pages = self.pages_at(path);
        !pages.is_empty()
            && self.rendered_lists.get(binding_id).is_some_and(|rendered| {
                pages
                    .iter()
                    .all(|page| page.is_some() && rendered.contains(page))
            })
    }

    pub(super) fn selection_available_at(&self, binding_id: &str, path: &str) -> bool {
        if !self.list_rendered_at(binding_id, path) {
            return false;
        }
        let placement_matches = |placement: &WidgetActionPlacement| {
            placement.location == WidgetActionLocation::Selection
                && placement.selection_binding_id.as_deref() == Some(binding_id)
        };
        if let Some(index) = path_index(path, "/payload/ux/actionBindings/") {
            let Some(binding) = self.ux.action_bindings.get(index) else {
                return false;
            };
            if self
                .ux
                .forms
                .iter()
                .any(|form| form.submit_binding_id == binding.binding_id)
            {
                return false;
            }
            let mut placements = self
                .ux
                .action_placements
                .iter()
                .filter(|placement| placement.binding_id == binding.binding_id);
            return placements.clone().next().is_some()
                && placements
                    .all(|placement| placement.form_id.is_none() && placement_matches(placement));
        }
        if let Some(index) = path_index(path, "/payload/ux/actionPlacements/") {
            return self
                .ux
                .action_placements
                .get(index)
                .is_some_and(placement_matches);
        }
        if let Some(index) = path_index(path, "/payload/ux/fields/") {
            return self
                .ux
                .fields
                .get(index)
                .is_some_and(|field| matches!(&field.target, WidgetFieldTarget::Property { .. }));
        }
        if path_index(path, "/payload/ux/overrides/").is_some() {
            return self
                .usage_at(path)
                .filter(|usage| {
                    !matches!(usage, Usage::Semantic(id) if self.form_owned_semantics.contains(id))
                })
                .and_then(|usage| self.usage_surfaces.get(&usage))
                .is_some_and(|surfaces| {
                    !surfaces.is_empty()
                        && surfaces
                            .iter()
                            .all(|surface| !matches!(surface, ActionSurface::Form(_)))
                });
        }
        path_index(path, "/payload/ux/navigationItems/").is_some()
    }

    fn record_source_available(&self, binding_id: &str, path: &str) -> bool {
        self.bindings.get(binding_id).is_some_and(|binding| {
            matches!(
                &binding.query,
                WidgetDataQuery::ObjectDetail { .. } | WidgetDataQuery::ObjectRuntimeDetail { .. }
            )
        }) || (self.list_selections.get(binding_id) == Some(&WidgetSelectionMode::Single)
            && self.list_rendered_at(binding_id, path))
    }

    pub(super) fn value_source_available_at(
        &self,
        binding_id: &str,
        path: &str,
        form: Option<&WidgetForm>,
    ) -> bool {
        match form {
            Some(form) => {
                form.data_binding_id.as_deref() == Some(binding_id)
                    && self.record_source_available_for_form(form, binding_id)
            }
            None => self.record_source_available_at(binding_id, path),
        }
    }

    pub(super) fn record_source_available_for_form(
        &self,
        form: &WidgetForm,
        binding_id: &str,
    ) -> bool {
        let Some(index) = self
            .ux
            .forms
            .iter()
            .position(|candidate| candidate.form_id == form.form_id)
        else {
            return false;
        };
        let path = format!("/payload/ux/forms/{index}");
        self.record_source_available(binding_id, &path)
            || (self.list_row_contexts.contains(binding_id)
                && self.list_rendered_at(binding_id, &path)
                && self.ux.action_placements.iter().any(|placement| {
                    placement.form_id.as_deref() == Some(form.form_id.as_str())
                        && placement.selection_binding_id.as_deref() == Some(binding_id)
                        && match placement.location {
                            WidgetActionLocation::Row => true,
                            WidgetActionLocation::Selection => {
                                self.list_selections.get(binding_id)
                                    == Some(&WidgetSelectionMode::Single)
                            }
                            WidgetActionLocation::Toolbar
                            | WidgetActionLocation::Detail
                            | WidgetActionLocation::FormFooter => false,
                        }
                }))
    }

    fn placement_draft_source_available(
        &self,
        placement: &WidgetActionPlacement,
        form_id: &str,
    ) -> bool {
        if let Some(opened_form) = &placement.form_id {
            return opened_form == form_id
                && placement.location != WidgetActionLocation::FormFooter;
        }
        placement.location == WidgetActionLocation::FormFooter
            && self
                .usage_surfaces
                .get(&Usage::Placement(placement.placement_id.clone()))
                .is_some_and(|surfaces| {
                    !surfaces.is_empty()
                        && surfaces.iter().all(
                            |surface| matches!(surface, ActionSurface::Form(id) if id == form_id),
                        )
                })
    }

    fn action_draft_source_available(
        &self,
        binding: &WidgetUxActionBinding,
        form_id: &str,
    ) -> bool {
        let mut placements = self
            .ux
            .action_placements
            .iter()
            .filter(|placement| placement.binding_id == binding.binding_id);
        let mut forms = self
            .ux
            .forms
            .iter()
            .filter(|form| form.submit_binding_id == binding.binding_id);
        let has_uses = placements.clone().next().is_some() || forms.clone().next().is_some();
        has_uses
            && placements.all(|placement| self.placement_draft_source_available(placement, form_id))
            && forms.all(|form| form.form_id == form_id)
    }

    pub(super) fn draft_source_available_at(&self, form_id: &str, path: &str) -> bool {
        let Some(usage) = self.usage_at(path) else {
            return false;
        };
        if let Usage::Action(id) = &usage {
            return self
                .action_bindings
                .get(id.as_str())
                .is_some_and(|binding| self.action_draft_source_available(binding, form_id));
        }
        self.usage_surfaces.get(&usage).is_some_and(|surfaces| {
            !surfaces.is_empty()
                && surfaces
                    .iter()
                    .all(|surface| matches!(surface, ActionSurface::Form(id) if id == form_id))
        })
    }

    fn action_record_source_available(
        &self,
        action_binding_index: usize,
        record_binding_id: &str,
    ) -> bool {
        let Some(binding) = self.ux.action_bindings.get(action_binding_index) else {
            return false;
        };
        if binding.inputs.record_binding_id.as_deref() != Some(record_binding_id) {
            return false;
        }
        let path = format!("/payload/ux/actionBindings/{action_binding_index}");
        if self.is_bounded_list_binding(record_binding_id)
            && !self.list_rendered_at(record_binding_id, &path)
        {
            return false;
        }
        let mut placements = self
            .ux
            .action_placements
            .iter()
            .filter(|placement| placement.binding_id == binding.binding_id);
        let mut forms = self
            .ux
            .forms
            .iter()
            .filter(|form| form.submit_binding_id == binding.binding_id);
        let has_uses = self
            .ux
            .action_placements
            .iter()
            .any(|placement| placement.binding_id == binding.binding_id)
            || self
                .ux
                .forms
                .iter()
                .any(|form| form.submit_binding_id == binding.binding_id);
        let placements_are_compatible = placements.all(|placement| match placement.location {
            WidgetActionLocation::Row => {
                placement.selection_binding_id.as_deref() == Some(record_binding_id)
            }
            WidgetActionLocation::Selection => {
                placement.selection_binding_id.as_deref() == Some(record_binding_id)
                    && self.list_selections.get(record_binding_id)
                        == Some(&WidgetSelectionMode::Single)
            }
            WidgetActionLocation::Detail => self.record_source_available(record_binding_id, &path),
            WidgetActionLocation::FormFooter => binding
                .inputs
                .draft_form_id
                .as_deref()
                .and_then(|form_id| self.forms.get(form_id).copied())
                .is_some_and(|form| {
                    form.data_binding_id.as_deref() == Some(record_binding_id)
                        && self.record_source_available_for_form(form, record_binding_id)
                }),
            WidgetActionLocation::Toolbar => false,
        });
        let forms_are_compatible = forms.all(|form| {
            form.data_binding_id.as_deref() == Some(record_binding_id)
                && self.record_source_available_for_form(form, record_binding_id)
        });
        has_uses && placements_are_compatible && forms_are_compatible
    }

    pub(super) fn record_source_available_at(&self, binding_id: &str, path: &str) -> bool {
        if self.is_bounded_list_binding(binding_id) && !self.list_rendered_at(binding_id, path) {
            return false;
        }
        if let Some(index) = path_index(path, "/payload/ux/navigationItems/") {
            let Some(item) = self.ux.navigation_items.get(index) else {
                return false;
            };
            return self
                .navigation_record_contexts(&item.item_id, path)
                .contains(binding_id);
        }
        if let Some(index) = path_index(path, "/payload/ux/actionBindings/") {
            return self.action_record_source_available(index, binding_id);
        }
        if let Some(index) = path_index(path, "/payload/ux/forms/") {
            return self.ux.forms.get(index).is_some_and(|form| {
                form.data_binding_id.as_deref() == Some(binding_id)
                    && self.record_source_available_for_form(form, binding_id)
            });
        }
        if self.record_source_available(binding_id, path) {
            return true;
        }
        if !self.list_row_contexts.contains(binding_id) {
            return false;
        }
        if let Some(index) = path_index(path, "/payload/ux/fields/") {
            return self
                .ux
                .fields
                .get(index)
                .is_some_and(|field| match &field.target {
                    WidgetFieldTarget::Property { property } => {
                        property.binding_id == binding_id
                            && self.row_record_available_at(binding_id, path)
                    }
                    WidgetFieldTarget::ChildRowProperty {
                        form_id,
                        collection_id,
                        ..
                    } => child_grid_component(self.ux, form_id, collection_id).is_some_and(
                        |component| {
                            matches!(
                                component,
                                WidgetComponent::ChildGrid {
                                    data_binding_id: Some(grid_binding),
                                    ..
                                } if grid_binding == binding_id
                            ) && self.row_record_available_at(binding_id, path)
                        },
                    ),
                    WidgetFieldTarget::FormInput { form_id, .. }
                    | WidgetFieldTarget::ParentProperty { form_id, .. } => self
                        .forms
                        .get(form_id.as_str())
                        .copied()
                        .is_some_and(|form| {
                            self.record_source_available_for_form(form, binding_id)
                        }),
                });
        }
        if let Some(index) = path_index(path, "/payload/ux/actionPlacements/") {
            return self
                .ux
                .action_placements
                .get(index)
                .is_some_and(|placement| {
                    placement.location == WidgetActionLocation::Row
                        && placement.selection_binding_id.as_deref() == Some(binding_id)
                });
        }
        path_index(path, "/payload/ux/overrides/").is_some()
            && self.row_record_available_at(binding_id, path)
    }

    fn row_record_available_at(&self, binding_id: &str, path: &str) -> bool {
        self.usage_at(path)
            .and_then(|usage| self.usage_surfaces.get(&usage))
            .is_some_and(|surfaces| {
                !surfaces.is_empty()
                    && surfaces.iter().all(
                        |surface| matches!(surface, ActionSurface::List(id) if id == binding_id),
                    )
            })
    }

    fn check_literal_value(&mut self, value: &JsonValue, property: &EffectiveProperty, path: &str) {
        if matches!(value, JsonValue::Null) && property.nullable {
            return;
        }
        let Ok(value) = crate::entity_instance::instance_json(value) else {
            self.error(path, "literal cannot be represented as a runtime value");
            return;
        };
        if let Err(error) = property.check_value(&value) {
            self.error(
                path,
                format!("literal does not match its property: {error:?}"),
            );
        }
    }
}

pub(super) fn binding_record_dependencies(
    binding: &WidgetDataBinding,
    index: usize,
) -> Vec<(&str, String)> {
    let path = format!("/payload/ux/dataBindings/{index}/query");
    match &binding.query {
        WidgetDataQuery::ObjectList { filters, .. } => filters
            .iter()
            .enumerate()
            .filter_map(|(index, filter)| {
                if let WidgetFilterValue::Record { property } = &filter.value {
                    Some((
                        property.binding_id.as_str(),
                        format!("{path}/filters/{index}/value/property/bindingId"),
                    ))
                } else {
                    None
                }
            })
            .collect(),
        WidgetDataQuery::ObjectRuntimeDetail { .. } => Vec::new(),
        WidgetDataQuery::ObjectDetail { key } => key
            .iter()
            .enumerate()
            .filter_map(|(index, member)| {
                if let WidgetFilterValue::Record { property } = &member.value {
                    Some((
                        property.binding_id.as_str(),
                        format!("{path}/key/{index}/value/property/bindingId"),
                    ))
                } else {
                    None
                }
            })
            .collect(),
        WidgetDataQuery::LinkTraversal {
            source_binding_id, ..
        } => vec![(
            source_binding_id.as_str(),
            format!("{path}/sourceBindingId"),
        )],
    }
}
