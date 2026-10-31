//! Closed-world validation for ontology business-pack profiles.
//!
//! OWL and RDF projections are useful for semantic interoperability, but they
//! are not an authority boundary.  This module validates the Norn-owned,
//! versioned intake contract before an ontology pack or imported semantic
//! instance can be promoted.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{Diagnostic, DiagnosticRemediation, DiagnosticSeverity, JsonValue};

mod link;
pub(crate) use link::validate_link_payload;

const PACK_CODE: &str = "ontology_pack.validation.invalid_payload";
const INSTANCE_CODE: &str = "ontology_instance.validation.invalid_payload";
const REGISTRY_CODE: &str = "ontology_registry.validation.invalid_payload";
const PACK_VERSION: &str = "0.1";
/// Licences reviewed for redistribution: the only ones a pack may vendor or
/// `import` from.
const REDISTRIBUTABLE_LICENSES: [&str; 3] = ["MIT", "Apache-2.0", "CC-BY-4.0"];
const IMPORT_MODES: [&str; 3] = ["import", "map_only", "study_only"];
const TERM_KINDS: [&str; 6] = [
    "class",
    "property",
    "value_object",
    "concept_scheme_root",
    "action",
    "predicate",
];
/// Term kinds that take one closed promotion shape; the others are
/// vocabulary handles (members, operations, conditions) and take none.
const SHAPED_KINDS: [&str; 3] = ["class", "value_object", "concept_scheme_root"];
/// Term kinds that belong to a class and must name it in `domainTermId`.
const DOMAIN_KINDS: [&str; 2] = ["property", "action"];

/// The terms of one pack: status and kind by term id.
#[derive(Debug, Default)]
struct PackTerms {
    statuses: BTreeMap<String, String>,
    kinds: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct RegistryTerm {
    status: String,
    kind: String,
    parent_term_id: Option<String>,
    pack_index: usize,
}

#[derive(Debug, Default)]
struct OntologyRegistryIndex {
    terms: BTreeMap<String, RegistryTerm>,
}

impl OntologyRegistryIndex {
    fn is_active_member_of(&self, term_id: &str, target_term_id: &str) -> bool {
        let Some(term) = self.terms.get(term_id) else {
            return false;
        };
        let Some(target) = self.terms.get(target_term_id) else {
            return false;
        };
        if term.status != "active" || target.status != "active" {
            return false;
        }

        let mut current = Some(term_id);
        let mut visited = BTreeSet::new();
        while let Some(candidate) = current {
            if candidate == target_term_id {
                return true;
            }
            if !visited.insert(candidate.to_string()) {
                return false;
            }
            current = self
                .terms
                .get(candidate)
                .and_then(|entry| entry.parent_term_id.as_deref());
        }
        false
    }
}

/// Validate one standalone ontology-pack payload before it enters an artifact
/// envelope or any projection pipeline.
///
/// # Errors
/// Returns payload diagnostics for malformed sources, terms, mappings or closed shapes.
pub fn validate_ontology_pack(pack_payload: &JsonValue) -> Result<(), Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    validate_ontology_pack_payload(pack_payload, &mut diagnostics, None);
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

/// Validate one semantic instance against the closed shapes in an ontology
/// pack payload.
///
/// This validates an import/projection candidate only.  Success does not grant
/// a capability and cannot authorize an authoritative transition.
///
/// # Errors
/// Returns pack diagnostics, or instance diagnostics for invalid fields, provenance or term membership.
pub fn validate_ontology_instance(
    pack_payload: &JsonValue,
    instance: &JsonValue,
) -> Result<(), Vec<Diagnostic>> {
    validate_ontology_pack(pack_payload)?;
    let mut diagnostics = Vec::new();
    let registry =
        build_registry_index(std::slice::from_ref(pack_payload), &mut diagnostics, false);
    validate_instance_payload(pack_payload, instance, &registry, &mut diagnostics);
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

/// Validate a closed set of ontology packs as one term-resolution registry.
///
/// The registry supplies the scope in which cross-pack `targetTermId`
/// references resolve. It does not perform OWL inference and does not grant
/// mutation authority.
///
/// # Errors
/// Returns pack diagnostics or registry diagnostics for duplicate terms and unresolved cross-pack references.
pub fn validate_ontology_registry(pack_payloads: &[JsonValue]) -> Result<(), Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    for pack in pack_payloads {
        if let Err(pack_diagnostics) = validate_ontology_pack(pack) {
            diagnostics.extend(pack_diagnostics);
        }
    }
    build_registry_index(pack_payloads, &mut diagnostics, true);
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

/// Check one instance using all packs in an explicitly supplied registry.
///
/// # Errors
/// Returns registry diagnostics, unresolved instance ownership or violations of the owning pack's closed shape.
pub fn validate_ontology_instance_in_registry(
    pack_payloads: &[JsonValue],
    instance: &JsonValue,
) -> Result<(), Vec<Diagnostic>> {
    validate_ontology_registry(pack_payloads)?;
    let mut diagnostics = Vec::new();
    let registry = build_registry_index(pack_payloads, &mut diagnostics, false);

    let term_id = as_object(instance).and_then(|value| string_field(value, "termId"));
    let owning_pack = term_id
        .and_then(|term_id| registry.terms.get(term_id))
        .and_then(|term| pack_payloads.get(term.pack_index));
    let Some(owning_pack) = owning_pack else {
        diagnostics.push(diagnostic(
            INSTANCE_CODE,
            "termId does not resolve uniquely in the supplied ontology registry",
            "instance.termId",
            None,
        ));
        return Err(diagnostics);
    };
    validate_instance_payload(owning_pack, instance, &registry, &mut diagnostics);
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

/// Decide bounded Norn term membership using the checked parent hierarchy.
///
/// # Errors
/// Returns registry diagnostics when the supplied packs do not form a valid closed registry.
pub fn ontology_term_is_a(
    pack_payloads: &[JsonValue],
    term_id: &str,
    target_term_id: &str,
) -> Result<bool, Vec<Diagnostic>> {
    validate_ontology_registry(pack_payloads)?;
    let mut diagnostics = Vec::new();
    let registry = build_registry_index(pack_payloads, &mut diagnostics, false);
    Ok(registry.is_active_member_of(term_id, target_term_id))
}

fn validate_instance_payload(
    pack_payload: &JsonValue,
    instance: &JsonValue,
    registry: &OntologyRegistryIndex,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(pack) = as_object(pack_payload) else {
        diagnostics.push(diagnostic(
            INSTANCE_CODE,
            "ontology pack payload must be an object",
            "packPayload",
            None,
        ));
        return;
    };
    let Some(instance) = as_object(instance) else {
        diagnostics.push(diagnostic(
            INSTANCE_CODE,
            "ontology instance must be an object",
            "instance",
            None,
        ));
        return;
    };

    reject_unknown_fields(
        instance,
        &["instanceId", "termId", "values", "provenance"],
        "instance",
        diagnostics,
        None,
        INSTANCE_CODE,
    );
    required_string(
        instance,
        "instanceId",
        "instance.instanceId",
        diagnostics,
        None,
        INSTANCE_CODE,
    );
    let term_id = required_string(
        instance,
        "termId",
        "instance.termId",
        diagnostics,
        None,
        INSTANCE_CODE,
    );
    validate_instance_provenance(instance.get("provenance"), diagnostics);

    let Some(values) = instance.get("values").and_then(as_object) else {
        diagnostics.push(diagnostic(
            INSTANCE_CODE,
            "instance.values must be an object",
            "instance.values",
            None,
        ));
        return;
    };

    let active_term = term_id
        .as_deref()
        .is_some_and(|wanted| find_term_status(pack, wanted) == Some("active"));
    if term_id.is_some() && !active_term {
        diagnostics.push(diagnostic(
            INSTANCE_CODE,
            "termId must resolve to an active ontology term; historical instance mode is not supported",
            "instance.termId",
            None,
        ));
    }
    let shape = term_id
        .as_deref()
        .filter(|_| active_term)
        .and_then(|wanted| find_shape(pack, wanted));
    let Some(shape) = shape else {
        if active_term {
            diagnostics.push(diagnostic(
                INSTANCE_CODE,
                "termId does not resolve to a closed ontology shape",
                "instance.termId",
                None,
            ));
        }
        return;
    };
    validate_instance_values(shape, values, registry, diagnostics);
}

fn build_registry_index(
    pack_payloads: &[JsonValue],
    diagnostics: &mut Vec<Diagnostic>,
    validate_targets: bool,
) -> OntologyRegistryIndex {
    let mut registry = OntologyRegistryIndex::default();
    let mut pack_ids = BTreeSet::new();

    if pack_payloads.is_empty() {
        diagnostics.push(diagnostic(
            REGISTRY_CODE,
            "ontology registry must contain at least one pack",
            "registry.packs",
            None,
        ));
    }

    for (pack_index, pack_payload) in pack_payloads.iter().enumerate() {
        let Some(pack) = as_object(pack_payload) else {
            continue;
        };
        if let Some(pack_id) = string_field(pack, "packId")
            && !pack_ids.insert(pack_id.to_string())
        {
            diagnostics.push(diagnostic(
                REGISTRY_CODE,
                "packId must be unique within one ontology registry",
                format!("registry.packs[{pack_index}].packId"),
                None,
            ));
        }
        let Some(terms) = pack.get("terms").and_then(as_array) else {
            continue;
        };
        for (term_index, term) in terms.iter().enumerate() {
            let Some(term) = as_object(term) else {
                continue;
            };
            let (Some(term_id), Some(status), Some(kind)) = (
                string_field(term, "termId"),
                string_field(term, "status"),
                string_field(term, "termKind"),
            ) else {
                continue;
            };
            let entry = RegistryTerm {
                status: status.to_string(),
                kind: kind.to_string(),
                parent_term_id: string_field(term, "parentTermId").map(str::to_string),
                pack_index,
            };
            if registry.terms.insert(term_id.to_string(), entry).is_some() {
                diagnostics.push(diagnostic(
                    REGISTRY_CODE,
                    "termId must be globally unique within one ontology registry",
                    format!("registry.packs[{pack_index}].terms[{term_index}].termId"),
                    None,
                ));
            }
        }
    }

    if validate_targets {
        validate_registry_targets(pack_payloads, &registry, diagnostics);
    }
    registry
}

fn validate_registry_targets(
    pack_payloads: &[JsonValue],
    registry: &OntologyRegistryIndex,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (pack_index, pack_payload) in pack_payloads.iter().enumerate() {
        let Some(shapes) = as_object(pack_payload)
            .and_then(|pack| pack.get("shapes"))
            .and_then(as_array)
        else {
            continue;
        };
        for (shape_index, shape) in shapes.iter().enumerate() {
            let Some(fields) = as_object(shape)
                .and_then(|shape| shape.get("fields"))
                .and_then(as_array)
            else {
                continue;
            };
            for (field_index, field) in fields.iter().enumerate() {
                let Some(field) = as_object(field) else {
                    continue;
                };
                if string_field(field, "valueType") != Some("term_ref") {
                    continue;
                }
                let Some(target_term_id) = string_field(field, "targetTermId") else {
                    continue;
                };
                let valid_target = registry
                    .terms
                    .get(target_term_id)
                    .is_some_and(|target| target.status == "active" && target.kind != "property");
                if !valid_target {
                    diagnostics.push(diagnostic(
                        REGISTRY_CODE,
                        format!(
                            "targetTermId '{target_term_id}' must resolve to an active non-property term in the supplied registry"
                        ),
                        format!(
                            "registry.packs[{pack_index}].shapes[{shape_index}].fields[{field_index}].targetTermId"
                        ),
                        None,
                    ));
                }
            }
        }
    }
}

pub(crate) fn validate_ontology_pack_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(pack) = as_object(value) else {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "payload must be an object",
            "payload",
            artifact_id,
        ));
        return;
    };

