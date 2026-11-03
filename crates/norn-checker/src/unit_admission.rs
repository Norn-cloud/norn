//! VAL-UNIT-1: resolve declared units against an explicitly pinned vocabulary.

use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, EnumValue, PropertyType, UnitSystem};

use crate::entity_artifacts::{Decoded, decode_v2};
use crate::{Diagnostic, Span};

const CATALOG: &str = concat!("core", ".", "units");
const CODES: &str = concat!("core", ".", "units", ".", "code");
const MISMATCH: &str = "core.values.unit_mismatch";

pub(crate) fn check(candidates: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for artifact in candidates {
        let vocabulary = pinned_codes(artifact, candidates, base);
        let codes = vocabulary.as_deref();
        let payload = artifact.payload.to_json_value();
        visit_schema(&payload, "/payload", artifact, codes, &mut out);
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        let mut inspect = |path: &str, i, ty: &PropertyType| {
            let pointer = format!("/payload/{path}/{i}/type");
            visit(ty, &pointer, artifact, codes, &mut out);
        };
        let (prefix, properties, links) = match *root {
            EntityRoot::V2(root) => {
                let object = root.object_type;
                ("objectType", object.properties, object.links)
            }
            EntityRoot::V2Extension(root) => {
                let extension = root.object_type_extension;
                ("objectTypeExtension", extension.properties, extension.links)
            }
            EntityRoot::V2Interface(root) => {
                for (i, p) in root.interface.properties.iter().enumerate() {
                    inspect("interface/properties", i, &p.property_type);
                }
                continue;
            }
            EntityRoot::V2PropertyTypes(root) => {
                for (i, p) in root.property_types.iter().enumerate() {
                    inspect("propertyTypes", i, &p.property_type);
                }
                continue;
            }
            EntityRoot::V1(_) | EntityRoot::V2Markings(_) => continue,
        };
        for (i, p) in properties.iter().enumerate() {
            inspect(&format!("{prefix}/properties"), i, &p.property_type);
        }
        for (i, link) in links.iter().enumerate() {
            let path = format!("{prefix}/links/{i}/properties");
            for (j, p) in link.properties.iter().enumerate() {
                inspect(&path, j, &p.property_type);
            }
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
    let PropertyType::Enum { values } = definition.property_type else {
        return None;
    };
    definitions.next().is_none().then_some(values)
}

fn visit(
    ty: &PropertyType,
    path: &str,
    owner: &ArtifactEnvelope,
    codes: Option<&[EnumValue]>,
    out: &mut Vec<Diagnostic>,
) {
    match ty {
        PropertyType::Decimal { unit, per_unit, .. } => {
            for (field, unit) in [("unit", unit.as_ref()), ("perUnit", per_unit.as_ref())] {
                let Some(unit) = unit else {
                    continue;
                };
                if !crate::unit_codes::admitted(
                    unit.system == UnitSystem::Ucum,
                    &unit.code,
                    codes
                        .into_iter()
                        .flatten()
                        .map(|value| value.value.as_str()),
                ) {
                    let mut diagnostic = crate::property_types::error(MISMATCH,"VAL-UNIT-1: unit requires a visible, exact core unit catalog pin and an admitted UCUM code; conversion must be explicit".to_owned(),Span::pointer(format!("{path}/{field}")));
                    diagnostic.artifact_id = Some(owner.artifact_id.clone());
                    out.push(diagnostic);
                }
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
        PropertyType::Text { .. }
        | PropertyType::Integer { .. }
        | PropertyType::Boolean { .. }
        | PropertyType::Timestamp { .. }
        | PropertyType::Date { .. }
        | PropertyType::Duration { .. }
        | PropertyType::Enum { .. }
        | PropertyType::JsonExt { .. }
        | PropertyType::Ref { .. } => {}
    }
}

fn visit_schema(
    value: &norn_ir::JsonValue,
    path: &str,
    owner: &ArtifactEnvelope,
    codes: Option<&[EnumValue]>,
    out: &mut Vec<Diagnostic>,
) {
    let mut pending = vec![(value, path.to_owned())];
    while let Some((value, path)) = pending.pop() {
        match value {
            norn_ir::JsonValue::Object(fields) => {
                if fields.contains_key("nornType")
                    && let Ok(Some(ty)) = norn_expr::schema::decimal_refinement(value, &path)
                {
                    visit(&ty, &format!("{path}/nornType"), owner, codes, out);
                }
                pending.extend(
                    fields
                        .iter()
                        .map(|(key, value)| (value, format!("{path}/{key}"))),
                );
            }
            norn_ir::JsonValue::Array(items) => {
                pending.extend(
                    items
                        .iter()
                        .enumerate()
                        .map(|(index, value)| (value, format!("{path}/{index}"))),
                );
            }
            norn_ir::JsonValue::Null
            | norn_ir::JsonValue::Bool(_)
            | norn_ir::JsonValue::Number(_)
            | norn_ir::JsonValue::String(_) => {}
        }
    }
}
