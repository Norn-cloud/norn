//! Additive model migration and fail-closed live-data checks for a definition
//! install. The shared preflight uses reads only; the commit applies its
//! already checked plan inside the definition transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use norn_checker::{
    EffectiveCatalog, EffectiveKind, EffectiveLink, EffectiveObjectType, EffectiveProperty,
    decode_artifact_json,
};
use norn_ir::{
    ArtifactEnvelope, ConstraintClass, InterfaceDef, Invariant, ObjectTypeDef, ObjectTypeName,
    PropertyType, RuntimeValue,
};
use norn_kernel::{
    FactKey, InvariantEvaluationError, OWNED_ROWS_TYPE, TypeCatalog, evaluate_invariants,
    invariant_evaluation_budget,
};
use norn_projections::sqlite_ddl::{
    ID_COLUMN, OVERFLOW_COLUMN, PROPERTY_COLUMN_PREFIX, SCOPE_COLUMN, SOURCE_COLUMN, TARGET_COLUMN,
    TYPE_COLUMN, WORLD_COLUMN, quote_identifier,
};
use norn_projections::{PropertyColumn, ValueEncoding, sqlite_ddl::LinkTable};

use crate::codec::{canonical_json, encode};
use crate::engine::{SqlEngine, SqlRow, SqlValue, params};
use crate::error::{
    CellError, DefinitionInvariantBudget, DefinitionInvariantFinding, DefinitionInvariantObject,
};
use crate::model::Model;
use crate::read::StoreReader;
use crate::system::{LINK_VERSIONS, META};

const CONTRACT_SCAN_PAGE_ROWS: usize = 32;
const CONTRACT_SCAN_ROW_LIMIT: usize = 4_096;
const MIGRATION_SCAN_BYTE_LIMIT: u64 = 8 * 1_024 * 1_024;
const MIGRATION_SCAN_ROW_OVERHEAD_BYTES: u64 = 64;
const SQLITE_ROWID_ALIASES: [&str; 3] = ["rowid", "_rowid_", "oid"];
/// `_norn_meta` key recording that property columns carry
/// [`PROPERTY_COLUMN_PREFIX`] names.
const PROPERTY_COLUMNS_KEY: &str = "property_columns";

/// Rename the raw-id property columns of an older file to their physical
/// `p:` names, once, inside the open transaction; then record the layout.
///
/// A table is renamed only when every property column of `model` is present
/// under its raw id and no physical name is taken; any other layout is left
/// for the projection statements and the DDL-hash check to refuse.
/// `RENAME COLUMN` rewrites every index over the column in place, and index
/// names embed property ids rather than column names, so data, keys and
/// binding indexes survive unchanged. Longer ids go first, so a raw id that
/// is itself `p:<id>` vacates that name before property `<id>` takes it.
/// Logical DDL, hashes and exports name properties by id and do not change.
pub(crate) fn upgrade_raw_property_columns<E: SqlEngine>(
    engine: &E,
    model: &Model,
) -> Result<(), CellError> {
    let meta = |key: &str| {
        engine.query_optional(
            &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
            params![key],
            |row| row.get::<String>(0),
        )
    };
    match meta(PROPERTY_COLUMNS_KEY)? {
        Some(layout) if layout == PROPERTY_COLUMN_PREFIX => return Ok(()),
        Some(layout) => {
            return Err(CellError::SchemaMismatch {
                expected: format!("{PROPERTY_COLUMNS_KEY} {PROPERTY_COLUMN_PREFIX}"),
                found: format!("{PROPERTY_COLUMNS_KEY} {layout}"),
            });
        }
        None => {}
    }
    // A fresh file has no format yet and creates its tables with `p:` names.
    if meta("format")?.is_some() {
        let schema = model.schema();
        let tables = schema
            .object_tables
            .iter()
            .map(|table| (&table.table, &table.columns))
            .chain(
                schema
                    .link_tables
                    .iter()
                    .map(|table| (&table.table, &table.columns)),
            );
        for (table, columns) in tables {
            let mut renames: Vec<(&str, &str)> = columns
                .iter()
                .filter_map(|column| {
                    let physical = column.column.as_deref()?;
                    Some((column.property.as_str(), physical))
                })
                .collect();
            if renames.is_empty() {
                continue;
            }
            let present: BTreeSet<String> = engine
                .query(
                    "SELECT \"name\" FROM pragma_table_info(?1)",
                    params![table.as_str()],
                )?
                .into_iter()
                .map(|row| SqlRow(row).get::<String>(0))
                .collect::<Result<_, _>>()?;
            let raw = renames
                .iter()
                .filter(|(raw, _)| present.contains(*raw))
                .count();
            let generated = renames
                .iter()
                .filter(|(_, physical)| present.contains(*physical))
                .count();
            // Every property column of a table is raw, or every one is generated, or the
            // table does not exist yet. Anything else is refused before the marker is
            // written, and the open transaction rolls back any rename already made.
            match (raw, generated) {
                (0, 0) if present.is_empty() => continue,
                (0, all) if all == renames.len() => continue,
                (all, 0) if all == renames.len() => {}
                _ => {
                    return Err(CellError::SchemaMismatch {
                        expected: format!(
                            "{table}: all {} property columns raw or all `{PROPERTY_COLUMN_PREFIX}`",
                            renames.len()
                        ),
                        found: format!("{raw} raw and {generated} `{PROPERTY_COLUMN_PREFIX}`"),
                    });
                }
            }
            renames.sort_by_key(|(raw, _)| std::cmp::Reverse(raw.len()));
            for (raw, physical) in renames {
                engine.execute_batch(&format!(
                    "ALTER TABLE {} RENAME COLUMN {} TO {}",
                    quote_identifier(table),
                    quote_identifier(raw),
                    quote_identifier(physical)
                ))?;
            }
        }
    }
    engine.execute(
        &format!("INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2)"),
        params![PROPERTY_COLUMNS_KEY, PROPERTY_COLUMN_PREFIX],
    )
}

pub(crate) struct MigrationPlan {
    candidate_catalog: EffectiveCatalog,
}

/// Prove the candidate against the live store without issuing any SQL writes.
/// Both check-only and install call this exact proof on the current version.
pub(crate) fn preflight_model_migration<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    previous_canonical_set: Option<&str>,
    candidate_canonical_set: &str,
) -> Result<MigrationPlan, CellError> {
    check_removed_types(engine, previous, candidate)?;
    check_removed_links(engine, previous, candidate)?;
    check_removed_properties(engine, previous, candidate)?;
    check_effective_storage_owners(previous, candidate)?;
    check_compatible_layouts(previous, candidate)?;
    let candidate_catalog = effective_catalog(candidate_canonical_set, "candidate")?;
    let previous_catalog = previous_canonical_set
        .map(|canonical_set| effective_catalog(canonical_set, "installed"))
        .transpose()?;
    check_owned_collection_changes(
        engine,
        previous,
        candidate,
        previous_catalog.as_ref(),
        &candidate_catalog,
    )?;
    if let Some(previous_catalog) = &previous_catalog {
        check_tightened_contract_values(
            engine,
            previous,
            candidate,
            previous_catalog,
            &candidate_catalog,
        )?;
    } else {
        check_candidate_live_values(engine, previous, candidate, &candidate_catalog)?;
    }

    check_new_acyclic_roles(
        engine,
        previous,
        candidate,
        previous_catalog.as_ref(),
        &candidate_catalog,
    )?;
    reconcile_indexes(engine, previous, candidate, MigrationMode::Check)?;
    reconcile_object_columns(
        engine,
        previous,
        candidate,
        &candidate_catalog,
        MigrationMode::Check,
    )?;
    reconcile_link_columns(
        engine,
        previous,
        candidate,
        &candidate_catalog,
        MigrationMode::Check,
    )?;
    check_added_unique_indexes(engine, previous, candidate, &candidate_catalog)?;
    validate_candidate_invariants(
        engine,
        previous,
        candidate,
        previous_catalog.as_ref(),
        &candidate_catalog,
    )?;
    Ok(MigrationPlan { candidate_catalog })
}

/// Apply the projected schema delta after the shared preflight admitted it.
/// The caller owns the surrounding transaction.
pub(crate) fn apply_model_migration<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    plan: &MigrationPlan,
) -> Result<(), CellError> {
    // Drop empty link structures before object tables they reference.
    for table in &previous.schema().link_tables {
        if candidate
            .schema()
            .link_table(table.source.as_str(), table.link.as_str())
            .is_none()
        {
            engine.execute_batch(&format!(
                "DROP TABLE IF EXISTS {}",
                quote_identifier(&table.table)
            ))?;
        }
    }
    for table in previous.schema().object_tables.iter().rev() {
        if candidate
            .schema()
            .object_table(table.object_type.as_str())
            .is_none()
        {
            engine.execute_batch(&format!(
                "DROP TABLE IF EXISTS {}",
                quote_identifier(&table.table)
            ))?;
        }
    }

    // Tables are emitted in dependency order. Existing names are no-ops;
    // new tables are created with the candidate projection in full.
    for table in &candidate.schema().object_tables {
        if previous
            .schema()
            .object_table(table.object_type.as_str())
            .is_none()
            && let Some(statement) = table.statements.first()
        {
            engine.execute_batch(statement)?;
        }
    }
    for table in &candidate.schema().link_tables {
        if previous
            .schema()
            .link_table(table.source.as_str(), table.link.as_str())
            .is_none()
            && let Some(statement) = table.statements.first()
        {
            engine.execute_batch(statement)?;
        }
    }

    ensure_added_overflow_columns(engine, previous, candidate)?;
    reconcile_indexes(engine, previous, candidate, MigrationMode::Apply)?;
    reconcile_object_columns(
        engine,
        previous,
        candidate,
        &plan.candidate_catalog,
        MigrationMode::Apply,
    )?;
    reconcile_link_columns(
        engine,
        previous,
        candidate,
        &plan.candidate_catalog,
        MigrationMode::Apply,
    )?;

    // New indexes are last so an index may refer to an added column.
    for table in &candidate.schema().object_tables {
        for statement in table.statements.iter().skip(1) {
            engine.execute_batch(statement)?;
        }
    }
    for table in &candidate.schema().link_tables {
        for statement in table.statements.iter().skip(1) {
            engine.execute_batch(statement)?;
        }
    }
    drop_removed_overflow_columns(engine, previous, candidate)?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MigrationMode {
    Check,
    Apply,
}

const DEFINITION_INVARIANT_FINDING_LIMIT: usize = 32;

/// Validate live objects affected by a changed invariant or effective stored
/// property contract. Candidate defaults are overlaid in memory, matching
/// what the later additive DDL and overflow backfill will materialize.
#[allow(
    clippy::too_many_lines,
    reason = "the bounded invariant proof remains inside the migration transaction"
)]
fn validate_candidate_invariants<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    previous_catalog: Option<&EffectiveCatalog>,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    let declared_catalog = TypeCatalog::new(candidate.object_types())
        .with_interfaces(candidate.interfaces())
        .with_property_types(candidate.property_types());
    let mut integrity_object_types = candidate.object_types().cloned().collect::<Vec<_>>();
    for object_type in &mut integrity_object_types {
        object_type
            .invariants
            .retain(|invariant| invariant.classification == ConstraintClass::Integrity);
    }
    let mut integrity_interfaces = candidate.interfaces().cloned().collect::<Vec<_>>();
    for interface in &mut integrity_interfaces {
        interface
            .invariants
            .retain(|invariant| invariant.classification == ConstraintClass::Integrity);
    }
    let integrity_catalog = TypeCatalog::new(&integrity_object_types)
        .with_interfaces(&integrity_interfaces)
        .with_property_types(candidate.property_types());
    let mut affected = BTreeMap::<String, BTreeSet<String>>::new();

    for object_type in candidate.object_types() {
        let name = object_type.name.as_str();
        // A newly admitted type has no live objects under the prior model,
        // and its projected table does not exist during read-only preflight.
        if !previous.has_type(name) {
            continue;
        }
        let typed_name = ObjectTypeName(name.to_owned());
        let has_invariants = declared_catalog
            .has_invariants(&typed_name)
            .map_err(|missing| {
                refused(format!(
                    "candidate invariant catalog for `{name}` references missing interface `{}`",
                    missing.as_str()
                ))
            })?;
        if !has_invariants {
            continue;
        }

        let candidate_signature = effective_invariant_signature(candidate, name)?;
        let candidate_integrity_signature = integrity_invariant_signature(candidate, name)?;
        let (changed, integrity_changed, property_contract_changed, temporal_contract_changed) =
            match previous_catalog {
                None => (
                    true,
                    !candidate_integrity_signature.is_empty(),
                    false,
                    effective_temporal_contract_changed(name, None, candidate_catalog)?,
                ),
                Some(_) if !previous.has_type(name) => (
                    true,
                    !candidate_integrity_signature.is_empty(),
                    true,
                    effective_temporal_contract_changed(name, None, candidate_catalog)?,
                ),
                Some(previous_catalog) => {
                    let previous_signature = effective_invariant_signature(previous, name)?;
                    let previous_integrity_signature =
                        integrity_invariant_signature(previous, name)?;
                    let property_contract_changed = effective_property_contract_changed(
                        name,
                        previous_catalog,
                        candidate_catalog,
                    )?;
                    let temporal_contract_changed = effective_temporal_contract_changed(
                        name,
                        Some(previous_catalog),
                        candidate_catalog,
                    )?;
                    let integrity_changed =
                        candidate_integrity_signature
                            .iter()
                            .any(|(key, invariant)| {
                                previous_integrity_signature.get(key) != Some(invariant)
                            });
                    (
                        previous_signature != candidate_signature || property_contract_changed,
                        integrity_changed,
                        property_contract_changed,
                        temporal_contract_changed,
                    )
                }
            };
        if !changed {
            continue;
        }
        // Adding, changing, or removing a quality-only declaration does not
        // change the install-time integrity contract. Skip its live-row scan
        // entirely so quality expectations cannot refuse an install through
        // the evaluator's row or fuel budgets. Changed temporal contracts
        // still scan implicit integrity checks; changed property contracts
        // also recheck any active integrity declarations.
        let candidate_has_integrity = !candidate_integrity_signature.is_empty();
        if !(integrity_changed
            || temporal_contract_changed
            || (candidate_has_integrity && property_contract_changed))
        {
            continue;
        }

        let root =
            candidate.tables(name)?.first().copied().ok_or_else(|| {
                refused(format!("candidate root table for `{name}` is unavailable"))
            })?;
        affected
            .entry(root.table.clone())
            .or_default()
            .insert(name.to_owned());
    }

    if affected.is_empty() {
        return Ok(());
    }

    let reader = StoreReader::new(engine, previous);
    let mut scanned = 0usize;
    let mut remaining_fuel = invariant_evaluation_budget();
    let mut violations = Vec::new();

    for (root_table, object_types) in affected {
        let object_types = object_types.into_iter().collect::<Vec<_>>();
        for names in object_types.chunks(90) {
            let mut cursor: Option<InvariantScanCursor> = None;
            loop {
                let rows = query_invariant_object_page(
                    engine,
                    &root_table,
                    names,
                    cursor.as_ref(),
                    scan_page_limit(scanned),
                )?;
                if rows.is_empty() {
                    break;
                }
                let remaining_rows = CONTRACT_SCAN_ROW_LIMIT.saturating_sub(scanned);
                if rows.len() > remaining_rows {
                    return Err(CellError::DefinitionInvariantBudgetExceeded {
                        resource: DefinitionInvariantBudget::Rows,
                        limit: u64::try_from(CONTRACT_SCAN_ROW_LIMIT).unwrap_or(u64::MAX),
                        scanned,
                        object: None,
                    });
                }

                let mut last = None;
                for values in rows {
                    let row = SqlRow(values);
                    let world: String = row.get(0)?;
                    let id: String = row.get(1)?;
                    let object_type: String = row.get(2)?;
                    let identity = DefinitionInvariantObject {
                        world: world.clone(),
                        object_type: object_type.clone(),
                        id: id.clone(),
                    };
                    let mut object =
                        reader.object(&world, &object_type, &id)?.ok_or_else(|| {
                            CellError::DefinitionInvariantEvaluationRefused {
                                object: identity.clone(),
                                reason: "the current root row could not be read".to_owned(),
                            }
                        })?;
                    overlay_candidate_object_values(
                        engine,
                        previous,
                        candidate,
                        candidate_catalog,
                        &world,
                        &id,
                        &object_type,
                        &mut object.values,
                    )?;
                    scanned = scanned.saturating_add(1);

                    let findings = match evaluate_invariants(
                        &integrity_catalog,
                        &FactKey::new(&object_type, &id),
                        &object.values,
                        &mut remaining_fuel,
                    ) {
                        Ok(findings) => findings,
                        Err(InvariantEvaluationError::BudgetExceeded) => {
                            return Err(CellError::DefinitionInvariantBudgetExceeded {
                                resource: DefinitionInvariantBudget::Fuel,
                                limit: invariant_evaluation_budget(),
                                scanned,
                                object: Some(identity),
                            });
                        }
                        Err(error) => {
                            return Err(CellError::DefinitionInvariantEvaluationRefused {
                                object: identity,
                                reason: format!("{error:?}"),
                            });
                        }
                    };
                    last = Some(InvariantScanCursor { world, id });
                    for finding in findings {
                        if violations.len() == DEFINITION_INVARIANT_FINDING_LIMIT {
                            return Err(CellError::DefinitionInvariantViolation {
                                violations,
                                truncated: true,
                            });
                        }
                        violations.push(DefinitionInvariantFinding {
                            object: identity.clone(),
                            owner: finding.owner,
                            id: finding.id,
                            message: finding.message,
                            outcome: finding.outcome,
                        });
                    }
                }
                cursor = last;
            }
        }
    }

    if !violations.is_empty() {
        return Err(CellError::DefinitionInvariantViolation {
            violations,
            truncated: false,
        });
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the overlay reads one live object with both schema versions and the candidate contract"
)]
fn overlay_candidate_object_values<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    catalog: &EffectiveCatalog,
    world: &str,
    id: &str,
    object_type: &str,
    values: &mut BTreeMap<String, RuntimeValue>,
) -> Result<(), CellError> {
    let columns = candidate.columns(object_type)?;
    let retained: BTreeSet<&str> = columns
        .iter()
        .map(|column| column.column.property.as_str())
        .collect();
    values.retain(|name, _| retained.contains(name.as_str()));
    for (name, value) in values.iter_mut() {
        *value = candidate.canonicalize_object_value(object_type, name, value);
    }
    for column in columns {
        let name = column.column.property.as_str();
        if values.contains_key(name) {
            continue;
        }
        let old_table = previous
            .schema()
            .object_table(column.table.object_type.as_str());
        let old_overflow_value = if column.column.column.is_none()
            && let Some(old_table) = old_table
            && old_table.has_overflow
        {
            let raw: Option<String> = engine
                .query_optional(
                    &format!(
                        "SELECT {} FROM {} WHERE {} = ?1 AND {} = ?2",
                        quote_identifier(OVERFLOW_COLUMN),
                        quote_identifier(&old_table.table),
                        quote_identifier(WORLD_COLUMN),
                        quote_identifier(ID_COLUMN),
                    ),
                    params![world, id],
                    |row| row.get(0),
                )?
                .flatten();
            raw.map(|text| serde_json::from_str::<BTreeMap<String, RuntimeValue>>(&text))
                .transpose()?
                .and_then(|properties| properties.get(name).cloned())
        } else {
            None
        };
        let value = if let Some(value) = old_overflow_value {
            value
        } else {
            let encoded = declared_default(
                candidate,
                catalog,
                column.table.object_type.as_str(),
                None,
                column.column,
            )?;
            encoded
                .as_ref()
                .map(|value| crate::codec::decode(&column.table.table, column.column, value))
                .transpose()?
                .unwrap_or(RuntimeValue::Null)
        };
        values.insert(
            name.to_owned(),
            candidate.canonicalize_object_value(object_type, name, &value),
        );
    }
    Ok(())
}

