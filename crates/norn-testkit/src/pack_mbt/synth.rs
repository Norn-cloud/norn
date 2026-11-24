//! Deterministic IR-driven value and object synthesizer (#1656).
//!
//! Synthesizes well-typed [`JsonObject`] values directly from an
//! [`ObjectTypeDef`] and its shared [`PropertyTypeDef`] catalog, respecting
//! property types, defaults, enum sets, exact decimal scales, dates, and simple
//! lifecycle guard predicates.

use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::norn_expr::{EvaluationBudget, Expression, Value as ExprValue};
use norn_kernel::norn_ir::{
    Decimal, Lifecycle, ObjectTypeDef, Property, PropertyType, PropertyTypeDef,
};

use super::AdmittedPack;

/// Synthesizes a valid object instance for `object_type` with identity `key`
/// and optional `seed` variation, choosing the first declared initial state for
/// every lifecycle property.
#[must_use]
pub fn synthesize_object(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    key: &str,
    seed: u64,
) -> JsonObject {
    let shared: BTreeMap<&str, &PropertyTypeDef> = pack
        .property_types
        .iter()
        .map(|def| (def.name.as_str(), def))
        .collect();
    let initial_by_prop: BTreeMap<&str, &str> = object_type
        .lifecycles
        .iter()
        .filter_map(|lc| {
            lc.initial
                .first()
                .map(|state| (lc.property.as_str(), state.as_str()))
        })
        .collect();
    let key_props: BTreeSet<&str> = object_type
        .identity
        .first()
        .map(|spec| {
            spec.properties
                .iter()
                .map(norn_kernel::norn_ir::PropertyId::as_str)
                .collect()
        })
        .unwrap_or_default();

    let mut values = JsonObject::new();
    for prop in &object_type.properties {
        if prop.is_read_only() {
            continue;
        }
        let id = prop.id.as_str();
        if let Some(init) = initial_by_prop.get(id) {
            values.insert(id.to_owned(), JsonValue::String((*init).to_owned()));
            continue;
        }
        if key_props.contains(id) {
            values.insert(id.to_owned(), JsonValue::String(key.to_owned()));
            continue;
        }
        if let Some(default) = &prop.default
            && let Some(converted) = json_default_to_runtime(&default.0, prop, &shared)
        {
            values.insert(id.to_owned(), converted);
            continue;
        }
        let synthesized = synthesize_property_value(prop, &shared, key, seed);
        values.insert(id.to_owned(), synthesized);
    }
    values
}

/// Synthesizes an object that satisfies all declared `guards` for `lifecycle`
/// when possible using deterministic candidate perturbations (booleans, equal
/// string pairs, positive integers/decimals).
#[must_use]
pub fn synthesize_guard_satisfying(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    lifecycle: &Lifecycle,
    guards: &[norn_kernel::norn_ir::ZenExpression],
    key: &str,
    seed: u64,
) -> JsonObject {
    let mut base = synthesize_object(pack, object_type, key, seed);
    if guards.is_empty() || guards_hold(guards, &base) {
        return base;
    }
    apply_guard_positive_heuristics(&mut base, object_type, lifecycle, guards);
    if guards_hold(guards, &base) {
        return base;
    }
    // Try flipping every non-lifecycle boolean property on/off as a small finite search.
    let bool_props = non_lifecycle_bool_properties(object_type, lifecycle);
    let combos = 1usize
        .checked_shl(u32::try_from(bool_props.len().min(6)).unwrap_or(0))
        .unwrap_or(1);
    for mask in 0..combos {
        let mut candidate = base.clone();
        for (idx, prop_id) in bool_props.iter().take(6).enumerate() {
            let bit = (mask >> idx) & 1 == 1;
            candidate.insert(prop_id.clone(), JsonValue::Bool(bit));
        }
        if guards_hold(guards, &candidate) {
            return candidate;
        }
    }
    base
}