    reject_unknown_fields(
        pack,
        &[
            "packId",
            "packVersion",
            "packKind",
            "sources",
            "terms",
            "mappings",
            "shapes",
        ],
        "payload",
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    if let Some(pack_id) = required_string(
        pack,
        "packId",
        "payload.packId",
        diagnostics,
        artifact_id,
        PACK_CODE,
    ) && !valid_prefixed_id(&pack_id, "norn.ontology-pack.")
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "ontology packId must use the opaque norn.ontology-pack.* namespace",
            "payload.packId",
            artifact_id,
        ));
    }
    require_literal(
        pack,
        "packKind",
        &["ontology"],
        "payload.packKind",
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    require_literal(
        pack,
        "packVersion",
        &[PACK_VERSION],
        "payload.packVersion",
        diagnostics,
        artifact_id,
        PACK_CODE,
    );

    let source_ids = validate_sources(pack.get("sources"), diagnostics, artifact_id);
    let terms = validate_terms(pack.get("terms"), diagnostics, artifact_id);
    validate_mappings(
        pack.get("mappings"),
        &source_ids,
        &terms.statuses,
        diagnostics,
        artifact_id,
    );
    validate_shapes(pack.get("shapes"), &terms, diagnostics, artifact_id);
}

/// Validates the source manifest; returns each source id with its import
/// mode (absent when the mode itself is invalid).
fn validate_sources(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> BTreeMap<String, Option<String>> {
    let Some(sources) = non_empty_array(
        value,
        "payload.sources",
        diagnostics,
        artifact_id,
        PACK_CODE,
    ) else {
        return BTreeMap::new();
    };
    let mut ids = BTreeMap::new();

    for (index, source) in sources.iter().enumerate() {
        let path = format!("payload.sources[{index}]");
        let Some(source) = as_object(source) else {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "source pin must be an object",
                path,
                artifact_id,
            ));
            continue;
        };
        reject_unknown_fields(
            source,
            &[
                "sourceId",
                "releaseId",
                "releasedAt",
                "retrievedAt",
                "canonicalUrl",
                "license",
                "redistribution",
                "importMode",
                "importer",
                "files",
            ],
            &path,
            diagnostics,
            artifact_id,
            PACK_CODE,
        );
        let source_id = required_string(
            source,
            "sourceId",
            format!("{path}.sourceId"),
            diagnostics,
            artifact_id,
            PACK_CODE,
        );
        let import_mode = require_literal(
            source,
            "importMode",
            &IMPORT_MODES,
            format!("{path}.importMode"),
            diagnostics,
            artifact_id,
            PACK_CODE,
        );
        if let Some(source_id) = source_id {
            if !valid_prefixed_id(&source_id, "source.") {
                diagnostics.push(diagnostic(
                    PACK_CODE,
                    "sourceId must use the lowercase source.* namespace",
                    format!("{path}.sourceId"),
                    artifact_id,
                ));
            }
            if ids.insert(source_id, import_mode.clone()).is_some() {
                diagnostics.push(diagnostic(
                    PACK_CODE,
                    "sourceId must be unique within the pack",
                    format!("{path}.sourceId"),
                    artifact_id,
                ));
            }
        }

        validate_source_release(source, &path, diagnostics, artifact_id);
        let license = required_string(
            source,
            "license",
            format!("{path}.license"),
            diagnostics,
            artifact_id,
            PACK_CODE,
        );
        let redistribution = require_literal(
            source,
            "redistribution",
            &["reference_only", "vendored"],
            format!("{path}.redistribution"),
            diagnostics,
            artifact_id,
            PACK_CODE,
        );
        validate_source_files(
            source.get("files"),
            redistribution.as_deref(),
            license.as_deref(),
            &path,
            diagnostics,
            artifact_id,
        );
        validate_source_use(
            source,
            import_mode.as_deref(),
            license.as_deref(),
            &path,
            diagnostics,
            artifact_id,
        );
    }

    ids
}

