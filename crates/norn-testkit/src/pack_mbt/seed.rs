//! ACTOR-STAMP-1 (#1689) seeding: an object whose type has protected
//! properties is created only through its governed create action.
//!
//! A protected property (FA-PROTECT-1) holds only the kernel's stamp of the
//! verified principal on a governed `create_only` create, so a direct write of
//! one is refused, as it must be. The generic MBT, Cedar matrix and DST create
//! such an object the way a host does: through the pack's stamping create
//! action (`commit_action`, with the action's slice and link targets seeded
//! first), the principal being the synthesized value of the protected
//! property, so the stored object equals what the harness predicted.

use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::cell_host::{
    FactKey, FactMutation, FactVersion, FactWrite, JsonObject, JsonValue,
};
use norn_kernel::pack_mbt_support::{
    ActionEffectProgram, IrJsonValue, ObjectTypeDef, SliceKey, UpsertWriteMode, ZenExpression,
};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, DecisionRefusal,
    DecisionTurn, TypeCatalog, World, commit_action,
};

use super::driver::{
    SeedWalkSpec, bfs_paths_to_states, collect_prefix_guards, seed_and_walk_object,
};
use super::synth::{collect_required_link_graph, synthesize_guard_satisfying, synthesize_object};
use super::{AdmittedAction, AdmittedPack, PackHost};

/// A stamped create's outcome: committed, or the kernel's refusal.
pub(crate) type Seeded<X> = Result<(), DecisionRefusal<X>>;

/// The principal a seeded create runs as when no protected value names one.
const SEED_PRINCIPAL: &str = "mbt-seed-creator";

/// Seeding is not an authorization test: every seeded call is allowed.
struct Seeder;

impl ActionAuthorizer for Seeder {
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

/// The protected properties of `object_type` and its ancestors.
pub(crate) fn protected_properties(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
) -> Vec<String> {
    let mut protected = Vec::new();
    let mut walk = Some(object_type);
    while let Some(def) = walk {
        protected.extend(
            def.properties
                .iter()
                .filter(|property| property.protected)
                .map(|property| property.id.as_str().to_owned()),
        );
        walk = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    protected
}

/// The `create_only` action creating `object_type` that stamps every one of
/// its `protected` properties from the verified principal.
fn stamping_creator<'p>(
    pack: &'p AdmittedPack,
    object_type: &ObjectTypeDef,
    protected: &[String],
) -> Option<&'p AdmittedAction> {
    pack.actions.iter().find(|action| {
        matches!(
            &action.root.effect_program,
            Some(ActionEffectProgram::UpsertInputObject { object_kind, write_mode, .. })
                if object_kind == object_type.name.as_str()
                    && matches!(write_mode, UpsertWriteMode::CreateOnly)
        ) && {
            let stamped = action.root.actor_stamped_fields();
            protected.iter().all(|property| stamped.contains(property))
        }
    })
}

