//! Shared cell-scope validation for definition install, check, migration, and restore (#1376).

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::EffectiveCatalog;
use norn_checker::entity_scopes::{ScopedView, check_entity_reference_scopes_all};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, EntityRoot, ObjectTypeDef, ObjectTypeName, Property,
    PropertyConstraints, PropertyId, PropertyType, ResourceStatus, ScopeRef,
};

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

fn bound_scope<E: SqlEngine>(engine: &E) -> Result<Option<String>, CellError> {
    engine.query_optional(
        &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
        params!["binding:scope"],
        |row| row.get::<String>(0),
    )
}

/// The cell's authoritative [`ScopeRef`]: `explicit_scope`, which must agree
/// with any persisted `binding:scope`, or that binding. A cell bound to no
/// scope has no consumer to check definitions for, so it is refused.
pub(crate) fn effective_cell_scope<E: SqlEngine>(
    engine: &E,
    explicit_scope: Option<&str>,
) -> Result<ScopeRef, CellError> {
    let bound = bound_scope(engine)?;
    if let (Some(explicit), Some(bound_val)) = (explicit_scope, bound.as_deref())
        && explicit != bound_val
    {
        return Err(refused(format!(
            "definition install scope {explicit:?} does not match bound cell scope {bound_val:?}"
        )));
    }
    explicit_scope
        .or(bound.as_deref())
        .map(parse_cell_scope)
        .ok_or_else(|| {
            refused("definitions are checked only for a cell bound to a scope".to_owned())
        })
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

/// Validate that every artifact in `canonical_set` lies in `cell_scope` and
/// that no cross-scope entity references exist, then build the cell's
/// [`EffectiveCatalog`] through its bound view.
pub(crate) fn validated_effective_catalog(
    canonical_set: &str,
    label: &str,
    cell_scope: &ScopeRef,
) -> Result<EffectiveCatalog, CellError> {
    let artifacts = scoped_artifacts(canonical_set, label, cell_scope)?;
    let catalog = ScopedView::bound(cell_scope, &artifacts, &[], &BTreeSet::new()).into_catalog();
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

fn scoped_artifacts(
    canonical_set: &str,
    label: &str,
    cell_scope: &ScopeRef,
) -> Result<Vec<ArtifactEnvelope>, CellError> {
    let artifacts = decode_artifacts(canonical_set, label)?;
    for artifact in &artifacts {
        if let Some((dim, _, _)) = norn_checker::scope_conflict(cell_scope, &artifact.scope) {
            return Err(refused(format!(
                "{label} artifact {:?} is scoped outside the cell scope ({dim})",
                artifact.artifact_id
            )));
        }
    }
    let scope_findings = check_entity_reference_scopes_all(&artifacts);
    if !scope_findings.is_empty() {
        return Err(refused(format!(
            "{label} definition set contains cross-scope entity references: {scope_findings:?}"
        )));
    }
    Ok(artifacts)
}

/// Resolve the cell scope, validate `canonical_set` against it, and require
/// `model` to be exactly the projection of that checked set, before
/// definition migration, install or restore.
pub(crate) fn preflight_candidate<E: SqlEngine>(
    engine: &E,
    canonical_set: &str,
    model: &Model,
    cell_scope: Option<&str>,
) -> Result<ScopeRef, CellError> {
    let scope = effective_cell_scope(engine, cell_scope)?;
    bind_model(
        model,
        &scoped_artifacts(canonical_set, "candidate", &scope)?,
    )?;
    Ok(scope)
}

/// Refuse `active` definitions recorded for another model than `model`.
pub(crate) fn check_active_model(
    model: &Model,
    active: &InstalledDefinitionSet,
) -> Result<(), CellError> {
    if active.model_hash != model.model_hash() || active.ddl_hash != model.ddl_hash() {
        return Err(CellError::SchemaMismatch {
            expected: format!("{} {}", model.ddl_hash(), model.model_hash()),
            found: format!("{} {}", active.ddl_hash, active.model_hash),
        });
    }
    Ok(())
}

/// Refuse to serve the `active` definitions unless `model` is the model they
/// recorded, the cell is bound to a scope they lie in, and `model` is their
/// projection (reopen and replayed provisioning, once history verifies).
pub(crate) fn verify_active<E: SqlEngine>(
    engine: &E,
    model: &Model,
    active: &InstalledDefinitionSet,
) -> Result<(), CellError> {
    check_active_model(model, active)?;
    let scope = effective_cell_scope(engine, None)?;
    bind_model(
        model,
        &scoped_artifacts(&active.canonical_set, "active", &scope)?,
    )
}

/// Bind the cell to `scope` inside the transaction that first activates
/// definitions for it, so an activated cell is never served under another.
pub(crate) fn bind_cell_scope<E: SqlEngine>(engine: &E, scope: &str) -> Result<(), CellError> {
    if bound_scope(engine)?.is_none() {
        engine.execute(
            &format!("INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2)"),
            params!["binding:scope", scope],
        )?;
    }
    Ok(())
}

/// Whether `name` is reserved for a kernel record type, compared as SQLite
/// compares table names: no tenant object type or interface may use it.
#[must_use]
pub fn is_reserved_kernel_name(name: &str) -> bool {
    [norn_kernel::ACTION_CALL_TYPE, norn_kernel::OWNED_ROWS_TYPE]
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}

/// The kernel's own record types a host adds beside a tenant's definitions:
/// action-call receipts and owned-row manifests. A cell model may carry them
/// only exactly as defined here.
#[must_use]
pub fn kernel_record_types() -> Vec<ObjectTypeDef> {
    let action_call = (
        norn_kernel::ACTION_CALL_TYPE,
        &[
            "actionId",
            "actionArtifactId",
            "actionVersion",
            "effectKind",
            "actor",
            "turn",
            "transition",
            "from",
            "to",
            "ruleBinding",
        ][..],
        &[
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "parameterPins",
            "rowOperations",
            "trace",
        ][..],
    );
    let owned_rows = (
        norn_kernel::OWNED_ROWS_TYPE,
        &["parentType", "parentKey", "collectionId"][..],
        &["rowIds"][..],
    );
    [action_call, owned_rows]
        .into_iter()
        .map(|(name, text, json)| {
            let typed = text
                .iter()
                .map(|id| (*id, PropertyType::Text { format: None }))
                .chain(
                    json.iter()
                        .map(|id| (*id, PropertyType::JsonExt { schema: None })),
                );
            ObjectTypeDef {
                immutable: false,
                name: ObjectTypeName(name.into()),
                role: None,
                description: None,
                status: ResourceStatus::default(),
                extends: None,
                implements: vec![],
                properties: typed
                    .map(|(id, property_type)| nullable(id, property_type))
                    .collect(),
                links: vec![],
                owned_collections: vec![],
                identity: vec![],
                invariants: vec![],
                lifecycles: vec![],
                markings: vec![],
                accounting_leg: None,
            }
        })
        .collect()
}

fn nullable(id: &str, property_type: PropertyType) -> Property {
    Property {
        protected: false,
        id: PropertyId(id.into()),
        property_type,
        description: None,
        constraints: PropertyConstraints {
            nullable: true,
            ..PropertyConstraints::default()
        },
        default: None,
        markings: vec![],
        computed: None,
    }
}

/// Refuse a `model` whose definitions are not those `artifacts` declare: the
/// same object types and interfaces, besides only the exact
/// [`kernel_record_types`] a host adds, and only shared property types the
/// set declares identically. As at provisioning, a set may declare neither a
/// reserved kernel name nor an object type extension, which a cell model
/// does not project.
fn bind_model(model: &Model, artifacts: &[ArtifactEnvelope]) -> Result<(), CellError> {
    let (mut objects, mut interfaces, mut properties) =
        (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root = serde_json::to_value(artifact.payload.to_json_value())
            .and_then(serde_json::from_value::<EntityRoot>)
            .map_err(CellError::Json)?;
        let declared = match &root {
            EntityRoot::V2(root) => Some(root.object_type.name.as_str()),
            EntityRoot::V2Interface(root) => Some(root.interface.name.as_str()),
            EntityRoot::V1(root) => Some(root.object_kind.as_str()),
            EntityRoot::V2PropertyTypes(_)
            | EntityRoot::V2Extension(_)
            | EntityRoot::V2Markings(_) => None,
        };
        if let Some(name) = declared.filter(|name| is_reserved_kernel_name(name)) {
            return Err(refused(format!(
                "{:?} declares {name:?}, a name reserved for kernel records",
                artifact.artifact_id
            )));
        }
        match root {
            EntityRoot::V2(root) => {
                objects.insert(root.object_type.name.0.clone(), root.object_type);
            }
            EntityRoot::V2Interface(root) => {
                interfaces.insert(root.interface.name.0.clone(), root.interface);
            }
            EntityRoot::V2PropertyTypes(root) => properties.extend(
                root.property_types
                    .into_iter()
                    .map(|def| (def.name.0.clone(), def)),
            ),
            EntityRoot::V2Extension(_) => {
                return Err(refused(format!(
                    "{:?} extends an object type, which a cell model does not project",
                    artifact.artifact_id
                )));
            }
            EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {}
        }
    }
    let kernel = kernel_record_types();
    let modelled: BTreeMap<_, _> = model
        .object_types()
        .filter(|def| !kernel.contains(def))
        .map(|def| (def.name.0.clone(), def.clone()))
        .collect();
    let model_interfaces: BTreeMap<_, _> = model
        .interfaces()
        .map(|def| (def.name.0.clone(), def.clone()))
        .collect();
    let foreign_property = model
        .property_types()
        .iter()
        .any(|def| properties.get(def.name.as_str()) != Some(def));
    if modelled != objects || model_interfaces != interfaces || foreign_property {
        return Err(CellError::DefinitionReceiptInvalid(
            "the candidate model is not the projection of its checked definition set".to_owned(),
        ));
    }
    Ok(())
}

impl<E: SqlEngine> CellStore<E> {
    /// Check a definition candidate against the current version and live
    /// values, under the cell's bound scope, returning the exact version/layout
    /// preview installation would retain. This performs SQL reads only and
    /// does not enter a transaction.
    ///
    /// # Errors
    /// A version conflict, a cell bound to no scope, a model that is not the
    /// projection of its definition set, unsafe live-data migration, or store error.
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
