//! Deterministic IR-driven value, object, and required-link graph synthesizer (#1656).
//!
//! Synthesizes well-typed [`JsonObject`] values directly from effective
//! [`ObjectTypeDef`], `InterfaceDef`, and shared [`PropertyTypeDef`]
//! contracts, respecting property types, defaults, enum sets, exact decimal
//! scales (including scale 0), recursive record members, cycle-safe required
//! link graphs, and transition guard predicates.

use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::cell_host::{
    FactKey, FactMutation, FactVersion, FactWrite, JsonObject, JsonValue, LinkKey, LinkMutation,
    LinkWrite,
};
use norn_kernel::pack_mbt_support::{
    Decimal, EvaluationBudget, ExprValue, Expression, IrJsonValue, Lifecycle, ObjectTypeDef,
    Property, PropertyId, PropertyType, PropertyTypeDef, ZenExpression,
};

use super::AdmittedPack;

/// Three-valued guard evaluation outcome — never conflates evaluation errors
/// with a genuine boolean `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardEval {
    /// Every guard evaluated cleanly to `true`.
    True,
    /// At least one guard evaluated cleanly to `false` (and none errored before it).
    False,
    /// A guard failed to compile, errored during evaluation, or returned a non-boolean.
    Error(String),
}

/// Synthesized target objects and links required to satisfy all
/// `cardinality.required` links of a root object before/during its creation.
#[derive(Debug, Default)]
pub struct RequiredLinkGraph {
    /// Target objects (and transitive targets) that must exist.
    pub setup_writes: Vec<FactWrite>,
    /// Transitive links between target objects.
    pub setup_links: Vec<LinkWrite>,
    /// Direct required links from the root object to its synthesized targets.
    pub direct_links: Vec<LinkWrite>,
}

/// Collects synthesized target objects and link writes for every effective
/// `cardinality.required` link on `object_type` (recursively and cycle-safely).
///
/// When a required-link cycle returns to a type already created in the same
/// atomic fixture, the edge reuses that compatible fixture node instead of
/// allocating an orphaned object with unsatisfied required links.
///
/// # Errors
///
/// Returns an error if a required link targets an object type or interface that
/// has no concrete [`ObjectTypeDef`] in `pack`.
pub fn collect_required_link_graph(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    source_key: &FactKey,
    seed: u64,
) -> Result<RequiredLinkGraph, String> {
    let mut nodes_by_type = BTreeMap::new();
    nodes_by_type.insert(object_type.name.as_str().to_owned(), source_key.clone());
    let mut state = LinkWalkState {
        pack,
        root_key: source_key,
        seed,
        counter: 0,
        nodes_by_type,
        graph: RequiredLinkGraph::default(),
    };
    collect_required_links_recursive(&mut state, object_type, source_key, true)?;
    Ok(state.graph)
}

struct LinkWalkState<'a> {
    pack: &'a AdmittedPack,
    root_key: &'a FactKey,
    seed: u64,
    counter: u64,
    nodes_by_type: BTreeMap<String, FactKey>,
    graph: RequiredLinkGraph,
}