/// Compare inherited object and interface invariant declarations for one
/// concrete type. The model stores the canonical declarations; walking its
/// lineage gives the same effective owner set used by `TypeCatalog`.
fn effective_invariant_signature(
    model: &Model,
    object_type: &str,
) -> Result<BTreeMap<(String, String, String), Invariant>, CellError> {
    let types: BTreeMap<String, &ObjectTypeDef> = model
        .object_types()
        .map(|definition| (definition.name.as_str().to_owned(), definition))
        .collect();
    let interfaces: BTreeMap<String, &InterfaceDef> = model
        .interfaces()
        .map(|definition| (definition.name.as_str().to_owned(), definition))
        .collect();
    let mut signature = BTreeMap::new();
    let mut current = Some(object_type.to_owned());
    let mut visited = BTreeSet::new();

    while let Some(name) = current {
        if !visited.insert(name.clone()) || visited.len() > types.len() {
            return Err(refused(format!(
                "object type lineage for `{object_type}` is cyclic"
            )));
        }
        let definition = types.get(&name).copied().ok_or_else(|| {
            refused(format!(
                "object type lineage for `{object_type}` misses `{name}`"
            ))
        })?;
        for invariant in &definition.invariants {
            signature.insert(
                (
                    "object".to_owned(),
                    name.clone(),
                    invariant.id.as_str().to_owned(),
                ),
                invariant.clone(),
            );
        }
        for interface_name in &definition.implements {
            let interface_name = interface_name.as_str();
            let interface = interfaces.get(interface_name).copied().ok_or_else(|| {
                refused(format!(
                    "object type `{object_type}` references missing interface `{interface_name}`"
                ))
            })?;
            for invariant in &interface.invariants {
                signature.insert(
                    (
                        "interface".to_owned(),
                        interface_name.to_owned(),
                        invariant.id.as_str().to_owned(),
                    ),
                    invariant.clone(),
                );
            }
        }
        current = definition
            .extends
            .as_ref()
            .map(|parent| parent.as_str().to_owned());
    }
    Ok(signature)
}

/// Keep only declarations that participate in the install-time integrity
/// proof. Quality declarations are evaluated at their finding boundaries.
fn integrity_invariant_signature(
    model: &Model,
    object_type: &str,
) -> Result<BTreeMap<(String, String, String), Invariant>, CellError> {
    let mut signature = effective_invariant_signature(model, object_type)?;
    signature.retain(|_, invariant| invariant.classification == ConstraintClass::Integrity);
    Ok(signature)
}

/// Compare the complete effective property view (including defaults,
/// inheritance, and interface requirements) for a concrete object type.
fn effective_property_contract_changed(
    object_type: &str,
    previous: &EffectiveCatalog,
    candidate: &EffectiveCatalog,
) -> Result<bool, CellError> {
    let candidate_type = candidate
        .object_type_named(object_type)
        .ok_or_else(|| {
            refused(format!(
                "candidate effective type `{object_type}` is missing"
            ))
        })?
        .map_err(|finding| refused(finding.message.clone()))?;
    let Some(previous_type) = previous.object_type_named(object_type) else {
        return Ok(true);
    };
    let previous_type = previous_type.map_err(|finding| refused(finding.message.clone()))?;
    if previous_type.properties.len() != candidate_type.properties.len() {
        return Ok(true);
    }
    for candidate_property in &candidate_type.properties {
        let Some(previous_property) = previous_type.property(candidate_property.id.as_str()) else {
            return Ok(true);
        };
        if !candidate_property.same_contract(previous_property)
            || candidate_property.default != previous_property.default
            || candidate_property.computed != previous_property.computed
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a newly introduced or changed temporal property needs its
/// implicit integrity check applied to live values.
fn effective_temporal_contract_changed(
    object_type: &str,
    previous: Option<&EffectiveCatalog>,
    candidate: &EffectiveCatalog,
) -> Result<bool, CellError> {
    let candidate_type = candidate
        .object_type_named(object_type)
        .ok_or_else(|| {
            refused(format!(
                "candidate effective type `{object_type}` is missing"
            ))
        })?
        .map_err(|finding| refused(finding.message.clone()))?;
    let candidate_temporal = candidate_type
        .properties
        .iter()
        .filter(|property| is_temporal_effective_property(property))
        .collect::<Vec<_>>();
    let Some(previous) = previous else {
        return Ok(!candidate_temporal.is_empty());
    };
    let Some(previous_type) = previous.object_type_named(object_type) else {
        return Ok(!candidate_temporal.is_empty());
    };
    let previous_type = previous_type.map_err(|finding| refused(finding.message.clone()))?;
    for candidate_property in candidate_temporal {
        let Some(previous_property) = previous_type.property(candidate_property.id.as_str()) else {
            return Ok(true);
        };
        if !candidate_property.same_contract(previous_property)
            || candidate_property.default != previous_property.default
            || candidate_property.computed != previous_property.computed
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_temporal_effective_property(property: &EffectiveProperty) -> bool {
    !property.computed
        && matches!(
            &property.kind,
            EffectiveKind::Base(PropertyType::Date {} | PropertyType::Timestamp { .. })
        )
}

#[derive(Debug, Clone)]
struct InvariantScanCursor {
    world: String,
    id: String,
}

fn query_invariant_object_page<E: SqlEngine>(
    engine: &E,
    root_table: &str,
    object_types: &[String],
    cursor: Option<&InvariantScanCursor>,
    limit: usize,
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let type_binds = vec!["?"; object_types.len()].join(", ");
    let mut sql = format!(
        "SELECT {}, {}, {} FROM {} WHERE {} IN ({type_binds})",
        quote_identifier(WORLD_COLUMN),
        quote_identifier(ID_COLUMN),
        quote_identifier(TYPE_COLUMN),
        quote_identifier(root_table),
        quote_identifier(TYPE_COLUMN),
    );
    let mut bindings: Vec<SqlValue> = object_types.iter().cloned().map(SqlValue::from).collect();
    if let Some(cursor) = cursor {
        let world = bindings.len().saturating_add(1);
        let id = world.saturating_add(1);
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = write!(
            sql,
            " AND ({} > ?{world} OR ({} = ?{world} AND {} > ?{id}))",
            quote_identifier(WORLD_COLUMN),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(ID_COLUMN),
        );
        bindings.push(SqlValue::from(cursor.world.clone()));
        bindings.push(SqlValue::from(cursor.id.clone()));
    }
    let limit_index = bindings.len().saturating_add(1);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "fmt::Write for String is infallible; its trait signature still returns Result"
    )]
    let _ = write!(
        sql,
        " ORDER BY {}, {} LIMIT ?{limit_index}",
        quote_identifier(WORLD_COLUMN),
        quote_identifier(ID_COLUMN),
    );
    bindings.push(SqlValue::from(i64::try_from(limit).unwrap_or(i64::MAX)));
    engine.query(&sql, &bindings)
}

fn check_effective_storage_owners(previous: &Model, candidate: &Model) -> Result<(), CellError> {
    for old_type in previous.object_types() {
        let name = old_type.name.as_str();
        if !candidate.has_type(name) {
            continue;
        }
        for old_column in previous.columns(name)? {
            let property = old_column.column.property.as_str();
            let Ok(new_column) = candidate.column(name, property) else {
                continue;
            };
            if old_column.table.table != new_column.table.table
                || old_column.column.column != new_column.column.column
            {
                return Err(refused(format!(
                    "effective property `{name}.{property}` changes physical storage owner or encoding"
                )));
            }
        }
        for old_link_table in &previous.schema().link_tables {
            if !previous.is_a(name, old_link_table.source.as_str()) {
                continue;
            }
            let link_id = old_link_table.link.as_str();
            let (Ok((_, old_link_table)), Ok((_, new_link_table))) =
                (previous.link(name, link_id), candidate.link(name, link_id))
            else {
                continue;
            };
            if old_link_table.table != new_link_table.table {
                return Err(refused(format!(
                    "effective link `{name}.{link_id}` changes physical storage owner"
                )));
            }
            for old_column in &old_link_table.columns {
                if let Some(new_column) = new_link_table
                    .columns
                    .iter()
                    .find(|column| column.property == old_column.property)
                    && old_column.column != new_column.column
                {
                    return Err(refused(format!(
                        "effective link property `{}.{link_id}.{}` changes physical storage or encoding",
                        name,
                        old_column.property.as_str()
                    )));
                }
            }
        }
    }
    Ok(())
}

fn check_tightened_contract_values<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    check_tightened_link_layouts(
        engine,
        previous,
        candidate,
        previous_catalog,
        candidate_catalog,
    )?;
    check_tightened_link_targets(engine, previous, previous_catalog, candidate_catalog)?;
    check_tightened_object_values(
        engine,
        previous,
        candidate,
        previous_catalog,
        candidate_catalog,
    )?;
    check_tightened_link_values(
        engine,
        previous,
        candidate,
        previous_catalog,
        candidate_catalog,
    )
}

fn check_candidate_live_values<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    for table in &candidate.schema().object_tables {
        if previous
            .schema()
            .object_table(table.object_type.as_str())
            .is_none()
        {
            continue;
        }
        let root = candidate
            .tables(table.object_type.as_str())?
            .first()
            .copied()
            .ok_or_else(|| CellError::UnknownObjectType(table.object_type.as_str().to_owned()))?;
        for column in &table.columns {
            let object_types =
                object_types_with_existing_column(previous, candidate, &table.table, column);
            if !object_types.is_empty() {
                scan_object_contract_column(
                    engine,
                    &table.table,
                    &root.table,
                    column,
                    &object_types,
                    candidate_catalog,
                    candidate_catalog,
                )?;
            }
        }
    }

    for table in &candidate.schema().link_tables {
        if previous
            .schema()
            .link_table(table.source.as_str(), table.link.as_str())
            .is_none()
        {
            continue;
        }
        let root = candidate
            .tables(table.source.as_str())?
            .first()
            .copied()
            .ok_or_else(|| CellError::UnknownObjectType(table.source.as_str().to_owned()))?;
        for column in &table.columns {
            let object_types = object_types_with_existing_link_column(
                previous,
                candidate,
                &table.table,
                table.link.as_str(),
                column,
            );
            if !object_types.is_empty() {
                scan_link_contract_column(
                    engine,
                    &table.table,
                    &root.table,
                    table.link.as_str(),
                    column,
                    &object_types,
                    candidate_catalog,
                    candidate_catalog,
                )?;
            }
        }
    }
    for table in &previous.schema().link_tables {
        scan_link_endpoints(engine, previous, table, candidate_catalog)?;
    }
    check_candidate_required_link_presence(engine, previous, candidate, candidate_catalog)?;
    Ok(())
}