/// Attempts to produce a valuation of `object_type` where at least one guard in
/// `guards` evaluates to `false` (for negative guard-refusal verification).
#[must_use]
pub fn synthesize_guard_violating(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    lifecycle: &Lifecycle,
    guards: &[norn_kernel::norn_ir::ZenExpression],
    key: &str,
    seed: u64,
) -> Option<JsonObject> {
    if guards.is_empty() {
        return None;
    }
    let mut base = synthesize_object(pack, object_type, key, seed);
    if !guards_hold(guards, &base) {
        return Some(base);
    }
    let lifecycle_props: BTreeSet<&str> = object_type
        .lifecycles
        .iter()
        .map(|lc| lc.property.as_str())
        .collect();
    for prop in &object_type.properties {
        let id = prop.id.as_str();
        if prop.is_read_only() || lifecycle_props.contains(id) {
            continue;
        }
        let mut candidate = base.clone();
        match &prop.property_type {
            PropertyType::Boolean {} => {
                let flipped = !matches!(base.get(id), Some(JsonValue::Bool(true)));
                candidate.insert(id.to_owned(), JsonValue::Bool(flipped));
            }
            PropertyType::Integer { .. } => {
                candidate.insert(id.to_owned(), JsonValue::from(0_i64));
            }
            PropertyType::Decimal { .. } => {
                if let Ok(zero) = Decimal::parse_wire("0") {
                    candidate.insert(id.to_owned(), JsonValue::Number(zero));
                }
            }
            PropertyType::Text { .. } => {
                let mut empty_candidate = base.clone();
                empty_candidate.insert(id.to_owned(), JsonValue::String(String::new()));
                if !guards_hold(guards, &empty_candidate) {
                    return Some(empty_candidate);
                }
                candidate.insert(id.to_owned(), JsonValue::String(format!("mismatch-{id}")));
            }
            PropertyType::Enum { .. }
            | PropertyType::Date {}
            | PropertyType::Duration {}
            | PropertyType::Timestamp { .. }
            | PropertyType::DecimalBinding { .. }
            | PropertyType::Record { .. }
            | PropertyType::JsonExt { .. }
            | PropertyType::Ref { .. } => continue,
        }
        if !guards_hold(guards, &candidate) {
            return Some(candidate);
        }
    }
    // Also try flipping all booleans to false together.
    for prop_id in non_lifecycle_bool_properties(object_type, lifecycle) {
        base.insert(prop_id, JsonValue::Bool(false));
    }
    (!guards_hold(guards, &base)).then_some(base)
}

/// Picks a writable, non-lifecycle, non-key property and produces an alternate
/// valid value distinct from `current` (used to test `IMMUTABLE_STATE_WRITE`
/// and non-lifecycle mutations).
#[must_use]
pub fn pick_non_lifecycle_mutation(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    current: &JsonObject,
    nonce: u64,
) -> Option<(String, JsonValue)> {
    let shared: BTreeMap<&str, &PropertyTypeDef> = pack
        .property_types
        .iter()
        .map(|def| (def.name.as_str(), def))
        .collect();
    let lifecycle_props: BTreeSet<&str> = object_type
        .lifecycles
        .iter()
        .map(|lc| lc.property.as_str())
        .collect();
    let key_props: BTreeSet<&str> = object_type
        .identity
        .iter()
        .flat_map(|spec| {
            spec.properties
                .iter()
                .map(norn_kernel::norn_ir::PropertyId::as_str)
        })
        .collect();

    for prop in &object_type.properties {
        let id = prop.id.as_str();
        if prop.is_read_only() || lifecycle_props.contains(id) || key_props.contains(id) {
            continue;
        }
        let resolved = resolve_property_type(&prop.property_type, &shared);
        let alt = alternate_value(resolved, current.get(id), id, nonce);
        if let Some(value) = alt {
            return Some((id.to_owned(), value));
        }
    }
    None
}

/// Evaluates whether all `guards` return `true` on `values`.
#[must_use]
pub fn guards_hold(guards: &[norn_kernel::norn_ir::ZenExpression], values: &JsonObject) -> bool {
    let Ok(self_val) = ExprValue::try_from(&JsonValue::Object(values.clone())) else {
        return false;
    };
    let context = ExprValue::object([("self", self_val)]);
    for guard in guards {
        let Ok(compiled) = Expression::compile(guard.as_str()) else {
            return false;
        };
        let Ok(evaluated) = compiled.evaluate(&context, EvaluationBudget::default()) else {
            return false;
        };
        if !matches!(evaluated.value(), ExprValue::Bool(true)) {
            return false;
        }
    }
    true
}