fn collect_required_links_recursive(
    state: &mut LinkWalkState<'_>,
    object_type: &ObjectTypeDef,
    source_key: &FactKey,
    is_root: bool,
) -> Result<(), String> {
    let shared = shared_property_map(state.pack);
    let required_links = effective_required_links(state.pack, object_type);

    for (link_id, target_name, link_props) in required_links {
        let target_ty = resolve_concrete_link_target(state.pack, &target_name).ok_or_else(
            || {
                format!(
                    "required link '{}.{link_id}' targets '{target_name}', which has no concrete ObjectTypeDef in pack '{}'",
                    object_type.name.as_str(),
                    state.pack.name
                )
            },
        )?;
        let ty_key = target_ty.name.as_str().to_owned();
        let (target_key, needs_recurse) = if let Some(existing) =
            state.nodes_by_type.get(&ty_key).cloned()
            && existing != *source_key
        {
            (existing, false)
        } else {
            state.counter = state.counter.saturating_add(1);
            let target_id = format!(
                "req-{}-{link_id}-{}-{}",
                state.root_key.key, state.seed, state.counter
            );
            let key = FactKey::new(target_ty.name.as_str(), &target_id);
            let target_values = synthesize_object(
                state.pack,
                target_ty,
                &target_id,
                state.seed.wrapping_add(state.counter),
            );
            state.graph.setup_writes.push(FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: target_values,
                },
            });
            state
                .nodes_by_type
                .entry(ty_key)
                .or_insert_with(|| key.clone());
            (key, true)
        };

        let mut link_values = JsonObject::new();
        for prop in &link_props {
            if !prop.is_read_only() {
                let val = synthesize_type_value(
                    &prop.property_type,
                    prop.id.as_str(),
                    &shared,
                    &target_key.key,
                    state.seed,
                );
                link_values.insert(prop.id.as_str().to_owned(), val);
            }
        }
        let link_write = LinkWrite {
            key: LinkKey::new(source_key.clone(), link_id, target_key.clone()),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: link_values,
            },
        };
        if is_root {
            state.graph.direct_links.push(link_write);
        } else {
            state.graph.setup_links.push(link_write);
        }
        if needs_recurse {
            collect_required_links_recursive(state, target_ty, &target_key, false)?;
        }
    }
    Ok(())
}

fn effective_required_links(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
) -> Vec<(String, String, Vec<Property>)> {
    let mut by_id: BTreeMap<String, (String, bool, Vec<Property>)> = BTreeMap::new();
    let mut concrete_chain = Vec::new();
    let mut visited_types = BTreeSet::new();
    let mut walk_ty = Some(object_type);
    while let Some(def) = walk_ty {
        if !visited_types.insert(def.name.as_str().to_owned()) {
            break;
        }
        concrete_chain.push(def);
        walk_ty = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    // Pass 1: concrete `extends` chain (subtype -> ancestor) takes precedence
    // for narrowed link targets and merges inherited link properties.
    for def in &concrete_chain {
        for link in &def.links {
            by_id
                .entry(link.id.as_str().to_owned())
                .and_modify(|entry| {
                    entry.1 |= link.cardinality.required;
                    for prop in &link.properties {
                        if !entry.2.iter().any(|existing| existing.id == prop.id) {
                            entry.2.push(prop.clone());
                        }
                    }
                })
                .or_insert_with(|| {
                    (
                        link.target.as_str().to_owned(),
                        link.cardinality.required,
                        link.properties.clone(),
                    )
                });
        }
    }
    // Pass 2: interface contracts add interface-only links and tighten
    // `required`, never overriding a narrower concrete declaration.
    for def in &concrete_chain {
        for iface_name in &def.implements {
            if let Some(iface) = pack
                .interfaces
                .iter()
                .find(|i| i.name.as_str() == iface_name.as_str())
            {
                for owner in
                    iface.contract_owners(|name| pack.interfaces.iter().find(|i| i.name == *name))
                {
                    for link in &owner.links {
                        by_id
                            .entry(link.id.as_str().to_owned())
                            .and_modify(|entry| entry.1 |= link.cardinality.required)
                            .or_insert_with(|| {
                                (
                                    link.target.as_str().to_owned(),
                                    link.cardinality.required,
                                    Vec::new(),
                                )
                            });
                    }
                }
            }
        }
    }
    by_id
        .into_iter()
        .filter_map(|(id, (target, required, props))| required.then_some((id, target, props)))
        .collect()
}

fn type_matches_target(pack: &AdmittedPack, candidate: &ObjectTypeDef, target_name: &str) -> bool {
    let mut visited = BTreeSet::new();
    let mut walk = Some(candidate);
    while let Some(def) = walk {
        if !visited.insert(def.name.as_str().to_owned()) {
            break;
        }
        if def.name.as_str() == target_name {
            return true;
        }
        for iface_name in &def.implements {
            if iface_name.as_str() == target_name {
                return true;
            }
            if let Some(iface) = pack
                .interfaces
                .iter()
                .find(|i| i.name.as_str() == iface_name.as_str())
                && iface
                    .contract_owners(|name| pack.interfaces.iter().find(|i| i.name == *name))
                    .into_iter()
                    .any(|owner| owner.name.as_str() == target_name)
            {
                return true;
            }
        }
        walk = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    false
}

fn resolve_concrete_link_target<'a>(
    pack: &'a AdmittedPack,
    target_name: &str,
) -> Option<&'a ObjectTypeDef> {
    pack.object_types
        .iter()
        .find(|ty| ty.name.as_str() == target_name)
        .or_else(|| {
            pack.object_types
                .iter()
                .find(|ty| type_matches_target(pack, ty, target_name))
        })
}

