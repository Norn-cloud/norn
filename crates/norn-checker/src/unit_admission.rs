//! VAL-UNIT-1: resolve declared units against an explicitly pinned vocabulary.

use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, EnumValue, PropertyType, UnitSystem};

use crate::entity_artifacts::{Decoded, decode_v2};
use crate::{Diagnostic, Span};

const CATALOG: &str = concat!("core", ".", "units");
const CODES: &str = concat!("core", ".", "units", ".", "code");
const MISMATCH: &str = "core.values.unit_mismatch";

/// VAL-UNIT-1: each unit-bearing declaration requires one visible, exact pin.
pub(crate) fn check(candidates: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for artifact in candidates {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        let nodes: Vec<_> = match *root {
            EntityRoot::V2(root) => root
                .object_type
                .properties
                .into_iter()
                .enumerate()
                .map(|(i, p)| (format!("objectType/properties/{i}/type"), p.property_type))
                .collect(),
            EntityRoot::V2Interface(root) => root
                .interface
                .properties
                .into_iter()
                .enumerate()
                .map(|(i, p)| (format!("interface/properties/{i}/type"), p.property_type))
                .collect(),
            EntityRoot::V2Extension(root) => root
                .object_type_extension
                .properties
                .into_iter()
                .enumerate()
                .map(|(i, p)| {
                    (
                        format!("objectTypeExtension/properties/{i}/type"),
                        p.property_type,
                    )
                })
                .collect(),
            EntityRoot::V2PropertyTypes(root) => root
                .property_types
                .into_iter()
                .enumerate()
                .map(|(i, p)| (format!("propertyTypes/{i}/type"), p.property_type))
                .collect(),
            EntityRoot::V1(_) | EntityRoot::V2Markings(_) => Vec::new(),
        };
        let codes = pinned_codes(artifact, candidates, base);
        for (path, ty) in nodes {
            visit(
                &ty,
                &format!("/payload/{path}"),
                artifact,
                codes.as_deref(),
                &mut out,
            );
        }
    }
    out
}

fn pinned_codes(
    owner: &ArtifactEnvelope,
    candidates: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Option<Vec<EnumValue>> {
    let mut pins = owner
        .dependencies
        .iter()
        .filter(|pin| pin.artifact_id == CATALOG);
    let pin = pins.next()?;
    if pins.next().is_some() || !pin.required || pin.kind != ArtifactKind::Entity {
        return None;
    }
    let version = pin
        .version
        .as_deref()
        .filter(|version| !version.trim().is_empty())?;
    let mut targets = base
        .iter()
        .chain(candidates)
        .filter(|target| target.artifact_id == CATALOG && target.version == version);
    let target = targets.next()?;
    if targets.next().is_some()
        || crate::artifact_set::scope_conflict(&owner.scope, &target.scope).is_some()
    {
        return None;
    }
    let Decoded::Root(root) = decode_v2(target) else {
        return None;
    };
    let EntityRoot::V2PropertyTypes(root) = *root else {
        return None;
    };
    let mut definitions = root
        .property_types
        .into_iter()
        .filter(|def| def.name.as_str() == CODES);
    let definition = definitions.next()?;
    if definitions.next().is_some() {
        return None;
    }
    match definition.property_type {
        PropertyType::Enum { values } => Some(values),
        _ => None,
    }
}

fn visit(
    ty: &PropertyType,
    path: &str,
    owner: &ArtifactEnvelope,
    codes: Option<&[EnumValue]>,
    out: &mut Vec<Diagnostic>,
) {
    match ty {
        PropertyType::Decimal {
            unit: Some(unit), ..
        } => {
            if !crate::unit_codes::admitted(
                unit.system == UnitSystem::Ucum,
                &unit.code,
                codes
                    .into_iter()
                    .flatten()
                    .map(|value| value.value.as_str()),
            ) {
                let mut diagnostic = crate::property_types::error(MISMATCH,
                    "VAL-UNIT-1: unit requires a visible, exact core unit catalog pin and an admitted UCUM code; conversion must be explicit".to_owned(),
                    Span::pointer(format!("{path}/unit")));
                diagnostic.artifact_id = Some(owner.artifact_id.clone());
                out.push(diagnostic);
            }
        }
        PropertyType::Record { members } => {
            for (index, member) in members.iter().enumerate() {
                visit(
                    &member.property_type,
                    &format!("{path}/members/{index}/type"),
                    owner,
                    codes,
                    out,
                );
            }
        }
        _ => {}
    }
}
