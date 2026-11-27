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

/// Result of a governed stamped create together with the ledger length captured
/// immediately before the create's own `commit_action` call (after any slice or
/// link target seeding).
pub(crate) struct StampedSeedOutcome<X> {
    pub(crate) before_commit: usize,
    pub(crate) commits: usize,
    pub(crate) result: Seeded<X>,
}

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
    for def in super::object_type_ancestors(pack, object_type) {
        protected.extend(
            def.properties
                .iter()
                .filter(|property| property.protected)
                .map(|property| property.id.as_str().to_owned()),
        );
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
) -> Result<Option<StampedSeedOutcome<H::Error>>, String> {
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
    let before_commit = host.ledger_len();
    let key = format!("seed-{name}-{id}-{before_commit}");
    let call = ActionCall {
        action: &creator.root,
        action_artifact_id: &creator.artifact_id,
        action_version: &creator.version,
        scope: [("orgId", "mbt-org"), ("environmentId", "mbt-env")]
            .into_iter()
            .map(|(dimension, value)| (dimension.to_owned(), value.to_owned()))
            .collect(),
        actor: principal,
        approved_intent: None,
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
    let (result, commits) = match commit_action(host, catalog, &call, &Seeder) {
        Ok(ActionOutcome::Committed { .. }) => {
            let (synced_res, synced) =
                sync_post_stamp_fields(host, catalog, object_type, &protected, id, values)?;
            (synced_res, if synced { 2 } else { 1 })
        }
        Ok(ActionOutcome::Replayed { .. }) => {
            return Err(format!("seed {name}/{id}: replayed a fresh call key"));
        }
        Err(ActionRefusal::Decision(refusal)) => (Err(refusal), 0),
        Err(ActionRefusal::WriteModeFailed { .. }) => (
            Err(DecisionRefusal::ObjectExists {
                key: FactKey::new(name, id),
            }),
            0,
        ),
        Err(other) => {
            return Err(format!(
                "seed {name}/{id} via {}: {other}",
                creator.artifact_id
            ));
        }
    };
    Ok(Some(StampedSeedOutcome {
        before_commit,
        commits,
        result,
    }))
}

/// Applies non-protected, non-lifecycle synthesized properties (such as
/// receipt-bound fields omitted from a create action's input schema) onto a
/// freshly stamped object so guard-satisfying and guard-violating fixtures
/// hold the exact state the harness synthesized.
fn sync_post_stamp_fields<H: PackHost>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeDef,
    protected: &[String],
    id: &str,
    values: &JsonObject,
) -> Result<(Seeded<H::Error>, bool), String> {
    if object_type.immutable {
        return Ok((Ok(()), false));
    }
    let name = object_type.name.as_str();
    let fact_key = FactKey::new(name, id);
    let state = host
        .load(&fact_key)
        .map_err(|err| format!("load seeded {name}/{id}: {err}"))?;
    let Some(stored) = state.values.as_ref() else {
        return Ok((Ok(()), false));
    };
    let immutable_state = object_type.lifecycles.iter().any(|lc| {
        stored
            .get(lc.property.as_str())
            .and_then(JsonValue::as_str)
            .is_some_and(|cur| lc.immutable.iter().any(|s| s == cur))
    });
    if immutable_state {
        return Ok((Ok(()), false));
    }
    let protected_set: BTreeSet<&str> = protected.iter().map(String::as_str).collect();
    let lifecycle_props: BTreeSet<&str> = object_type
        .lifecycles
        .iter()
        .map(|lc| lc.property.as_str())
        .collect();
    let key_props: BTreeSet<&str> = object_type
        .identity
        .iter()
        .flat_map(|spec| spec.properties.iter().map(|prop| prop.as_str()))
        .collect();
    let mut changes = Vec::new();
    for (prop, val) in values {
        let prop_str = prop.as_str();
        if protected_set.contains(prop_str)
            || lifecycle_props.contains(prop_str)
            || key_props.contains(prop_str)
        {
            continue;
        }
        let current = stored.get(prop_str).unwrap_or(&JsonValue::Null);
        if current != val {
            changes.push(norn_kernel::pack_mbt_support::FactChange::Set {
                object_type: object_type.name.clone(),
                key: id.to_owned(),
                property: norn_kernel::pack_mbt_support::PropertyId(prop.clone()),
                value: val.clone(),
            });
        }
    }
    if changes.is_empty() {
        return Ok((Ok(()), false));
    }
    let slice =
        norn_kernel::load_slice(host, [fact_key]).map_err(|err| format!("load_slice: {err}"))?;
    let turn = DecisionTurn {
        id: format!("seed-sync-{name}-{id}-{}", host.ledger_len()),
        now: host.logical_now(),
        world: World::Main,
        consumes: None,
    };
    let output = norn_kernel::pack_mbt_support::DecisionOutput {
        facts: changes,
        links: Vec::new(),
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let res = norn_kernel::commit_decision(host, catalog, &slice, &turn, &output).map(|_| ());
    let committed = res.is_ok();
    Ok((res, committed))
}

/// Arguments for [`try_fire_receipt_transition_action`].
pub(crate) struct ReceiptTransitionSpec<'a> {
    pub(crate) pack: &'a AdmittedPack,
    pub(crate) catalog: &'a TypeCatalog<'a>,
    pub(crate) object_type: &'a ObjectTypeDef,
    pub(crate) prop: &'a str,
    pub(crate) id: &'a str,
    pub(crate) transition_id: &'a str,
    pub(crate) turn_nonce: u64,
}