/// The source-manifest rules of an import mode (norn#56): an imported
/// source names its importer and carries a redistributable licence, and a
/// source the pack imports or maps from is content-pinned.
fn validate_source_use(
    source: &BTreeMap<String, JsonValue>,
    import_mode: Option<&str>,
    license: Option<&str>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    match source.get("importer") {
        Some(JsonValue::Object(importer)) => {
            let importer_path = format!("{path}.importer");
            reject_unknown_fields(
                importer,
                &["name", "version"],
                &importer_path,
                diagnostics,
                artifact_id,
                PACK_CODE,
            );
            if let Some(name) = required_string(
                importer,
                "name",
                format!("{importer_path}.name"),
                diagnostics,
                artifact_id,
                PACK_CODE,
            ) && !(name.contains('.') && valid_prefixed_id(&name, ""))
            {
                diagnostics.push(diagnostic(
                    PACK_CODE,
                    "importer name must be a lowercase dotted identifier",
                    format!("{importer_path}.name"),
                    artifact_id,
                ));
            }
            if let Some(version) = required_string(
                importer,
                "version",
                format!("{importer_path}.version"),
                diagnostics,
                artifact_id,
                PACK_CODE,
            ) && !valid_semantic_version(&version)
            {
                diagnostics.push(diagnostic(
                    PACK_CODE,
                    "importer version must be a strict semantic version",
                    format!("{importer_path}.version"),
                    artifact_id,
                ));
            }
        }
        Some(_) => diagnostics.push(diagnostic(
            PACK_CODE,
            "importer must be an object",
            format!("{path}.importer"),
            artifact_id,
        )),
        None if import_mode == Some("import") => diagnostics.push(diagnostic(
            PACK_CODE,
            "an imported source must name its importer",
            format!("{path}.importer"),
            artifact_id,
        )),
        None => {}
    }

    if import_mode == Some("import")
        && !license.is_some_and(|license| REDISTRIBUTABLE_LICENSES.contains(&license))
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "an imported source's license is not on the reviewed redistribution allowlist; map it instead",
            format!("{path}.license"),
            artifact_id,
        ));
    }

    if matches!(import_mode, Some("import" | "map_only")) && !source_is_content_pinned(source) {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "a source the pack imports or maps from must be content-pinned: file hashes, or a commit or hash in releaseId",
            format!("{path}.releaseId"),
            artifact_id,
        ));
    }
}

/// Whether a source's bytes are fixed: a content hash as (or after `@` in)
/// the release id, or at least one file hash.
fn source_is_content_pinned(source: &BTreeMap<String, JsonValue>) -> bool {
    let release_pinned = string_field(source, "releaseId").is_some_and(|release| {
        valid_content_hash(release)
            || release
                .rsplit_once('@')
                .is_some_and(|(_, pin)| valid_content_hash(pin))
    });
    let file_pinned =
        matches!(source.get("files"), Some(JsonValue::Array(files)) if !files.is_empty());
    release_pinned || file_pinned
}

fn validate_source_files(
    value: Option<&JsonValue>,
    redistribution: Option<&str>,
    license: Option<&str>,
    parent_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let files = match value {
        Some(JsonValue::Array(files)) => files,
        Some(_) => {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "files must be an array",
                format!("{parent_path}.files"),
                artifact_id,
            ));
            return;
        }
        None if redistribution == Some("vendored") => {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "vendored sources require file hashes",
                format!("{parent_path}.files"),
                artifact_id,
            ));
            return;
        }
        None => return,
    };

    if redistribution == Some("vendored") && files.is_empty() {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "vendored sources require at least one file hash",
            format!("{parent_path}.files"),
            artifact_id,
        ));
    }
    if redistribution == Some("vendored")
        && !license.is_some_and(|license| REDISTRIBUTABLE_LICENSES.contains(&license))
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "vendored source license is not on the reviewed redistribution allowlist",
            format!("{parent_path}.license"),
            artifact_id,
        ));
    }

    for (index, file) in files.iter().enumerate() {
        let path = format!("{parent_path}.files[{index}]");
        let Some(file) = as_object(file) else {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "source file pin must be an object",
                path,
                artifact_id,
            ));
            continue;
        };
        reject_unknown_fields(
            file,
            &["path", "sha256"],
            &path,
            diagnostics,
            artifact_id,
            PACK_CODE,
        );
        required_string(
            file,
            "path",
            format!("{path}.path"),
            diagnostics,
            artifact_id,
            PACK_CODE,
        );
        if let Some(hash) = required_string(
            file,
            "sha256",
            format!("{path}.sha256"),
            diagnostics,
            artifact_id,
            PACK_CODE,
        ) && !valid_sha256(&hash)
        {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "sha256 must be exactly 64 lowercase hexadecimal characters",
                format!("{path}.sha256"),
                artifact_id,
            ));
        }
    }
}

fn validate_terms(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> PackTerms {
    let Some(terms) = non_empty_array(value, "payload.terms", diagnostics, artifact_id, PACK_CODE)
    else {
        return PackTerms::default();
    };
    let mut statuses = BTreeMap::new();
    let mut kinds = BTreeMap::new();
    let mut parents = BTreeMap::new();
    let mut domains = Vec::new();
    let mut supersessions = Vec::new();

    for (index, term) in terms.iter().enumerate() {
        let path = format!("payload.terms[{index}]");
        let Some(term) = as_object(term) else {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "term must be an object",
                path,
                artifact_id,
            ));
            continue;
        };
        let TermDeclaration {
            term_id,
            term_kind,
            status,
            parent,
            superseded_by,
            domain,
        } = validate_term_declaration(term, &path, diagnostics, artifact_id);
        if let Some(kind) = term_kind.as_deref() {
            match &domain {
                Some(_) if SHAPED_KINDS.contains(&kind) => diagnostics.push(diagnostic(
                    PACK_CODE,
                    format!("a {kind} term has no domainTermId"),
                    format!("{path}.domainTermId"),
                    artifact_id,
                )),
                Some(domain) => domains.push((domain.clone(), path.clone())),
                None if DOMAIN_KINDS.contains(&kind) => diagnostics.push(diagnostic(
                    PACK_CODE,
                    format!("a {kind} term names the class it belongs to in domainTermId"),
                    format!("{path}.domainTermId"),
                    artifact_id,
                )),
                None => {}
            }
        }
        if let (Some(term_id), Some(term_kind)) = (term_id.as_ref(), term_kind) {
            kinds.insert(term_id.clone(), term_kind);
        }
        if let (Some(term_id), Some(status)) = (term_id, status) {
            if statuses.insert(term_id.clone(), status.clone()).is_some() {
                diagnostics.push(diagnostic(
                    PACK_CODE,
                    "termId must be unique within the pack",
                    format!("{path}.termId"),
                    artifact_id,
                ));
            }
            if let Some(parent) = parent {
                parents.insert(term_id.clone(), parent);
            }
            if status == "superseded" {
                match superseded_by {
                    Some(replacement) => supersessions.push((term_id, replacement, path.clone())),
                    None => diagnostics.push(diagnostic(
                        PACK_CODE,
                        "superseded terms require supersededBy",
                        format!("{path}.supersededBy"),
                        artifact_id,
                    )),
                }
            } else if superseded_by.is_some() {
                diagnostics.push(diagnostic(
                    PACK_CODE,
                    "only superseded terms may declare supersededBy",
                    format!("{path}.supersededBy"),
                    artifact_id,
                ));
            }
        }
    }

    validate_term_relationships(
        &statuses,
        &kinds,
        &parents,
        supersessions,
        diagnostics,
        artifact_id,
    );
    for (domain, path) in domains {
        if statuses.get(&domain).map(String::as_str) != Some("active")
            || kinds.get(&domain).map(String::as_str) != Some("class")
        {
            diagnostics.push(diagnostic(
                PACK_CODE,
                format!("domainTermId '{domain}' must resolve to an active class term in the pack"),
                format!("{path}.domainTermId"),
                artifact_id,
            ));
        }
    }

    PackTerms { statuses, kinds }
}

