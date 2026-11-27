//! Tri-state `FieldPatch` evaluation and `norn.feed/1` validation for
//! `FeedMode::Patch` (`FEED-MODE-1` / Lane B4).

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{Type, Value};
use norn_ir::feed::{FeedDeclaration, FeedMode, FieldPatch};
use norn_ir::mapping::{ActionInvocation, LinkPatch, MappedRecord, MappingIr};
use norn_ir::provenance::UtcTimestamp;
use norn_ir::source_schema::SourceSchemaIr;
use norn_ir::{PropertyId, RuntimeObject, RuntimeValue};

use super::codes;
use super::compile::{
    CompiledMap, CompiledMapping, Derivation, FieldRefusal, LinkTargetResolution, OBJECT_ID_FIELD,
    Refused, SourceDocument, apply_identity_metadata, apply_property_metadata, conform, encode,
    evaluate, evaluate_one_lifecycle_snapshot, expression_reads_union, json_text, map_matches,
    resolve_link_target, select_nested_value, text, type_name,
};
use crate::property_types::error;
use crate::{Diagnostic, Span};

/// Classification of a rule's source-field dependencies against the keys
/// actually present in a `FeedMode::Patch` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PatchPresence {
    /// Every required source field is present (or the rule is constant).
    Present,
    /// None of the rule's source fields were supplied in this patch.
    AllAbsent,
    /// Some of the rule's source fields were supplied and others were omitted;
    /// evaluating with fabricated nulls would corrupt the target value.
    PartialAbsent(Vec<String>),
}

/// Extract the exact source field names read by `expressions`. If any
/// expression inspects the whole `input` root, every declared input field is
/// required.
pub(super) fn rule_fields(
    expressions: &[&str],
    input_types: &BTreeMap<String, Type>,
) -> Vec<String> {
    let reads = expression_reads_union(expressions, "input");
    if reads.whole_root {
        return input_types.keys().cloned().collect();
    }
    reads
        .fields
        .into_iter()
        .filter(|field| input_types.contains_key(field))
        .collect()
}

/// Classify a rule's `source_fields` against the keys present in a patch row.
pub(super) fn classify_presence(
    source_fields: &[String],
    present_fields: &BTreeSet<String>,
) -> PatchPresence {
    if source_fields.is_empty() {
        return PatchPresence::Present;
    }
    let missing: Vec<String> = source_fields
        .iter()
        .filter(|field| !present_fields.contains(field.as_str()))
        .cloned()
        .collect();
    if missing.is_empty() {
        PatchPresence::Present
    } else if missing.len() == source_fields.len() {
        PatchPresence::AllAbsent
    } else {
        PatchPresence::PartialAbsent(missing)
    }
}

/// Validate `extensions["norn.feed/1"]` when present on a [`MappingIr`].
pub(super) fn validate_feed_declaration(
    mapping: &MappingIr,
    source: Option<&SourceSchemaIr>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let declaration = match FeedDeclaration::from_extensions(mapping.extensions.as_ref()) {
        Ok(Some(declaration)) => declaration,
        Ok(None) => return out,
        Err(reason) => {
            out.push(error(
                codes::INVALID_DOCUMENT,
                reason,
                Span::pointer("/extensions"),
            ));
            return out;
        }
    };
    if let Some(source) = source {
        for dataset in declaration.feeds.keys() {
            if !source.datasets.iter().any(|d| d.id == *dataset) {
                out.push(error(
                    codes::INVALID_DOCUMENT,
                    format!("norn.feed/1 references unknown source dataset {dataset:?}"),
                    Span::pointer("/extensions"),
                ));
            }
        }
    }
    out
}

/// Read the declared [`FeedMode`] for `dataset` from `mapping.extensions`.
pub(super) fn map_feed_mode(mapping: &MappingIr, dataset: &str) -> Result<FeedMode, String> {
    Ok(
        FeedDeclaration::from_extensions(mapping.extensions.as_ref())?
            .map_or(FeedMode::FullSnapshot, |decl| decl.mode_for(dataset)),
    )
}

