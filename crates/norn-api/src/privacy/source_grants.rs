//! DATA-SOURCES-1: source-capability checks and keyed cursor withholding
//! shared across live answers, historical answers, fork answers, and `:trace`.

use std::collections::BTreeSet;

use norn_cell::compute::QueryResult;
use norn_cell::compute::lineage::keyed_cursor_handle;
use norn_cell::{CellStore, MAIN_WORLD, ObjectRecord, StoreReader, engine::SqlEngine};
use norn_checker::metric::compute::QuestionPlan;

use crate::authz::{ReadCapability::Sources, ReadRequest};
use crate::host::DevHost;
use crate::question_routes::{QuestionRuntime, allowed};
use crate::refusal::Refusal;

/// DATA-SOURCES-1: whether `actor` holds the independent `Sources` capability
/// over an unmarked cell for every object type `plan` touches (its grain,
/// linked time targets, and `source.Connection` when present in the model).
pub(crate) fn plan_sources<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    store: &CellStore<E>,
    actor: &str,
    plan: &QuestionPlan,
) -> bool {
    let unmarked = host
        .marking_catalog(&store.read())
        .is_ok_and(|c| c.is_empty());
    if !unmarked {
        return false;
    }
    let mut types: BTreeSet<String> = BTreeSet::from([plan.grain().as_str().to_owned()]);
    for sum in plan.sums() {
        if let Some(link) = &sum.time_link {
            types.insert(link.target_type.as_str().to_owned());
        }
    }
    let reader = store.read();
    let seeds: Vec<String> = types.iter().cloned().collect();
    for ty in seeds {
        let Ok(inherited) = reader.object_type_question_source_types(MAIN_WORLD, &ty) else {
            return false;
        };
        for req in inherited {
            if req.requires_sources {
                types.insert(req.source_type);
            }
        }
    }
    if store.model().has_type("source.Connection") {
        types.insert("source.Connection".to_owned());
    }
    types.into_iter().all(|name| {
        runtime
            .collection_access(host.scope(), actor, Sources, &name)
            .is_ok_and(|access| {
                access.principal().subject_id == actor
                    && access.resource().object_type == name
                    && access.resource().id == name
                    && host.read_permitted(&ReadRequest {
                        capability: Sources,
                        access: &access,
                    })
            })
    })
}

/// DATA-SOURCES-1: row-level `Sources` check for the linked time target of one
/// evaluated grain member (`METRIC-GRAIN-LINK-1`). Fails closed on historical
/// linked targets or unreadable/ambiguous links.
fn linked_time_target_sources_granted<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    plan: &QuestionPlan,
    member_id: &str,
    historical: bool,
    check: &dyn Fn(&ObjectRecord) -> Result<bool, Refusal>,
) -> Result<bool, Refusal> {
    let Some(link) = plan.time_link() else {
        return Ok(true);
    };
    if historical {
        return Ok(false);
    }
    let Ok(Some(targets)) = reader.link_targets(
        world,
        plan.grain().as_str(),
        link.link.as_str(),
        member_id,
        2,
        65_536,
    ) else {
        return Ok(false);
    };
    let [target_id] = targets.as_slice() else {
        return Ok(false);
    };
    let Ok(Some(target_rec)) = reader.object(world, link.target_type.as_str(), target_id) else {
        return Ok(false);
    };
    check(&target_rec)
}

/// DATA-SOURCES-1: unified `Sources` check over the disclosed `source.Connection`
/// records and evaluated output member versions across `ExactCell`, IVM, OLAP,
/// fork answers, and `:trace`. Fails closed on missing or unproven records.
pub(crate) fn result_sources_granted<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    store: &CellStore<E>,
    (actor, question, plan): (&str, &str, &QuestionPlan),
    (result, historical): (&QueryResult, Option<(u64, u64)>),
) -> Result<bool, Refusal> {
    let sources: BTreeSet<&str> = result
        .context
        .source_cursors
        .keys()
        .chain(result.context.watermarks.keys())
        .map(String::as_str)
        .collect();
    if sources.is_empty() {
        return Ok(true);
    }
    if !plan_sources(host, runtime, store, actor, plan) {
        return Ok(false);
    }
    // Inside a forked store, inherited and fork-minted rows retain the
    // `MAIN_WORLD` storage label (`FORK-PIN-1`).
    let storage_world = if store.fork_pin().is_some() {
        MAIN_WORLD
    } else {
        result.context.world.as_str()
    };
    let check = |rec: &ObjectRecord| {
        allowed(host, runtime, actor, question, rec, Sources).map_err(Refusal::from_compute)
    };
    if store.model().has_type("source.Connection") {
        for source_id in sources {
            let Ok(Some(conn)) = store.connection_record_at(storage_world, source_id, historical)
            else {
                return Ok(false);
            };
            if !check(&conn)? {
                return Ok(false);
            }
        }
    }
    rows_sources_granted(
        store,
        storage_world,
        plan,
        result,
        historical.is_some(),
        &check,
    )
}

