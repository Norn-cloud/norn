//! Values a field renderer must draw faithfully (#1762): a marked property's value may arrive
//! masked or withheld, and a decimal property's value is exact. A page that renders such a
//! property through a catalog component (the contract's renderer for that field type) needs that
//! component to declare it handles the value (`handles`); otherwise it might show a mask as data
//! or round a decimal through a binary float. The Norn renderer's native fields handle both by
//! their own contract. The server masks and withholds; this law only keeps the page honest.

use std::collections::BTreeSet;

use norn_checker::entity_instance::{EffectiveKind, EffectiveProperty};
use norn_ir::widget_ux::WidgetValueHandling;
use norn_ir::{
    ActionUiEffect, ArtifactKind, PropertyRef, PropertyType, WidgetComponent, WidgetFieldTarget,
    WidgetNode, WidgetValueType,
};

use super::{DeclaredCodes, UxChecker};

const MARKED_UNHANDLED: &str = "widget.binding.marked_value_unhandled";
const EXACT_UNHANDLED: &str = "widget.binding.exact_value_unhandled";

/// The field type a property renders as, and whether its value is an exact decimal.
fn rendered(property: &EffectiveProperty) -> Option<(WidgetValueType, bool)> {
    Some(match &property.kind {
        EffectiveKind::Base(PropertyType::Text { .. } | PropertyType::Enum { .. }) => {
            (WidgetValueType::Text, false)
        }
        EffectiveKind::Base(PropertyType::Integer { .. }) => (WidgetValueType::Integer, false),
        EffectiveKind::Base(PropertyType::Decimal { .. } | PropertyType::DecimalBinding { .. }) => {
            (WidgetValueType::Number, true)
        }
        EffectiveKind::Base(PropertyType::Boolean {}) => (WidgetValueType::Boolean, false),
        EffectiveKind::Base(PropertyType::Timestamp { .. }) => (WidgetValueType::Timestamp, false),
        EffectiveKind::Base(PropertyType::Date {}) => (WidgetValueType::Date, false),
        EffectiveKind::Base(PropertyType::Duration {}) => (WidgetValueType::Duration, false),
        EffectiveKind::Base(
            PropertyType::Record { .. } | PropertyType::JsonExt { .. } | PropertyType::Ref { .. },
        )
        | EffectiveKind::Registry(_) => (WidgetValueType::Json, false),
        EffectiveKind::Unresolved => return None,
    })
}

fn rendered_refs<'u>(
    node: &'u WidgetNode,
    pointer: &str,
    out: &mut Vec<(&'u PropertyRef, String)>,
) {
    match &node.component {
        WidgetComponent::List { fields, .. } => {
            for (index, field) in fields.iter().enumerate() {
                out.push((field, format!("{pointer}/component/fields/{index}")));
            }
        }
        WidgetComponent::Section { section } => {
            for (index, field) in section.fields.iter().enumerate() {
                out.push((field, format!("{pointer}/component/section/fields/{index}")));
            }
        }
        WidgetComponent::Detail { .. }
        | WidgetComponent::Dialog { .. }
        | WidgetComponent::Form { .. }
        | WidgetComponent::ChildGrid { .. }
        | WidgetComponent::Field { .. }
        | WidgetComponent::Action { .. }
        | WidgetComponent::Navigation { .. }
        | WidgetComponent::Catalog { .. }
        | WidgetComponent::Control { .. }
        | WidgetComponent::Composition { .. } => {}
    }
    for (index, child) in node.children.iter().enumerate() {
        rendered_refs(child, &format!("{pointer}/children/{index}"), out);
    }
}