/// Synthesizes a valid object instance for `object_type` with identity `key`
/// and optional `seed` variation, choosing the first declared initial state for
/// every lifecycle property and including inherited interface properties.
#[must_use]
pub fn synthesize_object(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    key: &str,
    seed: u64,
) -> JsonObject {
    let shared = shared_property_map(pack);
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
        .map_or_default(|spec| spec.properties.iter().map(PropertyId::as_str).collect());

    // DX-8: when the pack supplies positive `LAW-EXAMPLE-1` witnesses for any
    // law targeting this entity type, seed non-key, non-lifecycle properties
    // from a `Holds` witness so strict multi-field arithmetic laws (e.g.
    // `Payslip.net_pay_egp == base + additions - deductions`) hold from step 0.
    let witness_subject = positive_law_witness_for(pack, object_type.name.as_str(), seed);

    let mut values = JsonObject::new();
    for prop in effective_properties(pack, object_type) {
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
        if let Some(witness) = witness_subject
            && let Some(witness_val) = witness.get(id)
        {
            values.insert(id.to_owned(), witness_val.clone());
            continue;
        }
        if let Some(default) = prop.default
            && let Some(converted) = json_default_to_runtime(default, &prop, &shared)
        {
            values.insert(id.to_owned(), converted);
            continue;
        }
        let synthesized = synthesize_property_value(&prop, &shared, key, seed);
        values.insert(id.to_owned(), synthesized);
    }
    values
}

fn positive_law_witness_for<'a>(
    pack: &'a AdmittedPack,
    object_type_name: &str,
    seed: u64,
) -> Option<&'a JsonObject> {
    let matching_law_ids: BTreeSet<&str> = pack
        .laws
        .iter()
        .filter(|law| law.subject == object_type_name)
        .map(|law| law.id.as_str())
        .collect();
    if matching_law_ids.is_empty() {
        return None;
    }
    let positives: Vec<&JsonObject> = pack
        .law_examples
        .iter()
        .filter(|ex| {
            ex.expected == super::Verdict::Holds && matching_law_ids.contains(ex.law_id.as_str())
        })
        .map(|ex| &ex.subject)
        .collect();
    if positives.is_empty() {
        return None;
    }
    let idx = usize::try_from(seed)
        .unwrap_or(0)
        .checked_rem(positives.len())
        .unwrap_or(0);
    positives.get(idx).copied()
}