fn compatible_parent_kinds(child: &str, parent: &str) -> bool {
    matches!(
        (child, parent),
        ("class", "class")
            | ("property", "property")
            | ("action", "action")
            | ("predicate", "predicate")
            | ("value_object", "value_object" | "concept_scheme_root")
    )
}

fn validate_parent_cycles(
    parents: &BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    for start in parents.keys() {
        let mut visited = BTreeSet::new();
        let mut current = Some(start.as_str());
        while let Some(term_id) = current {
            if !visited.insert(term_id.to_string()) {
                diagnostics.push(diagnostic(
                    PACK_CODE,
                    format!("term parent graph contains a cycle through '{term_id}'"),
                    format!("payload.terms.{start}.parentTermId"),
                    artifact_id,
                ));
                break;
            }
            current = parents.get(term_id).map(String::as_str);
        }
    }
}

fn validate_mappings(
    value: Option<&JsonValue>,
    source_ids: &BTreeMap<String, Option<String>>,
    terms: &BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(mappings) = non_empty_array(
        value,
        "payload.mappings",
        diagnostics,
        artifact_id,
        PACK_CODE,
    ) else {
        return;
    };
    let mut state = MappingValidation::default();

    for (index, mapping) in mappings.iter().enumerate() {
        let path = format!("payload.mappings[{index}]");
        let Some(mapping) = as_object(mapping) else {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "mapping must be an object",
                path,
                artifact_id,
            ));
            continue;
        };
        validate_mapping_record(
            mapping,
            &path,
            source_ids,
            terms,
            &mut state,
            diagnostics,
            artifact_id,
        );
    }

    validate_mapping_successors(&state.records, diagnostics, artifact_id);
}

fn validate_shapes(
    value: Option<&JsonValue>,
    terms: &PackTerms,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(shapes) =
        non_empty_array(value, "payload.shapes", diagnostics, artifact_id, PACK_CODE)
    else {
        return;
    };
    let mut shape_ids = BTreeSet::new();
    let mut shaped_terms = BTreeSet::new();

    for (index, shape) in shapes.iter().enumerate() {
        let path = format!("payload.shapes[{index}]");
        let Some(shape) = as_object(shape) else {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "shape must be an object",
                path,
                artifact_id,
            ));
            continue;
        };
        reject_unknown_fields(
            shape,
            &["shapeId", "termId", "closed", "fields"],
            &path,
            diagnostics,
            artifact_id,
            PACK_CODE,
        );
        if let Some(shape_id) = required_string(
            shape,
            "shapeId",
            format!("{path}.shapeId"),
            diagnostics,
            artifact_id,
            PACK_CODE,
        ) && !shape_ids.insert(shape_id)
        {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "shapeId must be unique within the pack",
                format!("{path}.shapeId"),
                artifact_id,
            ));
        }
        validate_shape_term(
            shape,
            &path,
            &terms.statuses,
            &mut shaped_terms,
            diagnostics,
            artifact_id,
        );
        match shape.get("closed") {
            Some(JsonValue::Bool(true)) => {}
            Some(JsonValue::Bool(false)) => diagnostics.push(diagnostic(
                PACK_CODE,
                "ontology promotion shapes must be closed",
                format!("{path}.closed"),
                artifact_id,
            )),
            _ => diagnostics.push(diagnostic(
                PACK_CODE,
                "closed must be the boolean true",
                format!("{path}.closed"),
                artifact_id,
            )),
        }
        validate_shape_fields(shape.get("fields"), &path, diagnostics, artifact_id);
    }

    for (term_id, kind) in &terms.kinds {
        if !SHAPED_KINDS.contains(&kind.as_str()) && shaped_terms.contains(term_id) {
            diagnostics.push(diagnostic(
                PACK_CODE,
                format!("a {kind} term '{term_id}' takes no closed shape"),
                "payload.shapes",
                artifact_id,
            ));
        }
    }
    for (term_id, status) in &terms.statuses {
        let shaped_kind = terms
            .kinds
            .get(term_id)
            .is_none_or(|kind| SHAPED_KINDS.contains(&kind.as_str()));
        if status == "active" && shaped_kind && !shaped_terms.contains(term_id) {
            diagnostics.push(diagnostic(
                PACK_CODE,
                format!("active term '{term_id}' requires one closed shape"),
                "payload.shapes",
                artifact_id,
            ));
        }
    }
}

fn validate_shape_fields(
    value: Option<&JsonValue>,
    parent_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(fields) = non_empty_array(
        value,
        format!("{parent_path}.fields"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    ) else {
        return;
    };
    let mut field_ids = BTreeSet::new();
    for (index, field) in fields.iter().enumerate() {
        let path = format!("{parent_path}.fields[{index}]");
        let Some(field) = as_object(field) else {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "shape field must be an object",
                path,
                artifact_id,
            ));
            continue;
        };
        reject_unknown_fields(
            field,
            &[
                "fieldId",
                "valueType",
                "targetTermId",
                "minCount",
                "maxCount",
            ],
            &path,
            diagnostics,
            artifact_id,
            PACK_CODE,
        );
        if let Some(field_id) = required_string(
            field,
            "fieldId",
            format!("{path}.fieldId"),
            diagnostics,
            artifact_id,
            PACK_CODE,
        ) && !field_ids.insert(field_id)
        {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "fieldId must be unique within a shape",
                format!("{path}.fieldId"),
                artifact_id,
            ));
        }
        validate_shape_field_type(field, &path, diagnostics, artifact_id);
        let min_count = required_cardinality(
            field,
            "minCount",
            format!("{path}.minCount"),
            diagnostics,
            artifact_id,
        );
        let max_count = required_cardinality(
            field,
            "maxCount",
            format!("{path}.maxCount"),
            diagnostics,
            artifact_id,
        );
        if let (Some(min_count), Some(max_count)) = (min_count, max_count)
            && min_count > max_count
        {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "minCount cannot exceed maxCount",
                format!("{path}.minCount"),
                artifact_id,
            ));
        }
    }
}

fn validate_instance_provenance(value: Option<&JsonValue>, diagnostics: &mut Vec<Diagnostic>) {
    let Some(provenance) = value.and_then(as_object) else {
        diagnostics.push(diagnostic(
            INSTANCE_CODE,
            "instance.provenance must be an object",
            "instance.provenance",
            None,
        ));
        return;
    };
    reject_unknown_fields(
        provenance,
        &["sourceKind", "sourceRef", "observedAt", "evidenceRefs"],
        "instance.provenance",
        diagnostics,
        None,
        INSTANCE_CODE,
    );
    require_literal(
        provenance,
        "sourceKind",
        &["imported_claim", "norn_projection"],
        "instance.provenance.sourceKind",
        diagnostics,
        None,
        INSTANCE_CODE,
    );
    required_string(
        provenance,
        "sourceRef",
        "instance.provenance.sourceRef",
        diagnostics,
        None,
        INSTANCE_CODE,
    );
    if let Some(observed_at) = required_string(
        provenance,
        "observedAt",
        "instance.provenance.observedAt",
        diagnostics,
        None,
        INSTANCE_CODE,
    ) && !valid_utc_date_time(&observed_at)
    {
        diagnostics.push(diagnostic(
            INSTANCE_CODE,
            "observedAt must use the supported UTC RFC3339 subset",
            "instance.provenance.observedAt",
            None,
        ));
    }
    non_empty_string_array(
        provenance.get("evidenceRefs"),
        "instance.provenance.evidenceRefs",
        diagnostics,
        None,
        INSTANCE_CODE,
    );
}

