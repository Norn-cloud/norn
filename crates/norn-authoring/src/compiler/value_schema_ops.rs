//! Closed value-schema merging, structural assignability, and cooperative
//! expansion budgets (`schema.ts`, `assignable.ts`, and `budget.ts`).

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{Map, Value};

use super::canonical::{compact_canonical_json, escaped_json_string_len, js_number_len};
use super::types::CompilerInstant;
use super::value_schema::{ObjectSchema, ValueSchema};
use super::value_types::DecimalType;

fn decimal_contract(schema: &ValueSchema) -> Option<&DecimalType> {
    if let ValueSchema::Number { norn_type, .. } = schema {
        norn_type.as_deref()
    } else {
        None
    }
}

fn same_decimal(left: Option<&DecimalType>, right: Option<&DecimalType>) -> bool {
    if let (Some(da), Some(db)) = (left, right) {
        compact_canonical_json(&da.to_json()).ok() == compact_canonical_json(&db.to_json()).ok()
    } else {
        false
    }
}

/// Computes the least closed schema admitting both `left` and `right`, or an
/// error message if no closed merge exists (`mergeSchemas` in `builder/schema.ts`).
///
/// # Errors
///
/// Returns an explanation string when the two schemas have no closed union.
pub(crate) fn merge_schemas(
    left: &ValueSchema,
    right: &ValueSchema,
    path: &str,
) -> Result<ValueSchema, String> {
    if matches!(left, ValueSchema::JsonValue { .. })
        || matches!(right, ValueSchema::JsonValue { .. })
    {
        return Ok(ValueSchema::json_value());
    }
    if matches!(left, ValueSchema::Null { .. }) {
        return Ok(right.clone().into_nullable());
    }
    if matches!(right, ValueSchema::Null { .. }) {
        return Ok(left.clone().into_nullable());
    }
    if matches!(left, ValueSchema::Nullable(_)) || matches!(right, ValueSchema::Nullable(_)) {
        let merged = merge_schemas(left.non_null(), right.non_null(), path)?;
        return Ok(merged.into_nullable());
    }
    if left.is_numeric() && right.is_numeric() {
        let exact_a = decimal_contract(left);
        let exact_b = decimal_contract(right);
        if exact_a.is_some() || exact_b.is_some() {
            if same_decimal(exact_a, exact_b) {
                return Ok(left.clone());
            }
            return Err(format!("{path}: different exact numeric contracts"));
        }
        return if matches!(left, ValueSchema::Integer { .. })
            && matches!(right, ValueSchema::Integer { .. })
        {
            Ok(ValueSchema::integer())
        } else {
            Ok(ValueSchema::number())
        };
    }
    if let (
        ValueSchema::String {
            enum_values: a_enum,
            ..
        },
        ValueSchema::String {
            enum_values: b_enum,
            ..
        },
    ) = (left, right)
    {
        let (Some(a_vals), Some(b_vals)) = (a_enum, b_enum) else {
            return Ok(ValueSchema::text());
        };
        let mut merged = Vec::with_capacity(a_vals.len().saturating_add(b_vals.len()));
        let mut seen = BTreeSet::new();
        for v in a_vals.iter().chain(b_vals.iter()) {
            if seen.insert(v.as_str()) {
                merged.push(v.clone());
            }
        }
        return Ok(ValueSchema::text_enum(merged));
    }
    if matches!(left, ValueSchema::Boolean { .. }) && matches!(right, ValueSchema::Boolean { .. }) {
        return Ok(ValueSchema::boolean());
    }
    if let (ValueSchema::Array { items: a, .. }, ValueSchema::Array { items: b, .. }) =
        (left, right)
    {
        let items = merge_schemas(a, b, &format!("{path}[]"))?;
        return Ok(ValueSchema::array(items));
    }
    if let (ValueSchema::Object(a), ValueSchema::Object(b)) = (left, right) {
        return merge_object_schemas(a, b, path);
    }
    Err(format!(
        "{path}: {} and {} have no closed union",
        left.type_label(),
        right.type_label()
    ))
}

