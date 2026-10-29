//! Mapping coverage (T8-LINT-1) over the [`MappingCoverage`] of every
//! mapping in the set. Findings belong to the mapping artifact
//! ([`MappingCoverage::mapping_artifact_id`]) and carry no pointer, because
//! coverage is computed from the mapping rather than read from its JSON.
//!
//! - [`codes::UNKNOWN_MAPPING_TARGET`] (error): the target object type is
//!   neither a candidate nor a base object type, or a covered member is not
//!   a property (or link) of the target or its supertypes.
//! - [`codes::UNMAPPED_REQUIRED_MEMBER`] (error): a required member of the
//!   target (own or inherited) is neither mapped nor an explicit gap. A
//!   property is required when it is not `nullable`, has no `default`, and is
//!   not `computed`; a link when its cardinality is `required`.
//! - [`codes::UNMAPPED_SOURCE_FIELD`] (warning): source fields the mapping
//!   neither reads nor explicitly ignores
//!   ([`MappingCoverage::unaccounted_source_fields`]); one finding per
//!   mapping lists them in source order.

use std::collections::BTreeMap;

use norn_ir::{LinkId, ObjectTypeDef, ObjectTypeName, Property, PropertyId};

use super::coverage::{MappingCoverage, MemberRef};
use super::finding;
use super::model::{LintSet, Owned};
use crate::Diagnostic;

/// Diagnostic codes of the mapping-coverage rules.
pub mod codes {
    /// A mapping targets an unknown object type or member.
    pub const UNKNOWN_MAPPING_TARGET: &str = "ontology.lint.unknown_mapping_target";
    /// A required member of the target is neither mapped nor a declared gap.
    pub const UNMAPPED_REQUIRED_MEMBER: &str = "ontology.lint.unmapped_required_member";
    /// Source fields are neither read nor explicitly ignored.
    pub const UNMAPPED_SOURCE_FIELD: &str = "ontology.lint.unmapped_source_field";
}

/// The mapping-coverage rules over every coverage of the set.
#[must_use]
pub(crate) fn lint_mapping_coverage(set: &LintSet<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for coverage in set.coverage {
        lint_one(set, coverage, &mut out);
    }
    out
}

fn lint_one(set: &LintSet<'_>, coverage: &MappingCoverage, out: &mut Vec<Diagnostic>) {
    let mapping = coverage.mapping_artifact_id.as_str();
    let target = coverage.target.as_str();
    let ancestry = set.ancestry(&coverage.target);
    if ancestry.is_empty() {
        out.push(finding(
            codes::UNKNOWN_MAPPING_TARGET,
            format!(
                "the mapping targets object type {target}, which is not an object type of the artifact set or the installed base"
            ),
            mapping,
            None,
        ));
        return;
    }

    let (properties, links) = target_members(set, &coverage.target, &ancestry);
    for entry in &coverage.members {
        let (declared, what) = match &entry.member {
            MemberRef::Property(id) => (properties.contains_key(id.as_str()), "property"),
            MemberRef::Link(id) => (links.contains_key(id.as_str()), "link"),
        };
        if !declared {
            out.push(finding(
                codes::UNKNOWN_MAPPING_TARGET,
                format!(
                    "the mapping covers {what} {} of {target}, which neither {target} nor its supertypes declare",
                    entry.member.as_str()
                ),
                mapping,
                None,
            ));
        }
    }

    let required = properties
        .iter()
        .filter(|(_, required)| **required)
        .map(|(id, _)| MemberRef::Property(PropertyId((*id).to_owned())))
        .chain(
            links
                .iter()
                .filter(|(_, required)| **required)
                .map(|(id, _)| MemberRef::Link(LinkId((*id).to_owned()))),
        );
    for member in required {
        if !coverage.accounts_for(&member) {
            let what = match member {
                MemberRef::Property(_) => "property",
                MemberRef::Link(_) => "link",
            };
            out.push(finding(
                codes::UNMAPPED_REQUIRED_MEMBER,
                format!(
                    "required {what} {} of {target} is neither mapped nor declared as a gap",
                    member.as_str()
                ),
                mapping,
                None,
            ));
        }
    }

    let unaccounted = coverage.unaccounted_source_fields();
    if !unaccounted.is_empty() {
        out.push(finding(
            codes::UNMAPPED_SOURCE_FIELD,
            format!(
                "source fields of {} in {} are neither mapped nor explicitly ignored: {}",
                coverage.source.entity,
                coverage.source.schema_id,
                unaccounted.join(", ")
            ),
            mapping,
            None,
        ));
    }
}

/// Every property and link of `target` with whether it is required: the
/// effective properties when the entity model derives them (inherited,
/// overridden, `ref`-resolved nullability and defaults, extension members),
/// else the declared ones along `ancestry`; links (own, inherited, and added
/// by extensions) from the declarations.
fn target_members<'s>(
    set: &'s LintSet<'_>,
    target: &ObjectTypeName,
    ancestry: &[&'s Owned<'_, ObjectTypeDef>],
) -> (BTreeMap<&'s str, bool>, BTreeMap<&'s str, bool>) {
    let mut properties: BTreeMap<&str, bool> = BTreeMap::new();
    if let Some(effective) = set.effective(target) {
        for property in &effective.properties {
            properties.insert(
                property.id.as_str(),
                !property.nullable && property.default.is_none() && !property.computed,
            );
        }
    } else {
        // Own members shadow inherited ones (an override only narrows).
        let declared = ancestry
            .iter()
            .flat_map(|owned| owned.def.properties.iter())
            .chain(
                ancestry
                    .iter()
                    .flat_map(|owned| set.extensions_of(&owned.def.name))
                    .flat_map(|extension| extension.def.properties.iter()),
            );
        for property in declared {
            properties
                .entry(property.id.as_str())
                .or_insert_with(|| declared_required(property));
        }
    }

    let mut links: BTreeMap<&str, bool> = BTreeMap::new();
    let declared = ancestry
        .iter()
        .flat_map(|owned| owned.def.links.iter())
        .chain(
            ancestry
                .iter()
                .flat_map(|owned| set.extensions_of(&owned.def.name))
                .flat_map(|extension| extension.def.links.iter()),
        );
    for link in declared {
        links
            .entry(link.id.as_str())
            .or_insert(link.cardinality.required);
    }
    (properties, links)
}

fn declared_required(property: &Property) -> bool {
    !property.constraints.nullable && property.default.is_none() && property.computed.is_none()
}