fn apply_guard_positive_heuristics(
    values: &mut JsonObject,
    object_type: &ObjectTypeDef,
    lifecycle: &Lifecycle,
    guards: &[norn_kernel::norn_ir::ZenExpression],
) {
    let lifecycle_props: BTreeSet<&str> = object_type
        .lifecycles
        .iter()
        .map(|lc| lc.property.as_str())
        .chain(std::iter::once(lifecycle.property.as_str()))
        .collect();
    for prop in &object_type.properties {
        let id = prop.id.as_str();
        if prop.is_read_only() || lifecycle_props.contains(id) {
            continue;
        }
        match &prop.property_type {
            PropertyType::Boolean {} => {
                values.insert(id.to_owned(), JsonValue::Bool(true));
            }
            PropertyType::Integer { .. } => {
                values.insert(id.to_owned(), JsonValue::from(100_i64));
            }
            PropertyType::Decimal { .. } => {
                if let Ok(val) = Decimal::parse_wire("100") {
                    values.insert(id.to_owned(), JsonValue::Number(val));
                }
            }
            PropertyType::Text { .. } => {
                if matches!(values.get(id), Some(JsonValue::String(s)) if s.is_empty()) {
                    values.insert(id.to_owned(), JsonValue::String(format!("nonempty-{id}")));
                }
            }
            PropertyType::Enum { .. }
            | PropertyType::Date {}
            | PropertyType::Duration {}
            | PropertyType::Timestamp { .. }
            | PropertyType::DecimalBinding { .. }
            | PropertyType::Record { .. }
            | PropertyType::JsonExt { .. }
            | PropertyType::Ref { .. } => {}
        }
    }
    // Equalize string pairs compared with `self.a == self.b` inside guards.
    for guard in guards {
        let text = guard.as_str();
        if let Some((lhs, rhs)) = extract_self_equality_pair(text)
            && !lifecycle_props.contains(lhs.as_str())
            && !lifecycle_props.contains(rhs.as_str())
            && let Some(lhs_val) = values.get(&lhs).cloned()
        {
            values.insert(rhs, lhs_val);
        }
    }
}

fn extract_self_equality_pair(expr: &str) -> Option<(String, String)> {
    let (_, rhs_part) = expr.split_once("==")?;
    let lhs_part = expr.split("==").next()?;
    let lhs_start = lhs_part.rfind("self.")?;
    let lhs_name: String = lhs_part
        .get(lhs_start.saturating_add(5)..)?
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let rhs_start = rhs_part.find("self.")?;
    let rhs_name: String = rhs_part
        .get(rhs_start.saturating_add(5)..)?
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!lhs_name.is_empty() && !rhs_name.is_empty()).then_some((lhs_name, rhs_name))
}

fn non_lifecycle_bool_properties(
    object_type: &ObjectTypeDef,
    lifecycle: &Lifecycle,
) -> Vec<String> {
    object_type
        .properties
        .iter()
        .filter(|prop| {
            !prop.is_read_only()
                && prop.id.as_str() != lifecycle.property.as_str()
                && matches!(prop.property_type, PropertyType::Boolean {})
        })
        .map(|prop| prop.id.as_str().to_owned())
        .collect()
}

fn resolve_property_type<'a>(
    ty: &'a PropertyType,
    shared: &'a BTreeMap<&'a str, &'a PropertyTypeDef>,
) -> &'a PropertyType {
    match ty {
        PropertyType::Ref { name, .. } => shared
            .get(name.as_str())
            .map_or(ty, |def| &def.property_type),
        PropertyType::Text { .. }
        | PropertyType::Integer { .. }
        | PropertyType::Decimal { .. }
        | PropertyType::DecimalBinding { .. }
        | PropertyType::Boolean {}
        | PropertyType::Enum { .. }
        | PropertyType::Date {}
        | PropertyType::Duration {}
        | PropertyType::Timestamp { .. }
        | PropertyType::Record { .. }
        | PropertyType::JsonExt { .. } => ty,
    }
}

fn json_default_to_runtime(
    raw: &norn_kernel::norn_ir::JsonValue,
    prop: &Property,
    shared: &BTreeMap<&str, &PropertyTypeDef>,
) -> Option<JsonValue> {
    let resolved = resolve_property_type(&prop.property_type, shared);
    match (resolved, raw) {
        (
            PropertyType::Integer { .. } | PropertyType::Decimal { .. },
            norn_kernel::norn_ir::JsonValue::String(text),
        ) => Decimal::parse_wire(text).ok().map(JsonValue::Number),
        (_, norn_kernel::norn_ir::JsonValue::Bool(b)) => Some(JsonValue::Bool(*b)),
        (_, norn_kernel::norn_ir::JsonValue::String(s)) => Some(JsonValue::String(s.clone())),
        (_, norn_kernel::norn_ir::JsonValue::Number(n)) => Decimal::parse_wire(&n.to_string())
            .ok()
            .map(JsonValue::Number),
        (
            _,
            norn_kernel::norn_ir::JsonValue::Null
            | norn_kernel::norn_ir::JsonValue::Array(_)
            | norn_kernel::norn_ir::JsonValue::Object(_),
        ) => None,
    }
}

