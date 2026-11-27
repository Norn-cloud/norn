//! VAL-UNIT-1: resolve declared units against an explicitly pinned vocabulary.

use std::collections::BTreeSet;

use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, EnumValue, PropertyType, UnitSystem};

use crate::entity_artifacts::{Decoded, decode_v2};
use crate::{Diagnostic, Span};

const CATALOG: &str = concat!("core", ".", "units");
const CODES: &str = concat!("core", ".", "units", ".", "code");
const PACK_PREFIX: &str = "units.";
const PACK_SUFFIX: &str = ".UnitDefinition.entity";
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

fn is_unit_catalog(artifact_id: &str) -> bool {
    artifact_id == CATALOG
        || artifact_id
            .strip_prefix(PACK_PREFIX)
            .and_then(|rest| rest.strip_suffix(PACK_SUFFIX))
            .is_some_and(|segment| !segment.is_empty() && !segment.contains('.'))
}

fn catalog_codes(target: &ArtifactEnvelope) -> Option<Vec<EnumValue>> {
    let Decoded::Root(root) = decode_v2(target) else {
        return None;
    };
    if target.artifact_id == CATALOG {
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
        return definitions.next().is_none().then_some(values);
    }
    let EntityRoot::V2(root) = *root else {
        return None;
    };
    let expected_name = target.artifact_id.strip_suffix(".entity")?;
    if root.object_type.name.as_str() != expected_name {
        return None;
    }
    let mut properties = root
        .object_type
        .properties
        .into_iter()
        .filter(|prop| prop.id.as_str() == "code");
    let property = properties.next()?;
    if property.constraints.nullable || properties.next().is_some() {
        return None;
    }
    let PropertyType::Enum { values } = property.property_type else {
        return None;
    };
    Some(values)
}

fn pinned_codes(
    owner: &ArtifactEnvelope,
    candidates: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Option<Vec<EnumValue>> {
    let mut seen = BTreeSet::new();
    let mut all_codes = Vec::new();
    for pin in owner
        .dependencies
        .iter()
        .filter(|pin| is_unit_catalog(&pin.artifact_id))
    {
        if !seen.insert(pin.artifact_id.as_str())
            || !pin.required
            || pin.kind != ArtifactKind::Entity
        {
            return None;
        }
        let version = pin
            .version
            .as_deref()
            .filter(|version| !version.trim().is_empty())?;
        let mut targets = base
            .iter()
            .chain(candidates)
            .filter(|target| target.artifact_id == pin.artifact_id && target.version == version);
        let target = targets.next()?;
        if targets.next().is_some()
            || crate::artifact_set::scope_conflict(&owner.scope, &target.scope).is_some()
        {
            return None;
        }
        all_codes.extend(catalog_codes(target)?);
    }
    (!seen.is_empty()).then_some(all_codes)
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
                    let mut diagnostic = crate::property_types::error(
                        MISMATCH,
                        "VAL-UNIT-1: unit requires a visible, exact core unit catalog pin and an admitted UCUM code; conversion must be explicit".to_owned(),
                        Span::pointer(format!("{path}/{field}")),
                    );
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
        | PropertyType::DecimalBinding { .. }
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
