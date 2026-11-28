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
    let mut types = BTreeSet::from([plan.grain().as_str()]);
    for sum in plan.sums() {
        if let Some(link) = &sum.time_link {
            types.insert(link.target_type.as_str());
        }
    }
    if store.model().has_type("source.Connection") {
        types.insert("source.Connection");
    }
    types.into_iter().all(|name| {
        runtime
            .collection_access(host.scope(), actor, Sources, name)
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
        .iter()
        .filter(|(_, p)| !p.is_empty())
        .map(|(s, _)| s.as_str())
        .chain(
            result
                .context
                .watermarks
                .iter()
                .filter(|(_, p)| !p.is_empty())
                .map(|(s, _)| s.as_str()),
        )
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
                historical.is_some(),
                &check,
            )? {
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