fn merge_object_schemas(
    left: &ObjectSchema,
    right: &ObjectSchema,
    path: &str,
) -> Result<ValueSchema, String> {
    let mut seen_names = BTreeSet::new();
    let mut ordered_names = Vec::new();
    for key in left
        .required
        .iter()
        .chain(left.properties.keys())
        .chain(right.required.iter())
        .chain(right.properties.keys())
    {
        if seen_names.insert(key.as_str()) {
            ordered_names.push(key.as_str());
        }
    }
    let req_a: BTreeSet<&str> = left.required.iter().map(String::as_str).collect();
    let req_b: BTreeSet<&str> = right.required.iter().map(String::as_str).collect();
    let mut fields = Vec::with_capacity(ordered_names.len());
    let mut optional = Vec::new();
    for name in ordered_names {
        match (left.properties.get(name), right.properties.get(name)) {
            (Some(l_val), Some(r_val)) => {
                let merged = merge_schemas(l_val, r_val, &format!("{path}.{name}"))?;
                fields.push((name.to_owned(), merged));
                if !req_a.contains(name) || !req_b.contains(name) {
                    optional.push(name.to_owned());
                }
            }
            (Some(val), None) | (None, Some(val)) => {
                fields.push((name.to_owned(), val.clone()));
                optional.push(name.to_owned());
            }
            (None, None) => {}
        }
    }
    Ok(ValueSchema::object(ObjectSchema::record_ordered(
        fields, &optional,
    )))
}

/// Returns `None` iff every value described by `from` is admitted by `to`
/// (`assignable` in `tools/norn-ts/src/builder/assignable.ts`).
#[must_use]
pub(crate) fn assignable(from: &ValueSchema, to: &ValueSchema, path: &str) -> Option<String> {
    if matches!(to, ValueSchema::JsonValue { .. }) {
        return None;
    }
    let mismatch = || {
        format!(
            "{path} is {}, expected {}",
            from.type_label(),
            to.type_label()
        )
    };
    if matches!(from, ValueSchema::Null { .. }) {
        return if matches!(to, ValueSchema::Null { .. } | ValueSchema::Nullable(_)) {
            None
        } else {
            Some(mismatch())
        };
    }
    if let ValueSchema::Nullable(from_inner) = from {
        let ValueSchema::Nullable(to_inner) = to else {
            return Some(mismatch());
        };
        return assignable(from_inner, to_inner, path);
    }
    if matches!(from, ValueSchema::JsonValue { .. }) {
        return Some(mismatch());
    }
    if let ValueSchema::Nullable(to_inner) = to {
        return assignable(from, to_inner, path);
    }
    if let Some(to_dec) = decimal_contract(to)
        && !same_decimal(decimal_contract(from), Some(to_dec))
    {
        return Some(format!(
            "{path} requires the declared exact decimal contract"
        ));
    }
    if matches!(from, ValueSchema::Integer { .. }) && matches!(to, ValueSchema::Number { .. }) {
        return None;
    }
    assignable_concrete(from, to, path, mismatch)
}

fn assignable_concrete(
    from: &ValueSchema,
    to: &ValueSchema,
    path: &str,
    mismatch: impl Fn() -> String,
) -> Option<String> {
    if let (
        ValueSchema::String {
            enum_values: from_enum,
            ..
        },
        ValueSchema::String {
            enum_values: to_enum,
            ..
        },
    ) = (from, to)
    {
        if let Some(allowed) = to_enum {
            let ok = from_enum
                .as_ref()
                .is_some_and(|vals| vals.iter().all(|v| allowed.contains(v)));
            if !ok {
                return Some(format!("{path} is outside the declared enum"));
            }
        }
        return None;
    }
    if matches!(
        (from, to),
        (ValueSchema::Integer { .. }, ValueSchema::Integer { .. })
            | (ValueSchema::Number { .. }, ValueSchema::Number { .. })
            | (ValueSchema::Boolean { .. }, ValueSchema::Boolean { .. })
            | (ValueSchema::Null { .. }, ValueSchema::Null { .. })
    ) {
        return None;
    }
    if let (ValueSchema::Array { items: a, .. }, ValueSchema::Array { items: b, .. }) = (from, to) {
        return assignable(a, b, &format!("{path}[]"));
    }
    if let (ValueSchema::Object(from_obj), ValueSchema::Object(to_obj)) = (from, to) {
        let mut req_sorted: Vec<&str> = to_obj.required.iter().map(String::as_str).collect();
        req_sorted.sort_unstable();
        for key in req_sorted {
            if !from_obj.required.iter().any(|k| k == key) {
                return Some(format!("{path}.{key} is required"));
            }
        }
        for (key, from_prop) in &from_obj.properties {
            let Some(to_prop) = to_obj.properties.get(key) else {
                return Some(format!("{path}.{key} is not declared"));
            };
            if let Some(problem) = assignable(from_prop, to_prop, &format!("{path}.{key}")) {
                return Some(problem);
            }
        }
        return None;
    }
    Some(mismatch())
}