fn build_row_input(
    map: &CompiledMap,
    row: &BTreeMap<String, norn_canonical::JsonValue>,
    mode: FeedMode,
    derivation: Derivation,
) -> Result<(BTreeMap<String, Value>, BTreeSet<String>), Refused> {
    let mut input = BTreeMap::new();
    let mut present_fields = BTreeSet::new();
    for (field, ty) in &map.input_types {
        let raw = row.get(field);
        if raw.is_some() {
            present_fields.insert(field.clone());
        } else if mode == FeedMode::Patch && !map.identity_fields.iter().any(|id| id == field) {
            continue;
        }
        let value = conform(raw, ty, derivation).map_err(|reason| Refused {
            code: codes::INVALID_RECORD,
            reason: format!("field {field:?} {reason}"),
            field: Some(Box::new(FieldRefusal {
                field: field.clone(),
                value: raw.map(json_text),
                expected: type_name(ty),
                reason,
            })),
        })?;
        input.insert(field.clone(), value);
    }
    Ok((input, present_fields))
}

fn partial_absent_refusal(kind: &str, member: &str, missing: &[String]) -> (&'static str, String) {
    (
        codes::INVALID_RECORD,
        format!(
            "patch record supplied part of {kind} rule {member} but omitted source field(s): {}",
            missing.join(", ")
        ),
    )
}

/// Apply one source row under either [`FeedMode::FullSnapshot`] or
/// [`FeedMode::Patch`].
///
/// In `FullSnapshot` mode this preserves exact legacy behavior (`field_patches`
/// omitted). In `Patch` mode, omitted source keys emit [`FieldPatch::Absent`]
/// and omit the property from `invocations[0].input` (preserving existing
/// canonical state), explicit `null` emits [`FieldPatch::ExplicitNull`] and
/// writes `RuntimeValue::Null`, and partial multi-field expressions fail closed.
pub(super) fn apply_row_with_mode(
    compiled: &CompiledMapping,
    map: &CompiledMap,
    row: Option<&norn_canonical::JsonValue>,
    document: &SourceDocument,
    recorded_at: &UtcTimestamp,
    derivation: Derivation,
) -> Result<Option<MappedRecord>, Refused> {
    let Some(norn_canonical::JsonValue::Object(row)) = row else {
        return Err((
            codes::INVALID_RECORD,
            "the record is not an object".to_owned(),
        )
            .into());
    };
    let mode = if document.mode == FeedMode::Patch || map.feed_mode == FeedMode::Patch {
        FeedMode::Patch
    } else {
        FeedMode::FullSnapshot
    };
    let (input, present_fields) = build_row_input(map, row, mode, derivation)?;
    let context = Value::object([("input", Value::Object(input))]);
    if !map_matches(map, &context)? {
        return Ok(None);
    }
    let key = text(&map.subject, &context)?;
    let source_record = text(&map.source_record, &context)?;
    let provenance =
        |member: &str| compiled.provenance_for(map, &source_record, member, document, recorded_at);
    let (values, field_patches, mut property_provenance) = evaluate_properties(
        map,
        row,
        &context,
        &present_fields,
        mode,
        derivation,
        &key,
        &provenance,
    )?;
    let lifecycle_snapshots =
        evaluate_lifecycles(map, &context, &present_fields, mode, &provenance)?;
    for snapshot in &lifecycle_snapshots {
        property_provenance.insert(snapshot.property.clone(), snapshot.provenance.clone());
    }
    let (links, cleared_links) =
        evaluate_links(map, &context, &present_fields, mode, &key, &provenance)?;
    let mut record_provenance = provenance("subject");
    apply_identity_metadata(&mut record_provenance, map);
    Ok(Some(MappedRecord {
        map: map.id.clone(),
        object_type: map.object_type.clone(),
        key,
        invocations: vec![ActionInvocation {
            action_id: map.observe_action.clone(),
            input: values,
        }],
        lifecycle_snapshots,
        links,
        cleared_links,
        field_patches,
        property_provenance,
        provenance: record_provenance,
    }))
}

#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "evaluates property rules with tri-state patch tracking"
)]
fn evaluate_properties(
    map: &CompiledMap,
    row: &BTreeMap<String, norn_canonical::JsonValue>,
    context: &Value,
    present_fields: &BTreeSet<String>,
    mode: FeedMode,
    derivation: Derivation,
    key: &str,
    provenance: &impl Fn(&str) -> norn_ir::provenance::InstanceProvenance,
) -> Result<
    (
        RuntimeObject,
        BTreeMap<PropertyId, FieldPatch<RuntimeValue>>,
        BTreeMap<PropertyId, norn_ir::provenance::InstanceProvenance>,
    ),
    Refused,