/// Create `id` of `object_type` with `values` through its stamping create
/// action when the type has protected properties. `None` when it has none
/// (seed it directly); otherwise the commit's outcome, a refused create being
/// the kernel's refusal (a duplicate as [`DecisionRefusal::ObjectExists`]).
///
/// # Errors
///
/// A type with protected properties but no create action that stamps them
/// all, a fixture target that cannot be seeded, or a host failure.
pub(crate) fn seed_stamped<H: PackHost>(
    host: &mut H,
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeDef,
    id: &str,
    values: &JsonObject,
    seq: u64,
) -> Result<Option<Seeded<H::Error>>, String> {
    let protected = protected_properties(pack, object_type);
    if protected.is_empty() {
        return Ok(None);
    }
    let name = object_type.name.as_str();
    let creator = stamping_creator(pack, object_type, &protected).ok_or_else(|| {
        format!("{name} has protected properties {protected:?}, but no create_only action stamps them all")
    })?;
    let Some(ActionEffectProgram::UpsertInputObject {
        object_id_field, ..
    }) = &creator.root.effect_program
    else {
        return Err(format!("{}: not an upsert", creator.artifact_id));
    };
    let principal = protected
        .first()
        .and_then(|property| values.get(property))
        .and_then(JsonValue::as_str)
        .unwrap_or(SEED_PRINCIPAL)
        .to_owned();
    let mut input = values.clone();
    input.insert(object_id_field.clone(), JsonValue::String(id.to_owned()));
    fill_missing_action_input_fields(&creator.root.input_schema, &mut input, id, seq);
    seed_create_links_and_slice_targets(pack, catalog, creator, &mut input, host, seq)?;
    encode_action_wire_input(&creator.root.input_schema, &mut input);
    // ACTION-ACTOR-1: the stamped fields come from the principal, never the input.
    for field in creator.root.actor_stamped_fields() {
        input.remove(&field);
    }
    let key = format!("seed-{name}-{id}-{}", host.ledger_len());
    let call = ActionCall {
        action: &creator.root,
        action_artifact_id: &creator.artifact_id,
        action_version: &creator.version,
        scope: [("orgId", "mbt-org"), ("environmentId", "mbt-env")]
            .into_iter()
            .map(|(dimension, value)| (dimension.to_owned(), value.to_owned()))
            .collect(),
        actor: principal,
        input,
        idempotency_key: key.clone(),
        object_type: None,
        turn: DecisionTurn {
            id: key,
            now: host.logical_now(),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    };
    Ok(Some(match commit_action(host, catalog, &call, &Seeder) {
        Ok(ActionOutcome::Committed { .. }) => Ok(()),
        Ok(ActionOutcome::Replayed { .. }) => {
            return Err(format!("seed {name}/{id}: replayed a fresh call key"));
        }
        Err(ActionRefusal::Decision(refusal)) => Err(refusal),
        Err(ActionRefusal::WriteModeFailed { .. }) => Err(DecisionRefusal::ObjectExists {
            key: FactKey::new(name, id),
        }),
        Err(other) => {
            return Err(format!(
                "seed {name}/{id} via {}: {other}",
                creator.artifact_id
            ));
        }
    }))
}

pub(crate) fn fill_missing_action_input_fields(
    schema: &BTreeMap<String, IrJsonValue>,
    values: &mut JsonObject,
    obj_id: &str,
    seq: u64,
) {
    let Some(props) = schema.get("properties").and_then(IrJsonValue::as_object) else {
        return;
    };
    for (name, prop_schema) in props {
        if values.contains_key(name) {
            continue;
        }
        let prop_obj = prop_schema.as_object();
        let is_decimal = prop_obj
            .and_then(|obj| obj.get("nornType"))
            .and_then(IrJsonValue::as_object)
            .and_then(|nt| nt.get("kind"))
            .and_then(IrJsonValue::as_string)
            == Some("decimal");
        let synthesized = if is_decimal {
            JsonValue::String("10.00".to_owned())
        } else {
            match prop_obj
                .and_then(|obj| obj.get("type"))
                .and_then(IrJsonValue::as_string)
            {
                Some("boolean") => JsonValue::Bool(true),
                Some("integer") => JsonValue::from(10_i64),
                Some("number") => JsonValue::String("10.00".to_owned()),
                Some(_) | None => JsonValue::String(format!("{name}-{obj_id}-{seq}")),
            }
        };
        values.insert(name.clone(), synthesized);
    }
}

pub(crate) fn encode_action_wire_input(
    schema: &BTreeMap<String, IrJsonValue>,
    values: &mut JsonObject,
) {
    let Some(props) = schema.get("properties").and_then(IrJsonValue::as_object) else {
        return;
    };
    values.retain(|k, _| props.contains_key(k));
    for (name, prop_schema) in props {
        let is_decimal = prop_schema
            .as_object()
            .and_then(|obj| obj.get("nornType"))
            .and_then(IrJsonValue::as_object)
            .and_then(|nt| nt.get("kind"))
            .and_then(IrJsonValue::as_string)
            == Some("decimal");
        if is_decimal && let Some(JsonValue::Number(dec)) = values.get(name) {
            let wire = dec.to_string();
            values.insert(name.clone(), JsonValue::String(wire));
        }
    }
}

pub(crate) fn seed_create_links_and_slice_targets<H: PackHost>(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    admitted: &AdmittedAction,
    input: &mut JsonObject,
    host: &mut H,
    seq: u64,
) -> Result<(), String> {
    let mut seeded_keys: BTreeSet<(String, String)> = BTreeSet::new();
    let desired_states = parse_slice_precondition_states(&admitted.root.preconditions);

    for slice_spec in &admitted.root.slice {
        if let Some(prop_name) = slice_input_key_field(&slice_spec.key)
            && let Some(JsonValue::String(target_id)) = input.get(prop_name)
        {
            let target_ty_name = slice_spec.object_type.as_str();
            if seeded_keys.insert((target_ty_name.to_owned(), target_id.clone())) {
                seed_related_target(
                    (pack, catalog),
                    host,
                    target_ty_name,
                    target_id,
                    desired_states.get(&slice_spec.name),
                    seq,
                )?;
            }
        }
    }
    for link in &admitted.root.create_links {
        if let Some(JsonValue::String(target_id)) = input.get(&link.target_id_field) {
            let target_ty_name = link.target_type.as_str();
            if seeded_keys.insert((target_ty_name.to_owned(), target_id.clone())) {
                seed_related_target((pack, catalog), host, target_ty_name, target_id, None, seq)?;
            }
        }
    }
    bind_input_to_preconditions(admitted, input, host)
}

fn slice_input_key_field(key_spec: &SliceKey) -> Option<&str> {
    match key_spec {
        SliceKey::Property { from, property } if from == "input" => Some(property.as_str()),
        SliceKey::Property { .. }
        | SliceKey::Parameter { .. }
        | SliceKey::TriggerObject
        | SliceKey::TriggerBefore
        | SliceKey::TriggerAfter
        | SliceKey::TriggerChanges
        | SliceKey::Link { .. }
        | SliceKey::Predicate { .. } => None,
    }
}

/// Each `==` conjunct of `preconditions` as its two sides, grouping trimmed;
/// of a disjunction, its first alternative (one way to satisfy it).
fn precondition_equalities(preconditions: &[ZenExpression]) -> Vec<(String, String)> {
    let side = |text: &str| {
        text.trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim()
            .to_owned()
    };
    let mut out = Vec::new();
    for expr in preconditions {
        for conj in expr.as_str().split(" and ").flat_map(|s| s.split("&&")) {
            let first = conj.split(" or ").next().unwrap_or(conj);
            let first = first.split("||").next().unwrap_or(first);
            if let Some((lhs, rhs)) = first.split_once("==") {
                out.push((side(lhs), side(rhs)));
            }
        }
    }
    out
}

/// A precondition literal as a value: quoted text, `null`, a boolean or an integer.
fn literal(text: &str) -> Option<JsonValue> {
    let quoted = (text.starts_with('\'') && text.ends_with('\''))
        || (text.starts_with('"') && text.ends_with('"'));
    if quoted && text.len() >= 2 {
        return text
            .get(1..text.len().saturating_sub(1))
            .map(|inner| JsonValue::String(inner.to_owned()));
    }
    match text {
        "null" => Some(JsonValue::Null),
        "true" => Some(JsonValue::Bool(true)),
        "false" => Some(JsonValue::Bool(false)),
        _ => text.parse::<i64>().ok().map(JsonValue::from),
    }
}

/// The slice targets' states each precondition asks for: `slice.S.P == 'state'`.
fn parse_slice_precondition_states(
    preconditions: &[ZenExpression],
) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    for (lhs, rhs) in precondition_equalities(preconditions) {
        let Some(rest) = lhs.strip_prefix("slice.") else {
            continue;
        };
        let mut parts = rest.split('.');
        if let (Some(slice_name), Some(prop_name), Some(JsonValue::String(desired))) =
            (parts.next(), parts.next(), literal(&rhs))
            && !desired.is_empty()
        {
            out.insert(slice_name.to_owned(), (prop_name.to_owned(), desired));
        }
    }
    out
}