/// Resource bounds for [`ExpansionBudget`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExpansionLimits {
    /// Maximum expansion work units per module.
    pub(crate) work: usize,
    /// Maximum accumulated output bytes across the compilation batch.
    pub(crate) output_bytes: usize,
    /// Optional cooperative wall-clock deadline duration from start.
    pub(crate) millis: Option<u64>,
}

impl Default for ExpansionLimits {
    fn default() -> Self {
        Self {
            work: 100_000,
            output_bytes: 8_388_608,
            millis: None,
        }
    }
}

/// Error returned when an expansion budget ceiling is breached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BudgetExceeded(pub String);

#[derive(Clone, Copy, Debug)]
struct Cost {
    work: usize,
    bytes: usize,
    lines: usize,
    height: usize,
}

/// Cooperative expansion budget guarding work units, emitted JSON bytes,
/// nesting depth, and wall-clock time (`builder/budget.ts`).
#[derive(Clone, Debug)]
pub(crate) struct ExpansionBudget {
    pack_limits: ExpansionLimits,
    limits: ExpansionLimits,
    started: CompilerInstant,
    work: usize,
    output: usize,
    pending_bytes: usize,
}

impl ExpansionBudget {
    /// Creates a budget anchored at an explicit [`CompilerInstant`] whose `limits` also
    /// act as the immutable pack-wide ceiling.
    #[must_use]
    pub(crate) const fn with_started(limits: ExpansionLimits, started: CompilerInstant) -> Self {
        Self {
            pack_limits: limits,
            limits,
            started,
            work: 0,
            output: 0,
            pending_bytes: 0,
        }
    }

    /// Checks whether the cooperative time bound has been exceeded.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] when the elapsed duration reaches `millis`.
    pub(crate) fn check(&self) -> Result<(), BudgetExceeded> {
        if let Some(max_ms) = self.limits.millis
            && self.started.elapsed() >= Duration::from_millis(max_ms)
        {
            return Err(BudgetExceeded(
                "cooperative expansion time bound exceeded".to_owned(),
            ));
        }
        Ok(())
    }

    /// Applies the module's expansion limits clamped strictly within the fixed
    /// pack-wide ceilings, resetting per-module work and pending literal
    /// counters while preserving pack-wide output bytes and the deadline anchor.
    pub(crate) fn begin_module(&mut self, module_limits: ExpansionLimits) {
        self.limits = ExpansionLimits {
            work: module_limits.work.min(self.pack_limits.work),
            output_bytes: module_limits
                .output_bytes
                .min(self.pack_limits.output_bytes),
            millis: match (self.pack_limits.millis, module_limits.millis) {
                (Some(pack_ms), Some(mod_ms)) => Some(pack_ms.min(mod_ms)),
                (Some(pack_ms), None) => Some(pack_ms),
                (None, mod_ms) => mod_ms,
            },
        };
        self.work = 0;
        self.pending_bytes = 0;
    }

    /// Returns work units charged since the last [`Self::begin_module`].
    #[must_use]
    pub(crate) const fn work_used(&self) -> usize {
        self.work
    }

    fn cap_work(&self, value: usize) -> usize {
        value.min(self.limits.work.saturating_add(1))
    }

    fn cap_bytes(&self, value: usize) -> usize {
        value.min(self.limits.output_bytes.saturating_add(1))
    }

    fn charge(&mut self, cost: Cost) -> Result<(), BudgetExceeded> {
        self.check()?;
        self.work = self.cap_work(self.work.saturating_add(cost.work));
        if self.work > self.limits.work
            || cost.bytes > self.limits.output_bytes.saturating_sub(self.output)
        {
            return Err(BudgetExceeded(
                "expanded data exceeds compiler work or output byte bound".to_owned(),
            ));
        }
        Ok(())
    }