fn rows_sources_granted<E: SqlEngine>(
    store: &CellStore<E>,
    storage_world: &str,
    plan: &QuestionPlan,
    result: &QueryResult,
    historical: bool,
    check: &dyn Fn(&ObjectRecord) -> Result<bool, Refusal>,
) -> Result<bool, Refusal> {
    let grain = plan.grain().as_str();
    let reader = store.read();
    for row in result.rows.values() {
        if row.member_count.is_some() && row.members.is_empty() {
            return Ok(false);
        }
        for (id, member) in &row.members {
            let rec = match reader.object(storage_world, grain, id) {
                Ok(Some(rec)) if rec.version == member.version => rec,
                Ok(_) => match reader.object_at_version(storage_world, grain, id, member.version) {
                    Ok(Some(rec)) => rec,
                    Ok(None) | Err(_) => return Ok(false),
                },
                Err(_) => return Ok(false),
            };
            if !check(&rec)? {
                return Ok(false);
            }
            if !linked_time_target_sources_granted(
                &reader,
                storage_world,
                plan,
                id,
                historical,
                check,
            )? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// QUESTION-STEP-1 / DATA-SOURCES-1: explicit authorization check for every
/// source ID that `StaleSource.sources` might emit (required sources, cursors,
/// and watermarks, including unsynced or empty-partition sources). Fails
/// closed unless every candidate source ID is explicitly authorized for
/// `Sources` under `actor`.
pub(crate) fn step_sources_granted<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    store: &CellStore<E>,
    (actor, question, plan): (&str, &str, &QuestionPlan),
    (result, required, historical): (
        &QueryResult,
        &norn_cell::compute::RequiredSources,
        Option<(u64, u64)>,
    ),
) -> Result<bool, Refusal> {
    let candidate_sources: BTreeSet<&str> = required
        .0
        .keys()
        .chain(result.context.source_cursors.keys())
        .chain(result.context.watermarks.keys())
        .map(String::as_str)
        .collect();
    if candidate_sources.is_empty() {
        return Ok(false);
    }
    if !plan_sources(host, runtime, store, actor, plan) {
        return Ok(false);
    }
    let storage_world = if store.fork_pin().is_some() {
        MAIN_WORLD
    } else {
        result.context.world.as_str()
    };
    let check = |rec: &ObjectRecord| {
        allowed(host, runtime, actor, question, rec, Sources).map_err(Refusal::from_compute)
    };
    let has_conn_type = store.model().has_type("source.Connection");
    for source_id in candidate_sources {
        let conn = if has_conn_type {
            match store.connection_record_at(storage_world, source_id, historical) {
                Ok(Some(conn)) => conn,
                Ok(None) => ObjectRecord {
                    object_type: "source.Connection".to_owned(),
                    id: source_id.to_owned(),
                    scope: host.scope().key(),
                    version: 0,
                    values: std::collections::BTreeMap::new(),
                },
                Err(_) => return Ok(false),
            }
        } else {
            ObjectRecord {
                object_type: plan.grain().as_str().to_owned(),
                id: source_id.to_owned(),
                scope: host.scope().key(),
                version: 0,
                values: std::collections::BTreeMap::new(),
            }
        };
        if !check(&conn)? {
            return Ok(false);
        }
    }
    rows_sources_granted(
        store,
        storage_world,
        plan,
        result,
        historical.is_some(),
        &check,
    )
}

/// QUESTION-STEP-1 / PRIVACY-3: row-level question authorization that also
/// enforces inherited `RunQuestionRead` permissions on Run rows (both specific
/// rows and `ALL_MEMBERS` whole-grain engine gates).
pub(crate) fn row_allowed<E: SqlEngine>(
    (host, runtime, store): (&DevHost<E>, &dyn QuestionRuntime<E>, &CellStore<E>),
    (actor, question, world): (&str, &str, &str),
    row: &ObjectRecord,
    capability: crate::authz::ReadCapability,
) -> Result<bool, norn_cell::CellError> {
    if !allowed(host, runtime, actor, question, row, capability)? {
        return Ok(false);
    }
    let storage_world = if store.fork_pin().is_some() {
        MAIN_WORLD
    } else {
        world
    };
    let reader = store.read();
    let sources = if row.id == norn_cell::compute::ALL_MEMBERS {
        reader.object_type_question_source_types(storage_world, &row.object_type)?
    } else {
        reader.run_question_source_types(storage_world, &row.object_type, &row.id)?
    };
    for req in &sources {
        let caps: &[crate::authz::ReadCapability] = if req.requires_sources {
            &[
                crate::authz::ReadCapability::Population,
                crate::authz::ReadCapability::Values,
                Sources,
            ]
        } else {
            &[
                crate::authz::ReadCapability::Population,
                crate::authz::ReadCapability::Values,
            ]
        };
        let every = ObjectRecord {
            object_type: req.source_type.clone(),
            id: norn_cell::compute::ALL_MEMBERS.to_owned(),
            scope: host.scope().key(),
            version: 0,
            values: std::collections::BTreeMap::new(),
        };
        for &cap in caps {
            let collection_ok = runtime
                .collection_access(host.scope(), actor, cap, &req.source_type)
                .is_ok_and(|access| {
                    access.principal().subject_id == actor
                        && access.resource().object_type == req.source_type
                        && access.resource().id == req.source_type
                        && host.read_permitted(&ReadRequest {
                            capability: cap,
                            access: &access,
                        })
                });
            if !collection_ok || !allowed(host, runtime, actor, question, &every, cap)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// DATA-SOURCES-1: key raw cursors whenever the caller lacks `Sources`.
pub(crate) fn withhold_source_cursors(result: &mut QueryResult, key: &[u8], sources_granted: bool) {
    if sources_granted {
        return;
    }
    let scope = result.context.scope.clone();
    for partitions in result.context.source_cursors.values_mut() {
        for cursor in partitions.values_mut() {
            *cursor = keyed_cursor_handle(key, &scope, cursor);
        }
    }
    for partitions in result.context.watermarks.values_mut() {
        for watermark in partitions.values_mut() {
            watermark.observed = keyed_cursor_handle(key, &scope, &watermark.observed);
        }
    }
}
