//! Subtype-inherited `protected`, `origin`, and `immutableAfterCreate` resolution
//! (`ORIGIN-DATAFLOW-1`).

use norn_ir::{ObjectTypeName, Property, PropertyOrigin};

use crate::entity_instance::EffectiveProperty;

/// Applies inherited `protected`, `immutableAfterCreate`, and `origin` metadata
/// across the root-to-subtype lineage so a subtype can tighten `Caller` to a
/// non-`Caller` origin while a non-`Caller` ancestor origin is never weakened.
pub(crate) fn apply_property_origin_flags<'a, F, I>(
    mut ancestors_nearest_first: Vec<&'a ObjectTypeName>,
    self_name: &'a ObjectTypeName,
    property: &mut EffectiveProperty,
    mut props_for: F,
) where
    F: FnMut(&'a ObjectTypeName) -> I,
    I: IntoIterator<Item = &'a Property>,
{
    ancestors_nearest_first.reverse();
    ancestors_nearest_first.push(self_name);
    let mut protected = false;
    let mut immutable_after_create = false;
    let mut effective_origin = None;
    for owner in ancestors_nearest_first {
        for candidate in props_for(owner) {
            if candidate.id != property.id {
                continue;
            }
            protected |= candidate.protected;
            immutable_after_create |= candidate.immutable_after_create;
            if let Some(origin) = candidate.origin
                && matches!(effective_origin, None | Some(PropertyOrigin::Caller))
            {
                effective_origin = Some(origin);
            }
        }
    }
    if protected {
        property.protect();
    }
    if immutable_after_create {
        property.set_immutable_after_create();
    }
    if let Some(origin) = effective_origin {
        property.set_origin(origin);
    }
}