fn check_candidate_required_link_presence<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    let mut required_tables = BTreeMap::<String, (LinkTable, BTreeSet<String>)>::new();
    for object_type in candidate.object_types() {
        let source_type = object_type.name.as_str();
        if !previous.has_type(source_type) {
            continue;
        }
        let Some(links) = candidate_catalog.links_for_object_type(source_type) else {
            continue;
        };
        for link in links.iter().filter(|link| link.cardinality.required) {
            let Ok((_, table)) = candidate.link(source_type, link.id.as_str()) else {
                if object_type_has_live_rows(engine, previous, source_type)? {
                    return Err(refused(format!(
                        "required link `{}.{}` has live source objects but no projected link table",
                        source_type,
                        link.id.as_str()
                    )));
                }
                continue;
            };
            let entry = required_tables
                .entry(table.table.clone())
                .or_insert_with(|| (table.clone(), BTreeSet::new()));
            entry.1.insert(source_type.to_owned());
        }
    }
    for (physical_table, (candidate_table, source_types)) in required_tables {
        let Some(existing_table) = previous
            .schema()
            .link_tables
            .iter()
            .find(|old| old.table == physical_table)
        else {
            for source_type in source_types {
                if object_type_has_live_rows(engine, previous, &source_type)? {
                    return Err(refused(format!(
                        "new required link `{}.{}` has live source objects but no installed link table",
                        source_type,
                        candidate_table.link.as_str()
                    )));
                }
            }
            continue;
        };
        scan_required_link_sources(
            engine,
            previous,
            existing_table,
            &source_types.into_iter().collect::<Vec<_>>(),
        )?;
    }
    Ok(())
}

fn scan_required_link_sources<E: SqlEngine>(
    engine: &E,
    model: &Model,
    table: &LinkTable,
    source_types: &[String],
) -> Result<(), CellError> {
    let root = model
        .tables(table.source.as_str())?
        .first()
        .copied()
        .ok_or_else(|| CellError::UnknownObjectType(table.source.as_str().to_owned()))?;
    let mut scanned = 0usize;
    for names in source_types.chunks(90) {
        let mut cursor: Option<ObjectCursor> = None;
        loop {
            let rows = query_required_link_source_page(
                engine,
                &table.table,
                &root.table,
                names,
                cursor.as_ref(),
                scan_page_limit(scanned),
            )?;
            if rows.is_empty() {
                break;
            }
            validate_scan_size(scanned, rows.len(), &table.table, "required link source")?;
            scanned = scanned.saturating_add(rows.len());
            let mut last = None;
            for values in rows {
                let row = SqlRow(values);
                let world: String = row.get(0)?;
                let id: String = row.get(1)?;
                let has_link: i64 = row.get(2)?;
                if has_link == 0 {
                    return Err(refused(format!(
                        "required link `{}.{}` has no row for live source `{id}`",
                        table.source.as_str(),
                        table.link.as_str()
                    )));
                }
                last = Some(ObjectCursor { world, id });
            }
            cursor = last;
        }
    }
    Ok(())
}

fn query_required_link_source_page<E: SqlEngine>(
    engine: &E,
    link_table: &str,
    root_table: &str,
    object_types: &[String],
    cursor: Option<&ObjectCursor>,
    limit: usize,
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let type_binds = vec!["?"; object_types.len()].join(", ");
    let mut sql = format!(
        "SELECT r.{world}, r.{id}, CASE WHEN EXISTS (SELECT 1 FROM {link} AS l WHERE l.{link_world} = r.{world} AND l.{source} = r.{id}) THEN 1 ELSE 0 END FROM {root} AS r WHERE r.{type_column} IN ({type_binds})",
        world = quote_identifier(WORLD_COLUMN),
        id = quote_identifier(ID_COLUMN),
        link = quote_identifier(link_table),
        link_world = quote_identifier(WORLD_COLUMN),
        source = quote_identifier(SOURCE_COLUMN),
        root = quote_identifier(root_table),
        type_column = quote_identifier(TYPE_COLUMN),
    );
    let mut bindings: Vec<SqlValue> = object_types.iter().cloned().map(SqlValue::from).collect();
    if let Some(cursor) = cursor {
        let world = bindings.len().saturating_add(1);
        let id = world.saturating_add(1);
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = write!(
            sql,
            " AND (r.{world_column} > ?{world} OR (r.{world_column} = ?{world} AND r.{id_column} > ?{id}))",
            world_column = quote_identifier(WORLD_COLUMN),
            id_column = quote_identifier(ID_COLUMN),
        );
        bindings.push(SqlValue::from(cursor.world.clone()));
        bindings.push(SqlValue::from(cursor.id.clone()));
    }
    let limit_index = bindings.len().saturating_add(1);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "fmt::Write for String is infallible; its trait signature still returns Result"
    )]
    let _ = write!(
        sql,
        " ORDER BY r.{}, r.{} LIMIT ?{limit_index}",
        quote_identifier(WORLD_COLUMN),
        quote_identifier(ID_COLUMN),
    );
    bindings.push(SqlValue::from(i64::try_from(limit).unwrap_or(i64::MAX)));
    engine.query(&sql, &bindings)
}

fn check_tightened_link_targets<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    for table in &previous.schema().link_tables {
        let mut target_set_narrowed = false;
        for source in previous.object_types() {
            let source_name = source.name.as_str();
            let Ok((_, effective_table)) = previous.link(source_name, table.link.as_str()) else {
                continue;
            };
            if effective_table.table != table.table {
                continue;
            }
            let installed =
                effective_link_contract(previous_catalog, source_name, table.link.as_str())?;
            let candidate =
                effective_link_contract(candidate_catalog, source_name, table.link.as_str())?;
            let narrowed = match (installed, candidate) {
                (Some(installed), Some(candidate)) => {
                    match (
                        previous_catalog.target_types(installed.target.as_str()),
                        candidate_catalog.target_types(candidate.target.as_str()),
                    ) {
                        (Some(installed), Some(candidate)) => !candidate.is_superset(&installed),
                        _ => true,
                    }
                }
                _ => true,
            };
            if narrowed {
                target_set_narrowed = true;
                break;
            }
        }
        if target_set_narrowed {
            scan_link_endpoints(engine, previous, table, candidate_catalog)?;
        }
    }
    Ok(())
}

fn effective_link_contract<'a>(
    catalog: &'a EffectiveCatalog,
    object_type: &str,
    link_id: &str,
) -> Result<Option<&'a EffectiveLink>, CellError> {
    if effective_object(catalog, object_type)?.is_none() {
        return Ok(None);
    }
    let links = catalog.links_for_object_type(object_type).ok_or_else(|| {
        refused(format!(
            "effective link catalog for `{object_type}` is unavailable"
        ))
    })?;
    Ok(links.iter().find(|link| link.id.as_str() == link_id))
}

#[allow(
    clippy::too_many_lines,
    reason = "one bounded link-endpoint scan validates both recorded and effective identities"
)]
fn scan_link_endpoints<E: SqlEngine>(
    engine: &E,
    model: &Model,
    table: &LinkTable,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    let root = model
        .tables(table.source.as_str())?
        .first()
        .copied()
        .ok_or_else(|| CellError::UnknownObjectType(table.source.as_str().to_owned()))?;
    let mut scanned = 0usize;
    let mut cursor: Option<LinkCursor> = None;
    let mut target_sets = BTreeMap::<String, Option<BTreeSet<String>>>::new();
    loop {
        let rows = query_link_endpoint_page(
            engine,
            &table.table,
            &root.table,
            cursor.as_ref(),
            scan_page_limit(scanned),
        )?;
        if rows.is_empty() {
            break;
        }
        validate_scan_size(scanned, rows.len(), &table.table, "link endpoint")?;
        scanned = scanned.saturating_add(rows.len());
        let mut last = None;
        for values in rows {
            let row = SqlRow(values);
            let world: String = row.get(0)?;
            let source: String = row.get(1)?;
            let target: String = row.get(2)?;
            let source_type: Option<String> = row.get(3)?;
            let stored_link: Option<String> = row.get(4)?;
            let stored_source_type: Option<String> = row.get(5)?;
            let stored_target_type: Option<String> = row.get(6)?;
            let source_type = source_type.ok_or_else(|| {
                refused(format!(
                    "live link `{}.{}` has no source object row",
                    table.source.as_str(),
                    table.link.as_str()
                ))
            })?;
            let stored_link = stored_link.ok_or_else(|| {
                refused(format!(
                    "live link `{}.{}` has no endpoint metadata",
                    table.source.as_str(),
                    table.link.as_str()
                ))
            })?;
            let stored_source_type = stored_source_type.ok_or_else(|| {
                refused(format!(
                    "live link `{}.{}` has no source-type metadata",
                    table.source.as_str(),
                    table.link.as_str()
                ))
            })?;
            let stored_target_type = stored_target_type.ok_or_else(|| {
                refused(format!(
                    "live link `{}.{}` has no target-type metadata",
                    table.source.as_str(),
                    table.link.as_str()
                ))
            })?;
            let source_link_matches = model
                .link(&source_type, table.link.as_str())
                .is_ok_and(|(_, source_table)| source_table.table == table.table);
            if stored_link != table.link.as_str()
                || !model.has_type(&stored_source_type)
                || !model.is_a(&source_type, &stored_source_type)
                || !source_link_matches
            {
                return Err(refused(format!(
                    "live link `{}.{}` has inconsistent endpoint metadata",
                    table.source.as_str(),
                    table.link.as_str()
                )));
            }
            let link = effective_link_contract(
                candidate_catalog,
                &source_type,
                table.link.as_str(),
            )?
            .ok_or_else(|| {
                refused(format!(
                    "live link `{}.{}` has no candidate source contract for `{source_type}`",
                    table.source.as_str(),
                    table.link.as_str()
                ))
            })?;
            let target_root = model
                .tables(&stored_target_type)
                .map_err(|_unknown_target_type| {
                    refused(format!(
                        "live link `{}.{}` has unknown target root `{stored_target_type}`",
                        table.source.as_str(),
                        table.link.as_str()
                    ))
                })?
                .first()
                .copied()
                .ok_or_else(|| {
                    refused(format!(
                        "live link `{}.{}` has no target root `{stored_target_type}`",
                        table.source.as_str(),
                        table.link.as_str()
                    ))
                })?;
            let actual_target_type =
                actual_object_type(engine, &target_root.table, &world, &target)?;
            if !model.is_a(&actual_target_type, &stored_target_type) {
                return Err(refused(format!(
                    "live link `{}.{}` target metadata `{stored_target_type}` does not match `{actual_target_type}`",
                    table.source.as_str(),
                    table.link.as_str()
                )));
            }
            let allowed_targets = target_sets
                .entry(link.target.as_str().to_owned())
                .or_insert_with(|| candidate_catalog.target_types(link.target.as_str()))
                .as_ref()
                .ok_or_else(|| {
                    refused(format!(
                        "candidate target contract `{}` for live link `{}.{}` is unavailable",
                        link.target.as_str(),
                        table.source.as_str(),
                        table.link.as_str()
                    ))
                })?;
            if !allowed_targets.contains(&actual_target_type) {
                return Err(refused(format!(
                    "live link `{}.{}` targets `{actual_target_type}`, outside candidate target `{}`",
                    table.source.as_str(),
                    table.link.as_str(),
                    link.target.as_str()
                )));
            }

            last = Some(LinkCursor {
                world,
                source,
                target,
            });
        }
        cursor = last;
    }
    Ok(())
}