fn synthesize_property_value(
    prop: &Property,
    shared: &BTreeMap<&str, &PropertyTypeDef>,
    key: &str,
    seed: u64,
) -> JsonValue {
    let id = prop.id.as_str();
    let resolved = resolve_property_type(&prop.property_type, shared);
    match resolved {
        PropertyType::Text { .. } | PropertyType::Ref { .. } => {
            if id.contains("currency") {
                JsonValue::String("USD".to_owned())
            } else if id.contains("date") || id.contains("valid_from") {
                JsonValue::String("2026-01-15".to_owned())
            } else if id.contains("valid_to") {
                JsonValue::String("2026-12-31".to_owned())
            } else if id.contains("start_local") {
                JsonValue::String("09:00".to_owned())
            } else if id.contains("end_local") {
                JsonValue::String("17:00".to_owned())
            } else if id.contains("timezone") || id.contains("iana") {
                JsonValue::String("UTC".to_owned())
            } else if id.contains("tzdb") {
                JsonValue::String("2025b".to_owned())
            } else if id.contains("sha256") {
                JsonValue::String("a".repeat(64))
            } else {
                JsonValue::String(format!("{id}-{key}-{seed}"))
            }
        }
        PropertyType::Integer { .. } => {
            let n = i64::try_from(seed % 100).unwrap_or(0).saturating_add(10);
            JsonValue::from(n)
        }
        PropertyType::Decimal { .. } | PropertyType::DecimalBinding { .. } => {
            let text = format!("{}.50", (seed % 100).saturating_add(10));
            Decimal::parse_wire(&text)
                .map(JsonValue::Number)
                .unwrap_or_else(|_| JsonValue::from(10_i64))
        }
        PropertyType::Boolean {} => JsonValue::Bool(true),
        PropertyType::Enum { values } => JsonValue::String(
            values
                .first()
                .map_or_else(|| "default".to_owned(), |v| v.value.clone()),
        ),
        PropertyType::Date {} => {
            if id.contains("end") || id.contains("to") {
                JsonValue::String("2026-12-31".to_owned())
            } else {
                JsonValue::String("2026-01-15".to_owned())
            }
        }
        PropertyType::Duration {} => JsonValue::String("PT1H".to_owned()),
        PropertyType::Timestamp { .. } => JsonValue::String("2026-01-15T12:00:00Z".to_owned()),
        PropertyType::Record { .. } | PropertyType::JsonExt { .. } => {
            JsonValue::Object(JsonObject::new())
        }
    }
}

fn alternate_value(
    ty: &PropertyType,
    current: Option<&JsonValue>,
    prop_id: &str,
    nonce: u64,
) -> Option<JsonValue> {
    match ty {
        PropertyType::Text { .. } | PropertyType::Ref { .. } => {
            Some(JsonValue::String(format!("mut-{prop_id}-{nonce}")))
        }
        PropertyType::Integer { .. } => {
            let next = i64::try_from(nonce % 10_000)
                .unwrap_or(0)
                .saturating_add(101);
            Some(JsonValue::from(next))
        }
        PropertyType::Decimal { .. } | PropertyType::DecimalBinding { .. } => {
            let text = format!("{}.25", (nonce % 10_000).saturating_add(101));
            Decimal::parse_wire(&text).ok().map(JsonValue::Number)
        }
        PropertyType::Boolean {} => {
            let flipped = !matches!(current, Some(JsonValue::Bool(true)));
            Some(JsonValue::Bool(flipped))
        }
        PropertyType::Enum { values } => values
            .iter()
            .find(|v| current.and_then(JsonValue::as_str) != Some(v.value.as_str()))
            .map(|v| JsonValue::String(v.value.clone())),
        PropertyType::Date {} => Some(JsonValue::String("2026-06-20".to_owned())),
        PropertyType::Duration {} => Some(JsonValue::String("PT2H".to_owned())),
        PropertyType::Timestamp { .. } => {
            Some(JsonValue::String("2026-06-20T15:30:00Z".to_owned()))
        }
        PropertyType::Record { .. } | PropertyType::JsonExt { .. } => {
            let mut obj = JsonObject::new();
            obj.insert(
                "nonce".to_owned(),
                JsonValue::from(i64::try_from(nonce).unwrap_or(1)),
            );
            Some(JsonValue::Object(obj))
        }
    }
}