fn find_shape<'a>(
    pack: &'a BTreeMap<String, JsonValue>,
    term_id: &str,
) -> Option<&'a BTreeMap<String, JsonValue>> {
    pack.get("shapes").and_then(as_array).and_then(|shapes| {
        shapes.iter().find_map(|shape| {
            let shape = as_object(shape)?;
            (string_field(shape, "termId") == Some(term_id)).then_some(shape)
        })
    })
}

fn find_term_status<'a>(pack: &'a BTreeMap<String, JsonValue>, term_id: &str) -> Option<&'a str> {
    pack.get("terms").and_then(as_array).and_then(|terms| {
        terms.iter().find_map(|term| {
            let term = as_object(term)?;
            (string_field(term, "termId") == Some(term_id))
                .then(|| string_field(term, "status"))
                .flatten()
        })
    })
}

fn validate_instance_values(
    shape: &BTreeMap<String, JsonValue>,
    values: &BTreeMap<String, JsonValue>,
    registry: &OntologyRegistryIndex,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(fields) = shape.get("fields").and_then(as_array) else {
        diagnostics.push(diagnostic(
            INSTANCE_CODE,
            "resolved shape has no fields",
            "packPayload.shapes",
            None,
        ));
        return;
    };
    let allowed: BTreeSet<String> = fields
        .iter()
        .filter_map(as_object)
        .filter_map(|field| string_field(field, "fieldId"))
        .map(str::to_string)
        .collect();
    for field_id in values.keys() {
        if !allowed.contains(field_id) {
            diagnostics.push(diagnostic(
                INSTANCE_CODE,
                format!("unknown field '{field_id}' is rejected by the closed shape"),
                format!("instance.values.{field_id}"),
                None,
            ));
        }
    }

    for field in fields.iter().filter_map(as_object) {
        let Some(field_id) = string_field(field, "fieldId") else {
            continue;
        };
        let value_type = string_field(field, "valueType").unwrap_or("");
        let min_count = cardinality_field(field, "minCount").unwrap_or(0.0);
        let max_count = cardinality_field(field, "maxCount").unwrap_or(0.0);
        let Some(encoded_values) = values.get(field_id) else {
            if min_count > 0.0 {
                diagnostics.push(diagnostic(
                    INSTANCE_CODE,
                    format!("field '{field_id}' violates its declared cardinality"),
                    format!("instance.values.{field_id}"),
                    None,
                ));
            }
            continue;
        };
        let Some(field_values) = as_array(encoded_values) else {
            diagnostics.push(diagnostic(
                INSTANCE_CODE,
                format!("field '{field_id}' must be represented as an array of values"),
                format!("instance.values.{field_id}"),
                None,
            ));
            continue;
        };
        let count = u32::try_from(field_values.len()).map_or(f64::INFINITY, f64::from);
        if count < min_count || count > max_count {
            diagnostics.push(diagnostic(
                INSTANCE_CODE,
                format!("field '{field_id}' violates its declared cardinality"),
                format!("instance.values.{field_id}"),
                None,
            ));
        }
        for (index, value) in field_values.iter().enumerate() {
            if !value_matches_type(value, value_type) {
                diagnostics.push(diagnostic(
                    INSTANCE_CODE,
                    format!("field '{field_id}' value does not match type '{value_type}'"),
                    format!("instance.values.{field_id}[{index}]"),
                    None,
                ));
                continue;
            }
            if value_type == "term_ref" {
                let target_term_id = string_field(field, "targetTermId").unwrap_or("");
                let referenced_term_id = match value {
                    JsonValue::String(value) => value.as_str(),
                    JsonValue::Null
                    | JsonValue::Bool(_)
                    | JsonValue::Number(_)
                    | JsonValue::Array(_)
                    | JsonValue::Object(_) => "",
                };
                if !registry.is_active_member_of(referenced_term_id, target_term_id) {
                    diagnostics.push(diagnostic(
                        INSTANCE_CODE,
                        format!(
                            "field '{field_id}' term reference must resolve to an active member of '{target_term_id}'"
                        ),
                        format!("instance.values.{field_id}[{index}]"),
                        None,
                    ));
                }
            }
        }
    }
}

fn value_matches_type(value: &JsonValue, value_type: &str) -> bool {
    match value_type {
        "string" | "category_ref" => {
            matches!(value, JsonValue::String(text) if has_non_whitespace_text(text))
        }
        "boolean" => matches!(value, JsonValue::Bool(_)),
        "integer" => matches!(
            value,
            JsonValue::Number(number)
                if number.is_finite()
                    && number.fract() == 0.0
                    && number.abs() <= 9_007_199_254_740_991.0
        ),
        "decimal" => matches!(value, JsonValue::Number(number) if number.is_finite()),
        "date_time" => matches!(value, JsonValue::String(text) if valid_utc_date_time(text)),
        "iri" => matches!(value, JsonValue::String(text) if is_absolute_web_iri(text)),
        "term_ref" => matches!(value, JsonValue::String(text) if valid_prefixed_id(text, "norn.")),
        "external_identifier" => as_object(value).is_some_and(valid_external_identifier),
        _ => false,
    }
}

fn valid_external_identifier(value: &BTreeMap<String, JsonValue>) -> bool {
    if value
        .keys()
        .any(|key| !matches!(key.as_str(), "scheme" | "value" | "issuer"))
    {
        return false;
    }
    string_field(value, "scheme").is_some_and(has_non_whitespace_text)
        && string_field(value, "value").is_some_and(has_non_whitespace_text)
        && value.get("issuer").is_none_or(
            |issuer| matches!(issuer, JsonValue::String(text) if has_non_whitespace_text(text)),
        )
}

fn required_cardinality(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> Option<f64> {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::Number(value))
            if value.is_finite() && value.fract() == 0.0 && *value >= 0.0 =>
        {
            Some(*value)
        }
        Some(_) => {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "cardinality must be a non-negative integer",
                path,
                artifact_id,
            ));
            None
        }
        None => {
            diagnostics.push(diagnostic(
                PACK_CODE,
                format!("{field} is required"),
                path,
                artifact_id,
            ));
            None
        }
    }
}

fn required_basis_points(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::Number(value))
            if value.is_finite() && value.fract() == 0.0 && (0.0..=10_000.0).contains(value) => {}
        Some(_) => {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "confidenceBasisPoints must be an integer from 0 through 10000",
                path,
                artifact_id,
            ));
        }
        None => {
            diagnostics.push(diagnostic(
                PACK_CODE,
                format!("{field} is required"),
                path,
                artifact_id,
            ));
        }
    }
}

fn cardinality_field(object: &BTreeMap<String, JsonValue>, field: &str) -> Option<f64> {
    match object.get(field) {
        Some(JsonValue::Number(value)) => Some(*value),
        _ => None,
    }
}

fn required_date_field(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let path = path.into();
    if let Some(value) = required_string(
        object,
        field,
        path.clone(),
        diagnostics,
        artifact_id,
        PACK_CODE,
    ) && !valid_iso_date(&value)
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "date must be a real Gregorian calendar date in YYYY-MM-DD form",
            path,
            artifact_id,
        ));
    }
}

fn valid_iso_date(value: &str) -> bool {
    if value.len() != 10 || !value.is_ascii() {
        return false;
    }
    let mut parts = value.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return false;
    }
    let Some(year) = parse_ascii_digits(year.as_bytes()) else {
        return false;
    };
    let Some(month) =
        parse_ascii_digits(month.as_bytes()).and_then(|value| u8::try_from(value).ok())
    else {
        return false;
    };
    let Some(day) = parse_ascii_digits(day.as_bytes()).and_then(|value| u8::try_from(value).ok())
    else {
        return false;
    };
    if year == 0 || !(1..=12).contains(&month) {
        return false;
    }
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days_in_month).contains(&day)
}