impl UxChecker<'_, '_, '_> {
    /// The resolved property a reference names, without reporting: other passes report a bad one.
    fn property_of(&self, property: &PropertyRef) -> Option<&EffectiveProperty> {
        let binding = self.bindings.get(property.binding_id.as_str())?;
        let entity = self.find_visible_exact(
            self.owner,
            &binding.object.id,
            &binding.object.version,
            &ArtifactKind::Entity,
        )?;
        let object = self.catalog.object_type(&entity.artifact_id)?.ok()?;
        object.property(&property.property_id)
    }

    fn check_rendered_property(&self, found: &EffectiveProperty, property_id: &str, pointer: &str) {
        let Some((value_type, exact)) = rendered(found) else {
            return;
        };
        let Some(renderer) = self
            .ux
            .catalog
            .iter()
            .find(|entry| entry.renders == Some(value_type))
        else {
            return;
        };
        let entry = &renderer.entry_id;
        if !found.markings.is_empty() && !renderer.handles.contains(&WidgetValueHandling::Masked) {
            self.coded(
                MARKED_UNHANDLED,
                pointer,
                format!(
                    "property `{property_id}` is marked, so its value may arrive masked or withheld, but its renderer `{entry}` does not declare `handles: [\"masked\"]`"
                ),
            );
        }
        if exact && !renderer.handles.contains(&WidgetValueHandling::Exact) {
            self.coded(
                EXACT_UNHANDLED,
                pointer,
                format!(
                    "property `{property_id}` is an exact decimal, but its renderer `{entry}` does not declare `handles: [\"exact\"]`"
                ),
            );
        }
    }

    /// Every rendered property reference whose value may be masked or is exact is drawn by a
    /// renderer that declares it handles that value.
    pub(super) fn check_value_handling(&self) {
        let mut refs = Vec::new();
        for (index, field) in self.ux.fields.iter().enumerate() {
            match &field.target {
                WidgetFieldTarget::Property { property } => {
                    refs.push((
                        property,
                        format!("/payload/ux/fields/{index}/target/property"),
                    ));
                }
                WidgetFieldTarget::FormInput {
                    initial: Some(property),
                    ..
                } => {
                    refs.push((
                        property,
                        format!("/payload/ux/fields/{index}/target/initial"),
                    ));
                }
                WidgetFieldTarget::FormInput { initial: None, .. } => {}
                WidgetFieldTarget::ParentProperty {
                    form_id,
                    property_id,
                    ..
                } => {
                    if let Some(form) = self.forms.get(form_id.as_str()).copied()
                        && let Some(action) = self.action_for_form(form)
                        && let Some(object) = self.parent_object(action)
                        && let Some(found) = object.property(property_id)
                    {
                        self.check_rendered_property(
                            found,
                            property_id,
                            &format!("/payload/ux/fields/{index}/target/propertyId"),
                        );
                    }
                }
                WidgetFieldTarget::ChildRowProperty {
                    form_id,
                    collection_id,
                    property_id,
                } => {
                    if let Some(form) = self.forms.get(form_id.as_str()).copied()
                        && let Some(action) = self.action_for_form(form)
                        && let Some(object) = self.child_object(action, collection_id)
                        && let Some(found) = object.property(property_id)
                    {
                        self.check_rendered_property(
                            found,
                            property_id,
                            &format!("/payload/ux/fields/{index}/target/propertyId"),
                        );
                    }
                }
            }
        }
        for (index, page) in self.ux.pages.iter().enumerate() {
            rendered_refs(
                &page.root,
                &format!("/payload/ux/pages/{index}/root"),
                &mut refs,
            );
        }
        for (property, pointer) in refs {
            let Some(found) = self.property_of(property) else {
                continue;
            };
            self.check_rendered_property(found, &property.property_id, &pointer);
        }
    }

    /// The error codes each parsed action binding's action declares.
    pub(super) fn declared_error_codes(&self) -> DeclaredCodes {
        self.actions
            .iter()
            .map(|(binding, parsed)| {
                let codes: BTreeSet<String> = parsed
                    .root
                    .effective_ui_effects()
                    .into_iter()
                    .filter_map(|effect| match effect {
                        ActionUiEffect::Error { code } => Some(code),
                        ActionUiEffect::External
                        | ActionUiEffect::Irreversible
                        | ActionUiEffect::Destructive
                        | ActionUiEffect::Money { .. }
                        | ActionUiEffect::Async
                        | ActionUiEffect::Idempotent
                        | ActionUiEffect::Result { .. } => None,
                    })
                    .collect();
                (binding.clone(), codes)
            })
            .collect()
    }
}