/// Bind the input to what the preconditions compare it with, once the slice
/// targets exist: `input.X == slice.S.P` takes the seeded target's value, and
/// `input.X == <literal>` the literal (a unit or currency that must match the
/// target's, a cumulative quantity that starts at zero).
fn bind_input_to_preconditions<H: PackHost>(
    admitted: &AdmittedAction,
    input: &mut JsonObject,
    host: &H,
) -> Result<(), String> {
    for (lhs, rhs) in precondition_equalities(&admitted.root.preconditions) {
        let (field, other) = match (lhs.strip_prefix("input."), rhs.strip_prefix("input.")) {
            (Some(field), _) => (field.to_owned(), rhs),
            (None, Some(field)) => (field.to_owned(), lhs),
            (None, None) => continue,
        };
        let value = if let Some(rest) = other.strip_prefix("slice.") {
            let mut parts = rest.split('.');
            let (Some(slice_name), Some(prop_name)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Some(spec) = admitted
                .root
                .slice
                .iter()
                .find(|spec| spec.name == slice_name)
            else {
                continue;
            };
            let Some(JsonValue::String(target_id)) =
                slice_input_key_field(&spec.key).and_then(|key| input.get(key))
            else {
                continue;
            };
            let target = FactKey::new(spec.object_type.as_str(), target_id);
            host.load(&target)
                .map_err(|err| format!("load {target}: {err}"))?
                .values
                .and_then(|values| values.get(prop_name).cloned())
        } else {
            literal(&other)
        };
        if let Some(value) = value {
            input.insert(field, value);
        }
    }
    Ok(())
}

fn seed_related_target<H: PackHost>(
    (pack, catalog): (&AdmittedPack, &TypeCatalog<'_>),
    host: &mut H,
    target_ty_name: &str,
    target_id: &str,
    desired_state: Option<&(String, String)>,
    seq: u64,
) -> Result<(), String> {
    // A target an earlier step already seeded is reused, never created twice.
    let existing = FactKey::new(target_ty_name, target_id);
    if host
        .load(&existing)
        .map_err(|err| format!("load {existing}: {err}"))?
        .exists()
    {
        return Ok(());
    }
    let target_ty = pack
        .object_types
        .iter()
        .find(|ty| ty.name.as_str() == target_ty_name)
        .ok_or_else(|| format!("Cedar fixture: related type '{target_ty_name}' not in pack"))?;
    if let Some((lc_prop, target_state)) = desired_state {
        let lifecycles = catalog.lifecycles(&target_ty.name);
        if let Some(lc) = lifecycles.values().find(|l| l.property.as_str() == lc_prop) {
            let paths = bfs_paths_to_states(lc);
            if let Some((init_state, prefix)) = paths.get(target_state) {
                let guards = collect_prefix_guards(lc, prefix);
                let mut vals =
                    synthesize_guard_satisfying(pack, target_ty, lc, &guards, target_id, seq);
                vals.insert(lc_prop.clone(), JsonValue::String(init_state.clone()));
                return seed_and_walk_object(
                    host,
                    SeedWalkSpec {
                        pack,
                        catalog,
                        object_type: target_ty,
                        prop: lc_prop,
                        id: target_id,
                        values: vals,
                        path: prefix,
                    },
                );
            }
        }
    }
    let vals = synthesize_object(pack, target_ty, target_id, seq);
    if let Some(created) = seed_stamped(host, pack, catalog, target_ty, target_id, &vals, seq)? {
        return created.map_err(|err| format!("Cedar fixture: seed {target_ty_name}: {err}"));
    }
    let key = FactKey::new(target_ty_name, target_id);
    let mut graph = collect_required_link_graph(pack, target_ty, &key, seq)?;
    graph.setup_writes.push(FactWrite {
        key,
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put { values: vals },
    });
    graph.setup_links.extend(graph.direct_links);
    host.commit(CellCommit {
        writes: graph.setup_writes,
        links: graph.setup_links,
        ..CellCommit::empty()
    })
    .map(|_| ())
    .map_err(|err| format!("Cedar fixture: seed related {target_ty_name}/{target_id}: {err}"))
}

/// The DST id of slot `slot` of target type `type_idx`.
pub(crate) fn entity_id(type_idx: usize, slot: u8) -> String {
    format!("dst-t{type_idx}-s{slot}")
}

/// A DST step that must not have written: the ledger is still `before` long.
pub(crate) fn ensure_unchanged<H: PackHost>(
    host: &H,
    before: usize,
    context: &str,
) -> Result<(), String> {
    super::driver::assert_zero_ledger_writes(host, before, "dst", context)
}