/// Accept the deliberately small timestamp subset shared with the TypeScript
/// checker: a real Gregorian date, UTC `Z`, whole seconds, and optionally one
/// to nine fractional-second digits. Offsets and leap seconds are rejected.
fn valid_utc_date_time(value: &str) -> bool {
    if !value.is_ascii() {
        return false;
    }
    let Some((date, time_with_zone)) = value.split_once('T') else {
        return false;
    };
    if !valid_iso_date(date) || time_with_zone.contains('T') {
        return false;
    }
    let Some(time) = time_with_zone.strip_suffix('Z') else {
        return false;
    };
    let (whole_seconds, fraction) = time
        .split_once('.')
        .map_or((time, None), |(whole, fraction)| (whole, Some(fraction)));
    if fraction.is_some_and(|fraction| {
        !(1..=9).contains(&fraction.len())
            || !fraction.chars().all(|character| character.is_ascii_digit())
    }) {
        return false;
    }
    let mut parts = whole_seconds.split(':');
    let (Some(hour), Some(minute), Some(second), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if hour.len() != 2 || minute.len() != 2 || second.len() != 2 {
        return false;
    }
    let Some(hour) = parse_ascii_digits(hour.as_bytes()).and_then(|value| u8::try_from(value).ok())
    else {
        return false;
    };
    let Some(minute) =
        parse_ascii_digits(minute.as_bytes()).and_then(|value| u8::try_from(value).ok())
    else {
        return false;
    };
    let Some(second) =
        parse_ascii_digits(second.as_bytes()).and_then(|value| u8::try_from(value).ok())
    else {
        return false;
    };
    hour <= 23 && minute <= 59 && second <= 59
}

fn parse_ascii_digits(bytes: &[u8]) -> Option<u16> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    bytes.iter().try_fold(0_u16, |value, digit| {
        value
            .checked_mul(10)?
            .checked_add(u16::from(digit.checked_sub(b'0')?))
    })
}

fn valid_immutable_release_id(value: &str) -> bool {
    if !value.is_ascii()
        || value.is_empty()
        || value
            .chars()
            .any(|character| character.is_ascii_whitespace() || character.is_ascii_control())
    {
        return false;
    }
    if valid_content_hash(value) || valid_semantic_version(value) || valid_w3c_release(value) {
        return true;
    }
    let Some((label, pin)) = value.rsplit_once('@') else {
        return false;
    };
    !label.is_empty()
        && label
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        && label.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
        && !label.ends_with(['.', '_', '-'])
        && (valid_content_hash(pin) || valid_iso_date(pin))
}

fn valid_content_hash(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
}

fn valid_semantic_version(value: &str) -> bool {
    let value = value.strip_prefix('v').unwrap_or(value);
    let part_count = value.split('.').count();
    if !value
        .split('.')
        .all(|part| !part.is_empty() && part.chars().all(|character| character.is_ascii_digit()))
    {
        return false;
    }
    (2..=3).contains(&part_count)
}

fn valid_w3c_release(value: &str) -> bool {
    let Some(date) = value.strip_prefix("w3c-rec-") else {
        return false;
    };
    if date.len() != 8 || !date.chars().all(|character| character.is_ascii_digit()) {
        return false;
    }
    let (Some(year), Some(month), Some(day)) = (date.get(0..4), date.get(4..6), date.get(6..8))
    else {
        return false;
    };
    let expanded = format!("{year}-{month}-{day}");
    valid_iso_date(&expanded)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
}

fn valid_prefixed_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && !value.ends_with(['.', '-'])
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '-')
        })
}

fn is_absolute_web_iri(value: &str) -> bool {
    let Some(remainder) = value.strip_prefix("https://") else {
        return false;
    };
    if remainder.is_empty()
        || remainder
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return false;
    }
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.contains('[') {
        return false;
    }
    let (host, port) = authority
        .rsplit_once(':')
        .map_or((authority, None), |(host, port)| (host, Some(port)));
    if port.is_some_and(|port| {
        port.is_empty()
            || !port.chars().all(|character| character.is_ascii_digit())
            || port.parse::<u16>().ok().is_none_or(|port| port == 0)
    }) {
        return false;
    }
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
}

fn reject_unknown_fields(
    object: &BTreeMap<String, JsonValue>,
    allowed: &[&str],
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    for key in object.keys() {
        if !allowed.iter().any(|allowed| key == allowed) {
            diagnostics.push(diagnostic(
                code,
                format!("unknown field '{key}' is not allowed"),
                format!("{path}.{key}"),
                artifact_id,
            ));
        }
    }
}

fn required_string(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) -> Option<String> {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::String(value)) if has_non_whitespace_text(value) => Some(value.clone()),
        Some(_) => {
            diagnostics.push(diagnostic(
                code,
                "value must be a non-empty string",
                path,
                artifact_id,
            ));
            None
        }
        None => {
            diagnostics.push(diagnostic(
                code,
                format!("{field} is required"),
                path,
                artifact_id,
            ));
            None
        }
    }
}

fn optional_string(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) -> Option<String> {
    let value = object.get(field)?;
    let path = path.into();
    match value {
        JsonValue::String(value) if has_non_whitespace_text(value) => Some(value.clone()),
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::String(_)
        | JsonValue::Array(_)
        | JsonValue::Object(_) => {
            diagnostics.push(diagnostic(
                code,
                "value must be a non-empty string",
                path,
                artifact_id,
            ));
            None
        }
    }
}

fn require_literal(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    allowed: &[&str],
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) -> Option<String> {
    let path = path.into();
    match object.get(field) {
        Some(JsonValue::String(value)) if allowed.iter().any(|allowed| value == allowed) => {
            Some(value.clone())
        }
        Some(_) => {
            diagnostics.push(diagnostic(
                code,
                format!("value must be one of: {}", allowed.join(", ")),
                path,
                artifact_id,
            ));
            None
        }
        None => {
            diagnostics.push(diagnostic(
                code,
                format!("{field} is required"),
                path,
                artifact_id,
            ));
            None
        }
    }
}

fn non_empty_array<'a>(
    value: Option<&'a JsonValue>,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) -> Option<&'a [JsonValue]> {
    let path = path.into();
    match value {
        Some(JsonValue::Array(items)) if !items.is_empty() => Some(items),
        Some(JsonValue::Array(_)) => {
            diagnostics.push(diagnostic(
                code,
                "array must not be empty",
                path,
                artifact_id,
            ));
            None
        }
        Some(_) => {
            diagnostics.push(diagnostic(
                code,
                "value must be an array",
                path,
                artifact_id,
            ));
            None
        }
        None => {
            diagnostics.push(diagnostic(code, "value is required", path, artifact_id));
            None
        }
    }
}

fn non_empty_string_array(
    value: Option<&JsonValue>,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let path = path.into();
    let Some(items) = non_empty_array(value, path.clone(), diagnostics, artifact_id, code) else {
        return;
    };
    for (index, item) in items.iter().enumerate() {
        if !matches!(item, JsonValue::String(value) if has_non_whitespace_text(value)) {
            diagnostics.push(diagnostic(
                code,
                "array items must be non-empty strings",
                format!("{path}[{index}]"),
                artifact_id,
            ));
        }
    }
}

fn as_object(value: &JsonValue) -> Option<&BTreeMap<String, JsonValue>> {
    match value {
        JsonValue::Object(object) => Some(object),
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::String(_)
        | JsonValue::Array(_) => None,
    }
}

fn as_array(value: &JsonValue) -> Option<&[JsonValue]> {
    match value {
        JsonValue::Array(items) => Some(items),
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::String(_)
        | JsonValue::Object(_) => None,
    }
}

