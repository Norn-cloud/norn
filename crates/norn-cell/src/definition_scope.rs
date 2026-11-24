//! Shared cell-scope validation for definition install, check, migration, and restore (#1376).

use std::collections::BTreeSet;

use norn_checker::EffectiveCatalog;
use norn_checker::entity_scopes::check_entity_reference_scopes_all;
use norn_ir::{ArtifactEnvelope, ScopeRef};

use crate::engine::{SqlEngine, params};
use crate::error::CellError;
use crate::model::Model;
use crate::store::{CellStore, InstalledDefinitionInput, InstalledDefinitionSet};
use crate::system::META;

fn refused(reason: String) -> CellError {
    CellError::DefinitionMigrationRefused(reason)
}

fn unescape_part(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    let mut rest = part;
    while !rest.is_empty() {
        if let Some(after) = rest
            .strip_prefix("%2F")
            .or_else(|| rest.strip_prefix("%2f"))
        {
            out.push('/');
            rest = after;
        } else if let Some(after) = rest.strip_prefix("%25") {
            out.push('%');
            rest = after;
        } else if let Some(ch) = rest.chars().next() {
            out.push(ch);
            rest = rest.get(ch.len_utf8()..).unwrap_or("");
        } else {
            break;
        }
    }
    out
}

/// Parse a cell scope key (`{escapedOrgId}/{escapedEnvironmentId}`) into its
/// [`ScopeRef`]. Unrecognized or unbound scope keys fail closed to the empty
/// global [`ScopeRef`], admitting only globally scoped artifacts.
#[must_use]
pub(crate) fn parse_cell_scope(raw: &str) -> ScopeRef {
    let mut parts = raw.split('/');
    let (Some(org), Some(env), None) = (parts.next(), parts.next(), parts.next()) else {
        return ScopeRef::default();
    };
    let org = unescape_part(org);
    let env = unescape_part(env);
    if org.trim().is_empty() || env.trim().is_empty() {
        return ScopeRef::default();
    }
    ScopeRef {
        org_id: Some(org),
        environment_id: Some(env),
        ..ScopeRef::default()
    }
}

/// Resolve the cell's authoritative [`ScopeRef`] from `explicit_scope` and any
/// persisted `binding:scope` metadata in `engine`.
pub(crate) fn effective_cell_scope<E: SqlEngine>(
    engine: &E,
    explicit_scope: Option<&str>,
) -> Result<Option<ScopeRef>, CellError> {
    let bound = engine.query_optional(
        &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
        params!["binding:scope"],
        |row| row.get::<String>(0),
    )?;
    if let (Some(explicit), Some(bound_val)) = (explicit_scope, bound.as_deref())
        && explicit != bound_val
    {
        return Err(refused(format!(
            "definition install scope {explicit:?} does not match bound cell scope {bound_val:?}"
        )));
    }
    Ok(explicit_scope.or(bound.as_deref()).map(parse_cell_scope))
}

fn decode_artifacts(canonical_set: &str, label: &str) -> Result<Vec<ArtifactEnvelope>, CellError> {
    let values: Vec<serde_json::Value> = serde_json::from_str(canonical_set).map_err(|error| {
        refused(format!(
            "{label} definition set is not a JSON artifact list: {error}"
        ))
    })?;
    values
        .iter()
        .map(|value| {
            norn_checker::decode_artifact_json(value).map_err(|findings| {
                refused(format!(
                    "{label} definition artifact could not be decoded: {findings:?}"
                ))
            })
        })
        .collect()
}

/// Validate that every artifact in `canonical_set` is compatible with
/// `cell_scope` (when bound or supplied) and that no cross-scope entity
/// references exist, then build the scope-validated [`EffectiveCatalog`].
pub(crate) fn validated_effective_catalog(
    canonical_set: &str,
    label: &str,
    cell_scope: Option<&ScopeRef>,
) -> Result<EffectiveCatalog, CellError> {
    let artifacts = decode_artifacts(canonical_set, label)?;
    if let Some(scope) = cell_scope {
        for artifact in &artifacts {
            if let Some((dim, _, _)) = norn_checker::scope_conflict(scope, &artifact.scope) {
                return Err(refused(format!(
                    "{label} artifact {:?} is scoped outside the cell scope ({dim})",
                    artifact.artifact_id
                )));
            }
        }
    }
    let scope_findings = check_entity_reference_scopes_all(&artifacts);
    if !scope_findings.is_empty() {
        return Err(refused(format!(
            "{label} definition set contains cross-scope entity references: {scope_findings:?}"
        )));
    }
    let catalog = match cell_scope {
        Some(scope) => EffectiveCatalog::for_scope(&artifacts, &BTreeSet::new(), scope),
        None => EffectiveCatalog::from_artifacts(&artifacts),
    };
    for artifact in &artifacts {
        let value = serde_json::to_value(artifact).map_err(CellError::Json)?;
        let Some(payload) = value.get("payload") else {
            continue;
        };
        let Some(type_name) = payload
            .get("objectType")
            .and_then(|object_type| object_type.get("name"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                payload
                    .get("objectKind")
                    .and_then(serde_json::Value::as_str)
            })
        else {
            continue;
        };
        match catalog.object_type_named(type_name) {
            Some(Ok(_)) => {}
            Some(Err(finding)) => {
                return Err(refused(format!(
                    "{label} effective contract for `{type_name}` is unavailable: {finding:?}"
                )));
            }
            None => {
                return Err(refused(format!(
                    "{label} effective contract for `{type_name}` is unavailable"
                )));
            }
        }
    }
    Ok(catalog)
}

/// Resolve the cell scope and validate `canonical_set` against it before
/// definition migration or install.
pub(crate) fn preflight_candidate<E: SqlEngine>(
    engine: &E,
    canonical_set: &str,
    cell_scope: Option<&str>,
) -> Result<Option<ScopeRef>, CellError> {
    let scope_ref = effective_cell_scope(engine, cell_scope)?;
    let _ = validated_effective_catalog(canonical_set, "candidate", scope_ref.as_ref())?;
    Ok(scope_ref)
}

impl<E: SqlEngine> CellStore<E> {
    /// Check a definition candidate against the current version and live
    /// values, returning the exact version/layout preview installation would
    /// retain. This performs SQL reads only and does not enter a transaction.
    ///
    /// # Errors
    /// A version conflict, unsafe live-data migration, or store error.
    pub fn check_definitions(
        &self,
        candidate_model: &Model,
        input: &InstalledDefinitionInput,
        expected_version: Option<u64>,
    ) -> Result<InstalledDefinitionSet, CellError> {
        self.check_definitions_inner(candidate_model, input, expected_version, None)
    }

    /// Like [`Self::check_definitions`], additionally checking every artifact
    /// in `input.canonical_set` against the explicit cell `scope`.
    ///
    /// # Errors
    /// A version conflict, scope violation, unsafe live-data migration, or store error.
    pub fn check_definitions_in_scope(
        &self,
        candidate_model: &Model,
        input: &InstalledDefinitionInput,
        expected_version: Option<u64>,
        scope: &str,
    ) -> Result<InstalledDefinitionSet, CellError> {
        self.check_definitions_inner(candidate_model, input, expected_version, Some(scope))
    }
}