fn actual_object_type<E: SqlEngine>(
    engine: &E,
    root_table: &str,
    world: &str,
    id: &str,
) -> Result<String, CellError> {
    engine
        .query_optional(
            &format!(
                "SELECT {} FROM {} WHERE {} = ?1 AND {} = ?2 LIMIT 1",
                quote_identifier(TYPE_COLUMN),
                quote_identifier(root_table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
            ),
            params![world, id],
            |row| row.get(0),
        )?
        .ok_or_else(|| {
            refused(format!(
                "live link target `{id}` has no object row in its recorded root"
            ))
        })
}

fn query_link_endpoint_page<E: SqlEngine>(
    engine: &E,
    table: &str,
    source_root: &str,
    cursor: Option<&LinkCursor>,
    limit: usize,
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let mut sql = format!(
        "SELECT p.{world}, p.{source}, p.{target}, s.{type_column}, v.{link}, v.{source_type}, v.{target_type} FROM {table} AS p LEFT JOIN {source_root} AS s ON p.{world} = s.{world} AND p.{source} = s.{id} LEFT JOIN {link_versions} AS v ON p.{world} = v.{link_world} AND v.{link_table} = ?1 AND p.{source} = v.{link_source} AND p.{target} = v.{link_target}",
        world = quote_identifier(WORLD_COLUMN),
        source = quote_identifier(SOURCE_COLUMN),
        target = quote_identifier(TARGET_COLUMN),
        type_column = quote_identifier(TYPE_COLUMN),
        link = quote_identifier("link"),
        source_type = quote_identifier("source_type"),
        target_type = quote_identifier("target_type"),
        link_world = quote_identifier("world"),
        link_source = quote_identifier("source"),
        link_target = quote_identifier("target"),
        table = quote_identifier(table),
        source_root = quote_identifier(source_root),
        id = quote_identifier(ID_COLUMN),
        link_versions = quote_identifier(LINK_VERSIONS),
        link_table = quote_identifier("link_table"),
    );
    let mut bindings = vec![SqlValue::from(table)];
    if let Some(cursor) = cursor {
        let world = bindings.len().saturating_add(1);
        let source = world.saturating_add(1);
        let target = source.saturating_add(1);
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = write!(
            sql,
            " WHERE (p.{world_column} > ?{world} OR (p.{world_column} = ?{world} AND p.{source_column} > ?{source}) OR (p.{world_column} = ?{world} AND p.{source_column} = ?{source} AND p.{target_column} > ?{target}))",
            world_column = quote_identifier(WORLD_COLUMN),
            source_column = quote_identifier(SOURCE_COLUMN),
            target_column = quote_identifier(TARGET_COLUMN),
        );
        bindings.push(SqlValue::from(cursor.world.clone()));
        bindings.push(SqlValue::from(cursor.source.clone()));
        bindings.push(SqlValue::from(cursor.target.clone()));
    }
    let limit_index = bindings.len().saturating_add(1);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "fmt::Write for String is infallible; its trait signature still returns Result"
    )]
    let _ = write!(
        sql,
        " ORDER BY p.{}, p.{}, p.{} LIMIT ?{limit_index}",
        quote_identifier(WORLD_COLUMN),
        quote_identifier(SOURCE_COLUMN),
        quote_identifier(TARGET_COLUMN),
    );
    bindings.push(SqlValue::from(i64::try_from(limit).unwrap_or(i64::MAX)));
    engine.query(&sql, &bindings)
}

fn object_types_with_existing_column(
    previous: &Model,
    candidate: &Model,
    table: &str,
    column: &PropertyColumn,
) -> Vec<String> {
    let mut object_types = Vec::new();
    for object_type in candidate.object_types() {
        let name = object_type.name.as_str();
        if !previous.has_type(name) {
            continue;
        }
        let (Ok(old_column), Ok(new_column)) = (
            previous.column(name, column.property.as_str()),
            candidate.column(name, column.property.as_str()),
        ) else {
            continue;
        };
        if old_column.table.table == table
            && new_column.table.table == table
            && old_column.column.column == column.column
            && new_column.column.column == column.column
        {
            object_types.push(name.to_owned());
        }
    }
    object_types.sort();
    object_types.dedup();
    object_types
}

fn object_types_with_existing_link_column(
    previous: &Model,
    candidate: &Model,
    table: &str,
    link_id: &str,
    column: &PropertyColumn,
) -> Vec<String> {
    let mut object_types = Vec::new();
    for object_type in candidate.object_types() {
        let name = object_type.name.as_str();
        if !previous.has_type(name) {
            continue;
        }
        let (Ok((_, old_table)), Ok((_, new_table))) =
            (previous.link(name, link_id), candidate.link(name, link_id))
        else {
            continue;
        };
        let old_column = old_table
            .columns
            .iter()
            .find(|old| old.property == column.property);
        let new_column = new_table
            .columns
            .iter()
            .find(|new| new.property == column.property);
        if old_table.table == table
            && new_table.table == table
            && old_column.is_some_and(|old| old.column == column.column)
            && new_column.is_some_and(|new| new.column == column.column)
        {
            object_types.push(name.to_owned());
        }
    }
    object_types.sort();
    object_types.dedup();
    object_types
}

fn effective_catalog(canonical_set: &str, label: &str) -> Result<EffectiveCatalog, CellError> {
    let values: Vec<serde_json::Value> = serde_json::from_str(canonical_set).map_err(|error| {
        refused(format!(
            "{label} definition set is not a JSON artifact list: {error}"
        ))
    })?;
    let artifacts: Vec<ArtifactEnvelope> = values
        .iter()
        .map(|value| {
            decode_artifact_json(value).map_err(|findings| {
                refused(format!(
                    "{label} definition artifact could not be decoded: {findings:?}"
                ))
            })
        })
        .collect::<Result<_, _>>()?;
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
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

fn effective_object<'a>(
    catalog: &'a EffectiveCatalog,
    object_type: &str,
) -> Result<Option<&'a EffectiveObjectType>, CellError> {
    match catalog.object_type_named(object_type) {
        Some(Ok(object_type)) => Ok(Some(object_type)),
        Some(Err(finding)) => Err(refused(format!(
            "effective contract for `{object_type}` is unavailable: {finding:?}"
        ))),
        None => Ok(None),
    }
}

/// Refuse to add, change, or remove ownership metadata while rows, links, or
/// manifests that belong to the collection are live. There is no admitted
/// migration that can infer or discard parent-owned membership safely.
fn check_owned_collection_changes<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    previous_catalog: Option<&EffectiveCatalog>,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    let mut checked_child_types = BTreeSet::new();
    let mut checked_link_tables = BTreeSet::new();
    let mut parent_names = previous
        .object_types()
        .map(|parent| parent.name.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    parent_names.extend(
        candidate
            .object_types()
            .map(|parent| parent.name.as_str().to_owned()),
    );
    for parent_name in parent_names {
        let old_effective = previous_catalog
            .map(|catalog| effective_object(catalog, &parent_name))
            .transpose()?
            .flatten();
        let new_effective = effective_object(candidate_catalog, &parent_name)?;

        if let (Some(old_catalog), Some(old_effective)) = (previous_catalog, old_effective) {
            for old_collection in &old_effective.owned_collections {
                let unchanged = new_effective.is_some_and(|new| {
                    new.owned_collections.iter().any(|current| {
                        current.id == old_collection.id && {
                            let mut widened = old_collection.clone();
                            widened.max_rows = current.max_rows;
                            current == &widened && current.max_rows.0 >= old_collection.max_rows.0
                        }
                    })
                });
                if !unchanged {
                    check_live_owned_collection_data(
                        engine,
                        previous,
                        previous,
                        old_catalog,
                        &parent_name,
                        old_collection,
                        "removing or changing",
                        &mut checked_child_types,
                        &mut checked_link_tables,
                    )?;
                }
            }
        }

        if let Some(new_effective) = new_effective {
            for new_collection in &new_effective.owned_collections {
                let unchanged = old_effective.is_some_and(|old| {
                    old.owned_collections.iter().any(|prior| {
                        prior.id == new_collection.id && {
                            let mut widened = prior.clone();
                            widened.max_rows = new_collection.max_rows;
                            new_collection == &widened
                                && new_collection.max_rows.0 >= prior.max_rows.0
                        }
                    })
                });
                if !unchanged {
                    check_live_owned_collection_data(
                        engine,
                        previous,
                        candidate,
                        candidate_catalog,
                        &parent_name,
                        new_collection,
                        "adding or changing",
                        &mut checked_child_types,
                        &mut checked_link_tables,
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "one migration guard checks the stored child, link, and manifest representations"
)]
fn check_live_owned_collection_data<E: SqlEngine>(
    engine: &E,
    storage_model: &Model,
    contract_model: &Model,
    contract_catalog: &EffectiveCatalog,
    parent_name: &str,
    collection: &norn_ir::OwnedChildCollection,
    change: &str,
    checked_child_types: &mut BTreeSet<String>,
    checked_link_tables: &mut BTreeSet<String>,
) -> Result<(), CellError> {
    for child in contract_model.object_types() {
        let child_name = child.name.as_str();
        if !storage_model.has_type(child_name) {
            continue;
        }
        let Some(child_effective) = effective_object(contract_catalog, child_name)? else {
            continue;
        };
        if !child_effective
            .lineage
            .iter()
            .any(|ancestor| ancestor == &collection.child_type)
            || !checked_child_types.insert(child_name.to_owned())
        {
            continue;
        }
        if object_type_has_live_rows(engine, storage_model, child_name)? {
            return Err(refused(format!(
                "{change} owned collection `{}.{}` requires an explicit migration while child type `{child_name}` has live rows",
                parent_name,
                collection.id.as_str()
            )));
        }
    }

    match storage_model.link(parent_name, collection.id.as_str()) {
        Ok((_, table)) => {
            if checked_link_tables.insert(table.table.clone())
                && table_has_rows(engine, &table.table)?
            {
                return Err(refused(format!(
                    "{change} owned collection `{}.{}` requires an explicit migration while its parent link has live rows",
                    parent_name,
                    collection.id.as_str()
                )));
            }
        }
        Err(CellError::UnknownLink { .. } | CellError::UnknownObjectType(_)) => {}
        Err(error) => return Err(error),
    }

    if owned_manifest_has_live_rows(engine, storage_model, parent_name, collection.id.as_str())? {
        return Err(refused(format!(
            "{change} owned collection `{}.{}` requires an explicit migration while its manifest has live rows",
            parent_name,
            collection.id.as_str()
        )));
    }
    Ok(())
}

fn owned_manifest_has_live_rows<E: SqlEngine>(
    engine: &E,
    model: &Model,
    parent_name: &str,
    collection_id: &str,
) -> Result<bool, CellError> {
    if !model.has_type(OWNED_ROWS_TYPE) {
        return Ok(false);
    }
    let root = model
        .tables(OWNED_ROWS_TYPE)?
        .first()
        .copied()
        .ok_or_else(|| CellError::UnknownObjectType(OWNED_ROWS_TYPE.to_owned()))?;
    let parent_type = model.column(OWNED_ROWS_TYPE, "parentType");
    let collection = model.column(OWNED_ROWS_TYPE, "collectionId");
    let Ok(parent_type) = parent_type else {
        return object_type_has_live_rows(engine, model, OWNED_ROWS_TYPE);
    };
    let Ok(collection) = collection else {
        return object_type_has_live_rows(engine, model, OWNED_ROWS_TYPE);
    };
    let (Some(parent_type_column), Some(collection_column)) =
        (&parent_type.column.column, &collection.column.column)
    else {
        return object_type_has_live_rows(engine, model, OWNED_ROWS_TYPE);
    };
    if parent_type.table.table.as_str() != root.table.as_str()
        || collection.table.table.as_str() != root.table.as_str()
    {
        return object_type_has_live_rows(engine, model, OWNED_ROWS_TYPE);
    }
    Ok(engine
        .query_optional(
            &format!(
                "SELECT 1 FROM {} WHERE {} = ?1 AND {} = ?2 AND {} = ?3 LIMIT 1",
                quote_identifier(&root.table),
                quote_identifier(TYPE_COLUMN),
                quote_identifier(parent_type_column),
                quote_identifier(collection_column),
            ),
            params![OWNED_ROWS_TYPE, parent_name, collection_id],
            |row| row.get::<i64>(0),
        )?
        .is_some())
}

fn effective_property<'a>(
    catalog: &'a EffectiveCatalog,
    object_type: &str,
    property: &str,
) -> Result<Option<&'a EffectiveProperty>, CellError> {
    Ok(effective_object(catalog, object_type)?.and_then(|object| object.property(property)))
}

fn effective_link_property<'a>(
    catalog: &'a EffectiveCatalog,
    object_type: &str,
    link_id: &str,
    property: &str,
) -> Result<Option<&'a EffectiveProperty>, CellError> {
    if effective_object(catalog, object_type)?.is_none() {
        return Ok(None);
    }
    let Some(link) = catalog
        .links_for_object_type(object_type)
        .unwrap_or_default()
        .iter()
        .find(|link| link.id.as_str() == link_id)
    else {
        return Ok(None);
    };
    Ok(link
        .properties
        .iter()
        .find(|candidate| candidate.id.as_str() == property))
}

