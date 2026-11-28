//! Precondition-aware fixture seeding helpers for `pack_mbt` (#1656).
//!
//! Keeps [`super::seed`] focused on stamped-create and lifecycle fixture walks
//! while giving multi-slice, aggregate-reconciled, and `update_only` actions
//! clean, pack-agnostic fixture preparation.

use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::pack_mbt_support::{
    ActionEffectProgram, IrJsonValue, ObjectTypeDef, PropertyId, PropertyType, PropertyTypeDef,
    SliceKey, UpsertWriteMode, ZenExpression,
};
use norn_kernel::{ActionCall, ActionOutcome, DecisionTurn, TypeCatalog, World, commit_action};

use super::seed::{Seeder, encode_action_wire_input, fill_missing_action_input_fields};
use super::{AdmittedAction, AdmittedPack, PackHost};

/// Strips balanced outer parentheses around `text`, respecting string literals.
pub(super) fn strip_outer_parens(mut text: &str) -> &str {
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

const fn is_ident_char(ch: char) -> bool {
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
pub(super) fn precondition_equalities(preconditions: &[ZenExpression]) -> Vec<(String, String)> {
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
pub(super) fn literal(text: &str) -> Option<JsonValue> {
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
pub(super) fn parse_slice_precondition_states(
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

/// Unwraps `timestamp(...)`, `date(...)`, and `(expr ?? fallback)` around a
/// precondition equality operand so slice/input bindings recognize wrapped
/// field references.
pub(super) fn unwrap_operand(side: &str) -> &str {
    let mut cur = strip_outer_parens(side);
    loop {
        if let Some(inner) = cur
            .strip_prefix("timestamp(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            cur = strip_outer_parens(inner);
            continue;
        }
        if let Some(inner) = cur
            .strip_prefix("date(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            cur = strip_outer_parens(inner);
            continue;
        }
        if let Some((before, _fallback)) = cur.split_once("??") {
            cur = strip_outer_parens(before);
            continue;
        }
        return cur;
    }
}

fn extract_mul_input_field(side: &str) -> Option<&str> {
    let cur = strip_outer_parens(side);
    let (lhs, _rhs) = cur.split_once('*')?;
    unwrap_operand(lhs).strip_prefix("input.")
}

/// Binds `input.X == <literal>`, `input.X == aggregates.<name>`, and
/// `(input.X * input.Y) == aggregates.<name>` before slice targets are seeded.
/// In a fresh fixture cell with no unrelated aggregate rows, every sum/count
/// aggregate evaluates to zero.
pub(super) fn prebind_input_literals_and_aggregates(
    admitted: &AdmittedAction,
    input: &mut JsonObject,
) {
    for (raw_lhs, raw_rhs) in precondition_equalities(&admitted.root.preconditions) {
        let lhs = unwrap_operand(&raw_lhs);
        let rhs = unwrap_operand(&raw_rhs);
        if let Some(field) = lhs.strip_prefix("input.") {
            if let Some(lit) = literal(rhs) {
                input.insert(field.to_owned(), lit);
            } else if rhs.starts_with("aggregates.") {
                input.insert(field.to_owned(), JsonValue::from(0_i64));
            }
            continue;
        }
        if let Some(field) = rhs.strip_prefix("input.") {
            if let Some(lit) = literal(lhs) {
                input.insert(field.to_owned(), lit);
            } else if lhs.starts_with("aggregates.") {
                input.insert(field.to_owned(), JsonValue::from(0_i64));
            }
            continue;
        }
        if rhs.starts_with("aggregates.")
            && let Some(field) = extract_mul_input_field(&raw_lhs)
        {
            input.insert(field.to_owned(), JsonValue::from(0_i64));
        } else if lhs.starts_with("aggregates.")
            && let Some(field) = extract_mul_input_field(&raw_rhs)
        {
            input.insert(field.to_owned(), JsonValue::from(0_i64));
        }
    }
}

/// Collects `slice.<name>.<prop> == input.<field>` bindings from preconditions
/// so related slice targets are seeded with matching cross-slice keys and
/// aggregate-bound quantities.
pub(super) fn slice_input_overrides(
    preconditions: &[ZenExpression],
    input: &JsonObject,
) -> BTreeMap<String, Vec<(String, JsonValue)>> {
    let mut out: BTreeMap<String, Vec<(String, JsonValue)>> = BTreeMap::new();
    for (raw_lhs, raw_rhs) in precondition_equalities(preconditions) {
        let lhs = unwrap_operand(&raw_lhs);
        let rhs = unwrap_operand(&raw_rhs);
        let pair = if let (Some(field), Some(slice_rest)) =
            (lhs.strip_prefix("input."), rhs.strip_prefix("slice."))
        {
            Some((field, slice_rest))
        } else if let (Some(field), Some(slice_rest)) =
            (rhs.strip_prefix("input."), lhs.strip_prefix("slice."))
        {
            Some((field, slice_rest))
        } else {
            None
        };
        let Some((field, slice_rest)) = pair else {
            continue;
        };
        let mut parts = slice_rest.split('.');
        let (Some(slice_name), Some(prop_name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if let Some(val) = input.get(field) {
            out.entry(slice_name.to_owned())
                .or_default()
                .push((prop_name.to_owned(), val.clone()));
        }
    }
    out
}

fn value_compatible_with_type(
    ty: &PropertyType,
    shared: &BTreeMap<&str, &PropertyTypeDef>,
    val: &JsonValue,
) -> bool {
    match super::resolve_property_type_chain(ty, shared) {
        PropertyType::Text { .. }
        | PropertyType::Ref { .. }
        | PropertyType::Timestamp { .. }
        | PropertyType::Date {}
        | PropertyType::Duration {} => matches!(val, JsonValue::String(_)),
        PropertyType::Enum { values } => {
            matches!(val, JsonValue::String(s) if values.iter().any(|v| v.value == *s))
        }
        PropertyType::Integer { .. }
        | PropertyType::Decimal { .. }
        | PropertyType::DecimalBinding { .. } => matches!(val, JsonValue::Number(_)),
        PropertyType::Boolean {} => matches!(val, JsonValue::Bool(_)),
        PropertyType::Record { .. } | PropertyType::JsonExt { .. } => false,
    }
}

/// Applies type-checked precondition overrides onto a synthesized slice object,
/// skipping primary identity keys, lifecycle properties, and read-only fields.
pub(super) fn apply_safe_overrides(
    pack: &AdmittedPack,
    target_ty: &ObjectTypeDef,
    vals: &mut JsonObject,
    overrides: &[(String, JsonValue)],
) {
    if overrides.is_empty() {
        return;
    }
    let shared: BTreeMap<&str, &PropertyTypeDef> = pack
        .property_types
        .iter()
        .map(|def| (def.name.as_str(), def))
        .collect();
    let key_props: BTreeSet<&str> = target_ty
        .identity
        .first()
        .map_or_default(|spec| spec.properties.iter().map(PropertyId::as_str).collect());
    let lifecycle_props: BTreeSet<&str> = target_ty
        .lifecycles
        .iter()
        .map(|lc| lc.property.as_str())
        .collect();
    let chain = super::object_type_ancestors(pack, target_ty);
    for (prop_name, value) in overrides {
        if key_props.contains(prop_name.as_str()) || lifecycle_props.contains(prop_name.as_str()) {
            continue;
        }
        let prop = chain
            .iter()
            .flat_map(|def| def.properties.iter())
            .find(|p| p.id.as_str() == prop_name.as_str());
        if let Some(prop) = prop
            && !prop.is_read_only()
            && value_compatible_with_type(&prop.property_type, &shared, value)
        {
            vals.insert(prop_name.clone(), value.clone());
        }
    }
}

/// Identifies the slice index of an `update_only` action's own target object
/// when no other slice entry depends on reading a property from that slice.
pub(super) fn deferrable_update_self_slice(admitted: &AdmittedAction) -> Option<usize> {
    let Some(ActionEffectProgram::UpsertInputObject {
        object_kind,
        object_id_field,
        write_mode: UpsertWriteMode::UpdateOnly,
    }) = &admitted.root.effect_program
    else {
        return None;
    };
    admitted.root.slice.iter().enumerate().find_map(|(idx, s)| {
        let is_self = s.object_type.as_str() == object_kind.as_str()
            && matches!(
                &s.key,
                SliceKey::Property { from, property }
                    if from == "input" && property == object_id_field
            );
        if !is_self {
            return None;
        }
        let depended_on = admitted.root.slice.iter().any(|other| {
            matches!(
                &other.key,
                SliceKey::Property { from, .. } if from == &s.name
            )
        });
        (!depended_on).then_some(idx)
    })
}

/// Fallback seeder for an `update_only` target object whose laws require
/// governed links or derived fields: invokes a matching `create_only` action on
/// the same entity type using the already-bound fixture input.
pub(super) fn seed_update_target_via_creator<H: PackHost>(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    host: &mut H,
    target_ty_name: &str,
    target_id: &str,
    input: &JsonObject,
    seq: u64,
) -> Result<bool, String> {
    let mut candidates: Vec<(usize, &AdmittedAction, &str)> = pack
        .actions
        .iter()
        .filter_map(|action| {
            let Some(ActionEffectProgram::UpsertInputObject {
                object_kind,
                object_id_field,
                write_mode: UpsertWriteMode::CreateOnly,
            }) = &action.root.effect_program
            else {
                return None;
            };
            if object_kind != target_ty_name {
                return None;
            }
            let overlap = action
                .root
                .input_schema
                .get("properties")
                .and_then(IrJsonValue::as_object)
                .map_or(0, |props| {
                    props.keys().filter(|k| input.contains_key(*k)).count()
                });
            Some((overlap, action, object_id_field.as_str()))
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    for (_overlap, creator, object_id_field) in candidates {
        let mut create_input = input.clone();
        create_input.insert(
            object_id_field.to_owned(),
            JsonValue::String(target_id.to_owned()),
        );
        fill_missing_action_input_fields(
            &creator.root.input_schema,
            &mut create_input,
            target_id,
            seq,
        );
        encode_action_wire_input(&creator.root.input_schema, &mut create_input);
        for field in creator.root.actor_stamped_fields() {
            create_input.remove(&field);
        }
        let scope: BTreeMap<String, String> = creator
            .root
            .scope_required
            .iter()
            .map(|dim| (dim.clone(), format!("mbt-{dim}")))
            .collect();
        let key = format!(
            "seed-create-{target_ty_name}-{target_id}-{}",
            host.ledger_len()
        );
        let call = ActionCall {
            action: &creator.root,
            action_artifact_id: &creator.artifact_id,
            action_version: &creator.version,
            scope,
            actor: "mbt-seed-creator".to_owned(),
            approved_intent: None,
            input: create_input,
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
        if matches!(
            commit_action(host, catalog, &call, &Seeder),
            Ok(ActionOutcome::Committed { .. })
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}