fn string_field<'a>(object: &'a BTreeMap<String, JsonValue>, field: &str) -> Option<&'a str> {
    match object.get(field) {
        Some(JsonValue::String(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn has_non_whitespace_text(value: &str) -> bool {
    value.chars().any(|character| !character.is_whitespace())
}

fn diagnostic(
    code: &str,
    message: impl Into<String>,
    path: impl Into<String>,
    artifact_id: Option<&str>,
) -> Diagnostic {
    Diagnostic {
        code: code.to_string(),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        artifact_id: artifact_id.map(str::to_string),
        path: Some(path.into()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

fn validate_source_release(
    source: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if let Some(release_id) = required_string(
        source,
        "releaseId",
        format!("{path}.releaseId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    ) && !valid_immutable_release_id(&release_id)
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "releaseId must be a strict immutable version, dated-standard release, or content commit/hash pin",
            format!("{path}.releaseId"),
            artifact_id,
        ));
    }
    required_date_field(
        source,
        "releasedAt",
        format!("{path}.releasedAt"),
        diagnostics,
        artifact_id,
    );
    required_date_field(
        source,
        "retrievedAt",
        format!("{path}.retrievedAt"),
        diagnostics,
        artifact_id,
    );
    if let Some(url) = required_string(
        source,
        "canonicalUrl",
        format!("{path}.canonicalUrl"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    ) && !is_absolute_web_iri(&url)
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "canonicalUrl must be an absolute HTTPS URL with a non-empty host",
            format!("{path}.canonicalUrl"),
            artifact_id,
        ));
    }
}

fn validate_shape_term(
    shape: &BTreeMap<String, JsonValue>,
    path: &str,
    terms: &BTreeMap<String, String>,
    shaped_terms: &mut BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let term_id = required_string(
        shape,
        "termId",
        format!("{path}.termId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    if let Some(term_id) = term_id {
        match terms.get(&term_id).map(String::as_str) {
            None => diagnostics.push(diagnostic(
                PACK_CODE,
                format!("termId '{term_id}' does not resolve within the pack"),
                format!("{path}.termId"),
                artifact_id,
            )),
            Some("active") => {}
            Some(_) => diagnostics.push(diagnostic(
                PACK_CODE,
                "closed promotion shapes may target active terms only; historical shape mode is not supported",
                format!("{path}.termId"),
                artifact_id,
            )),
        }
        if !shaped_terms.insert(term_id) {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "each term may declare only one closed shape",
                format!("{path}.termId"),
                artifact_id,
            ));
        }
    }
}

fn validate_shape_field_type(
    field: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let value_type = require_literal(
        field,
        "valueType",
        &[
            "string",
            "boolean",
            "integer",
            "decimal",
            "date_time",
            "iri",
            "term_ref",
            "external_identifier",
            "category_ref",
        ],
        format!("{path}.valueType"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    let target_term_id = optional_string(
        field,
        "targetTermId",
        format!("{path}.targetTermId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    match (value_type.as_deref(), target_term_id.as_deref()) {
        (Some("term_ref"), Some(target)) if valid_prefixed_id(target, "norn.") => {}
        (Some("term_ref"), Some(_)) => diagnostics.push(diagnostic(
            PACK_CODE,
            "targetTermId must be an opaque lowercase Norn-owned norn.* identifier",
            format!("{path}.targetTermId"),
            artifact_id,
        )),
        (Some("term_ref"), None) => diagnostics.push(diagnostic(
            PACK_CODE,
            "term_ref fields require targetTermId for bounded membership checking",
            format!("{path}.targetTermId"),
            artifact_id,
        )),
        (Some(_), Some(_)) => diagnostics.push(diagnostic(
            PACK_CODE,
            "targetTermId is allowed only on term_ref fields",
            format!("{path}.targetTermId"),
            artifact_id,
        )),
        _ => {}
    }
}

struct TermDeclaration {
    term_id: Option<String>,
    term_kind: Option<String>,
    status: Option<String>,
    parent: Option<String>,
    superseded_by: Option<String>,
    domain: Option<String>,
}

fn validate_term_identity(
    term: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> Option<String> {
    reject_unknown_fields(
        term,
        &[
            "termId",
            "label",
            "definition",
            "termKind",
            "status",
            "semanticAuthority",
            "parentTermId",
            "supersededBy",
            "domainTermId",
        ],
        path,
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    let term_id = required_string(
        term,
        "termId",
        format!("{path}.termId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    if let Some(term_id) = term_id.as_deref()
        && !valid_prefixed_id(term_id, "norn.")
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "termId must be an opaque lowercase Norn-owned norn.* identifier",
            format!("{path}.termId"),
            artifact_id,
        ));
    }
    required_string(
        term,
        "label",
        format!("{path}.label"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    required_string(
        term,
        "definition",
        format!("{path}.definition"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    term_id
}

fn validate_term_declaration(
    term: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> TermDeclaration {
    let term_id = validate_term_identity(term, path, diagnostics, artifact_id);
    let term_kind = require_literal(
        term,
        "termKind",
        &TERM_KINDS,
        format!("{path}.termKind"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    let status = require_literal(
        term,
        "status",
        &["active", "deprecated", "superseded"],
        format!("{path}.status"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    require_literal(
        term,
        "semanticAuthority",
        &["semantic_only"],
        format!("{path}.semanticAuthority"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );

    let parent = optional_string(
        term,
        "parentTermId",
        format!("{path}.parentTermId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    let superseded_by = optional_string(
        term,
        "supersededBy",
        format!("{path}.supersededBy"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    let domain = optional_string(
        term,
        "domainTermId",
        format!("{path}.domainTermId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    TermDeclaration {
        term_id,
        term_kind,
        status,
        parent,
        superseded_by,
        domain,
    }
}

fn validate_term_relationships(
    statuses: &BTreeMap<String, String>,
    kinds: &BTreeMap<String, String>,
    parents: &BTreeMap<String, String>,
    supersessions: Vec<(String, String, String)>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    for (term_id, parent) in parents {
        if !statuses.contains_key(parent) {
            diagnostics.push(diagnostic(
                PACK_CODE,
                format!("parentTermId '{parent}' does not resolve within the pack"),
                format!("payload.terms.{term_id}.parentTermId"),
                artifact_id,
            ));
            continue;
        }
        if let (Some(child_kind), Some(parent_kind)) = (kinds.get(term_id), kinds.get(parent))
            && !compatible_parent_kinds(child_kind, parent_kind)
        {
            diagnostics.push(diagnostic(
                PACK_CODE,
                format!("termKind '{child_kind}' cannot be a child of termKind '{parent_kind}'"),
                format!("payload.terms.{term_id}.parentTermId"),
                artifact_id,
            ));
        }
        if statuses.get(term_id).map(String::as_str) == Some("active")
            && statuses.get(parent).map(String::as_str) != Some("active")
        {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "an active term requires an active parentTermId",
                format!("payload.terms.{term_id}.parentTermId"),
                artifact_id,
            ));
        }
    }
    validate_parent_cycles(parents, diagnostics, artifact_id);
    for (term_id, replacement, path) in supersessions {
        match statuses.get(&replacement).map(String::as_str) {
            Some("active") => {}
            _ => diagnostics.push(diagnostic(
                PACK_CODE,
                format!("supersededBy '{replacement}' must resolve to an active term"),
                format!("{path}.supersededBy"),
                artifact_id,
            )),
        }
        if term_id == replacement {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "a term cannot supersede itself",
                format!("{path}.supersededBy"),
                artifact_id,
            ));
        }
    }
}

#[derive(Default)]
struct MappingValidation {
    mapping_ids: BTreeSet<String>,
    exact_pairs: BTreeSet<(String, String)>,
    records: BTreeMap<String, MappingRecord>,
}
struct MappingRecord {
    term_id: Option<String>,
    source_id: Option<String>,
    superseded_by: Option<String>,
    path: String,
}
struct MappingFields {
    term_id: Option<String>,
    source_id: Option<String>,
    relation: Option<String>,
    superseded_by: Option<String>,
}

fn validate_mapping_identity(
    mapping: &BTreeMap<String, JsonValue>,
    path: &str,
    state: &mut MappingValidation,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> (Option<String>, bool) {
    reject_unknown_fields(
        mapping,
        &[
            "mappingId",
            "termId",
            "sourceId",
            "externalIri",
            "relation",
            "justification",
            "evidenceRefs",
            "confidenceBasisPoints",
            "reviewedBy",
            "reviewedAt",
            "supersededByMappingId",
            "ratifiedBy",
            "ratifiedAt",
        ],
        path,
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    let mapping_id = required_string(
        mapping,
        "mappingId",
        format!("{path}.mappingId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    let mapping_id_is_unique = if let Some(mapping_id) = mapping_id.as_deref() {
        if state.mapping_ids.insert(mapping_id.to_string()) {
            true
        } else {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "mappingId must be unique within the pack",
                format!("{path}.mappingId"),
                artifact_id,
            ));
            false
        }
    } else {
        false
    };
    (mapping_id, mapping_id_is_unique)
}

fn validate_mapping_references(
    mapping: &BTreeMap<String, JsonValue>,
    path: &str,
    source_ids: &BTreeMap<String, Option<String>>,
    terms: &BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> (Option<String>, Option<String>) {
    let term_id = required_string(
        mapping,
        "termId",
        format!("{path}.termId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    if let Some(term_id) = term_id.as_deref()
        && !terms.contains_key(term_id)
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            format!("termId '{term_id}' does not resolve within the pack"),
            format!("{path}.termId"),
            artifact_id,
        ));
    }
    let source_id = required_string(
        mapping,
        "sourceId",
        format!("{path}.sourceId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    if let Some(source_id) = source_id.as_deref() {
        match source_ids.get(source_id) {
            None => diagnostics.push(diagnostic(
                PACK_CODE,
                format!("sourceId '{source_id}' does not resolve within the pack"),
                format!("{path}.sourceId"),
                artifact_id,
            )),
            Some(Some(mode)) if mode == "study_only" => diagnostics.push(diagnostic(
                PACK_CODE,
                format!("sourceId '{source_id}' is study_only; a mapping may cite only import or map_only sources"),
                format!("{path}.sourceId"),
                artifact_id,
            )),
            Some(_) => {}
        }
    }
    if let Some(iri) = required_string(
        mapping,
        "externalIri",
        format!("{path}.externalIri"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    ) && !is_absolute_web_iri(&iri)
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "externalIri must be an absolute HTTPS IRI with a non-empty host",
            format!("{path}.externalIri"),
            artifact_id,
        ));
    }
    (term_id, source_id)
}

fn validate_mapping_review(
    mapping: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> Option<String> {
    let relation = require_literal(
        mapping,
        "relation",
        &[
            "exact_match",
            "close_match",
            "broad_match",
            "narrow_match",
            "related_match",
        ],
        format!("{path}.relation"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    required_string(
        mapping,
        "justification",
        format!("{path}.justification"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    non_empty_string_array(
        mapping.get("evidenceRefs"),
        format!("{path}.evidenceRefs"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    required_basis_points(
        mapping,
        "confidenceBasisPoints",
        format!("{path}.confidenceBasisPoints"),
        diagnostics,
        artifact_id,
    );
    required_string(
        mapping,
        "reviewedBy",
        format!("{path}.reviewedBy"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    if let Some(reviewed_at) = required_string(
        mapping,
        "reviewedAt",
        format!("{path}.reviewedAt"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    ) && !valid_utc_date_time(&reviewed_at)
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "reviewedAt must use the supported UTC RFC3339 subset",
            format!("{path}.reviewedAt"),
            artifact_id,
        ));
    }
    relation
}

fn validate_mapping_record(
    mapping: &BTreeMap<String, JsonValue>,
    path: &str,
    source_ids: &BTreeMap<String, Option<String>>,
    terms: &BTreeMap<String, String>,
    state: &mut MappingValidation,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let (mapping_id, unique) =
        validate_mapping_identity(mapping, path, state, diagnostics, artifact_id);
    let (term_id, source_id) =
        validate_mapping_references(mapping, path, source_ids, terms, diagnostics, artifact_id);
    let relation = validate_mapping_review(mapping, path, diagnostics, artifact_id);
    let superseded_by = optional_string(
        mapping,
        "supersededByMappingId",
        format!("{path}.supersededByMappingId"),
        diagnostics,
        artifact_id,
        PACK_CODE,
    );
    if unique && let Some(mapping_id) = mapping_id {
        state.records.insert(
            mapping_id,
            MappingRecord {
                term_id: term_id.clone(),
                source_id: source_id.clone(),
                superseded_by: superseded_by.clone(),
                path: path.to_owned(),
            },
        );
    }
    let fields = MappingFields {
        term_id,
        source_id,
        relation,
        superseded_by,
    };
    validate_exact_mapping(
        mapping,
        path,
        &fields,
        terms,
        &mut state.exact_pairs,
        diagnostics,
        artifact_id,
    );
}

fn validate_exact_mapping(
    mapping: &BTreeMap<String, JsonValue>,
    path: &str,
    fields: &MappingFields,
    terms: &BTreeMap<String, String>,
    exact_pairs: &mut BTreeSet<(String, String)>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let exact = fields.relation.as_deref() == Some("exact_match");
    // Required on exact mappings; optional elsewhere, but typed the same
    // when present (T1-IR-2).
    let mut ratify = |field: &str| {
        let field_path = format!("{path}.{field}");
        if exact {
            required_string(
                mapping,
                field,
                field_path,
                diagnostics,
                artifact_id,
                PACK_CODE,
            )
        } else {
            optional_string(
                mapping,
                field,
                field_path,
                diagnostics,
                artifact_id,
                PACK_CODE,
            )
        }
    };
    ratify("ratifiedBy");
    if let Some(ratified_at) = ratify("ratifiedAt")
        && !valid_utc_date_time(&ratified_at)
    {
        diagnostics.push(diagnostic(
            PACK_CODE,
            "ratifiedAt must use the supported UTC RFC3339 subset",
            format!("{path}.ratifiedAt"),
            artifact_id,
        ));
    }

    if exact {
        if let Some(term_id) = fields.term_id.as_deref()
            && terms.get(term_id).map(String::as_str) != Some("active")
        {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "exact mappings may target active Norn terms only",
                format!("{path}.termId"),
                artifact_id,
            ));
        }
        if fields.superseded_by.is_none()
            && let (Some(term_id), Some(source_id)) =
                (fields.term_id.clone(), fields.source_id.clone())
            && !exact_pairs.insert((term_id, source_id))
        {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "only one exact mapping is allowed per Norn term and source release",
                format!("{path}.relation"),
                artifact_id,
            ));
        }
    }
}

fn validate_mapping_successors(
    mapping_records: &BTreeMap<String, MappingRecord>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    for (
        mapping_id,
        MappingRecord {
            term_id,
            source_id,
            superseded_by,
            path,
        },
    ) in mapping_records
    {
        let Some(replacement_id) = superseded_by.as_deref() else {
            continue;
        };
        if replacement_id == mapping_id {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "a mapping cannot supersede itself",
                format!("{path}.supersededByMappingId"),
                artifact_id,
            ));
            continue;
        }
        let Some(MappingRecord {
            term_id: replacement_term,
            source_id: replacement_source,
            superseded_by: replacement_successor,
            ..
        }) = mapping_records.get(replacement_id)
        else {
            diagnostics.push(diagnostic(
                PACK_CODE,
                format!(
                    "supersededByMappingId '{replacement_id}' does not resolve within the pack"
                ),
                format!("{path}.supersededByMappingId"),
                artifact_id,
            ));
            continue;
        };
        if replacement_successor.is_some() {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "supersededByMappingId must resolve directly to an active mapping record",
                format!("{path}.supersededByMappingId"),
                artifact_id,
            ));
        }
        if replacement_term != term_id || replacement_source != source_id {
            diagnostics.push(diagnostic(
                PACK_CODE,
                "a replacement mapping must preserve the exact Norn term and pinned source identity",
                format!("{path}.supersededByMappingId"),
                artifact_id,
            ));
        }
    }
}