/// Synthesizes an object that satisfies all declared `guards` for `lifecycle`
/// when possible using deterministic candidate perturbations (booleans, equal
/// string pairs, non-empty strings, positive integers/decimals).
#[must_use]
pub fn synthesize_guard_satisfying(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    lifecycle: &Lifecycle,
    guards: &[ZenExpression],
    key: &str,
    seed: u64,
) -> JsonObject {
    let mut base = synthesize_object(pack, object_type, key, seed);
    if guards.is_empty() || guards_hold(guards, &base) {
        return base;
    }
    apply_guard_positive_heuristics(pack, &mut base, object_type, lifecycle, guards);
    if guards_hold(guards, &base) {
        return base;
    }
    let bool_props = non_lifecycle_bool_properties(pack, object_type, lifecycle);
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

/// Attempts to produce a valuation of `object_type` at `source_state` where at
/// least one guard in `guards` cleanly evaluates to [`GuardEval::False`] (never
/// [`GuardEval::Error`]). Returns the full violating object plus the single
/// non-lifecycle property mutation that caused the guard to become false when
/// starting from a valid base.
#[must_use]
pub fn synthesize_guard_violating_at_state(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    lifecycle: &Lifecycle,
    source_state: &str,
    guards: &[ZenExpression],
    key: &str,
    seed: u64,
) -> Option<(JsonObject, Option<(String, JsonValue)>)> {
    if guards.is_empty() {
        return None;
    }
    let shared = shared_property_map(pack);
    let mut base = synthesize_guard_satisfying(pack, object_type, lifecycle, guards, key, seed);
    base.insert(
        lifecycle.property.as_str().to_owned(),
        JsonValue::String(source_state.to_owned()),
    );
    if evaluate_guards(guards, &base) == GuardEval::False {
        return Some((base, None));
    }
    let lifecycle_props = lifecycle_property_names(object_type, lifecycle);
    for prop in effective_properties(pack, object_type) {
        let id = prop.id.as_str();
        if prop.is_read_only() || lifecycle_props.contains(id) {
            continue;
        }
        let resolved = resolve_property_type(prop.property_type, &shared);
        for candidate_val in guard_violating_candidates(resolved, base.get(id), id) {
            let mut candidate = base.clone();
            candidate.insert(id.to_owned(), candidate_val.clone());
            if evaluate_guards(guards, &candidate) == GuardEval::False {
                return Some((candidate, Some((id.to_owned(), candidate_val))));
            }
        }
    }
    let mut all_false = base;
    for prop_id in non_lifecycle_bool_properties(pack, object_type, lifecycle) {
        all_false.insert(prop_id, JsonValue::Bool(false));
    }
    (evaluate_guards(guards, &all_false) == GuardEval::False).then_some((all_false, None))
}

fn guard_violating_candidates(
    ty: &PropertyType,
    current: Option<&JsonValue>,
    id: &str,
) -> Vec<JsonValue> {
    match ty {
        PropertyType::Boolean {} => {
            let flipped = !matches!(current, Some(JsonValue::Bool(true)));
            vec![JsonValue::Bool(flipped)]
        }
        PropertyType::Integer { .. } => vec![JsonValue::from(0_i64), JsonValue::from(-1_i64)],
        PropertyType::Decimal { scale, .. } => {
            let mut vals = Vec::new();
            if let Ok(zero) = Decimal::parse_wire(&format_zero_decimal(scale.0)) {
                vals.push(JsonValue::Number(zero));
            }
            if let Ok(neg) = Decimal::parse_wire(&format_decimal(-1, scale.0, false)) {
                vals.push(JsonValue::Number(neg));
            }
            vals
        }
        PropertyType::Text { .. } => vec![
            JsonValue::String(String::new()),
            JsonValue::String(format!("mismatch-{id}")),
        ],
        PropertyType::Enum { values } => values
            .iter()
            .filter(|v| current.and_then(JsonValue::as_str) != Some(v.value.as_str()))
            .map(|v| JsonValue::String(v.value.clone()))
            .collect(),
        PropertyType::Date {}
        | PropertyType::Duration {}
        | PropertyType::Timestamp { .. }
        | PropertyType::DecimalBinding { .. }
        | PropertyType::Record { .. }
        | PropertyType::JsonExt { .. }
        | PropertyType::Ref { .. } => Vec::new(),
    }
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
    let shared = shared_property_map(pack);
    let lifecycle_props: BTreeSet<&str> = object_type
        .lifecycles
        .iter()
        .map(|lc| lc.property.as_str())
        .collect();
    let key_props: BTreeSet<&str> = object_type
        .identity
        .iter()
        .flat_map(|spec| spec.properties.iter().map(PropertyId::as_str))
        .collect();

    for prop in effective_properties(pack, object_type) {
        let id = prop.id.as_str();
        // FA-PROTECT-1: a protected value is never rewritten, so never a mutation.
        if prop.is_read_only()
            || prop.protected
            || lifecycle_props.contains(id)
            || key_props.contains(id)
        {
            continue;
        }
        let resolved = resolve_property_type(prop.property_type, &shared);
        if let Some(value) = alternate_value(resolved, &shared, current.get(id), id, nonce)
            && current.get(id) != Some(&value)
        {
            return Some((id.to_owned(), value));
        }
    }
    None
}

/// Evaluates all `guards` on `values`, separating [`GuardEval::True`],
/// [`GuardEval::False`], and [`GuardEval::Error`].
#[must_use]
pub fn evaluate_guards(guards: &[ZenExpression], values: &JsonObject) -> GuardEval {
    let self_val = match ExprValue::try_from(&JsonValue::Object(values.clone())) {
        Ok(val) => val,
        Err(err) => return GuardEval::Error(format!("self conversion failed: {err}")),
    };
    let context = ExprValue::object([("self", self_val)]);
    for guard in guards {
        let compiled = match Expression::compile(guard.as_str()) {
            Ok(expr) => expr,
            Err(err) => return GuardEval::Error(format!("compile `{}`: {err}", guard.as_str())),
        };
        let evaluated = match compiled.evaluate(&context, EvaluationBudget::default()) {
            Ok(res) => res,
            Err(err) => return GuardEval::Error(format!("eval `{}`: {err}", guard.as_str())),
        };
        match evaluated.value() {
            ExprValue::Bool(true) => {}
            ExprValue::Bool(false) => return GuardEval::False,
            other @ (ExprValue::Null
            | ExprValue::Number(_)
            | ExprValue::String(_)
            | ExprValue::Array(_)
            | ExprValue::Object(_)) => {
                return GuardEval::Error(format!(
                    "guard `{}` returned non-boolean {other:?}",
                    guard.as_str()
                ));
            }
        }
    }
    GuardEval::True
}

/// Evaluates whether all `guards` cleanly return `true` on `values`.
#[must_use]
pub fn guards_hold(guards: &[ZenExpression], values: &JsonObject) -> bool {
    evaluate_guards(guards, values) == GuardEval::True
}

fn apply_guard_positive_heuristics(
    pack: &AdmittedPack,
    values: &mut JsonObject,
    object_type: &ObjectTypeDef,
    lifecycle: &Lifecycle,
    guards: &[ZenExpression],
) {
    let shared = shared_property_map(pack);
    let lifecycle_props = lifecycle_property_names(object_type, lifecycle);
    for prop in effective_properties(pack, object_type) {
        let id = prop.id.as_str();
        if prop.is_read_only() || lifecycle_props.contains(id) {
            continue;
        }
        let resolved = resolve_property_type(prop.property_type, &shared);
        match resolved {
            PropertyType::Boolean {} => {
                values.insert(id.to_owned(), JsonValue::Bool(true));
            }
            PropertyType::Integer { .. } => {
                values.insert(id.to_owned(), JsonValue::from(100_i64));
            }
            PropertyType::Decimal { scale, .. } => {
                if let Ok(val) = Decimal::parse_wire(&format_decimal(100, scale.0, false)) {
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

fn shared_property_map(pack: &AdmittedPack) -> BTreeMap<&str, &PropertyTypeDef> {
    pack.property_types
        .iter()
        .map(|def| (def.name.as_str(), def))
        .collect()
}

fn lifecycle_property_names<'a>(
    object_type: &'a ObjectTypeDef,
    lifecycle: &'a Lifecycle,
) -> BTreeSet<&'a str> {
    object_type
        .lifecycles
        .iter()
        .map(|lc| lc.property.as_str())
        .chain(std::iter::once(lifecycle.property.as_str()))
        .collect()
}

struct EffectiveProp<'a> {
    id: &'a PropertyId,
    property_type: &'a PropertyType,
    read_only: bool,
    protected: bool,
    default: Option<&'a IrJsonValue>,
}

impl EffectiveProp<'_> {
    fn is_read_only(&self) -> bool {
        self.read_only
    }
}

fn effective_properties<'a>(
    pack: &'a AdmittedPack,
    object_type: &'a ObjectTypeDef,
) -> Vec<EffectiveProp<'a>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut walk = Some(object_type);
    while let Some(def) = walk {
        for prop in &def.properties {
            if seen.insert(prop.id.as_str()) {
                out.push(EffectiveProp {
                    id: &prop.id,
                    property_type: &prop.property_type,
                    read_only: prop.is_read_only(),
                    protected: prop.protected,
                    default: prop.default.as_ref().map(|d| &d.0),
                });
            }
        }
        for iface_name in &def.implements {
            if let Some(iface) = pack
                .interfaces
                .iter()
                .find(|i| i.name.as_str() == iface_name.as_str())
            {
                for prop in &iface.properties {
                    if seen.insert(prop.id.as_str()) {
                        out.push(EffectiveProp {
                            id: &prop.id,
                            property_type: &prop.property_type,
                            read_only: false,
                            protected: false,
                            default: None,
                        });
                    }
                }
            }
        }
        walk = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    out
}

fn non_lifecycle_bool_properties(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    lifecycle: &Lifecycle,
) -> Vec<String> {
    let shared = shared_property_map(pack);
    effective_properties(pack, object_type)
        .into_iter()
        .filter(|prop| {
            !prop.is_read_only()
                && prop.id.as_str() != lifecycle.property.as_str()
                && matches!(
                    resolve_property_type(prop.property_type, &shared),
                    PropertyType::Boolean {}
                )
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
    raw: &IrJsonValue,
    prop: &EffectiveProp<'_>,
    shared: &BTreeMap<&str, &PropertyTypeDef>,
) -> Option<JsonValue> {
    let resolved = resolve_property_type(prop.property_type, shared);
    match (resolved, raw) {
        (
            PropertyType::Integer { .. } | PropertyType::Decimal { .. },
            IrJsonValue::String(text),
        ) => Decimal::parse_wire(text).ok().map(JsonValue::Number),
        (_, IrJsonValue::Bool(b)) => Some(JsonValue::Bool(*b)),
        (_, IrJsonValue::String(s)) => Some(JsonValue::String(s.clone())),
        (_, IrJsonValue::Number(n)) => Decimal::parse_wire(&n.to_string())
            .ok()
            .map(JsonValue::Number),
        (_, IrJsonValue::Null | IrJsonValue::Array(_) | IrJsonValue::Object(_)) => None,
    }
}

fn format_zero_decimal(scale: u64) -> String {
    if scale == 0 {
        return "0".to_owned();
    }
    let digits = usize::try_from(scale.min(28)).unwrap_or(2);
    format!("0.{}", "0".repeat(digits))
}

fn format_decimal(whole: i64, scale: u64, with_fraction: bool) -> String {
    if scale == 0 {
        return whole.to_string();
    }
    let digits = usize::try_from(scale.min(28)).unwrap_or(2);
    if !with_fraction {
        return format!("{whole}.{}", "0".repeat(digits));
    }
    let mut frac = String::with_capacity(digits);
    frac.push('5');
    for _ in 1..digits {
        frac.push('0');
    }
    format!("{whole}.{frac}")
}

fn synthesize_property_value(
    prop: &EffectiveProp<'_>,
    shared: &BTreeMap<&str, &PropertyTypeDef>,
    key: &str,
    seed: u64,
) -> JsonValue {
    synthesize_type_value(prop.property_type, prop.id.as_str(), shared, key, seed)
}

fn synthesize_type_value(
    ty: &PropertyType,
    id: &str,
    shared: &BTreeMap<&str, &PropertyTypeDef>,
    key: &str,
    seed: u64,
) -> JsonValue {
    let resolved = resolve_property_type(ty, shared);
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
        PropertyType::Decimal { scale, .. } => {
            let whole = i64::try_from(seed % 100).unwrap_or(0).saturating_add(10);
            let text = format_decimal(whole, scale.0, true);
            Decimal::parse_wire(&text).map_or_else(|_| JsonValue::from(whole), JsonValue::Number)
        }
        PropertyType::DecimalBinding { .. } => {
            let whole = i64::try_from(seed % 100).unwrap_or(0).saturating_add(10);
            Decimal::parse_wire(&whole.to_string())
                .map_or_else(|_| JsonValue::from(whole), JsonValue::Number)
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
        PropertyType::Record { members } => {
            let mut obj = JsonObject::new();
            for member in members {
                let val = synthesize_type_value(
                    &member.property_type,
                    member.id.as_str(),
                    shared,
                    key,
                    seed,
                );
                obj.insert(member.id.as_str().to_owned(), val);
            }
            JsonValue::Object(obj)
        }
        PropertyType::JsonExt { .. } => JsonValue::Object(JsonObject::new()),
    }
}

fn alternate_value(
    ty: &PropertyType,
    shared: &BTreeMap<&str, &PropertyTypeDef>,
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
        PropertyType::Decimal { scale, .. } => {
            let whole = i64::try_from(nonce % 10_000)
                .unwrap_or(0)
                .saturating_add(101);
            let text = format_decimal(whole, scale.0, true);
            Decimal::parse_wire(&text).ok().map(JsonValue::Number)
        }
        PropertyType::DecimalBinding { .. } => {
            let whole = i64::try_from(nonce % 10_000)
                .unwrap_or(0)
                .saturating_add(101);
            Decimal::parse_wire(&whole.to_string())
                .ok()
                .map(JsonValue::Number)
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
        PropertyType::Record { members } => {
            let mut obj = JsonObject::new();
            for member in members {
                let val = synthesize_type_value(
                    &member.property_type,
                    member.id.as_str(),
                    shared,
                    "mut",
                    nonce.saturating_add(7),
                );
                obj.insert(member.id.as_str().to_owned(), val);
            }
            Some(JsonValue::Object(obj))
        }
        PropertyType::JsonExt { .. } => {
            let mut obj = JsonObject::new();
            obj.insert(
                "nonce".to_owned(),
                JsonValue::from(i64::try_from(nonce).unwrap_or(1)),
            );
            Some(JsonValue::Object(obj))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_mbt::{AdmittedPack, LawExampleCase, Verdict};
    use serde_json::json;

    #[test]
    fn synthesize_object_seeds_from_positive_law_example_witness() {
        let object_type: ObjectTypeDef = serde_json::from_value(json!({
            "name": "hr.Payslip",
            "properties": [
                { "id": "id", "type": { "kind": "text" } },
                { "id": "base", "type": { "kind": "integer" } },
                { "id": "deduction", "type": { "kind": "integer" } },
                { "id": "net", "type": { "kind": "integer" } }
            ],
            "identity": [{ "name": "primary", "properties": ["id"] }]
        }))
        .expect("payslip type");
        let law: norn_kernel::pack_mbt_support::LawIR = serde_json::from_value(json!({
            "lawVersion": "0.1",
            "id": "hr.Payslip.exact_net",
            "owner": "hr",
            "severity": "error",
            "rationale": "net == base - deduction",
            "provenance": { "source": "authoring_sdk", "sourcePath": "hr.ts" },
            "subject": "hr.Payslip",
            "scope": { "root": { "kind": "entity", "id": "hr.Payslip" }, "objectTypes": ["hr.Payslip"] },
            "modality": { "kind": "always" },
            "response": { "kind": "predicate", "expression": "self.net == self.base - self.deduction" }
        }))
        .expect("payslip law");
        let subject: JsonObject = serde_json::from_value(json!({
            "id": "ps_witness",
            "base": 8000,
            "deduction": 500,
            "net": 7500
        }))
        .expect("subject");
        let pack = AdmittedPack {
            name: "hr".to_owned(),
            object_types: vec![object_type.clone()],
            laws: vec![law],
            ..AdmittedPack::default()
        }
        .with_law_examples(vec![LawExampleCase {
            name: "valid payslip".to_owned(),
            law_id: "hr.Payslip.exact_net".to_owned(),
            subject,
            expected: Verdict::Holds,
        }]);

        let synthesized = synthesize_object(&pack, &object_type, "ps_generated_1", 0);
        assert_eq!(synthesized.get("id"), Some(&JsonValue::String("ps_generated_1".to_owned())));
        assert_eq!(synthesized.get("base"), Some(&JsonValue::from(8000_i64)));
        assert_eq!(synthesized.get("deduction"), Some(&JsonValue::from(500_i64)));
        assert_eq!(synthesized.get("net"), Some(&JsonValue::from(7500_i64)));
    }
}