    fn value_cost(&self, value: &Value, depth: usize) -> Result<Cost, BudgetExceeded> {
        self.check()?;
        if depth > 128 {
            return Err(BudgetExceeded(
                "expanded data exceeds nesting bound".to_owned(),
            ));
        }
        match value {
            Value::Null => Ok(Cost {
                work: 1,
                bytes: 4,
                lines: 0,
                height: 0,
            }),
            Value::Bool(b) => Ok(Cost {
                work: 1,
                bytes: if *b { 4 } else { 5 },
                lines: 0,
                height: 0,
            }),
            Value::Number(num) => Ok(Cost {
                work: 1,
                bytes: js_number_len(num),
                lines: 0,
                height: 0,
            }),
            Value::String(s) => Ok(Cost {
                work: 1,
                bytes: escaped_json_string_len(s),
                lines: 0,
                height: 0,
            }),
            Value::Array(items) => self.array_cost(items, depth),
            Value::Object(map) => self.object_cost(map, depth),
        }
    }

    fn array_cost(&self, items: &[Value], depth: usize) -> Result<Cost, BudgetExceeded> {
        let mut work = 1_usize;
        let mut bytes = if items.is_empty() { 2_usize } else { 3_usize };
        let mut lines = usize::from(!items.is_empty());
        let mut height = 0_usize;
        let next_depth = depth.saturating_add(1);
        for (idx, item) in items.iter().enumerate() {
            let c = self.value_cost(item, next_depth)?;
            work = self.cap_work(work.saturating_add(c.work));
            let comma = usize::from(idx.saturating_add(1) < items.len());
            let indent = c.lines.saturating_add(1).saturating_mul(2);
            bytes = self.cap_bytes(
                bytes
                    .saturating_add(c.bytes)
                    .saturating_add(indent)
                    .saturating_add(1)
                    .saturating_add(comma),
            );
            lines = self.cap_bytes(lines.saturating_add(c.lines).saturating_add(1));
            height = height.max(c.height.saturating_add(1));
        }
        if depth.saturating_add(height) > 128 {
            return Err(BudgetExceeded(
                "expanded data exceeds nesting bound".to_owned(),
            ));
        }
        Ok(Cost {
            work,
            bytes,
            lines,
            height,
        })
    }

    fn object_cost(&self, map: &Map<String, Value>, depth: usize) -> Result<Cost, BudgetExceeded> {
        let mut work = 1_usize;
        let mut bytes = if map.is_empty() { 2_usize } else { 3_usize };
        let mut lines = usize::from(!map.is_empty());
        let mut height = 0_usize;
        let next_depth = depth.saturating_add(1);
        let len = map.len();
        for (idx, (key, child)) in map.iter().enumerate() {
            let c = self.value_cost(child, next_depth)?;
            work = self.cap_work(work.saturating_add(c.work));
            let comma = usize::from(idx.saturating_add(1) < len);
            let indent = c.lines.saturating_add(1).saturating_mul(2);
            let key_bytes = escaped_json_string_len(key).saturating_add(2);
            bytes = self.cap_bytes(
                bytes
                    .saturating_add(c.bytes)
                    .saturating_add(indent)
                    .saturating_add(1)
                    .saturating_add(comma)
                    .saturating_add(key_bytes),
            );
            lines = self.cap_bytes(lines.saturating_add(c.lines).saturating_add(1));
            height = height.max(c.height.saturating_add(1));
        }
        if depth.saturating_add(height) > 128 {
            return Err(BudgetExceeded(
                "expanded data exceeds nesting bound".to_owned(),
            ));
        }
        Ok(Cost {
            work,
            bytes,
            lines,
            height,
        })
    }

    /// Charges the expansion cost of `value`.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] when work, byte, depth, or time limits are exceeded.
    pub(crate) fn assert_value(&mut self, value: &Value) -> Result<(), BudgetExceeded> {
        let cost = self.value_cost(value, 0)?;
        self.charge(cost)
    }