/// Adding acyclicity needs a complete live proof, including inherited roles
/// and every world. Bound both SQL enumeration and bytes before fetching keys.
#[allow(
    clippy::too_many_lines,
    reason = "one fail-closed graph proof keeps scan and traversal budgets together"
)]
fn check_new_acyclic_roles<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    previous_catalog: Option<&EffectiveCatalog>,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    let mut changed = BTreeMap::<(String, String), BTreeSet<String>>::new();
    for ty in candidate.object_types() {
        let name = ty.name.as_str();
        for link in candidate_catalog
            .links_for_object_type(name)
            .unwrap_or_default()
        {
            let was_acyclic = previous_catalog
                .and_then(|catalog| catalog.links_for_object_type(name))
                .unwrap_or_default()
                .iter()
                .any(|old| old.id == link.id && old.acyclic);
            if link.acyclic && !was_acyclic {
                changed
                    .entry((
                        candidate
                            .link(name, link.id.as_str())?
                            .1
                            .source
                            .as_str()
                            .to_owned(),
                        link.id.as_str().to_owned(),
                    ))
                    .or_default()
                    .insert(name.to_owned());
            }
        }
    }
    for ((owner, role), changed_types) in changed {
        // A role may have edges stored on several concrete source tables.
        // Only this declaring relation participates. Same-name members on
        // unrelated types are independent graphs and spend no proof budget.
        let mut graph = BTreeMap::<(String, FactKey), BTreeSet<(String, FactKey)>>::new();
        let mut starts = BTreeSet::new();
        let mut scanned = 0usize;
        let mut bytes = 0u64;
        for table in previous
            .schema()
            .link_tables
            .iter()
            .filter(|table| table.link.as_str() == role && table.source.as_str() == owner)
        {
            let root = previous
                .tables(table.source.as_str())?
                .first()
                .copied()
                .ok_or_else(|| CellError::UnknownObjectType(table.source.as_str().to_owned()))?;
            let remaining = CONTRACT_SCAN_ROW_LIMIT.saturating_sub(scanned);
            let metadata: (i64, i64) = engine.query_row(&format!(
                "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM (SELECT octet_length(p.{world}) + octet_length(p.{source}) + octet_length(p.{target}) + COALESCE(octet_length(s.{type_column}), 0) + COALESCE(octet_length(v.link), 0) + COALESCE(octet_length(v.source_type), 0) + COALESCE(octet_length(v.target_type), 0) + 64 AS bytes FROM {table} AS p LEFT JOIN {root} AS s ON p.{world} = s.{world} AND p.{source} = s.{id} LEFT JOIN {versions} AS v ON v.world = p.{world} AND v.link_table = ?2 AND v.source = p.{source} AND v.target = p.{target} LIMIT ?1)",
                world = quote_identifier(WORLD_COLUMN), source = quote_identifier(SOURCE_COLUMN),
                target = quote_identifier(TARGET_COLUMN), table = quote_identifier(&table.table),
                root = quote_identifier(&root.table), type_column = quote_identifier(TYPE_COLUMN),
                id = quote_identifier(ID_COLUMN), versions = quote_identifier(LINK_VERSIONS),
            ), params![i64::try_from(remaining.saturating_add(1)).unwrap_or(i64::MAX), table.table.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)))?;
            let count = usize::try_from(metadata.0)
                .map_err(|error| refused(format!("invalid acyclic scan count: {error}")))?;
            validate_scan_size(scanned, count, &table.table, "acyclic graph")?;
            bytes = bytes.saturating_add(u64::try_from(metadata.1).unwrap_or(u64::MAX));
            if bytes > MIGRATION_SCAN_BYTE_LIMIT {
                return Err(refused(
                    "acyclic proof exceeds the 8 MiB migration scan byte limit".to_owned(),
                ));
            }
            // The same bounded endpoint authority refuses inconsistent or
            // dangling metadata before it can be mistaken for a graph proof.
            scan_link_endpoints(engine, previous, table, candidate_catalog)?;
            let mut cursor = None;
            loop {
                let rows = query_link_endpoint_page(
                    engine,
                    &table.table,
                    &root.table,
                    cursor.as_ref(),
                    scan_page_limit(scanned),
                )?;
                if rows.is_empty() {
                    break;
                }
                validate_scan_size(scanned, rows.len(), &table.table, "acyclic graph")?;
                scanned = scanned.saturating_add(rows.len());
                for values in rows {
                    let row = SqlRow(values);
                    let world: String = row.get(0)?;
                    let source: String = row.get(1)?;
                    let target: String = row.get(2)?;
                    let concrete: Option<String> = row.get(3)?;
                    let concrete = concrete
                        .ok_or_else(|| refused("acyclic proof has a missing source".to_owned()))?;
                    let target_type: Option<String> = row.get(6)?;
                    let target_type = target_type.ok_or_else(|| {
                        refused("acyclic proof has missing target metadata".to_owned())
                    })?;
                    let target_root = previous
                        .chain(&target_type)?
                        .first()
                        .ok_or_else(|| refused("acyclic proof has no target root".to_owned()))?;
                    let from = (
                        world.clone(),
                        FactKey::new(root.object_type.as_str(), &source),
                    );
                    let to = (world.clone(), FactKey::new(target_root, &target));
                    if changed_types.contains(&concrete) {
                        starts.insert(from.clone());
                    }
                    graph.entry(from).or_default().insert(to);
                    cursor = Some(LinkCursor {
                        world,
                        source,
                        target,
                    });
                }
            }
        }
        // Iterative DFS with gray/black states: linear in the bounded graph,
        // avoids recursive stack growth and refuses any reachable cycle.
        let mut done = BTreeSet::new();
        for start in starts {
            let mut active = BTreeSet::new();
            let mut pending = vec![(start, false)];
            while let Some((node, exiting)) = pending.pop() {
                if exiting {
                    active.remove(&node);
                    done.insert(node);
                    continue;
                }
                if done.contains(&node) {
                    continue;
                }
                if !active.insert(node.clone()) {
                    return Err(refused(format!(
                        "acyclic link `{role}` has a cycle in the live graph"
                    )));
                }
                pending.push((node.clone(), true));
                if let Some(targets) = graph.get(&node) {
                    pending.extend(targets.iter().cloned().map(|target| (target, false)));
                }
            }
        }
    }
    Ok(())
}

fn check_tightened_link_layouts<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    for object_type in candidate.object_types() {
        let name = object_type.name.as_str();
        if !previous.has_type(name) {
            continue;
        }
        if effective_object(previous_catalog, name)?.is_none() {
            continue;
        }
        if effective_object(candidate_catalog, name)?.is_none() {
            return Err(refused(format!(
                "candidate effective contracts for `{name}` are missing"
            )));
        }
        let old_links = previous_catalog
            .links_for_object_type(name)
            .unwrap_or_default();
        let new_links = candidate_catalog
            .links_for_object_type(name)
            .unwrap_or_default();
        for old_link in old_links {
            let Some(new_link) = new_links.iter().find(|link| link.id == old_link.id) else {
                continue;
            };
            if old_link.target != new_link.target
                || !compatible_link_cardinality(old_link.cardinality, new_link.cardinality)
                || old_link.inverse != new_link.inverse
            {
                return Err(refused(format!(
                    "effective link `{}.{}` changes target, cardinality, or inverse and is not additive",
                    name,
                    old_link.id.as_str()
                )));
            }
        }
        for new_link in new_links {
            if old_links.iter().any(|link| link.id == new_link.id) || !new_link.cardinality.required
            {
                continue;
            }
            if object_type_has_live_rows(engine, candidate, name)? {
                return Err(refused(format!(
                    "adding required link `{}.{}` would leave live source objects without a link",
                    name,
                    new_link.id.as_str()
                )));
            }
        }
    }
    Ok(())
}

fn object_type_has_live_rows<E: SqlEngine>(
    engine: &E,
    model: &Model,
    object_type: &str,
) -> Result<bool, CellError> {
    let root = model
        .tables(object_type)?
        .first()
        .copied()
        .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))?;
    Ok(engine
        .query_optional(
            &format!(
                "SELECT 1 FROM {} WHERE {} = ?1 LIMIT 1",
                quote_identifier(&root.table),
                quote_identifier(TYPE_COLUMN)
            ),
            params![object_type],
            |row| row.get::<i64>(0),
        )?
        .is_some())
}

fn tightened_object_types(
    previous: &Model,
    candidate: &Model,
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
    table: &str,
    property: &str,
) -> Result<Vec<String>, CellError> {
    let mut narrowed = Vec::new();
    for object_type in candidate.object_types() {
        let name = object_type.name.as_str();
        if !previous.has_type(name) {
            continue;
        }
        let (Ok(old_column), Ok(new_column)) = (
            previous.column(name, property),
            candidate.column(name, property),
        ) else {
            continue;
        };
        if old_column.table.table != table || new_column.table.table != table {
            continue;
        }
        let Some(old) = effective_property(previous_catalog, name, property)? else {
            continue;
        };
        let Some(new) = effective_property(candidate_catalog, name, property)? else {
            return Err(refused(format!(
                "candidate effective contract for `{name}.{property}` is missing"
            )));
        };
        if !new.narrowing_reasons_from(old).is_empty() {
            narrowed.push(name.to_owned());
        }
    }
    narrowed.sort();
    narrowed.dedup();
    Ok(narrowed)
}

fn tightened_link_types(
    previous: &Model,
    candidate: &Model,
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
    table: &str,
    link_id: &str,
    property: &str,
) -> Result<Vec<String>, CellError> {
    let mut narrowed = Vec::new();
    for object_type in candidate.object_types() {
        let name = object_type.name.as_str();
        if !previous.has_type(name) {
            continue;
        }
        let (Ok((_, old_table)), Ok((_, new_table))) =
            (previous.link(name, link_id), candidate.link(name, link_id))
        else {
            continue;
        };
        if old_table.table != table || new_table.table != table {
            continue;
        }
        let Some(old) = effective_link_property(previous_catalog, name, link_id, property)? else {
            continue;
        };
        let Some(new) = effective_link_property(candidate_catalog, name, link_id, property)? else {
            return Err(refused(format!(
                "candidate effective link contract for `{name}.{link_id}.{property}` is missing"
            )));
        };
        if !new.narrowing_reasons_from(old).is_empty() {
            narrowed.push(name.to_owned());
        }
    }
    narrowed.sort();
    narrowed.dedup();
    Ok(narrowed)
}

fn check_tightened_object_values<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    for table in &candidate.schema().object_tables {
        let Some(previous_table) = previous.schema().object_table(table.object_type.as_str())
        else {
            continue;
        };
        let root = candidate
            .tables(table.object_type.as_str())?
            .first()
            .copied()
            .ok_or_else(|| CellError::UnknownObjectType(table.object_type.as_str().to_owned()))?;
        for column in &table.columns {
            if !previous_table
                .columns
                .iter()
                .any(|old| old.property == column.property)
            {
                continue;
            }
            let narrowed = tightened_object_types(
                previous,
                candidate,
                previous_catalog,
                candidate_catalog,
                &table.table,
                column.property.as_str(),
            )?;
            if !narrowed.is_empty() {
                scan_object_contract_column(
                    engine,
                    &table.table,
                    &root.table,
                    column,
                    &narrowed,
                    previous_catalog,
                    candidate_catalog,
                )?;
            }
        }
    }
    Ok(())
}

fn check_tightened_link_values<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    for table in &candidate.schema().link_tables {
        let Some(previous_table) = previous
            .schema()
            .link_table(table.source.as_str(), table.link.as_str())
        else {
            continue;
        };
        let root = candidate
            .tables(table.source.as_str())?
            .first()
            .copied()
            .ok_or_else(|| CellError::UnknownObjectType(table.source.as_str().to_owned()))?;
        for column in &table.columns {
            if !previous_table
                .columns
                .iter()
                .any(|old| old.property == column.property)
            {
                continue;
            }
            let narrowed = tightened_link_types(
                previous,
                candidate,
                previous_catalog,
                candidate_catalog,
                &table.table,
                table.link.as_str(),
                column.property.as_str(),
            )?;
            if !narrowed.is_empty() {
                scan_link_contract_column(
                    engine,
                    &table.table,
                    &root.table,
                    table.link.as_str(),
                    column,
                    &narrowed,
                    previous_catalog,
                    candidate_catalog,
                )?;
            }
        }
    }
    Ok(())
}

fn check_candidate_value(
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
    object_type: &str,
    link_id: Option<&str>,
    property: &str,
    value: &RuntimeValue,
) -> Result<(), CellError> {
    let (previous_contract, candidate_contract) = if let Some(link_id) = link_id {
        (
            effective_link_property(previous_catalog, object_type, link_id, property)?,
            effective_link_property(candidate_catalog, object_type, link_id, property)?,
        )
    } else {
        (
            effective_property(previous_catalog, object_type, property)?,
            effective_property(candidate_catalog, object_type, property)?,
        )
    };
    let previous_contract = previous_contract.ok_or_else(|| {
        refused(format!(
            "installed effective contract for `{object_type}.{property}` is missing"
        ))
    })?;
    let candidate_contract = candidate_contract.ok_or_else(|| {
        refused(format!(
            "candidate effective contract for `{object_type}.{property}` is missing"
        ))
    })?;
    candidate_contract
        .check_runtime_value_from(previous_contract, value)
        .map_err(|error| {
            refused(format!(
                "stored value violates candidate contract for `{object_type}.{property}`: {error:?}"
            ))
        })
}

fn validate_scan_size(
    scanned: usize,
    returned: usize,
    owner: &str,
    property: &str,
) -> Result<(), CellError> {
    if returned > CONTRACT_SCAN_ROW_LIMIT.saturating_sub(scanned) {
        return Err(refused(format!(
            "validating live data for `{owner}.{property}` would exceed the 4,096-row migration scan limit; refusing an incomplete live-data check"
        )));
    }
    Ok(())
}

fn scan_page_limit(scanned: usize) -> usize {
    CONTRACT_SCAN_PAGE_ROWS.min(
        CONTRACT_SCAN_ROW_LIMIT
            .saturating_sub(scanned)
            .saturating_add(1),
    )
}

#[derive(Debug, Clone)]
struct ObjectCursor {
    world: String,
    id: String,
}

#[derive(Debug, Clone)]
struct LinkCursor {
    world: String,
    source: String,
    target: String,
}

#[derive(Debug, Clone)]
struct MigrationCursor {
    rowid: i64,
}

fn scan_object_contract_column<E: SqlEngine>(
    engine: &E,
    table: &str,
    root_table: &str,
    column: &PropertyColumn,
    object_types: &[String],
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    let mut scanned = 0usize;
    for names in object_types.chunks(90) {
        let mut cursor: Option<ObjectCursor> = None;
        loop {
            let rows = query_object_contract_page(
                engine,
                table,
                root_table,
                column,
                names,
                cursor.as_ref(),
                scan_page_limit(scanned),
            )?;
            if rows.is_empty() {
                break;
            }
            validate_scan_size(scanned, rows.len(), table, column.property.as_str())?;
            scanned = scanned.saturating_add(rows.len());
            let mut last = None;
            for values in rows {
                let row = SqlRow(values);
                let object_type: String = row.get(0)?;
                let world: String = row.get(1)?;
                let id: String = row.get(2)?;
                let value = if column.column.is_some() {
                    crate::codec::decode(table, column, row.get_ref(3)?)?
                } else {
                    let text: Option<String> = row.get(3)?;
                    overflow_property_value(text.as_deref(), column.property.as_str())?
                };
                check_candidate_value(
                    previous_catalog,
                    candidate_catalog,
                    &object_type,
                    None,
                    column.property.as_str(),
                    &value,
                )?;
                last = Some(ObjectCursor { world, id });
            }
            cursor = last;
        }
    }
    Ok(())
}

