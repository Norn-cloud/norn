//! Structured [`crate::CounterexampleWitness::CoreLink`] diagnostic builders
//! and override/parent-interface link checks for `EntityIR` v2 models.

use std::collections::BTreeMap;

use norn_ir::{InterfaceDef, Link, LinkCardinality, Multiplicity, ObjectTypeName};

use super::Checker;
use super::links::{LinkDecl, LinkRequirement, cardinality_narrows};
use crate::object_types::{Origin, codes};
use crate::property_types::{error, span_at};
use crate::{Diagnostic, DiagnosticWitness, Span};

impl<'a> Checker<'a, '_> {
    /// A link override only narrows the link it overrides, and inherits its
    /// inverse role and link properties.
    pub(super) fn check_link_override(
        &mut self,
        at: &[&str],
        child_name: &str,
        link: &'a Link,
        overridden: &LinkDecl<'a>,
    ) {
        let id = link.id.as_str();
        let (owner, parent_name) = match overridden.origin {
            Origin::Type(owner) => (format!("supertype {:?}", owner.as_str()), owner.as_str()),
            Origin::Extension { .. } => (
                "an extension of a supertype".to_owned(),
                "supertype_extension",
            ),
        };
        for (member, present) in [
            ("inverse", link.inverse.is_some()),
            ("properties", !link.properties.is_empty()),
        ] {
            if present {
                self.out.push(error(
                    codes::LINK_OVERRIDE_REDECLARES,
                    format!(
                        "link {id:?} overrides the one {owner} declares and redeclares its {member}, which the override inherits; omit it"
                    ),
                    span_at(at, &[member]),
                ));
            }
        }
        let inherited = overridden.link;
        let expected = format_link_contract(&inherited.target, inherited.cardinality);
        let actual = format_link_contract(&link.target, link.cardinality);
        if self.target_narrows(&link.target, &inherited.target) == Some(false) {
            self.out.push(link_override_target_mismatch(
                child_name,
                parent_name,
                &owner,
                id,
                &inherited.target,
                expected.clone(),
                actual.clone(),
                span_at(at, &["target"]),
            ));
        }
        if inherited.acyclic != link.acyclic {
            self.out.push(error(
                codes::LINK_OVERRIDE_CHANGES_ACYCLIC,
                format!(
                    "link {id:?} overrides the one {owner} declares and changes its acyclic constraint; an override must preserve the inherited value (subtype-only acyclicity is unsound)"
                ),
                span_at(at, &["acyclic"]),
            ));
        }
        if !cardinality_narrows(link.cardinality, inherited.cardinality) {
            self.out.push(link_override_widens_cardinality(
                child_name,
                parent_name,
                &owner,
                id,
                expected,
                actual,
                span_at(at, &["cardinality"]),
            ));
        }
    }

    pub(super) fn check_interface_parent_links(&mut self, base: &[&str], def: &'a InterfaceDef) {
        let lineage = self.interface_lineage(def);
        let mut groups: BTreeMap<&str, Vec<LinkRequirement<'a>>> = BTreeMap::new();
        for owner in &lineage {
            for link in &owner.links {
                groups
                    .entry(link.id.as_str())
                    .or_default()
                    .push((Some(0), owner, link));
            }
        }
        for (id, group) in groups {
            let properties: Vec<_> = lineage
                .iter()
                .filter(|owner| {
                    owner
                        .properties
                        .iter()
                        .any(|property| property.id.as_str() == id)
                })
                .map(|owner| (Some(0), *owner))
                .collect();
            let conflict = self.link_requirement_conflict(&group, &properties);
            let widened = group.iter().find_map(|(_, child, link)| {
                if child.name != def.name {
                    return None;
                }
                group.iter().find_map(|(_, parent, requirement)| {
                    (parent.name != def.name
                        && (self.target_narrows(&link.target, &requirement.target) == Some(false)
                            || !cardinality_narrows(link.cardinality, requirement.cardinality)))
                    .then(|| {
                        (
                            parent.name.as_str(),
                            format_link_contract(&requirement.target, requirement.cardinality),
                            format_link_contract(&link.target, link.cardinality),
                        )
                    })
                })
            });
            if let Some(reason) = conflict {
                let parent_name = group
                    .iter()
                    .find(|(_, owner, _)| owner.name != def.name)
                    .map_or(def.name.as_str(), |(_, owner, _)| owner.name.as_str());
                self.out.push(interface_parent_link_conflict(
                    def.name.as_str(),
                    parent_name,
                    id,
                    reason,
                    span_at(base, &["extends"]),
                ));
            } else if let Some((parent_name, expected, actual)) = widened {
                self.out.push(interface_parent_link_widens(
                    def.name.as_str(),
                    parent_name,
                    id,
                    expected,
                    actual,
                    span_at(base, &["extends"]),
                ));
            }
        }
    }
}

pub(super) fn format_link_contract(
    target: &ObjectTypeName,
    cardinality: LinkCardinality,
) -> String {
    let to = match cardinality.to {
        Multiplicity::One => "one",
        Multiplicity::Many => "many",
    };
    let from = match cardinality.from {
        Multiplicity::One => "one",
        Multiplicity::Many => "many",
    };
    format!(
        "target={}, to={to}, from={from}, required={}",
        target.as_str(),
        cardinality.required
    )
}

fn with_witness(mut diagnostic: Diagnostic, witness: DiagnosticWitness) -> Diagnostic {
    diagnostic.witness = Some(witness);
    diagnostic
}