/// If `pack` declares a `fire_transition` action with `acceptReceipt` for
/// `(object_type, prop, transition_id)` and `host` supports seeding an
/// authenticated receipt, executes that transition through `commit_action`
/// (exercising real `RECEIPT-CONSUME-1` resolution and consumption during
/// lifecycle MBT) and returns the ledger length immediately before the action
/// commit. Returns `Ok(None)` when the transition is not receipt-bound or the
/// host declines receipt seeding.
pub(crate) fn try_fire_receipt_transition_action<H: PackHost>(
    host: &mut H,
    spec: &ReceiptTransitionSpec<'_>,
) -> Result<Option<usize>, String> {
    let ReceiptTransitionSpec {
        pack,
        catalog,
        object_type,
        prop,
        id,
        transition_id,
        turn_nonce,
    } = *spec;
    let ty_name = object_type.name.as_str();
    let Some((admitted, object_id_field, binding)) = pack.actions.iter().find_map(|action| {
        let Some(ActionEffectProgram::FireTransition {
            object_id_field,
            object_kind,
            lifecycle,
            transition,
            accept_receipt: Some(binding),
            ..
        }) = &action.root.effect_program
        else {
            return None;
        };
        (object_kind == ty_name
            && lifecycle.as_str() == prop
            && transition.as_str() == transition_id)
            .then_some((action, object_id_field.as_str(), binding))
    }) else {
        return Ok(None);
    };
    let stored = host
        .load(&FactKey::new(ty_name, id))
        .map_err(|err| format!("load {ty_name}/{id} before receipt transition: {err}"))?
        .values;
    let synthesized = synthesize_object(pack, object_type, id, turn_nonce);
    let mut summary = JsonObject::new();
    for (target_prop, member) in &binding.result_bindings {
        let value = stored
            .as_ref()
            .and_then(|vals| vals.get(target_prop.as_str()))
            .filter(|val| !matches!(val, JsonValue::Null))
            .cloned()
            .or_else(|| synthesized.get(target_prop.as_str()).cloned());
        if let Some(value) = value {
            summary.insert(member.as_str().to_owned(), value);
        }
    }
    let Some(receipt_digest) =
        host.seed_accepted_receipt(id, binding.manifest_digest.as_str(), summary)?
    else {
        return Ok(None);
    };
    let before = host.ledger_len();
    let key = format!("mbt-receipt-tr-{id}-{transition_id}-{turn_nonce}-{before}");
    let input = JsonObject::from([
        (object_id_field.to_owned(), JsonValue::String(id.to_owned())),
        (
            binding.reference_field.clone(),
            JsonValue::String(receipt_digest),
        ),
    ]);
    let call = ActionCall {
        action: &admitted.root,
        action_artifact_id: &admitted.artifact_id,
        action_version: &admitted.version,
        scope: [("orgId", "mbt-org"), ("environmentId", "mbt-env")]
            .into_iter()
            .map(|(dimension, value)| (dimension.to_owned(), value.to_owned()))
            .collect(),
        actor: "mbt-receipt-actor".to_owned(),
        approved_intent: None,
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
    match commit_action(host, catalog, &call, &Seeder) {
        Ok(ActionOutcome::Committed { .. }) => Ok(Some(before)),
        Ok(ActionOutcome::Replayed { .. }) => Err(format!(
            "{ty_name}.{prop}.{transition_id}: receipt transition replayed a fresh key"
        )),
        Err(err) => Err(format!(
            "{ty_name}.{prop}.{transition_id} via {}: {err}",
            admitted.artifact_id
        )),
    }
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
    let mut seeded_slices: BTreeMap<String, FactKey> = BTreeMap::new();
    let desired_states = parse_slice_precondition_states(&admitted.root.preconditions);

    for slice_spec in &admitted.root.slice {
        let resolved_id = match &slice_spec.key {
            SliceKey::Property { from, property } if from == "input" => input
                .get(property.as_str())
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
            SliceKey::Property { from, property } => seeded_slices
                .get(from.as_str())
                .map(|parent| {
                    host.load(parent)
                        .map_err(|err| format!("load {parent}: {err}"))
                        .map(|loaded| {
                            loaded
                                .values
                                .as_ref()
                                .and_then(|values| values.get(property.as_str()))
                                .and_then(JsonValue::as_str)
                                .map(str::to_owned)
                        })
                })
                .transpose()?
                .flatten(),
            SliceKey::Parameter { .. }
            | SliceKey::TriggerObject
            | SliceKey::TriggerBefore
            | SliceKey::TriggerAfter
            | SliceKey::TriggerChanges
            | SliceKey::Link { .. }
            | SliceKey::Predicate { .. } => None,
        };
        if let Some(target_id) = resolved_id {
            let target_ty_name = slice_spec.object_type.as_str();
            if seeded_keys.insert((target_ty_name.to_owned(), target_id.clone())) {
                seed_related_target(
                    (pack, catalog),
                    host,
                    target_ty_name,
                    &target_id,
                    desired_states.get(&slice_spec.name),
                    seq,
                )?;
            }
            seeded_slices.insert(
                slice_spec.name.clone(),
                FactKey::new(target_ty_name, &target_id),
            );
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
    bind_input_to_preconditions(admitted, input, &seeded_slices, host)
}

/// Strips balanced outer parentheses around `text`, respecting string literals.
fn strip_outer_parens(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim();
        if !(trimmed.starts_with('(') && trimmed.ends_with(')')) {
            return trimmed;
        }
        let mut depth = 0_usize;
        let mut in_quote: Option<char> = None;
        let mut escaped = false;
        let mut wraps_whole = true;
        let last_idx = trimmed.len().saturating_sub(1);
        for (idx, ch) in trimmed.char_indices() {
            if let Some(q) = in_quote {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == q {
                    in_quote = None;
                }
                continue;
            }
            match ch {
                '\'' | '"' => in_quote = Some(ch),
                '(' => depth = depth.saturating_add(1),
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 && idx < last_idx {
                        wraps_whole = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if wraps_whole && depth == 0 && in_quote.is_none() {
            let Some(inner) = trimmed.get(1..last_idx) else {
                return trimmed;
            };
            text = inner;
        } else {
            return trimmed;
        }
    }
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Splits `text` on a top-level logical operator (`symbol` like `&&` or word
/// `word` like `and`) outside quotes and nested parentheses.
fn split_top_level_logic<'a>(text: &'a str, symbol: &str, word: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut start = 0_usize;
    let mut skip_until = 0_usize;
    let mut depth = 0_usize;
    let mut in_quote: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in text.char_indices() {
        if idx < skip_until {
            continue;
        }
        if let Some(q) = in_quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                in_quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                in_quote = Some(ch);
                continue;
            }
            '(' => {
                depth = depth.saturating_add(1);
                continue;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                continue;
            }
            _ => {}
        }
        if depth != 0 {
            continue;
        }
        let rest = text.get(idx..).unwrap_or("");
        let matched = if rest.starts_with(symbol) {
            Some(symbol.len())
        } else if rest.starts_with(word) {
            let prev_ok = text
                .get(..idx)
                .and_then(|before| before.chars().next_back())
                .is_some_and(|prev| !is_ident_char(prev));
            let next_ok = rest
                .get(word.len()..)
                .and_then(|after| after.chars().next())
                .is_some_and(|next| !is_ident_char(next));
            (prev_ok && next_ok).then_some(word.len())
        } else {
            None
        };
        if let Some(len) = matched {
            if let Some(part) = text.get(start..idx) {
                parts.push(part);
            }
            start = idx.saturating_add(len);
            skip_until = start;
        }
    }
    if let Some(tail) = text.get(start..) {
        parts.push(tail);
    }
    parts
}

/// Splits `text` on a single top-level `==` (ignoring `!=`, `<=`, `>=`, `===`,
/// quoted strings, and parenthesized sub-expressions).
fn split_top_level_eq(text: &str) -> Option<(&str, &str)> {
    let mut depth = 0_usize;
    let mut in_quote: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in text.char_indices() {
        if let Some(q) = in_quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                in_quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => in_quote = Some(ch),
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            '=' if depth == 0 => {
                let rest = text.get(idx..).unwrap_or("");
                let prev = text
                    .get(..idx)
                    .and_then(|before| before.chars().next_back());
                let after = rest.get(2..).and_then(|tail| tail.chars().next());
                if rest.starts_with("==")
                    && !matches!(prev, Some('!' | '=' | '<' | '>'))
                    && after != Some('=')
                {
                    let lhs = text.get(..idx)?;
                    let rhs = rest.get(2..)?;
                    return Some((lhs, rhs));
                }
            }
            _ => {}
        }
    }
    None
}

fn collect_top_level_conjuncts<'a>(text: &'a str, depth: usize, out: &mut Vec<&'a str>) {
    let stripped = strip_outer_parens(text);
    if depth >= 16 {
        out.push(stripped);
        return;
    }
    let parts = split_top_level_logic(stripped, "&&", "and");
    if parts.len() <= 1 {
        out.push(stripped);
        return;
    }
    for part in parts {
        collect_top_level_conjuncts(part, depth.saturating_add(1), out);
    }
}

/// Each `==` conjunct of `preconditions` as its two sides, grouping trimmed;
/// of a disjunction, its first alternative (one way to satisfy it).
fn precondition_equalities(preconditions: &[ZenExpression]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for expr in preconditions {
        let mut conjuncts = Vec::new();
        collect_top_level_conjuncts(expr.as_str(), 0, &mut conjuncts);
        for conj in conjuncts {
            let first = split_top_level_logic(conj, "||", "or")
                .into_iter()
                .next()
                .unwrap_or(conj);
            let first = strip_outer_parens(first);
            if let Some((lhs, rhs)) = split_top_level_eq(first) {
                out.push((
                    strip_outer_parens(lhs).to_owned(),
                    strip_outer_parens(rhs).to_owned(),
                ));
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
    seeded_slices: &BTreeMap<String, FactKey>,
    host: &H,
) -> Result<(), String> {
    for (lhs, rhs) in precondition_equalities(&admitted.root.preconditions) {
        // A precondition rooted at the input names its fields bare (`line_id == null`).
        let bare = |side: &str| {
            !side.is_empty()
                && side
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && literal(side).is_none()
        };
        let (field, other) = match (lhs.strip_prefix("input."), rhs.strip_prefix("input.")) {
            (Some(field), _) => (field.to_owned(), rhs),
            (None, Some(field)) => (field.to_owned(), lhs),
            (None, None) if bare(&lhs) && literal(&rhs).is_some() => (lhs, rhs),
            (None, None) => continue,
        };
        let value = if let Some(rest) = other.strip_prefix("slice.") {
            let mut parts = rest.split('.');
            let (Some(slice_name), Some(prop_name)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Some(target) = seeded_slices.get(slice_name) else {
                continue;
            };
            host.load(target)
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
        return created
            .result
            .map_err(|err| format!("Cedar fixture: seed {target_ty_name}: {err}"));
    }
    let key = FactKey::new(target_ty_name, target_id);
    let mut graph = collect_required_link_graph(pack, target_ty, &key, seq)?;
    stamp_link_targets(host, pack, catalog, &mut graph.setup_writes, seq)?;
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

/// ACTOR-STAMP-1 for required-link targets: each fixture write `writes` would
/// make directly of a type with protected creators is made by its governed
/// create instead (or skipped when the target already exists), and removed
/// from `writes`; every other write is kept for the direct fixture commit.
pub(crate) fn stamp_link_targets<H: PackHost>(
    host: &mut H,
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    writes: &mut Vec<FactWrite>,
    seq: u64,
) -> Result<(), String> {
    let mut kept = Vec::new();
    for write in std::mem::take(writes) {
        let protected_type = pack.object_types.iter().find(|ty| {
            ty.name == write.key.object_type && !protected_properties(pack, ty).is_empty()
        });
        let (Some(object_type), FactMutation::Put { values }) = (protected_type, &write.mutation)
        else {
            kept.push(write);
            continue;
        };
        if host
            .load(&write.key)
            .map_err(|err| format!("load {}: {err}", write.key))?
            .exists()
        {
            continue;
        }
        let key = write.key.key.clone();
        match seed_stamped(host, pack, catalog, object_type, &key, values, seq)? {
            Some(StampedSeedOutcome {
                result: Ok(()) | Err(DecisionRefusal::ObjectExists { .. }),
                ..
            }) => {}
            Some(StampedSeedOutcome {
                result: Err(refusal),
                ..
            }) => return Err(format!("seed {}: {refusal}", write.key)),
            None => kept.push(write),
        }
    }
    *writes = kept;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_slice_precondition_states, precondition_equalities};
    use norn_kernel::pack_mbt_support::ZenExpression;

    #[test]
    fn precondition_equalities_handles_quotes_parens_and_inequality() {
        let exprs = [ZenExpression(
            "(slice.a.status == 'open and ready' or slice.a.status == 'draft') \
             and input.tag == 'a == b' && input.other != 'skip' \
             && (input.x == 'ok' && input.y == 'yes')"
                .to_owned(),
        )];
        let pairs = precondition_equalities(&exprs);
        assert_eq!(
            pairs,
            vec![
                ("slice.a.status".to_owned(), "'open and ready'".to_owned()),
                ("input.tag".to_owned(), "'a == b'".to_owned()),
                ("input.x".to_owned(), "'ok'".to_owned()),
                ("input.y".to_owned(), "'yes'".to_owned()),
            ]
        );
        let states = parse_slice_precondition_states(&exprs);
        assert_eq!(
            states.get("a"),
            Some(&("status".to_owned(), "open and ready".to_owned()))
        );
    }
}