fn query_object_contract_page<E: SqlEngine>(
    engine: &E,
    table: &str,
    root_table: &str,
    column: &PropertyColumn,
    object_types: &[String],
    cursor: Option<&ObjectCursor>,
    limit: usize,
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let type_binds = vec!["?"; object_types.len()].join(", ");
    let value_projection = contract_value_projection(column, "p");
    let mut sql = format!(
        "SELECT r.{}, p.{}, p.{}, {value_projection} FROM {} AS p JOIN {} AS r ON p.{} = r.{} AND p.{} = r.{} WHERE r.{} IN ({type_binds})",
        quote_identifier(TYPE_COLUMN),
        quote_identifier(WORLD_COLUMN),
        quote_identifier(ID_COLUMN),
        quote_identifier(table),
        quote_identifier(root_table),
        quote_identifier(WORLD_COLUMN),
        quote_identifier(WORLD_COLUMN),
        quote_identifier(ID_COLUMN),
        quote_identifier(ID_COLUMN),
        quote_identifier(TYPE_COLUMN),
    );
    let mut bindings: Vec<SqlValue> = object_types.iter().cloned().map(SqlValue::from).collect();
    if let Some(cursor) = cursor {
        let world = bindings.len().saturating_add(1);
        let id = world.saturating_add(1);
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = write!(
            sql,
            " AND (p.{} > ?{world} OR (p.{} = ?{world} AND p.{} > ?{id}))",
            quote_identifier(WORLD_COLUMN),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(ID_COLUMN),
        );
        bindings.push(SqlValue::from(cursor.world.clone()));
        bindings.push(SqlValue::from(cursor.id.clone()));
    }
    let limit_index = bindings.len().saturating_add(1);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "fmt::Write for String is infallible; its trait signature still returns Result"
    )]
    let _ = write!(
        sql,
        " ORDER BY p.{}, p.{} LIMIT ?{limit_index}",
        quote_identifier(WORLD_COLUMN),
        quote_identifier(ID_COLUMN),
    );
    bindings.push(SqlValue::from(i64::try_from(limit).unwrap_or(i64::MAX)));
    engine.query(&sql, &bindings)
}

fn overflow_property_value(text: Option<&str>, property: &str) -> Result<RuntimeValue, CellError> {
    let properties: BTreeMap<String, RuntimeValue> = text
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    Ok(properties
        .get(property)
        .cloned()
        .unwrap_or(RuntimeValue::Null))
}

fn contract_value_projection(column: &PropertyColumn, alias: &str) -> String {
    let column_name = column
        .column
        .as_deref()
        .map_or_else(|| quote_identifier(OVERFLOW_COLUMN), quote_identifier);
    let qualified = format!("{alias}.{column_name}");
    if column.column.is_some() && column.encoding == ValueEncoding::Integer {
        format!(
            "CASE WHEN typeof({qualified}) = 'integer' THEN CAST({qualified} AS TEXT) ELSE {qualified} END"
        )
    } else {
        qualified
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "link scan requires both catalogs and the projected source layout"
)]
fn scan_link_contract_column<E: SqlEngine>(
    engine: &E,
    table: &str,
    root_table: &str,
    link_id: &str,
    column: &PropertyColumn,
    object_types: &[String],
    previous_catalog: &EffectiveCatalog,
    candidate_catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    let mut scanned = 0usize;
    for names in object_types.chunks(90) {
        let mut cursor: Option<LinkCursor> = None;
        loop {
            let rows = query_link_contract_page(
                engine,
                table,
                root_table,
                column,
                names,
                cursor.as_ref(),
                scan_page_limit(scanned),
            )?;
            if rows.is_empty() {
                break;
            }
            validate_scan_size(scanned, rows.len(), table, column.property.as_str())?;
            scanned = scanned.saturating_add(rows.len());
            let mut last = None;
            for values in rows {
                let row = SqlRow(values);
                let object_type: String = row.get(0)?;
                let world: String = row.get(1)?;
                let source: String = row.get(2)?;
                let target: String = row.get(3)?;
                let value = if column.column.is_some() {
                    crate::codec::decode(table, column, row.get_ref(4)?)?
                } else {
                    let text: Option<String> = row.get(4)?;
                    overflow_property_value(text.as_deref(), column.property.as_str())?
                };
                check_candidate_value(
                    previous_catalog,
                    candidate_catalog,
                    &object_type,
                    Some(link_id),
                    column.property.as_str(),
                    &value,
                )?;
                last = Some(LinkCursor {
                    world,
                    source,
                    target,
                });
            }
            cursor = last;
        }
    }
    Ok(())
}

fn query_link_contract_page<E: SqlEngine>(
    engine: &E,
    table: &str,
    root_table: &str,
    column: &PropertyColumn,
    object_types: &[String],
    cursor: Option<&LinkCursor>,
    limit: usize,
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let type_binds = vec!["?"; object_types.len()].join(", ");
    let value_projection = contract_value_projection(column, "p");
    let mut sql = format!(
        "SELECT r.{}, p.{}, p.{}, p.{}, {value_projection} FROM {} AS p JOIN {} AS r ON p.{} = r.{} AND p.{} = r.{} WHERE r.{} IN ({type_binds})",
        quote_identifier(TYPE_COLUMN),
        quote_identifier(WORLD_COLUMN),
        quote_identifier(SOURCE_COLUMN),
        quote_identifier(TARGET_COLUMN),
        quote_identifier(table),
        quote_identifier(root_table),
        quote_identifier(WORLD_COLUMN),
        quote_identifier(WORLD_COLUMN),
        quote_identifier(SOURCE_COLUMN),
        quote_identifier(ID_COLUMN),
        quote_identifier(TYPE_COLUMN),
    );
    let mut bindings: Vec<SqlValue> = object_types.iter().cloned().map(SqlValue::from).collect();
    if let Some(cursor) = cursor {
        let world = bindings.len().saturating_add(1);
        let source = world.saturating_add(1);
        let target = source.saturating_add(1);
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = write!(
            sql,
            " AND (p.{world_col} > ?{world} OR (p.{world_col} = ?{world} AND p.{source_col} > ?{source}) OR (p.{world_col} = ?{world} AND p.{source_col} = ?{source} AND p.{target_col} > ?{target}))",
            world_col = quote_identifier(WORLD_COLUMN),
            source_col = quote_identifier(SOURCE_COLUMN),
            target_col = quote_identifier(TARGET_COLUMN),
        );
        bindings.push(SqlValue::from(cursor.world.clone()));
        bindings.push(SqlValue::from(cursor.source.clone()));
        bindings.push(SqlValue::from(cursor.target.clone()));
    }
    let limit_index = bindings.len().saturating_add(1);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "fmt::Write for String is infallible; its trait signature still returns Result"
    )]
    let _ = write!(
        sql,
        " ORDER BY p.{}, p.{}, p.{} LIMIT ?{limit_index}",
        quote_identifier(WORLD_COLUMN),
        quote_identifier(SOURCE_COLUMN),
        quote_identifier(TARGET_COLUMN),
    );
    bindings.push(SqlValue::from(i64::try_from(limit).unwrap_or(i64::MAX)));
    engine.query(&sql, &bindings)
}

fn ensure_added_overflow_columns<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
) -> Result<(), CellError> {
    for table in &candidate.schema().object_tables {
        if let Some(old) = previous.schema().object_table(table.object_type.as_str())
            && !old.has_overflow
            && table.has_overflow
        {
            engine.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN {} TEXT NOT NULL DEFAULT '{{}}'",
                quote_identifier(&table.table),
                quote_identifier(OVERFLOW_COLUMN)
            ))?;
        }
    }
    for table in &candidate.schema().link_tables {
        if let Some(old) = previous
            .schema()
            .link_table(table.source.as_str(), table.link.as_str())
            && !old.has_overflow
            && table.has_overflow
        {
            engine.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN {} TEXT NOT NULL DEFAULT '{{}}'",
                quote_identifier(&table.table),
                quote_identifier(OVERFLOW_COLUMN)
            ))?;
        }
    }
    Ok(())
}

fn drop_removed_overflow_columns<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
) -> Result<(), CellError> {
    for old in &previous.schema().object_tables {
        if let Some(new) = candidate.schema().object_table(old.object_type.as_str())
            && old.has_overflow
            && !new.has_overflow
        {
            engine.execute_batch(&format!(
                "ALTER TABLE {} DROP COLUMN {}",
                quote_identifier(&old.table),
                quote_identifier(OVERFLOW_COLUMN)
            ))?;
        }
    }
    for old in &previous.schema().link_tables {
        if let Some(new) = candidate
            .schema()
            .link_table(old.source.as_str(), old.link.as_str())
            && old.has_overflow
            && !new.has_overflow
        {
            engine.execute_batch(&format!(
                "ALTER TABLE {} DROP COLUMN {}",
                quote_identifier(&old.table),
                quote_identifier(OVERFLOW_COLUMN)
            ))?;
        }
    }
    Ok(())
}

fn check_removed_types<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
) -> Result<(), CellError> {
    for table in &previous.schema().object_tables {
        if candidate
            .schema()
            .object_table(table.object_type.as_str())
            .is_none()
            && table_has_rows(engine, &table.table)?
        {
            return Err(refused(format!(
                "removing object type `{}` would orphan stored rows",
                table.object_type.as_str()
            )));
        }
    }
    Ok(())
}

fn check_removed_links<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
) -> Result<(), CellError> {
    for table in &previous.schema().link_tables {
        if candidate
            .schema()
            .link_table(table.source.as_str(), table.link.as_str())
            .is_none()
            && table_has_rows(engine, &table.table)?
        {
            return Err(refused(format!(
                "removing link `{}.{}` would orphan stored rows",
                table.source.as_str(),
                table.link.as_str()
            )));
        }
    }
    Ok(())
}

fn check_removed_properties<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
) -> Result<(), CellError> {
    for old in &previous.schema().object_tables {
        let Some(new) = candidate.schema().object_table(old.object_type.as_str()) else {
            continue;
        };
        for column in &old.columns {
            if !new
                .columns
                .iter()
                .any(|candidate| candidate.property == column.property)
                && property_has_value(engine, &old.table, &old.columns, column)?
            {
                return Err(refused(format!(
                    "removing property `{}.{}` would orphan stored values",
                    old.object_type.as_str(),
                    column.property.as_str()
                )));
            }
        }
    }
    for old in &previous.schema().link_tables {
        let Some(new) = candidate
            .schema()
            .link_table(old.source.as_str(), old.link.as_str())
        else {
            continue;
        };
        for column in &old.columns {
            if !new
                .columns
                .iter()
                .any(|candidate| candidate.property == column.property)
                && property_has_value(engine, &old.table, &old.columns, column)?
            {
                return Err(refused(format!(
                    "removing link property `{}.{}.{}` would orphan stored values",
                    old.source.as_str(),
                    old.link.as_str(),
                    column.property.as_str()
                )));
            }
        }
    }
    Ok(())
}

// Presence may safely relax; multiplicity and storage continue to match.
fn compatible_link_cardinality(
    old: norn_ir::LinkCardinality,
    new: norn_ir::LinkCardinality,
) -> bool {
    old.to == new.to && old.from == new.from && (old.required || !new.required)
}

fn check_compatible_layouts(previous: &Model, candidate: &Model) -> Result<(), CellError> {
    for old in &previous.schema().object_tables {
        let Some(new) = candidate.schema().object_table(old.object_type.as_str()) else {
            continue;
        };
        if old.parent != new.parent {
            return Err(refused(format!(
                "object type `{}` changes its parent-table layout",
                old.object_type.as_str()
            )));
        }
        check_column_placements(&old.table, &old.columns, &new.columns)?;
    }
    for old in &previous.schema().link_tables {
        let Some(new) = candidate
            .schema()
            .link_table(old.source.as_str(), old.link.as_str())
        else {
            continue;
        };
        if old.target != new.target
            || !compatible_link_cardinality(old.cardinality, new.cardinality)
        {
            return Err(refused(format!(
                "link `{}.{}` changes its target or cardinality layout",
                old.source.as_str(),
                old.link.as_str()
            )));
        }
        check_column_placements(&old.table, &old.columns, &new.columns)?;
    }
    Ok(())
}

fn check_column_placements(
    table: &str,
    previous: &[PropertyColumn],
    candidate: &[PropertyColumn],
) -> Result<(), CellError> {
    for old in previous {
        if let Some(new) = candidate.iter().find(|new| new.property == old.property)
            && old.column != new.column
        {
            return Err(refused(format!(
                "property `{}` in `{table}` changes physical storage placement",
                old.property.as_str()
            )));
        }
    }
    Ok(())
}

fn reconcile_indexes<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    mode: MigrationMode,
) -> Result<(), CellError> {
    for (old_statements, new_statements, table) in object_and_link_statements(previous, candidate) {
        let old_indexes: BTreeMap<String, &str> = old_statements
            .iter()
            .skip(1)
            .filter_map(|statement| index_name(statement).map(|name| (name, statement.as_str())))
            .collect();
        let new_indexes: BTreeMap<String, &str> = new_statements
            .iter()
            .skip(1)
            .filter_map(|statement| index_name(statement).map(|name| (name, statement.as_str())))
            .collect();
        for (name, statement) in &old_indexes {
            if let Some(new_statement) = new_indexes.get(name) {
                if *new_statement != *statement {
                    return Err(refused(format!("changing index `{name}` is not additive")));
                }
            } else {
                // Binding property indexes are optional read access paths,
                // not constraints on live data. Removing a widget query must
                // be able to retire them during ordinary readmission.
                let optional_binding_index = name
                    .starts_with(&format!("{table}/binding-property/"))
                    && statement.starts_with("CREATE INDEX IF NOT EXISTS ");
                if !optional_binding_index && table_has_rows(engine, table)? {
                    return Err(refused(format!(
                        "removing index `{name}` requires an empty table `{table}`"
                    )));
                }
                if mode == MigrationMode::Apply {
                    engine.execute_batch(&format!(
                        "DROP INDEX IF EXISTS {}",
                        quote_identifier(name)
                    ))?;
                }
            }
        }
    }
    Ok(())
}

fn object_and_link_statements<'a>(
    previous: &'a Model,
    candidate: &'a Model,
) -> Vec<(&'a [String], &'a [String], &'a str)> {
    let mut out = Vec::new();
    for old in &previous.schema().object_tables {
        if let Some(new) = candidate.schema().object_table(old.object_type.as_str()) {
            out.push((
                old.statements.as_slice(),
                new.statements.as_slice(),
                old.table.as_str(),
            ));
        }
    }
    for old in &previous.schema().link_tables {
        if let Some(new) = candidate
            .schema()
            .link_table(old.source.as_str(), old.link.as_str())
        {
            out.push((
                old.statements.as_slice(),
                new.statements.as_slice(),
                old.table.as_str(),
            ));
        }
    }
    out
}