> {
    let mut values = RuntimeObject::new();
    values.insert(
        OBJECT_ID_FIELD.to_owned(),
        RuntimeValue::String(key.to_owned()),
    );
    let mut field_patches = BTreeMap::new();
    let mut property_provenance = BTreeMap::new();
    for entry in &map.properties {
        if mode == FeedMode::Patch {
            match classify_presence(&entry.source_fields, present_fields) {
                PatchPresence::AllAbsent => {
                    field_patches.insert(entry.property.clone(), FieldPatch::Absent);
                    continue;
                }
                PatchPresence::PartialAbsent(missing) => {
                    return Err(partial_absent_refusal(
                        "property",
                        entry.property.as_str(),
                        &missing,
                    )
                    .into());
                }
                PatchPresence::Present => {}
            }
        }
        let value = match &entry.nested {
            Some(nested) => select_nested_value(
                nested,
                row,
                &entry.property,
                &entry.expression,
                &entry.target_type,
                derivation,
            )?,
            None => evaluate(&entry.expression, context)?,
        };
        if let Some(encoded) = encode(&value, &entry.target_type).map_err(|reason| {
            (
                codes::EVALUATION_FAILED,
                format!("property {}: {reason}", entry.property.as_str()),
            )
        })? {
            if mode == FeedMode::Patch {
                let patch = if encoded.is_null() {
                    FieldPatch::ExplicitNull
                } else {
                    FieldPatch::Value(encoded.clone())
                };
                field_patches.insert(entry.property.clone(), patch);
            }
            values.insert(entry.property.as_str().to_owned(), encoded);
            let mut recorded = provenance(entry.property.as_str());
            apply_property_metadata(&mut recorded, map, entry.property.as_str());
            property_provenance.insert(entry.property.clone(), recorded);
        }
    }
    Ok((values, field_patches, property_provenance))
}

fn evaluate_lifecycles(
    map: &CompiledMap,
    context: &Value,
    present_fields: &BTreeSet<String>,
    mode: FeedMode,
    provenance: &impl Fn(&str) -> norn_ir::provenance::InstanceProvenance,
) -> Result<Vec<norn_ir::mapping::ObservedLifecycleSnapshot>, Refused> {
    let mut snapshots = Vec::new();
    for lifecycle in &map.lifecycles {
        if mode == FeedMode::Patch {
            match classify_presence(&lifecycle.source_fields, present_fields) {
                PatchPresence::AllAbsent => continue,
                PatchPresence::PartialAbsent(missing) => {
                    return Err(partial_absent_refusal(
                        "lifecycle",
                        lifecycle.property.as_str(),
                        &missing,
                    )
                    .into());
                }
                PatchPresence::Present => {}
            }
        }
        if let Some(mut snapshot) = evaluate_one_lifecycle_snapshot(lifecycle, context, provenance)?
        {
            apply_property_metadata(&mut snapshot.provenance, map, snapshot.property.as_str());
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

fn evaluate_links(
    map: &CompiledMap,
    context: &Value,
    present_fields: &BTreeSet<String>,
    mode: FeedMode,
    key: &str,
    provenance: &impl Fn(&str) -> norn_ir::provenance::InstanceProvenance,
) -> Result<(Vec<LinkPatch>, Vec<norn_ir::LinkId>), Refused> {
    let mut links = Vec::new();
    let mut cleared_links = Vec::new();
    for link in &map.links {
        if mode == FeedMode::Patch {
            match classify_presence(&link.source_fields, present_fields) {
                PatchPresence::AllAbsent => continue,
                PatchPresence::PartialAbsent(missing) => {
                    return Err(partial_absent_refusal("link", link.link.as_str(), &missing).into());
                }
                PatchPresence::Present => {}
            }
        }
        match resolve_link_target(link, map, context)? {
            LinkTargetResolution::Clear => cleared_links.push(link.link.clone()),
            LinkTargetResolution::Target(target_key) => {
                links.push(LinkPatch {
                    object_type: map.object_type.clone(),
                    key: key.to_owned(),
                    link: link.link.clone(),
                    target_type: link.target_type.clone(),
                    target_key,
                    provenance: provenance(link.link.as_str()),
                });
            }
        }
    }
    Ok((links, cleared_links))
}