    /// Charges the expansion cost of `schema`.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] when work, byte, depth, or time limits are exceeded.
    pub(crate) fn assert_schema(&mut self, schema: &ValueSchema) -> Result<(), BudgetExceeded> {
        self.assert_value(&schema.to_json())
    }

    /// Reserves `work` units ahead of materialization.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] if work or time bounds are exceeded.
    pub(crate) fn reserve(&mut self, work: usize) -> Result<(), BudgetExceeded> {
        let capped = self.cap_work(work);
        self.charge(Cost {
            work: capped,
            bytes: 0,
            lines: 0,
            height: 0,
        })
    }

    /// Charges work and pending literal output bytes ahead of allocating or
    /// cloning literal strings, arrays, or objects.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] when work, output byte, or time limits are exceeded.
    pub(crate) fn charge_literal(
        &mut self,
        work: usize,
        bytes: usize,
    ) -> Result<(), BudgetExceeded> {
        self.check()?;
        self.work = self.cap_work(self.work.saturating_add(work));
        self.pending_bytes = self.cap_bytes(self.pending_bytes.saturating_add(bytes));
        if self.work > self.limits.work
            || self.pending_bytes > self.limits.output_bytes.saturating_sub(self.output)
        {
            return Err(BudgetExceeded(
                "expanded data exceeds compiler work or output byte bound".to_owned(),
            ));
        }
        Ok(())
    }

    /// Charges work and accumulates output bytes for emitted data.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] if work, output byte, depth, or time limits are exceeded.
    pub(crate) fn add_output(&mut self, value: &Value) -> Result<(), BudgetExceeded> {
        self.pending_bytes = 0;
        let cost = self.value_cost(value, 0)?;
        self.charge(cost)?;
        self.output = self.cap_bytes(self.output.saturating_add(cost.bytes).saturating_add(1));
        if self.output > self.limits.output_bytes {
            return Err(BudgetExceeded(
                "accumulated rows exceed compiler output byte bound".to_owned(),
            ));
        }
        Ok(())
    }

    /// Charges raw work and output bytes (for expression expansion preflight).
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] when limits are exceeded.
    pub(crate) fn charge_expr_cost(
        &mut self,
        work: usize,
        bytes: usize,
    ) -> Result<(), BudgetExceeded> {
        let cost = Cost {
            work: self.cap_work(work),
            bytes: self.cap_bytes(bytes),
            lines: 0,
            height: 0,
        };
        self.charge(cost)?;
        self.output = self.cap_bytes(self.output.saturating_add(cost.bytes));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CompilerInstant, ExpansionBudget, ExpansionLimits};

    #[test]
    fn output_budget_charges_escaped_bytes_for_control_heavy_strings() {
        let raw = "\u{0000}".repeat(40);
        // Raw length is 40 bytes, but escaped JSON is `"\u0000..."` = 2 + 240 = 242 bytes.
        let mut budget = ExpansionBudget::with_started(
            ExpansionLimits {
                work: 1_000,
                output_bytes: 100,
                millis: None,
            },
            CompilerInstant::now(),
        );
        assert!(budget.assert_value(&json!(raw)).is_err());

        let mut generous = ExpansionBudget::with_started(
            ExpansionLimits {
                work: 1_000,
                output_bytes: 300,
                millis: None,
            },
            CompilerInstant::now(),
        );
        assert!(generous.assert_value(&json!(raw)).is_ok());
    }

    #[test]
    fn begin_module_clamps_within_fixed_pack_ceilings() {
        let mut budget = ExpansionBudget::with_started(
            ExpansionLimits {
                work: 500,
                output_bytes: 200,
                millis: Some(1_000),
            },
            CompilerInstant::now(),
        );
        budget.begin_module(ExpansionLimits {
            work: 9_999,
            output_bytes: 9_999,
            millis: Some(9_999),
        });
        assert_eq!(budget.limits.work, 500);
        assert_eq!(budget.limits.output_bytes, 200);
        assert_eq!(budget.limits.millis, Some(1_000));

        budget.begin_module(ExpansionLimits {
            work: 100,
            output_bytes: 50,
            millis: Some(250),
        });
        assert_eq!(budget.limits.work, 100);
        assert_eq!(budget.limits.output_bytes, 50);
        assert_eq!(budget.limits.millis, Some(250));
    }
}