pub(super) fn unknown_link_target(
    owner_name: &str,
    id: &str,
    target: &ObjectTypeName,
    span: Span,
) -> Diagnostic {
    let target_name = target.as_str();
    with_witness(
        error(
            codes::UNKNOWN_LINK_TARGET,
            format!("link target {target_name:?} names no object type or interface"),
            span,
        ),
        DiagnosticWitness::core_link(
            owner_name,
            target_name,
            id,
            "admitted object type or interface in effective catalog",
            format!("unresolved target {target_name}"),
        ),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "link override target mismatch witness carries both endpoints and formatted contracts"
)]
pub(super) fn link_override_target_mismatch(
    child_name: &str,
    parent_name: &str,
    owner: &str,
    id: &str,
    inherited_target: &ObjectTypeName,
    expected: String,
    actual: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            codes::LINK_OVERRIDE_TARGET_MISMATCH,
            format!(
                "link {id:?} overrides the one {owner} declares with a target that is not {:?} or a narrowing of it; an override may only narrow",
                inherited_target.as_str()
            ),
            span,
        ),
        DiagnosticWitness::core_link(child_name, parent_name, id, expected, actual),
    )
}

pub(super) fn link_override_widens_cardinality(
    child_name: &str,
    parent_name: &str,
    owner: &str,
    id: &str,
    expected: String,
    actual: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            codes::LINK_OVERRIDE_WIDENS_CARDINALITY,
            format!(
                "link {id:?} overrides the one {owner} declares with a wider cardinality; an override may only narrow (many to one, optional to required)"
            ),
            span,
        ),
        DiagnosticWitness::core_link(child_name, parent_name, id, expected, actual),
    )
}

pub(super) fn inverse_requires_forward_link(
    child_name: &str,
    interface_name: &str,
    id: &str,
    expected: String,
    actual: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            codes::MISSING_INTERFACE_LINK,
            format!(
                "interface {interface_name:?} requires link {id:?} to one target, but the type's member of that id is an inverse role; declare a forward link"
            ),
            span,
        ),
        DiagnosticWitness::core_link(child_name, interface_name, id, expected, actual),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "inverse target mismatch witness carries both endpoints and formatted contracts"
)]
pub(super) fn inverse_target_mismatch(
    child_name: &str,
    interface_name: &str,
    id: &str,
    source: &ObjectTypeName,
    required_target: &ObjectTypeName,
    expected: String,
    actual: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            codes::INTERFACE_LINK_TARGET_MISMATCH,
            format!(
                "inverse role {id:?} reads {:?}, which does not narrow {:?}, the target interface {interface_name:?} requires",
                source.as_str(),
                required_target.as_str()
            ),
            span,
        ),
        DiagnosticWitness::core_link(child_name, interface_name, id, expected, actual),
    )
}

pub(super) fn inverse_widens_cardinality(
    child_name: &str,
    interface_name: &str,
    id: &str,
    expected: String,
    actual: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            codes::INTERFACE_LINK_WIDENS_CARDINALITY,
            format!(
                "inverse role {id:?} has a wider cardinality than interface {interface_name:?} requires"
            ),
            span,
        ),
        DiagnosticWitness::core_link(child_name, interface_name, id, expected, actual),
    )
}

pub(super) fn missing_interface_link(
    child_name: &str,
    interface_name: &str,
    id: &str,
    found: &str,
    expected: String,
    actual: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            codes::MISSING_INTERFACE_LINK,
            format!("interface {interface_name:?} requires link {id:?}, but {found}"),
            span,
        ),
        DiagnosticWitness::core_link(child_name, interface_name, id, expected, actual),
    )
}

pub(super) fn interface_link_target_mismatch(
    child_name: &str,
    interface_name: &str,
    id: &str,
    required_target: &ObjectTypeName,
    expected: String,
    actual: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            codes::INTERFACE_LINK_TARGET_MISMATCH,
            format!(
                "link {id:?} does not target {:?}, which interface {interface_name:?} requires, or a narrowing of it",
                required_target.as_str()
            ),
            span,
        ),
        DiagnosticWitness::core_link(child_name, interface_name, id, expected, actual),
    )
}

pub(super) fn interface_link_widens_cardinality(
    child_name: &str,
    interface_name: &str,
    id: &str,
    expected: String,
    actual: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            codes::INTERFACE_LINK_WIDENS_CARDINALITY,
            format!(
                "link {id:?} has a wider cardinality than interface {interface_name:?} requires"
            ),
            span,
        ),
        DiagnosticWitness::core_link(child_name, interface_name, id, expected, actual),
    )
}

pub(super) fn interface_parent_link_conflict(
    child_name: &str,
    parent_name: &str,
    id: &str,
    reason: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            crate::interfaces::codes::INVALID_INHERITANCE,
            reason.clone(),
            span,
        ),
        DiagnosticWitness::core_link(
            child_name,
            parent_name,
            id,
            "jointly satisfiable inherited link requirements",
            reason,
        ),
    )
}

pub(super) fn interface_parent_link_widens(
    child_name: &str,
    parent_name: &str,
    id: &str,
    expected: String,
    actual: String,
    span: Span,
) -> Diagnostic {
    with_witness(
        error(
            crate::interfaces::codes::INVALID_INHERITANCE,
            format!(
                "interface {child_name:?} redeclares link {id:?} with a wider contract than parent {parent_name:?} (expected {expected}, got {actual})"
            ),
            span,
        ),
        DiagnosticWitness::core_link(child_name, parent_name, id, expected, actual),
    )
}
