//! Owned child-row grid validation and tree component lookup helpers.

use std::collections::BTreeSet;

use norn_expr::Type;
use norn_expr::schema::Schema;
use norn_ir::{
    ActionEffectProgram, PropertyType, WidgetComponent, WidgetDataQuery, WidgetFieldControl,
    WidgetFieldTarget, WidgetFormMode, WidgetNode, WidgetText, WidgetUx,
};

use norn_checker::entity_instance::EffectiveKind;

use super::types::property_type;
use super::{ActionSurface, UxChecker, has_initial_lifecycle_default};

pub(super) struct ChildGridSpec<'a> {
    pub(super) form_id: &'a str,
    pub(super) label: &'a WidgetText,
    pub(super) collection_id: &'a str,
    pub(super) ordering_key: &'a str,
    pub(super) data_binding_id: Option<&'a str>,
    pub(super) row_ops_input_name: &'a str,
    pub(super) max_rows: u64,
    pub(super) field_ids: &'a [String],
}

struct ChildGridFieldContext<'a> {
    form_id: &'a str,
    collection_id: &'a str,
    collection: &'a norn_ir::OwnedChildCollection,
    child: &'a norn_checker::entity_instance::EffectiveObjectType,
    derived_properties:
        Option<&'a std::collections::BTreeMap<norn_ir::PropertyId, norn_ir::ZenExpression>>,
}

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_child_grid(
        &mut self,
        spec: &ChildGridSpec<'_>,
        surface: &ActionSurface,
        path: &str,
    ) {
        let &ChildGridSpec {
            form_id,
            label,
            collection_id,
            field_ids,
            ..
        } = spec;
        if label.is_blank() {
            self.error(
                &format!("{path}/component/label"),
                "child grid label must contain nonblank localized text",
            );
        }
        if !self
            .child_grid_collections
            .insert((form_id.to_owned(), collection_id.to_owned()))
        {
            self.error(
                &format!("{path}/component/collectionId"),
                "one form may render an owned collection in only one child grid",
            );
        }
        if !matches!(surface, ActionSurface::Form(parent_form) if parent_form == form_id) {
            self.error(
                &format!("{path}/component/formId"),
                "child grid must be a child of its matching form component",
            );
        }
        let Some(form) = self.forms.get(form_id).copied() else {
            self.error(
                &format!("{path}/component/formId"),
                "child grid names an unknown form",
            );
            return;
        };
        let Some(action) = self.action_for_form(form).cloned() else {
            self.error(
                &format!("{path}/component/formId"),
                "child grid form action is unavailable",
            );
            return;
        };
        let Some(ActionEffectProgram::MutateParentWithRows {
            entity_artifact_id,
            entity_artifact_version,
            row_ops_field,
            collections,
            ..
        }) = action.root.effect_program.as_ref()
        else {
            self.error(
                &format!("{path}/component"),
                "child grid requires a parent-and-rows submit action",
            );
            return;
        };
        let Some(collection) = self.resolve_child_grid_collection(
            form,
            &action,
            spec,
            row_ops_field,
            collections,
            path,
        ) else {
            return;
        };
        if !self.check_child_grid_binding_mode(
            form,
            spec,
            &collection,
            (entity_artifact_id, entity_artifact_version),
            path,
        ) {
            return;
        }
        let Some(child) = self.child_object(&action, collection_id) else {
            self.error(
                &format!("{path}/component/collectionId"),
                "owned child object type is unavailable",
            );
            return;
        };
        self.check_child_grid_schema(&child, &collection, path);
        let derived_properties = collections
            .iter()
            .find(|declared| declared.id.as_str() == collection_id)
            .map(|declared| &declared.derive);
        let field_ctx = ChildGridFieldContext {
            form_id,
            collection_id,
            collection: &collection,
            child: &child,
            derived_properties,
        };
        self.check_child_grid_fields(&field_ctx, field_ids, path);
    }

    fn resolve_child_grid_collection(
        &mut self,
        form: &norn_ir::WidgetForm,
        action: &super::ParsedAction,
        spec: &ChildGridSpec<'_>,
        row_ops_field: &str,
        collections: &[norn_ir::OwnedCollectionAction],
        path: &str,
    ) -> Option<norn_ir::OwnedChildCollection> {
        let &ChildGridSpec {
            form_id,
            collection_id,
            ordering_key,
            row_ops_input_name,
            max_rows,
            ..
        } = spec;
        if row_ops_input_name != row_ops_field
            || !form
                .input_names
                .iter()
                .any(|name| name == row_ops_input_name)
            || !action
                .fields
                .get(row_ops_input_name)
                .is_some_and(|(schema, _)| matches!(schema, Schema::Json))
        {
            self.error(
                &format!("{path}/component/rowOpsInputName"),
                "child grid must populate the included bounded JSON input declared by its submit action",
            );
        }
        self.included_form_inputs
            .insert((form_id.to_owned(), row_ops_input_name.to_owned()));
        self.presented_form_inputs
            .insert((form_id.to_owned(), row_ops_input_name.to_owned()));
        let Some(parent) = self.parent_object(action) else {
            self.error(
                &format!("{path}/component/collectionId"),
                "parent effect entity is unavailable",
            );
            return None;
        };
        let Some(collection) = parent
            .owned_collections
            .iter()
            .find(|collection| collection.id.as_str() == collection_id)
            .cloned()
        else {
            self.error(
                &format!("{path}/component/collectionId"),
                "child grid names no owned collection on its action entity",
            );
            return None;
        };
        if ordering_key != collection.ordering_key.as_str() {
            self.error(
                &format!("{path}/component/orderingKey"),
                "child grid orderingKey must match the owned collection's declared ordering property",
            );
        }
        if !collections
            .iter()
            .any(|declared| declared.id.as_str() == collection_id)
        {
            self.error(
                &format!("{path}/component/collectionId"),
                "submit action has no trusted behavior declaration for this collection",
            );
        }
        if max_rows == 0 || max_rows > collection.max_rows.0 {
            self.error(
                &format!("{path}/component/maxRows"),
                "child grid maxRows must be positive and no greater than the entity collection bound",
            );
        }
        Some(collection)
    }

    fn check_child_grid_binding_mode(
        &mut self,
        form: &norn_ir::WidgetForm,
        spec: &ChildGridSpec<'_>,
        collection: &norn_ir::OwnedChildCollection,
        parent_entity: (&str, &str),
        path: &str,
    ) -> bool {
        let collection_id = spec.collection_id;
        let data_binding_id = spec.data_binding_id;
        let (entity_artifact_id, entity_artifact_version) = parent_entity;
        match (form.mode, data_binding_id) {
            (WidgetFormMode::Edit, Some(binding_id)) => {
                let parent_binding_id = form.data_binding_id.as_deref();
                let Some(binding) = self.bindings.get(binding_id).copied().cloned() else {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "child grid traversal binding does not exist",
                    );
                    return false;
                };
                let exact_traversal = matches!(
                    &binding.query,
                    WidgetDataQuery::LinkTraversal {
                        source_binding_id,
                        link_id,
                        max_depth,
                        max_rows: traversal_rows,
                        page,
                    } if Some(source_binding_id.as_str()) == parent_binding_id
                        && link_id == collection_id
                        && max_depth.0 == 1
                        && traversal_rows.0 >= collection.max_rows.0
                        && page.max_size.0 >= collection.max_rows.0
                );
                if !exact_traversal {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "edit grid requires a complete, bounded depth-one traversal of its owned link from the matching parent form binding",
                    );
                }
                if self
                    .entity_object_name(
                        &binding.object,
                        &format!("{path}/component/dataBindingId/object"),
                    )
                    .as_deref()
                    != Some(collection.child_type.as_str())
                {
                    self.error(
                        &format!("{path}/component/dataBindingId/object"),
                        "child grid traversal must read the exact owned child object type",
                    );
                }
                if let Some(parent_binding_id) = parent_binding_id
                    && self
                        .bindings
                        .get(parent_binding_id)
                        .is_none_or(|parent_binding| {
                            parent_binding.object.id != entity_artifact_id
                                || parent_binding.object.version != entity_artifact_version
                        })
                {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "child traversal source must bind the action's exact parent entity artifact",
                    );
                }
            }
            (WidgetFormMode::Edit, None) => self.error(
                &format!("{path}/component/dataBindingId"),
                "edit grid requires an authoritative owned-row traversal snapshot",
            ),
            (WidgetFormMode::Create, Some(_)) => self.error(
                &format!("{path}/component/dataBindingId"),
                "create grid starts empty and must not read rows from an uncommitted parent",
            ),
            (WidgetFormMode::Create, None) => {}
            (WidgetFormMode::ActionInput, _) => self.error(
                &format!("{path}/component/formId"),
                "child grid cannot belong to an action_input form",
            ),
        }
        true
    }

    fn check_child_grid_schema(
        &self,
        child: &norn_checker::entity_instance::EffectiveObjectType,
        collection: &norn_ir::OwnedChildCollection,
        path: &str,
    ) {
        for lifecycle in &child.lifecycles {
            let property_id = lifecycle.lifecycle.property.as_str();
            let Some(property) = child.property(property_id) else {
                continue;
            };
            if !has_initial_lifecycle_default(property, &lifecycle.lifecycle.initial) {
                self.error(
                    &format!("{path}/component/fieldIds"),
                    format!(
                        "child grid Add requires lifecycle property {property_id:?} to declare a default in one of its initial states"
                    ),
                );
            }
        }
        let row_id_property = child.property(collection.row_id_property.as_str());
        if row_id_property.is_none_or(|property| {
            !matches!(
                &property.kind,
                EffectiveKind::Base(PropertyType::Text {
                    format: None | Some(norn_ir::TextFormat::Uuid)
                })
            ) || property.constraints.iter().any(|constraint| {
                constraint.min_length.is_some_and(|min| min.0 > 36)
                    || constraint.max_length.is_some_and(|max| max.0 < 36)
                    || constraint.pattern.is_some()
            })
        }) {
            self.error(
                &format!("{path}/component/collectionId"),
                "child grid row ID must admit generated UUIDs: text or UUID format, length including 36, and no additional pattern",
            );
        }
        let ordering_property = child.property(collection.ordering_key.as_str());
        if ordering_property.is_none_or(|property| {
            property.nullable
                || property.is_computed()
                || !property_type(property).is_some_and(|type_| matches!(type_, Type::Integer))
        }) {
            self.error(
                &format!("{path}/component/orderingKey"),
                "owned collection ordering property must be a required stored integer on the child object",
            );
        }
    }

    fn check_child_grid_fields(
        &mut self,
        ctx: &ChildGridFieldContext<'_>,
        field_ids: &[String],
        path: &str,
    ) {
        let mut grid_fields = BTreeSet::new();
        let mut editable_child_properties = BTreeSet::new();
        for (index, field_id) in field_ids.iter().enumerate() {
            let field_path = format!("{path}/component/fieldIds/{index}");
            if !grid_fields.insert(field_id.as_str()) {
                self.error(&field_path, "child grid field ids must be unique");
                continue;
            }
            self.check_child_grid_field(ctx, field_id, &field_path, &mut editable_child_properties);
        }
        for property in ctx.child.properties.iter().filter(|property| {
            !property.nullable
                && !property.is_computed()
                && property.default().is_none()
                && ctx.child.lifecycle(property.id.as_str()).is_none()
                && property.id != ctx.collection.row_id_property
                && property.id != ctx.collection.ordering_key
                && !ctx
                    .derived_properties
                    .is_some_and(|derived| derived.contains_key(&property.id))
        }) {
            if !editable_child_properties.contains(property.id.as_str()) {
                self.error(
                    &format!("{path}/component/fieldIds"),
                    format!(
                        "child grid must expose required child property {:?} as an unconditional editable field, or provide a declared default or derivation, so Add can create a complete row",
                        property.id.as_str()
                    ),
                );
            }
        }
    }

    fn check_child_grid_field(
        &mut self,
        ctx: &ChildGridFieldContext<'_>,
        field_id: &str,
        field_path: &str,
        editable_child_properties: &mut BTreeSet<String>,
    ) {
        let &ChildGridFieldContext {
            form_id,
            collection_id,
            collection,
            child,
            derived_properties,
        } = ctx;
        let Some(field) = self
            .ux
            .fields
            .iter()
            .find(|field| field.field_id == field_id)
            .cloned()
        else {
            self.error(
                field_path,
                "child grid field id does not name a field presentation",
            );
            return;
        };
        if !self.used_field_ids.insert(field_id.to_owned()) {
            self.error(
                field_path,
                "field presentation may appear only once across the widget tree and grids",
            );
        }
        let WidgetFieldTarget::ChildRowProperty {
            form_id: target_form,
            collection_id: target_collection,
            property_id,
        } = &field.target
        else {
            self.error(
                field_path,
                "child grid field must target a child-row property",
            );
            return;
        };
        if target_form != form_id || target_collection != collection_id {
            self.error(
                field_path,
                "child grid field target must name the same form and owned collection",
            );
            return;
        }
        let Some(property) = child.property(property_id) else {
            self.error(
                field_path,
                "child grid field property does not exist on the child object type",
            );
            return;
        };
        let Some(value_type) = property_type(property) else {
            self.error(
                field_path,
                "child grid field property has an unresolved type",
            );
            return;
        };
        let reserved = property_id == collection.row_id_property.as_str()
            || property_id == collection.ordering_key.as_str();
        let lifecycle_managed = child.lifecycle(property_id.as_str()).is_some();
        let action_derived =
            derived_properties.is_some_and(|derived| derived.contains_key(&property.id));
        if (reserved || property.is_computed() || lifecycle_managed || action_derived)
            && field.control != WidgetFieldControl::Display
        {
            self.error(field_path, "row id, ordering key, computed, lifecycle-managed child properties may only be displayed read-only; action-derived properties must also be display-only");
        }
        if !reserved
            && !property.is_computed()
            && !lifecycle_managed
            && !action_derived
            && field.control != WidgetFieldControl::Display
            && field.visible_when.is_none()
            && field.read_only_when.is_none()
        {
            editable_child_properties.insert(property_id.as_str().to_owned());
        }
        self.check_control_type(
            field.control,
            field.format,
            field.option_query_binding_id.as_deref(),
            &value_type,
            field_path,
        );
        let option_type = field
            .option_query_binding_id
            .as_deref()
            .and_then(|id| self.option_binding_key_type(id, field_path));
        self.register_semantic_node(
            field_id,
            Some(value_type),
            true,
            false,
            option_type,
            field_path,
        );
    }
}

pub(super) fn child_grid_component<'a>(
    ux: &'a WidgetUx,
    form_id: &str,
    collection_id: &str,
) -> Option<&'a WidgetComponent> {
    fn find<'a>(
        node: &'a WidgetNode,
        form_id: &str,
        collection_id: &str,
    ) -> Option<&'a WidgetComponent> {
        if matches!(
            &node.component,
            WidgetComponent::ChildGrid {
                form_id: owner,
                collection_id: collection,
                ..
            } if owner == form_id && collection == collection_id
        ) {
            return Some(&node.component);
        }
        node.children
            .iter()
            .find_map(|child| find(child, form_id, collection_id))
    }

    ux.pages
        .iter()
        .find_map(|page| find(&page.root, form_id, collection_id))
}

pub(super) fn widget_node_by_id<'a>(ux: &'a WidgetUx, node_id: &str) -> Option<&'a WidgetNode> {
    fn find<'a>(node: &'a WidgetNode, node_id: &str) -> Option<&'a WidgetNode> {
        if node.node_id == node_id {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, node_id))
    }

    ux.pages.iter().find_map(|page| find(&page.root, node_id))
}