fn index_name(statement: &str) -> Option<String> {
    let (_, after) = statement.split_once("INDEX IF NOT EXISTS ")?;
    let mut chars = after.chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut name = String::new();
    while let Some(character) = chars.next() {
        match character {
            '"' if chars.as_str().starts_with('"') => {
                name.push('"');
                let _quote = chars.next();
            }
            '"' => return Some(name),
            character => name.push(character),
        }
    }
    None
}

fn migration_rowid_alias(
    table: &str,
    previous_columns: &[PropertyColumn],
    candidate_columns: &[PropertyColumn],
) -> Result<&'static str, CellError> {
    SQLITE_ROWID_ALIASES
        .iter()
        .copied()
        .find(|alias| {
            previous_columns
                .iter()
                .chain(candidate_columns)
                .all(|column| {
                    column
                        .column
                        .as_deref()
                        .is_none_or(|name| !name.eq_ignore_ascii_case(alias))
                })
        })
        .ok_or_else(|| {
            refused(format!(
                "migration scan for `{table}` cannot address the hidden rowid because all SQLite rowid aliases are shadowed"
            ))
        })
}

/// SQLite builds a new identity-key index over all existing rows. Check the
/// same non-NULL tuples in advance, substituting defaults for columns that
/// the migration has not added yet. Each proposed index gets its own bounded
/// 4,096-row / 8 MiB validation budget. The input subquery is limited before
/// grouping, and a length-only pass admits its bytes before SQLite groups the
/// candidate tuples.
#[allow(
    clippy::too_many_lines,
    reason = "the shared identity-index proof keeps byte admission beside the bounded grouping query"
)]
fn check_added_unique_indexes<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    catalog: &EffectiveCatalog,
) -> Result<(), CellError> {
    for definition in candidate.object_types() {
        let Some(old_table) = previous.schema().object_table(definition.name.as_str()) else {
            continue;
        };
        let table = candidate.table(definition.name.as_str())?;
        for key in &definition.identity {
            let index = format!("{}/key:{}", table.table, key.name.as_str());
            if old_table
                .statements
                .iter()
                .skip(1)
                .any(|statement| index_name(statement).as_deref() == Some(index.as_str()))
            {
                continue;
            }
            let rowid_alias = migration_rowid_alias(&old_table.table, &old_table.columns, &[])?;
            let mut components = vec![
                format!(
                    "migration_candidate_rows.{}",
                    quote_identifier(SCOPE_COLUMN)
                ),
                format!(
                    "migration_candidate_rows.{}",
                    quote_identifier(WORLD_COLUMN)
                ),
            ];
            for property in &key.properties {
                let candidate_column = table
                    .columns
                    .iter()
                    .find(|column| column.property == *property)
                    .ok_or_else(|| {
                        refused(format!(
                            "identity key `{}` has no projected column `{}`",
                            key.name.as_str(),
                            property.as_str(),
                        ))
                    })?;
                let expression = if let Some(old_column) = old_table
                    .columns
                    .iter()
                    .find(|column| column.property == *property)
                {
                    format!(
                        "migration_candidate_rows.{}",
                        quote_identifier(old_column.column.as_deref().ok_or_else(|| {
                            refused("an identity key component is stored in overflow")
                        })?)
                    )
                } else {
                    let literal = declared_default(
                        candidate,
                        catalog,
                        definition.name.as_str(),
                        None,
                        candidate_column,
                    )?
                    .as_ref()
                    .map(sql_literal)
                    .transpose()?
                    .unwrap_or_else(|| "NULL".to_owned());
                    // A bare numeric literal in GROUP BY is a select-column
                    // ordinal, and ALTER TABLE applies column affinity.
                    format!(
                        "CAST({literal} AS {})",
                        candidate_column.encoding.sql_type()
                    )
                };
                components.push(expression);
            }
            let table_name = quote_identifier(&old_table.table);
            let input_limit =
                i64::try_from(CONTRACT_SCAN_ROW_LIMIT.saturating_add(1)).unwrap_or(i64::MAX);
            let byte_aliases = (0..components.len())
                .map(|index| quote_identifier(&format!("_migration_key_bytes_{index}")))
                .collect::<Vec<_>>();
            // octet_length reads stored TEXT sizes without loading their
            // payloads; a cast to BLOB would materialize oversized values.
            let byte_projection = components
                .iter()
                .zip(&byte_aliases)
                .map(|(component, alias)| {
                    format!("COALESCE(octet_length({component}), 0) AS {alias}")
                })
                .collect::<Vec<_>>();
            let bounded_metadata = format!(
                "SELECT {} FROM {table_name} AS migration_candidate_rows ORDER BY {} LIMIT ?1",
                byte_projection.join(", "),
                quote_identifier(rowid_alias),
            );
            let rowid_column = quote_identifier(rowid_alias);
            let bounded_rowids = format!(
                "SELECT {rowid_column} AS _migration_rowid FROM {table_name} ORDER BY {rowid_column} LIMIT ?1"
            );
            let row_bytes = std::iter::once(MIGRATION_SCAN_ROW_OVERHEAD_BYTES.to_string())
                .chain(byte_aliases.iter().cloned())
                .collect::<Vec<_>>()
                .join(" + ");
            let budget_sql = format!(
                "SELECT COUNT(*), COALESCE(SUM({row_bytes}), 0) FROM ({bounded_metadata}) AS migration_metadata"
            );
            let grouped_sql = format!(
                "SELECT 1 FROM ({bounded_rowids}) AS migration_candidates JOIN {table_name} AS migration_candidate_rows ON migration_candidate_rows.{rowid_column} = migration_candidates._migration_rowid WHERE {} GROUP BY {} HAVING COUNT(*) > 1 LIMIT 1",
                components
                    .iter()
                    .map(|component| format!("{component} IS NOT NULL"))
                    .collect::<Vec<_>>()
                    .join(" AND "),
                components.join(", "),
            );
            if budget_sql.len() > norn_projections::sqlite_ddl::MAX_STATEMENT_BYTES
                || grouped_sql.len() > norn_projections::sqlite_ddl::MAX_STATEMENT_BYTES
            {
                return Err(refused(
                    "identity index preflight exceeds the SQLite statement byte limit",
                ));
            }
            let (scanned, bytes): (i64, i64) =
                engine.query_row(&budget_sql, params![input_limit], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
            if scanned > i64::try_from(CONTRACT_SCAN_ROW_LIMIT).unwrap_or(i64::MAX) {
                return Err(refused(format!(
                    "validating identity index `{index}` exceeds the 4,096-row migration scan limit; refusing an incomplete live-data check"
                )));
            }
            if bytes > i64::try_from(MIGRATION_SCAN_BYTE_LIMIT).unwrap_or(i64::MAX) {
                return Err(refused(format!(
                    "validating identity index `{index}` exceeds the 8 MiB migration scan byte limit; refusing an incomplete live-data check"
                )));
            }
            if engine
                .query_optional(&grouped_sql, params![input_limit], |row| row.get::<i64>(0))?
                .is_some()
            {
                return Err(refused(format!(
                    "adding identity index `{index}` would conflict with live rows"
                )));
            }
        }
    }
    Ok(())
}

fn reconcile_object_columns<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    effective_catalog: &EffectiveCatalog,
    mode: MigrationMode,
) -> Result<(), CellError> {
    for old in &previous.schema().object_tables {
        let Some(new) = candidate.schema().object_table(old.object_type.as_str()) else {
            continue;
        };
        reconcile_columns(
            engine,
            &old.table,
            old.object_type.as_str(),
            None,
            &old.columns,
            &new.columns,
            old.has_overflow,
            new.has_overflow,
            candidate,
            effective_catalog,
            mode,
        )?;
    }
    Ok(())
}

fn reconcile_link_columns<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
    effective_catalog: &EffectiveCatalog,
    mode: MigrationMode,
) -> Result<(), CellError> {
    for old in &previous.schema().link_tables {
        let Some(new) = candidate
            .schema()
            .link_table(old.source.as_str(), old.link.as_str())
        else {
            continue;
        };
        reconcile_columns(
            engine,
            &old.table,
            &format!("{}.{}", old.source.as_str(), old.link.as_str()),
            Some((new.source.as_str(), new.link.as_str())),
            &old.columns,
            &new.columns,
            old.has_overflow,
            new.has_overflow,
            candidate,
            effective_catalog,
            mode,
        )?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the column migration operates on one projected table delta"
)]
fn reconcile_columns<E: SqlEngine>(
    engine: &E,
    table_name: &str,
    owner: &str,
    link: Option<(&str, &str)>,
    previous: &[PropertyColumn],
    candidate_columns: &[PropertyColumn],
    previous_overflow: bool,
    candidate_overflow: bool,
    candidate_model: &Model,
    effective_catalog: &EffectiveCatalog,
    mode: MigrationMode,
) -> Result<(), CellError> {
    let old_by_name: BTreeMap<&str, &PropertyColumn> = previous
        .iter()
        .map(|column| (column.property.as_str(), column))
        .collect();
    let new_by_name: BTreeMap<&str, &PropertyColumn> = candidate_columns
        .iter()
        .map(|column| (column.property.as_str(), column))
        .collect();
    for old in previous {
        match new_by_name.get(old.property.as_str()) {
            None => {
                if mode == MigrationMode::Apply
                    && let Some(column) = &old.column
                {
                    engine.execute_batch(&format!(
                        "ALTER TABLE {} DROP COLUMN {}",
                        quote_identifier(table_name),
                        quote_identifier(column)
                    ))?;
                }
            }
            Some(new) if old.encoding != new.encoding || old.nullable != new.nullable => {
                return Err(refused(format!(
                    "changing the stored representation of `{owner}.{}` is not additive",
                    old.property.as_str()
                )));
            }
            Some(_) => {}
        }
    }
    let added_overflow: Vec<&PropertyColumn> = candidate_columns
        .iter()
        .filter(|column| !old_by_name.contains_key(column.property.as_str()))
        .filter(|column| column.column.is_none())
        .collect();
    for column in &added_overflow {
        if !candidate_overflow {
            return Err(refused(format!(
                "property `{}` cannot be migrated through a missing overflow column",
                column.property.as_str()
            )));
        }
    }
    for column in candidate_columns {
        if !old_by_name.contains_key(column.property.as_str()) && column.column.is_some() {
            add_column(
                engine,
                table_name,
                owner,
                link,
                column,
                candidate_model,
                effective_catalog,
                mode,
            )?;
        }
    }
    if !added_overflow.is_empty() {
        let rowid_alias = migration_rowid_alias(table_name, previous, candidate_columns)?;
        let keys = if link.is_some() {
            vec![SOURCE_COLUMN, TARGET_COLUMN]
        } else {
            vec![ID_COLUMN]
        };
        fill_overflow_defaults(
            engine,
            table_name,
            rowid_alias,
            &keys,
            &added_overflow,
            owner,
            link,
            candidate_model,
            effective_catalog,
            previous_overflow || mode == MigrationMode::Apply,
            mode,
        )?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared column plan needs its table, contract, default encoding, and write mode"
)]
fn add_column<E: SqlEngine>(
    engine: &E,
    table_name: &str,
    owner: &str,
    link: Option<(&str, &str)>,
    column: &PropertyColumn,
    candidate_model: &Model,
    effective_catalog: &EffectiveCatalog,
    mode: MigrationMode,
) -> Result<(), CellError> {
    let name = column
        .column
        .as_deref()
        .ok_or_else(|| refused("overflow property is not a SQL column"))?;
    let default = declared_default(candidate_model, effective_catalog, owner, link, column)?;
    if !column.nullable && default.is_none() && table_has_rows(engine, table_name)? {
        return Err(refused(format!(
            "new required property `{owner}.{}` has live rows and no declared default",
            column.property.as_str()
        )));
    }
    if !column.nullable && default.is_none() {
        return Err(refused(format!(
            "new required property `{owner}.{}` needs a declared default for ALTER TABLE",
            column.property.as_str()
        )));
    }
    let default_sql = default
        .as_ref()
        .map(sql_literal)
        .transpose()?
        .map(|value| format!(" DEFAULT {value}"))
        .unwrap_or_default();
    let not_null = if column.nullable { "" } else { " NOT NULL" };
    let sql = format!(
        "ALTER TABLE {} ADD COLUMN {} {}{not_null}{default_sql}",
        quote_identifier(table_name),
        quote_identifier(name),
        column.encoding.sql_type()
    );
    if sql.len() > norn_projections::sqlite_ddl::MAX_STATEMENT_BYTES {
        return Err(refused(
            "projected column migration exceeds the SQLite statement limit",
        ));
    }
    if mode == MigrationMode::Apply {
        engine.execute_batch(&sql)?;
    }
    Ok(())
}

fn declared_default(
    model: &Model,
    effective_catalog: &EffectiveCatalog,
    owner: &str,
    link: Option<(&str, &str)>,
    column: &PropertyColumn,
) -> Result<Option<SqlValue>, CellError> {
    let effective_property = if let Some((source, link_id)) = link {
        effective_catalog
            .links_for_object_type(source)
            .and_then(|links| {
                links
                    .iter()
                    .find(|effective| effective.id.as_str() == link_id)
            })
            .and_then(|effective| {
                effective
                    .properties
                    .iter()
                    .find(|property| property.id == column.property)
            })
    } else {
        match effective_catalog.object_type_named(owner) {
            Some(Ok(object_type)) => object_type.property(column.property.as_str()),
            Some(Err(finding)) => {
                return Err(refused(format!(
                    "candidate property `{owner}.{}` is unavailable: {}",
                    column.property.as_str(),
                    finding.message
                )));
            }
            None => None,
        }
    };
    let declared = if let Some(property) = effective_property {
        property.default.as_ref()
    } else if let Some((source, link_id)) = link {
        model
            .link(source, link_id)?
            .0
            .properties
            .iter()
            .find(|property| property.id == column.property)
            .and_then(|property| property.default.as_ref())
    } else {
        model
            .declaration(owner, column.property.as_str())?
            .and_then(|property| property.default.as_ref())
    };
    let Some(default) = declared else {
        return Ok(None);
    };
    let mut value = if let Some(property) = effective_property {
        property.runtime_default_literal(&default.0)?
    } else if column.kind == norn_ir::PropertyKind::Record {
        return Err(refused(
            "record defaults require an effective typed contract",
        ));
    } else {
        RuntimeValue::from_ir(&default.0)?
    };
    if let Some(property) = effective_property {
        value = property.canonical_runtime_value(&value);
    }
    encode(owner, column, &value).map(Some)
}

