//! Hash-bound proposal evidence in the reserved `MappingIR` extension slot.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{ArtifactEnvelope, mapping::MappingIr, source_schema::SourceSchemaIr};

use crate::receipt::ArtifactRef;

pub use norn_ir::mapping_evidence::{
    EVIDENCE_SLOT, EvidenceArtifact, EvidenceDecision, EvidenceEdge, MappingEvidence, ReviewedKey,
    ReviewedReference,
};

/// Pinned source-owned projection declarations shared by generator and admission.
pub const SOURCE_RECORD_SLOT: &str = "norn.source-record/0.1.0";
/// Imported Frappe declarations use the native `name` record identity.
pub const FRAPPE_IMPORT_SLOT: &str = "norn.importers.frappe/0.1.0";

/// Generator provenance outside the removable evidence payload.
pub const CORE_MAPPING_GENERATOR: &str = "norn-ontology/core-mapping/0.1.0";

const IDENTITY_CHUNK_KEYS: usize = 16;

/// Return impacted target ids when source or target evidence is stale.
///
/// # Errors
/// Reject malformed reserved evidence rather than treating it as absent.
#[allow(
    clippy::too_many_lines,
    reason = "one evidence admission pass keeps hash, source-node and target impact validation coherent"
)]
pub fn evidence_impacts(
    mapping: &MappingIr,
    artifacts: &[ArtifactEnvelope],
) -> Result<Vec<String>, String> {
    let Some(evidence) = read_evidence(mapping)? else {
        return Ok(Vec::new());
    };
    if evidence.generator.trim().is_empty()
        || evidence.source.artifact_id != mapping.source.schema_id
        || evidence.source.version != mapping.source.version
        || evidence.source.sha256 != mapping.source.sha256
        || evidence.edges.is_empty()
        || evidence
            .edges
            .iter()
            .any(|edge| edge.dataset.is_empty() || edge.reason.trim().is_empty())
    {
        return Err("invalid or unbound mapping evidence".into());
    }
    let references: BTreeMap<_, _> = artifacts
        .iter()
        .map(|artifact| {
            evidence_artifact(artifact).map(|reference| (artifact.artifact_id.as_str(), reference))
        })
        .collect::<Result<_, _>>()?;
    let current = |reference: &EvidenceArtifact| {
        references.get(reference.artifact_id.as_str()) == Some(reference)
    };
    let source = artifacts
        .iter()
        .find(|a| a.artifact_id == mapping.source.schema_id)
        .ok_or("evidence source missing")?;
    let source: SourceSchemaIr = serde_json::from_value(
        serde_json::to_value(source.payload.to_json_value()).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let source_changed = !current(&evidence.source);
    validate_identity_table(
        &evidence.reviewed_keys,
        (!source_changed).then_some(&source),
    )?;
    validate_reference_table(
        &evidence.reviewed_references,
        (!source_changed).then_some(&source),
    )?;
    for reference in &evidence.reviewed_references {
        if !mapping.maps.iter().any(|map| {
            map.source.dataset == reference.dataset
                && map
                    .links
                    .iter()
                    .any(|link| link.link.as_str() == reference.link)
        }) {
            return Err("canonical reference evidence is not bound to a LinkMap".into());
        }
    }
    for edge in &evidence.edges {
        if !source_changed
            && (source.dataset(&edge.dataset).is_none()
                || edge
                    .field
                    .as_deref()
                    .is_some_and(|field| source.field(&edge.dataset, field).is_none()))
        {
            return Err("mapping evidence references an unknown source node".into());
        }
    }
    let target_refs: BTreeMap<_, _> = evidence
        .targets
        .iter()
        .map(|target| (target.artifact_id.as_str(), target))
        .collect();
    if target_refs.len() != evidence.targets.len()
        || evidence
            .edges
            .iter()
            .any(|edge| !target_refs.contains_key(edge.target.as_str()))
    {
        return Err("duplicate or unresolved evidence target identity".into());
    }
    let mut impacted: Vec<_> = evidence
        .edges
        .iter()
        .filter(|edge| {
            source_changed
                || target_refs
                    .get(edge.target.as_str())
                    .is_none_or(|target| !current(target))
        })
        .map(|edge| edge.target.clone())
        .collect();
    impacted.sort();
    impacted.dedup();
    if impacted.is_empty() {
        validate_reviewed_identity_maps(mapping, &evidence, &source, artifacts)?;
        validate_reviewed_reference_maps(mapping, &evidence, &source)?;
    }
    if evidence.mapping_sha256 != mapping_evidence_sha256(mapping)? {
        return Err("mapping evidence binding differs from executable content or retained field decisions; regenerate and review the proposal".into());
    }
    Ok(impacted)
}

/// Bind a generated mapping and all its retained review decisions together.
/// The canonical hash covers the complete typed payload, including the evidence
/// and unrelated extension slots. Only the digest field itself is omitted; no
/// generator-specific field-to-property policy is duplicated in admission.
///
/// # Errors
/// Missing/malformed reserved evidence or canonical serialization failure.
pub fn mapping_evidence_sha256(mapping: &MappingIr) -> Result<String, String> {
    let mut value = serde_json::to_value(mapping).map_err(|e| e.to_string())?;
    let evidence = value
        .get_mut("extensions")
        .and_then(|extensions| extensions.get_mut(EVIDENCE_SLOT))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("mapping evidence binding requires a reserved evidence object")?;
    evidence.remove("mappingSha256");
    norn_ir::canonical::domain_hash_hex(b"norn.mapping-evidence.binding.v1\0", &value)
        .map_err(|e| e.to_string())
}

/// Hash a definition using the checker receipt implementation.
///
/// # Errors
/// Return canonical hashing errors without substituting a digest.
pub fn evidence_artifact(artifact: &ArtifactEnvelope) -> Result<EvidenceArtifact, String> {
    let reference = ArtifactRef::of(artifact).map_err(|e| e.to_string())?;
    Ok(EvidenceArtifact {
        artifact_id: reference.artifact_id,
        version: reference.version,
        sha256: reference.sha256,
    })
}

/// Decode the reserved extension contract without treating malformed evidence as absent.
///
/// # Errors
/// Invalid reserved-slot content.
pub fn read_evidence(mapping: &MappingIr) -> Result<Option<MappingEvidence>, String> {
    mapping
        .extensions
        .as_ref()
        .and_then(|e| e.get(EVIDENCE_SLOT))
        .map(|value| {
            serde_json::from_value(serde_json::to_value(value).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())
        })
        .transpose()
}

/// Build bounded, disjoint guard/subject chunks for reviewed `name` identities.
/// Admission uses the same profile as proposal generation, so retained evidence
/// cannot describe different executable identity decisions.
///
/// # Errors
/// Refuse an identity that cannot fit the unchanged expression type, cost and
/// structural profile, or return string serialization failures.
pub fn reviewed_identity_profiles(
    dataset: &str,
    keys: &[ReviewedKey],
    source: &SourceSchemaIr,
) -> Result<Vec<(String, String)>, String> {
    let limits = norn_expr::StructuralLimits::NORN_EXPR_4;
    let source_limit = usize::try_from(limits.max_source_bytes).map_err(|e| e.to_string())?;
    let literal_limit =
        usize::try_from(limits.max_string_literal_bytes).map_err(|e| e.to_string())?;
    let fields = super::input_fields(source, dataset).ok_or("identity dataset missing")?;
    let env = norn_expr::ZenTypeEnv::input_only(fields, norn_expr::EnvOptions::default())
        .map_err(|e| e.to_string())?;
    let check_profile = |profile: &(String, String)| -> Result<(), String> {
        for (expression, expected) in [
            (profile.0.as_str(), norn_expr::Type::Bool),
            (profile.1.as_str(), norn_expr::Type::text().optional()),
        ] {
            if expression.len() > source_limit {
                return Err(
                    "reviewed identity exceeds the admitted expression source budget".into(),
                );
            }
            let expression = norn_expr::Expression::compile(expression)
                .map_err(|e| format!("reviewed identity expression: {e}"))?;
            norn_expr::check_expression(&expression, &env, &expected).map_err(|diagnostics| {
                format!("reviewed identity expression profile: {diagnostics:?}")
            })?;
        }
        Ok(())
    };
    let mut profiles = Vec::new();
    let mut chunk = Vec::new();
    for key in keys.iter().filter(|key| key.dataset == dataset) {
        if key.external_key.len() > literal_limit || key.canonical_key.len() > literal_limit {
            return Err("reviewed identity exceeds the admitted expression literal budget".into());
        }
        let mut next = chunk.clone();
        next.push(key);
        let next_profile = identity_chunk_expressions(&next)?;
        if next.len() > IDENTITY_CHUNK_KEYS || check_profile(&next_profile).is_err() {
            if chunk.is_empty() {
                check_profile(&next_profile)?;
            } else {
                profiles.push(identity_chunk_expressions(&chunk)?);
            }
            next = vec![key];
            check_profile(&identity_chunk_expressions(&next)?)?;
        }
        chunk = next;
    }
    if profiles.is_empty() || !chunk.is_empty() {
        profiles.push(identity_chunk_expressions(&chunk)?);
    }
    Ok(profiles)
}

fn identity_chunk_expressions(keys: &[&ReviewedKey]) -> Result<(String, String), String> {
    let mut guards = Vec::new();
    let mut subject = keys.last().map_or_else(
        || Ok("input.name".to_owned()),
        |key| mapping_text_expression(&key.canonical_key),
    )?;
    for key in keys {
        let external = mapping_text_expression(&key.external_key)?;
        guards.push(format!("input.name == {external}"));
    }
    for key in keys.iter().take(keys.len().saturating_sub(1)).rev() {
        let external = mapping_text_expression(&key.external_key)?;
        let canonical = mapping_text_expression(&key.canonical_key)?;
        subject = format!("input.name == {external} ? {canonical} : ({subject})");
    }
    let guard = if guards.is_empty() {
        "false".into()
    } else {
        guards.join(" or ")
    };
    Ok((guard, subject))
}

/// Build independent nullable key expressions for one reviewed relationship.
/// Each chunk passes the unchanged expression profile; chunks compose within
/// one link, rather than multiplying the source-owned object maps.
///
/// # Errors
/// A single binding cannot fit the admitted source, type or metered limits.
pub fn reviewed_reference_profiles(
    references: &[&ReviewedReference],
    source: &SourceSchemaIr,
) -> Result<Vec<String>, String> {
    let first = references.first().ok_or("reviewed reference group empty")?;
    let fields = super::input_fields(source, &first.dataset).ok_or("reference dataset missing")?;
    let env = norn_expr::ZenTypeEnv::input_only(fields, norn_expr::EnvOptions::default())
        .map_err(|e| e.to_string())?;
    let check = |text: &str| -> Result<(), String> {
        let expression = norn_expr::Expression::compile(text).map_err(|e| e.to_string())?;
        norn_expr::check_expression(&expression, &env, &norn_expr::Type::text().optional())
            .map_err(|diagnostics| {
                format!("reviewed reference expression profile: {diagnostics:?}")
            })?;
        Ok(())
    };
    let mut profiles = Vec::new();
    let mut chunk = Vec::new();
    for reference in references {
        let mut next = chunk.clone();
        next.push(*reference);
        let expression = reference_chunk_expression(&next, source)?;
        if next.len() > IDENTITY_CHUNK_KEYS || check(&expression).is_err() {
            if chunk.is_empty() {
                check(&expression)?;
            } else {
                profiles.push(reference_chunk_expression(&chunk, source)?);
            }
            next = vec![*reference];
            check(&reference_chunk_expression(&next, source)?)?;
        }
        chunk = next;
    }
    if !chunk.is_empty() {
        profiles.push(reference_chunk_expression(&chunk, source)?);
    }
    Ok(profiles)
}

fn reference_chunk_expression(
    references: &[&ReviewedReference],
    source: &SourceSchemaIr,
) -> Result<String, String> {
    let mut expression = "null".to_owned();
    for reference in references.iter().rev() {
        let mut predicate = exact_reference_predicate(
            source,
            &reference.dataset,
            &reference.field,
            &reference.external_key,
        )?;
        if let (Some(field), Some(value)) = (&reference.selector_field, &reference.selector_value) {
            predicate = format!(
                "({predicate}) and ({})",
                exact_reference_predicate(source, &reference.dataset, field, value)?
            );
        }
        expression = format!(
            "({predicate}) ? {} : ({expression})",
            mapping_text_expression(&reference.canonical_key)?
        );
    }
    Ok(expression)
}

fn exact_reference_predicate(
    source: &SourceSchemaIr,
    dataset: &str,
    field: &str,
    expected: &str,
) -> Result<String, String> {
    let input = format!("input[{}]", mapping_text_expression(field)?);
    let expected = mapping_text_expression(expected)?;
    if source
        .field(dataset, field)
        .ok_or("reviewed reference source field missing")?
        .required
        == Some(true)
    {
        Ok(format!("{input} == {expected}"))
    } else if !source.code_sets.iter().any(|set| {
        set.dataset == dataset
            && set.field == field
            && set.origin == norn_ir::source_schema::Origin::Declared
    }) {
        // Reviewed keys are nonempty. The empty text fallback makes absence
        // false with one bounded comparison, including nullable discriminators.
        Ok(format!("({input} ?? \"\") == {expected}"))
    } else {
        // Preserve absence without inventing a value outside a closed enum.
        Ok(format!(
            "{input} == null ? false : (({input} ?? {expected}) == {expected})"
        ))
    }
}

/// Bind retained reference decisions to complete, disjoint executable chunks.
///
/// # Errors
/// Missing, duplicated, edited, retargeted or unbound reference chunks.
pub fn validate_reviewed_reference_maps(
    mapping: &MappingIr,
    evidence: &MappingEvidence,
    source: &SourceSchemaIr,
) -> Result<(), String> {
    validate_reference_table(&evidence.reviewed_references, Some(source))?;
    for map in &mapping.maps {
        let anchor = map.properties.is_empty()
            && map.links.is_empty()
            && map.lifecycles.is_empty()
            && mapping
                .maps
                .iter()
                .flat_map(|other| &other.links)
                .any(|link| link.target_map.as_deref() == Some(map.id.as_str()));
        if anchor {
            continue;
        }
        let mut groups: BTreeMap<&str, Vec<&ReviewedReference>> = BTreeMap::new();
        for reference in evidence
            .reviewed_references
            .iter()
            .filter(|reference| reference.dataset == map.source.dataset)
        {
            groups.entry(&reference.link).or_default().push(reference);
        }
        for (link, references) in &groups {
            let mut pending: BTreeSet<_> = reviewed_reference_profiles(references, source)?
                .into_iter()
                .collect();
            let mut target = None;
            for entry in map
                .links
                .iter()
                .filter(|entry| entry.link.as_str() == *link)
            {
                if entry.identity_binding
                    || entry.target_map.is_none()
                    || !pending.remove(entry.key.as_str())
                    || target.is_some_and(|previous| Some(previous) != entry.target_map.as_deref())
                {
                    return Err("reviewed reference evidence differs from executable chunks".into());
                }
                target = entry.target_map.as_deref();
            }
            if !pending.is_empty() {
                return Err(
                    "reviewed reference evidence is not bound to complete executable chunks".into(),
                );
            }
        }
        for link in &map.links {
            let reference_anchor = link
                .target_map
                .as_deref()
                .and_then(|id| mapping.map(id))
                .is_some_and(|target| {
                    target.properties.is_empty()
                        && target.links.is_empty()
                        && target.lifecycles.is_empty()
                        && target
                            .when
                            .as_ref()
                            .is_some_and(|guard| guard.as_str() == "false")
                });
            if reference_anchor && !groups.contains_key(link.link.as_str()) {
                return Err("reviewed reference evidence missing for executable chunks".into());
            }
        }
    }
    Ok(())
}

/// Encode exact text using admitted ZEN literals and concatenation.
/// ZEN closes a literal at its next matching quote and treats backslashes
/// literally, so JSON escaping cannot represent arbitrary reviewed keys.
///
/// # Errors
/// Refuse control-bearing text rather than emitting executable control tokens.
pub fn mapping_text_expression(value: &str) -> Result<String, String> {
    if crate::source_schema::source_identity_has_control_chars(value) {
        return Err("mapping expression text contains control characters".into());
    }
    Ok(if !value.contains('"') {
        format!("\"{value}\"")
    } else if !value.contains('\'') {
        format!("'{value}'")
    } else {
        let parts: Vec<_> = value.split('\'').map(|part| format!("'{part}'")).collect();
        format!("({})", parts.join(" + \"'\" + "))
    })
}

/// Derive record identity from the hash-pinned target's projection declaration.
///
/// # Errors
/// Refuse absent, mismatched or non-text source identity declarations.
pub fn source_owned_identity_expressions(
    target: &ArtifactEnvelope,
    source: &SourceSchemaIr,
    dataset: &str,
) -> Result<(String, String), String> {
    let target = serde_json::to_value(target.payload.to_json_value()).map_err(|e| e.to_string())?;
    let extensions = target
        .get("extensions")
        .ok_or("source identity declarations missing")?;
    let field = if let Some(declaration) = extensions.get(SOURCE_RECORD_SLOT) {
        if declaration
            .get("dataset")
            .and_then(serde_json::Value::as_str)
            != Some(dataset)
        {
            return Err("source identity declaration names a different dataset".into());
        }
        declaration
            .get("identityField")
            .and_then(serde_json::Value::as_str)
            .ok_or("source identityField declaration missing")?
    } else if extensions
        .get(FRAPPE_IMPORT_SLOT)
        .and_then(|declaration| declaration.get("doctype"))
        .and_then(serde_json::Value::as_str)
        == Some(dataset)
    {
        "name"
    } else {
        return Err("source identity declaration is not bound to the dataset".into());
    };
    if !source.field(dataset, field).is_some_and(|field| {
        matches!(
            field.type_candidate.normalized,
            norn_ir::source_schema::NormalizedType::String
                | norn_ir::source_schema::NormalizedType::Text
                | norn_ir::source_schema::NormalizedType::Reference
        )
    }) {
        return Err("source identity declaration requires a text source field".into());
    }
    let record = format!("input[{}]", mapping_text_expression(field)?);
    let subject = source_owned_identity_subject(&source.source.instance, dataset, &record)?;
    Ok((record, subject))
}

/// Namespace a source-owned subject using its pinned source instance and dataset.
///
/// # Errors
/// Return canonical hashing or string serialization failures.
pub fn source_owned_identity_subject(
    instance: &str,
    dataset: &str,
    source_record: &str,
) -> Result<String, String> {
    let namespace =
        norn_ir::canonical::domain_hash_hex(b"norn.imported-record-key.v1\0", &(instance, dataset))
            .map_err(|e| e.to_string())?;
    Ok(format!(
        "{} + ({source_record})",
        mapping_text_expression(&format!("source:{namespace}:"))?
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "one role-based pass binds every identity chunk and source-owned declaration"
)]
fn validate_reviewed_identity_maps(
    mapping: &MappingIr,
    evidence: &MappingEvidence,
    source: &SourceSchemaIr,
    artifacts: &[ArtifactEnvelope],
) -> Result<(), String> {
    let identities: BTreeSet<_> = evidence
        .edges
        .iter()
        .filter(|edge| {
            edge.field.is_none()
                && edge.member.is_none()
                && edge.decision == EvidenceDecision::ReferenceCandidate
        })
        .map(|edge| (edge.dataset.as_str(), edge.target.as_str()))
        .collect();
    let mut pending = identities
        .iter()
        .map(|identity| {
            reviewed_identity_profiles(identity.0, &evidence.reviewed_keys, source)
                .map(|profiles| (*identity, profiles.into_iter().collect::<BTreeSet<_>>()))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    for map in &mapping.maps {
        let identity = (map.source.dataset.as_str(), map.target.entity.as_str());
        let disabled = map
            .when
            .as_ref()
            .is_some_and(|when| when.as_str() == "false");
        let anchor = map.properties.is_empty()
            && map.links.is_empty()
            && map.lifecycles.is_empty()
            && mapping
                .maps
                .iter()
                .flat_map(|other| &other.links)
                .any(|link| link.target_map.as_deref() == Some(map.id.as_str()));
        if anchor {
            if !disabled {
                return Err("reviewed identity evidence has an active reference-only map".into());
            }
            continue;
        }
        if let Some(profiles) = pending.get_mut(&identity) {
            let profile = (
                map.when
                    .as_ref()
                    .map_or("", norn_ir::ZenExpression::as_str)
                    .to_owned(),
                map.subject.as_str().to_owned(),
            );
            if !profiles.remove(&profile)
                || map.source_record.as_str() != "input[\"name\"]"
                || map.source.nested_in.is_some()
            {
                return Err(
                    "reviewed identity evidence differs from executable identity chunks".into(),
                );
            }
        } else if evidence.edges.iter().any(|edge| {
            edge.dataset == map.source.dataset
                && edge.target == map.target.entity
                && edge.field.is_none()
                && edge.member.is_none()
                && edge.decision == EvidenceDecision::DerivedFrom
        }) {
            let target = artifacts
                .iter()
                .find(|artifact| artifact.artifact_id == map.target.entity)
                .ok_or("source-owned identity target missing")?;
            let (record, subject) =
                source_owned_identity_expressions(target, source, &map.source.dataset)
                    .map_err(|reason| format!("reviewed identity evidence: {reason}"))?;
            if map.source_record.as_str() != record.as_str()
                || map.subject.as_str() != subject.as_str()
                || map.source.nested_in.is_some()
            {
                return Err(
                    "reviewed identity evidence differs from the declared source-owned identity"
                        .into(),
                );
            }
        } else if !disabled
            || !evidence.edges.iter().any(|edge| {
                edge.dataset == map.source.dataset
                    && edge.target == map.target.entity
                    && edge.field.is_some()
                    && edge.decision == EvidenceDecision::ReferenceCandidate
            })
        {
            return Err(
                "reviewed identity evidence has an unbound or active reference-only map".into(),
            );
        }
    }
    if pending.values().any(|profiles| !profiles.is_empty())
        || evidence.reviewed_keys.iter().any(|key| {
            !identities
                .iter()
                .any(|(dataset, _)| *dataset == key.dataset)
        })
    {
        return Err("reviewed identity evidence is not bound to complete executable chunks".into());
    }
    Ok(())
}

/// Validate exact reviewed identities at both proposal and admission boundaries.
///
/// # Errors
/// Empty, wildcard, control-bearing, oversized, ambiguous or unknown identities.
pub fn validate_identity_table(
    keys: &[ReviewedKey],
    source: Option<&SourceSchemaIr>,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for key in keys {
        if !super::exact_entitlement_value(&key.external_key)
            || !super::exact_entitlement_value(&key.canonical_key)
            || key.canonical_key.len() > norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES
            || key.dataset.is_empty()
            || key.evidence.trim().is_empty()
            || source.is_some_and(|source| source.dataset(&key.dataset).is_none())
            || !seen.insert((&key.dataset, &key.external_key))
        {
            return Err("invalid or ambiguous reviewed identity evidence".into());
        }
    }
    Ok(())
}

/// Validate exact-key reference configuration, including overlapping selector ambiguity.
///
/// # Errors
/// Empty, ambiguous, non-text or unknown source reference fields.
pub fn validate_reference_table(
    references: &[ReviewedReference],
    source: Option<&SourceSchemaIr>,
) -> Result<(), String> {
    let mut keys = BTreeSet::new();
    let mut links = BTreeMap::new();
    let mut fields = BTreeMap::new();
    for reference in references {
        if !super::exact_entitlement_value(&reference.external_key)
            || !super::exact_entitlement_value(&reference.canonical_key)
            || reference.canonical_key.len() > norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES
            || reference.dataset.is_empty()
            || reference.field.is_empty()
            || reference.link.is_empty()
            || reference.target_type.is_empty()
            || reference.evidence.trim().is_empty()
            || reference.selector_field.is_some() != reference.selector_value.is_some()
            || reference
                .selector_value
                .as_deref()
                .is_some_and(|value| !super::exact_entitlement_value(value))
            || !keys.insert((
                &reference.dataset,
                &reference.link,
                &reference.external_key,
                &reference.selector_value,
            ))
        {
            return Err("invalid or ambiguous reviewed canonical reference".into());
        }
        let configuration = (
            &reference.field,
            &reference.target_type,
            &reference.selector_field,
        );
        if links
            .insert((&reference.dataset, &reference.link), configuration)
            .is_some_and(|previous| previous != configuration)
        {
            return Err(
                "one reviewed link must have one source field, target type and discriminator"
                    .into(),
            );
        }
        let destination = (&reference.link, &reference.target_type);
        if fields
            .insert((&reference.dataset, &reference.field), destination)
            .is_some_and(|previous| previous != destination)
        {
            return Err("one source reference field must have one canonical relationship".into());
        }
        if let Some(source) = source {
            let text = |field: &str| {
                source
                    .field(&reference.dataset, field)
                    .is_some_and(|field| {
                        matches!(
                            field.type_candidate.normalized,
                            norn_ir::source_schema::NormalizedType::String
                                | norn_ir::source_schema::NormalizedType::Text
                                | norn_ir::source_schema::NormalizedType::Reference
                        )
                    })
            };
            if !text(&reference.field)
                || reference
                    .selector_field
                    .as_deref()
                    .is_some_and(|field| !text(field))
            {
                return Err(
                    "reviewed reference and discriminator require declared text source fields"
                        .into(),
                );
            }
        }
    }
    Ok(())
}