fn sql_literal(value: &SqlValue) -> Result<String, CellError> {
    match value {
        SqlValue::Null => Ok("NULL".to_owned()),
        SqlValue::Integer(value) => Ok(value.to_string()),
        SqlValue::Text(value) => Ok(format!("'{}'", value.replace('\'', "''"))),
        SqlValue::Real(_) | SqlValue::Blob(_) => Err(refused(
            "projected property default has an unsupported SQLite storage class",
        )),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the overflow backfill needs the projected table and its effective candidate contract"
)]
#[allow(
    clippy::too_many_lines,
    reason = "the overflow proof must admit a bounded metadata page before fetching and applying payloads"
)]
fn fill_overflow_defaults<E: SqlEngine>(
    engine: &E,
    table_name: &str,
    rowid_alias: &str,
    keys: &[&str],
    columns: &[&PropertyColumn],
    owner: &str,
    link: Option<(&str, &str)>,
    candidate_model: &Model,
    effective_catalog: &EffectiveCatalog,
    source_has_overflow: bool,
    mode: MigrationMode,
) -> Result<(), CellError> {
    let key_projection = keys
        .iter()
        .map(|key| quote_identifier(key))
        .collect::<Vec<_>>();
    let mut cursor = None;
    let mut scanned = 0usize;
    let mut scanned_bytes = 0u64;
    let mut defaults = None;
    loop {
        let metadata_projection =
            overflow_metadata_projection(rowid_alias, keys, source_has_overflow);
        let metadata_rows = query_migration_rowid_page(
            engine,
            table_name,
            rowid_alias,
            cursor.as_ref().map(|cursor: &MigrationCursor| cursor.rowid),
            scan_page_limit(scanned),
            &metadata_projection,
        )?;
        if metadata_rows.is_empty() {
            break;
        }
        validate_scan_size(
            scanned,
            metadata_rows.len(),
            table_name,
            "overflow backfill",
        )?;

        // Defaults are candidate data too. Include their canonical JSON bytes
        // in every row's charge before fetching any stored overflow payload.
        if defaults.is_none() {
            let mut encoded_defaults = Vec::with_capacity(columns.len());
            let mut runtime_defaults = BTreeMap::new();
            for column in columns {
                let encoded =
                    declared_default(candidate_model, effective_catalog, owner, link, column)?;
                let value = encoded
                    .as_ref()
                    .map(|value| crate::codec::decode(table_name, column, value))
                    .transpose()?
                    .unwrap_or(RuntimeValue::Null);
                encoded_defaults.push(encoded);
                runtime_defaults.insert(column.property.as_str().to_owned(), value);
            }
            let encoded = canonical_json(&RuntimeValue::Object(runtime_defaults.clone()))?;
            let default_bytes = u64::try_from(encoded.len().saturating_sub(2)).unwrap_or(u64::MAX);
            defaults = Some((encoded_defaults, runtime_defaults, default_bytes));
        }
        let (encoded_defaults, runtime_defaults, default_bytes) = defaults
            .as_ref()
            .ok_or_else(|| CellError::Engine("overflow defaults were not prepared".to_owned()))?;
        let page_row_count = metadata_rows.len();

        let mut page_bytes = 0u64;
        let mut rowids = Vec::with_capacity(metadata_rows.len());
        for values in &metadata_rows {
            let metadata = SqlRow(values.clone());
            page_bytes = page_bytes.saturating_add(migration_overflow_row_bytes(
                &metadata,
                keys.len(),
                *default_bytes,
            )?);
            rowids.push(metadata.get::<i64>(0)?);
        }
        if scanned_bytes.saturating_add(page_bytes) > MIGRATION_SCAN_BYTE_LIMIT {
            return Err(refused(format!(
                "backfilling overflow properties for `{owner}` exceeds the 8 MiB migration scan byte limit; refusing an incomplete live-data check"
            )));
        }
        scanned_bytes = scanned_bytes.saturating_add(page_bytes);

        let full_projection = std::iter::once(format!(
            "{} AS _migration_rowid",
            quote_identifier(rowid_alias)
        ))
        .chain(std::iter::once(quote_identifier(WORLD_COLUMN)))
        .chain(key_projection.iter().cloned())
        .chain(std::iter::once(if source_has_overflow {
            quote_identifier(OVERFLOW_COLUMN)
        } else {
            "'{}'".to_owned()
        }))
        .collect::<Vec<_>>()
        .join(", ");
        let full_rows = query_migration_rows_by_rowid(
            engine,
            table_name,
            rowid_alias,
            &rowids,
            &full_projection,
        )?;
        if full_rows.len() != metadata_rows.len() {
            return Err(refused(format!(
                "live rows in `{table_name}` changed during the overflow migration check"
            )));
        }

        let mut last = None;
        for (metadata_values, values) in metadata_rows.into_iter().zip(full_rows) {
            let metadata = SqlRow(metadata_values);
            let row = SqlRow(values);
            let rowid: i64 = row.get(0)?;
            let metadata_rowid: i64 = metadata.get(0)?;
            if rowid != metadata_rowid {
                return Err(refused(format!(
                    "live rows in `{table_name}` changed during the overflow migration check"
                )));
            }
            let world: String = row.get(1)?;
            let keys_found: Vec<String> = (0..keys.len())
                .map(|index| row.get(index.saturating_add(2)))
                .collect::<Result<_, _>>()?;
            let overflow: Option<String> = row.get(keys.len().saturating_add(2))?;
            let expected_overflow_bytes: i64 = metadata.get(keys.len().saturating_add(2))?;
            if overflow.as_ref().map_or(0, String::len)
                != usize::try_from(expected_overflow_bytes).unwrap_or(usize::MAX)
            {
                return Err(refused(format!(
                    "overflow data in `{table_name}` changed during the migration check"
                )));
            }
            let mut properties: BTreeMap<String, RuntimeValue> = overflow
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or_default();
            let mut changed = false;
            for (column, encoded_default) in columns.iter().zip(encoded_defaults) {
                if !properties.contains_key(column.property.as_str()) {
                    if encoded_default.is_none() && !column.nullable {
                        return Err(refused(format!(
                            "new required overflow property `{owner}.{}` needs a declared default",
                            column.property.as_str()
                        )));
                    }
                    properties.insert(
                        column.property.as_str().to_owned(),
                        runtime_defaults
                            .get(column.property.as_str())
                            .cloned()
                            .unwrap_or(RuntimeValue::Null),
                    );
                    changed = true;
                }
            }
            if changed {
                let encoded = canonical_json(&RuntimeValue::Object(properties))?;
                if mode == MigrationMode::Apply {
                    let mut where_terms = vec![format!("{} = ?2", quote_identifier(WORLD_COLUMN))];
                    where_terms.extend(keys.iter().enumerate().map(|(index, key)| {
                        format!("{} = ?{}", quote_identifier(key), index.saturating_add(3))
                    }));
                    let mut bindings = vec![SqlValue::from(encoded), SqlValue::from(world)];
                    bindings.extend(keys_found.into_iter().map(SqlValue::from));
                    engine.execute(
                        &format!(
                            "UPDATE {} SET {} = ?1 WHERE {}",
                            quote_identifier(table_name),
                            quote_identifier(OVERFLOW_COLUMN),
                            where_terms.join(" AND "),
                        ),
                        &bindings,
                    )?;
                }
            }
            last = Some(MigrationCursor { rowid });
        }
        scanned = scanned.saturating_add(page_row_count);
        cursor = last;
    }
    Ok(())
}

fn overflow_metadata_projection(
    rowid_alias: &str,
    keys: &[&str],
    source_has_overflow: bool,
) -> String {
    std::iter::once(format!(
        "{} AS _migration_rowid",
        quote_identifier(rowid_alias)
    ))
    .chain(std::iter::once(format!(
        "COALESCE(octet_length({}), 0)",
        quote_identifier(WORLD_COLUMN)
    )))
    .chain(
        keys.iter()
            .map(|key| format!("COALESCE(octet_length({}), 0)", quote_identifier(key))),
    )
    .chain(std::iter::once(if source_has_overflow {
        format!(
            "COALESCE(octet_length({}), 0)",
            quote_identifier(OVERFLOW_COLUMN)
        )
    } else {
        // ALTER TABLE creates the first overflow column with '{}'. The
        // preview must charge the same bytes as the later apply pass.
        "2".to_owned()
    }))
    .collect::<Vec<_>>()
    .join(", ")
}

fn migration_overflow_row_bytes(
    row: &SqlRow,
    key_count: usize,
    default_bytes: u64,
) -> Result<u64, CellError> {
    let mut bytes = MIGRATION_SCAN_ROW_OVERHEAD_BYTES.saturating_add(default_bytes);
    for index in 1..=key_count.saturating_add(2) {
        let length: i64 = row.get(index)?;
        let length = u64::try_from(length)
            .map_err(|_error| refused("overflow migration returned an invalid byte length"))?;
        bytes = bytes.saturating_add(length);
    }
    Ok(bytes)
}

fn query_migration_rowid_page<E: SqlEngine>(
    engine: &E,
    table_name: &str,
    rowid_alias: &str,
    after_rowid: Option<i64>,
    limit: usize,
    projection: &str,
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let mut bindings = Vec::new();
    let rowid_column = quote_identifier(rowid_alias);
    let mut sql = format!("SELECT {projection} FROM {}", quote_identifier(table_name));
    if let Some(after_rowid) = after_rowid {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = write!(sql, " WHERE {rowid_column} > ?1");
        bindings.push(SqlValue::from(after_rowid));
    }
    let limit_index = bindings.len().saturating_add(1);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "fmt::Write for String is infallible; its trait signature still returns Result"
    )]
    let _ = write!(sql, " ORDER BY {rowid_column} LIMIT ?{limit_index}");
    bindings.push(SqlValue::from(i64::try_from(limit).unwrap_or(i64::MAX)));
    engine.query(&sql, &bindings)
}

fn query_migration_rows_by_rowid<E: SqlEngine>(
    engine: &E,
    table_name: &str,
    rowid_alias: &str,
    rowids: &[i64],
    projection: &str,
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let rowid_column = quote_identifier(rowid_alias);
    let placeholders = (1..=rowids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {projection} FROM {} WHERE {rowid_column} IN ({placeholders}) ORDER BY {rowid_column}",
        quote_identifier(table_name)
    );
    let bindings = rowids
        .iter()
        .copied()
        .map(SqlValue::from)
        .collect::<Vec<_>>();
    engine.query(&sql, &bindings)
}

fn table_has_rows<E: SqlEngine>(engine: &E, table: &str) -> Result<bool, CellError> {
    Ok(engine
        .query_optional(
            &format!("SELECT 1 FROM {} LIMIT 1", quote_identifier(table)),
            params![],
            |row| row.get::<i64>(0),
        )?
        .is_some())
}

fn property_has_value<E: SqlEngine>(
    engine: &E,
    table: &str,
    physical_columns: &[PropertyColumn],
    column: &PropertyColumn,
) -> Result<bool, CellError> {
    let rowid_alias = migration_rowid_alias(table, physical_columns, &[])?;
    if let Some(name) = &column.column {
        let rowid_column = quote_identifier(rowid_alias);
        let (scanned, present): (i64, i64) = engine.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(MAX(present), 0) FROM (SELECT {} IS NOT NULL AS present FROM {} ORDER BY {rowid_column} LIMIT ?1)",
                quote_identifier(name),
                quote_identifier(table),
            ),
            params![i64::try_from(CONTRACT_SCAN_ROW_LIMIT.saturating_add(1)).unwrap_or(i64::MAX)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let scanned = usize::try_from(scanned)
            .map_err(|_error| refused("property removal returned an invalid row count"))?;
        validate_scan_size(0, scanned, table, column.property.as_str())?;
        return Ok(present != 0);
    }
    let overflow = quote_identifier(OVERFLOW_COLUMN);
    let metadata_projection = format!(
        "{} AS _migration_rowid, COALESCE(octet_length({overflow}), 0)",
        quote_identifier(rowid_alias)
    );
    let mut after_rowid = None;
    let mut scanned = 0usize;
    let mut scanned_bytes = 0u64;
    loop {
        let metadata = query_migration_rowid_page(
            engine,
            table,
            rowid_alias,
            after_rowid,
            scan_page_limit(scanned),
            &metadata_projection,
        )?;
        if metadata.is_empty() {
            return Ok(false);
        }
        validate_scan_size(scanned, metadata.len(), table, column.property.as_str())?;
        scanned = scanned.saturating_add(metadata.len());
        let mut rowids = Vec::with_capacity(metadata.len());
        for values in metadata {
            let row = SqlRow(values);
            let bytes: i64 = row.get(1)?;
            let bytes = u64::try_from(bytes)
                .map_err(|_error| refused("property removal returned an invalid byte length"))?;
            scanned_bytes = scanned_bytes
                .saturating_add(MIGRATION_SCAN_ROW_OVERHEAD_BYTES)
                .saturating_add(bytes);
            rowids.push(row.get::<i64>(0)?);
        }
        if scanned_bytes > MIGRATION_SCAN_BYTE_LIMIT {
            return Err(refused(format!(
                "validating removal of `{table}.{}` exceeds the 8 MiB migration scan byte limit; refusing an incomplete live-data check",
                column.property.as_str(),
            )));
        }
        let rows = query_migration_rows_by_rowid(engine, table, rowid_alias, &rowids, &overflow)?;
        for values in rows {
            let text: Option<String> = SqlRow(values).get(0)?;
            let Some(text) = text else { continue };
            let properties: BTreeMap<String, RuntimeValue> = serde_json::from_str(&text)?;
            if properties
                .get(column.property.as_str())
                .is_some_and(|value| !value.is_null())
            {
                return Ok(true);
            }
        }
        after_rowid = rowids.last().copied();
    }
}

fn refused(message: impl Into<String>) -> CellError {
    CellError::DefinitionMigrationRefused(message.into())
}
