//! ZEN expression rendering and expression expansion budget accounting.
//!
//! Split out of `expr.rs` to keep every module under 800 LOC.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::canonical::is_snake_ident;
use super::expr::{
    Binders, Binding, CallFn, CompareOp, Expr, ExprError, LogicalOp, Scope, TemporalKind,
    temporal_kind,
};
use super::expr_type::{infer, resolve_binding};
use super::value_schema::{BudgetExceeded, ExpansionBudget, ObjectSchema, ValueSchema};

/// Renders a ZEN string literal; refuses control characters, backslashes, or mixed quotes.
///
/// # Errors
///
/// Returns [`ExprError`] (`NORN-FLOW-EXPRESSION`) if `value` requires escape sequences.
pub(crate) fn zen_string(value: &str) -> Result<String, ExprError> {
    let needs_escape = value
        .chars()
        .any(|ch| ch == '\\' || ch == '\n' || ch == '\r' || u32::from(ch) <= 0x1F)
        || (value.contains('\'') && value.contains('"'));
    if needs_escape {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!("string {value:?} needs escapes, which ZEN v1 lowering does not emit"),
        ));
    }
    if value.contains('\'') {
        Ok(format!("\"{value}\""))
    } else {
        Ok(format!("'{value}'"))
    }
}

/// Renders a ZEN numeric literal.
///
/// # Errors
///
/// Returns [`ExprError`] if the number is non-finite or requires scientific notation.
pub(crate) fn zen_number(value: &serde_json::Number) -> Result<String, ExprError> {
    if let Some(i) = value.as_i64() {
        if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&i) {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                format!("number {i} has no exact ZEN literal"),
            ));
        }
        return Ok(i.to_string());
    }
    if let Some(u) = value.as_u64() {
        if u > 9_007_199_254_740_991 {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                format!("number {u} has no exact ZEN literal"),
            ));
        }
        return Ok(u.to_string());
    }
    let Some(f) = value.as_f64() else {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!("number {value} has no exact ZEN literal"),
        ));
    };
    let text = f.to_string();
    if !f.is_finite() || text.contains('e') || text.contains('E') {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!("number {text} has no exact ZEN literal"),
        ));
    }
    Ok(text)
}

/// Validates a ZEN record key identifier (`[A-Za-z_][A-Za-z0-9_]*`).
///
/// # Errors
///
/// Returns [`ExprError`] if `name` is not a valid ZEN identifier.
pub(crate) fn zen_key(name: &str) -> Result<&str, ExprError> {
    let mut bytes = name.bytes();
    let ok = bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !ok {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!("record key {name:?} is not an identifier"),
        ));
    }
    Ok(name)
}

/// Renders a JSON value as a ZEN literal.
///
/// # Errors
///
/// Returns [`ExprError`] if any nested string, number, or object key cannot be represented.
pub(crate) fn zen_json(value: &Value) -> Result<String, ExprError> {
    match value {
        Value::Null => Ok("null".to_owned()),
        Value::Bool(b) => Ok(if *b { "true" } else { "false" }.to_owned()),
        Value::String(s) => zen_string(s),
        Value::Number(n) => zen_number(n),
        Value::Array(items) => {
            let mut rendered = Vec::with_capacity(items.len());
            for item in items {
                rendered.push(zen_json(item)?);
            }
            Ok(format!("[{}]", rendered.join(", ")))
        }
        Value::Object(map) => {
            let mut rendered = Vec::with_capacity(map.len());
            for (k, v) in map {
                let key = zen_key(k)?;
                let val = zen_json(v)?;
                rendered.push(format!("{key}: {val}"));
            }
            Ok(format!("{{{}}}", rendered.join(", ")))
        }
    }
}

fn reserve_schema_names(schema: &ValueSchema, used: &mut BTreeSet<String>) {
    match schema {
        ValueSchema::Array { items, .. } => reserve_schema_names(items, used),
        ValueSchema::Object(obj) => {
            for (key, member) in &obj.properties {
                used.insert(key.clone());
                reserve_schema_names(member, used);
            }
        }
        ValueSchema::Nullable(inner) => reserve_schema_names(inner, used),
        ValueSchema::String { .. }
        | ValueSchema::Integer { .. }
        | ValueSchema::Number { .. }
        | ValueSchema::Boolean { .. }
        | ValueSchema::Null { .. }
        | ValueSchema::JsonValue { .. } => {}
    }
}

fn collect_zen_idents(text: &str, used: &mut BTreeSet<String>) {
    let mut current = String::new();
    for ch in text.chars() {
        if current.is_empty() {
            if ch.is_ascii_alphabetic() || ch == '_' {
                current.push(ch);
            }
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            used.insert(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        used.insert(current);
    }
}

fn reserve_scope_names(scope: &Scope, used: &mut BTreeSet<String>, depth: usize) {
    if depth > 32 {
        return;
    }
    for (name, binding) in scope {
        used.insert(name.clone());
        reserve_schema_names(binding.schema(), used);
        match binding {
            Binding::Zen { zen, .. } => collect_zen_idents(zen, used),
            Binding::Inline { scope: inner, .. } => {
                reserve_scope_names(inner, used, depth.saturating_add(1));
            }
        }
    }
}

fn fresh_alias(scope: &Scope, binders: &Binders, element: &ValueSchema) -> String {
    let mut used = BTreeSet::new();
    reserve_scope_names(scope, &mut used, 0);
    reserve_schema_names(element, &mut used);
    for (zen, schema) in binders.values() {
        used.insert(zen.clone());
        reserve_schema_names(schema, &mut used);
    }
    let mut idx = 0_usize;
    loop {
        let candidate = format!("b{idx}");
        if !used.contains(&candidate) {
            return candidate;
        }
        idx = idx.saturating_add(1);
    }
}

/// Renders `expr` into ZEN expression text in `scope`.
///
/// # Errors
///
/// Returns [`ExprError`] if rendering fails typing or encounters an unrenderable literal.
pub(crate) fn render(expr: &Expr, scope: &Scope) -> Result<String, ExprError> {
    render_in(expr, scope, &BTreeMap::new())
}

fn render_in(expr: &Expr, scope: &Scope, binders: &Binders) -> Result<String, ExprError> {
    match expr {
        Expr::Binary { op, left, right } => Ok(format!(
            "{} {} {}",
            render_operand(left, scope, binders)?,
            op.wire_str(),
            render_operand(right, scope, binders)?
        )),
        Expr::Call { func, args } => render_call(*func, args, scope, binders),
        Expr::Closure {
            func,
            list,
            binder,
            body,
        } => {
            let list_schema = infer(list, scope, binders)?;
            let ValueSchema::Array { items, .. } = list_schema else {
                return Err(ExprError::new(
                    "NORN-FLOW-EXPRESSION",
                    format!(
                        "{} iterates a list, not {}",
                        func.wire_str(),
                        list_schema.type_label()
                    ),
                ));
            };
            let alias = fresh_alias(scope, binders, &items);
            let mut inner = binders.clone();
            inner.insert(*binder, (alias.clone(), *items));
            Ok(format!(
                "{}({} as {alias}, {})",
                func.wire_str(),
                render_operand(list, scope, binders)?,
                render_in(body, scope, &inner)?
            ))
        }
        Expr::Conditional { condition, yes, no } => Ok(format!(
            "{} ? {} : {}",
            render_operand(condition, scope, binders)?,
            render_operand(yes, scope, binders)?,
            render_operand(no, scope, binders)?
        )),
        Expr::Coalesce { value, fallback } => Ok(format!(
            "{} ?? {}",
            render_operand(value, scope, binders)?,
            render_operand(fallback, scope, binders)?
        )),
        Expr::Literal(val) => zen_json(val),
        Expr::Bound { binder, path } => {
            let binding = binders.get(binder).map(|(zen, schema)| Binding::Zen {
                zen: zen.clone(),
                schema: schema.clone(),
            });
            Ok(resolve_binding(binding.as_ref(), "closure element", path)?.0)
        }
        Expr::Input { name, path } => Ok(resolve_binding(scope.get(name), name, path)?.0),
        Expr::Compare { op, left, right } => Ok(format!(
            "{} {} {}",
            render_operand(left, scope, binders)?,
            op.wire_str(),
            render_operand(right, scope, binders)?
        )),
        Expr::Logical { op, left, right } => Ok(format!(
            "{} {} {}",
            render_operand(left, scope, binders)?,
            op.wire_str(),
            render_operand(right, scope, binders)?
        )),
        Expr::Not(operand) => Ok(format!("not {}", render_operand(operand, scope, binders)?)),
        Expr::List(items) => {
            let mut rendered = Vec::with_capacity(items.len());
            for item in items {
                rendered.push(render_in(item, scope, binders)?);
            }
            Ok(format!("[{}]", rendered.join(", ")))
        }
        Expr::Record(fields) => {
            let mut rendered = Vec::with_capacity(fields.len());
            for (name, val) in fields {
                let key = zen_key(name)?;
                let value = render_in(val, scope, binders)?;
                rendered.push(format!("{key}: {value}"));
            }
            Ok(format!("{{{}}}", rendered.join(", ")))
        }
    }
}

fn render_call(
    func: CallFn,
    args: &[Expr],
    scope: &Scope,
    binders: &Binders,
) -> Result<String, ExprError> {
    match func {
        CallFn::Concat => {
            let mut rendered = Vec::with_capacity(args.len());
            for arg in args {
                rendered.push(render_in(arg, scope, binders)?);
            }
            Ok(format!("flatten([{}])", rendered.join(", ")))
        }
        CallFn::DateBefore => {
            let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
                return Err(ExprError::new(
                    "NORN-FLOW-EXPRESSION",
                    "dateBefore expects 2 arguments",
                ));
            };
            Ok(format!(
                "date({}) < date({})",
                render_in(a, scope, binders)?,
                render_in(b, scope, binders)?
            ))
        }
        CallFn::TimestampBefore => {
            let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
                return Err(ExprError::new(
                    "NORN-FLOW-EXPRESSION",
                    "timestampBefore expects 2 arguments",
                ));
            };
            Ok(format!(
                "{} < {}",
                render_in(a, scope, binders)?,
                render_in(b, scope, binders)?
            ))
        }
        CallFn::TimestampAtOrBefore => {
            let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
                return Err(ExprError::new(
                    "NORN-FLOW-EXPRESSION",
                    "timestampAtOrBefore expects 2 arguments",
                ));
            };
            Ok(format!(
                "{} <= {}",
                render_in(a, scope, binders)?,
                render_in(b, scope, binders)?
            ))
        }
        CallFn::Lower
        | CallFn::Upper
        | CallFn::Trim
        | CallFn::Len
        | CallFn::Take
        | CallFn::Contains
        | CallFn::StartsWith
        | CallFn::EndsWith
        | CallFn::Date
        | CallFn::Timestamp
        | CallFn::Sum
        | CallFn::Floor
        | CallFn::DaysBetween
        | CallFn::AddDays
        | CallFn::SecondsBetween
        | CallFn::AddSeconds
        | CallFn::IntervalsOverlap
        | CallFn::IntervalGapSeconds
        | CallFn::IntervalIntersectionSeconds
        | CallFn::ResolveLocalTimestamp => {
            let mut rendered = Vec::with_capacity(args.len());
            for arg in args {
                rendered.push(render_in(arg, scope, binders)?);
            }
            Ok(format!("{}({})", func.wire_str(), rendered.join(", ")))
        }
    }
}

pub(crate) fn render_operand(
    expr: &Expr,
    scope: &Scope,
    binders: &Binders,
) -> Result<String, ExprError> {
    let text = render_in(expr, scope, binders)?;
    let atomic = match expr {
        Expr::Literal(_)
        | Expr::Input { .. }
        | Expr::Bound { .. }
        | Expr::List(_)
        | Expr::Record(_)
        | Expr::Closure { .. } => true,
        Expr::Call { func, .. } => !matches!(
            func,
            CallFn::DateBefore | CallFn::TimestampBefore | CallFn::TimestampAtOrBefore
        ),
        Expr::Binary { .. }
        | Expr::Conditional { .. }
        | Expr::Coalesce { .. }
        | Expr::Compare { .. }
        | Expr::Logical { .. }
        | Expr::Not(_) => false,
    };
    if atomic {
        Ok(text)
    } else {
        Ok(format!("({text})"))
    }
}

/// Charges expansion budget for rendering `expr` in `scope`.
///
/// # Errors
///
/// Returns [`BudgetExceeded`] if expression expansion exceeds depth, work, byte, or time limits.
pub(crate) fn assert_expression_budget(
    budget: &mut ExpansionBudget,
    expr: &Expr,
    scope: &Scope,
) -> Result<(), BudgetExceeded> {
    let (work, bytes) = expr_cost(budget, expr, scope, 0)?;
    budget.charge_expr_cost(work, bytes)
}

fn expr_cost(
    budget: &ExpansionBudget,
    expr: &Expr,
    scope: &Scope,
    depth: usize,
) -> Result<(usize, usize), BudgetExceeded> {
    budget.check()?;
    if depth > 128 {
        return Err(BudgetExceeded(
            "inline expressions exceed expansion nesting bound".to_owned(),
        ));
    }
    let next_depth = depth.saturating_add(1);
    let mut acc = (1_usize, 64_usize);
    let add = |acc: &mut (usize, usize),
               child: &Expr,
               where_scope: &Scope|
     -> Result<(), BudgetExceeded> {
        let (cw, cb) = expr_cost(budget, child, where_scope, next_depth)?;
        acc.0 = acc.0.saturating_add(cw);
        acc.1 = acc.1.saturating_add(cb);
        Ok(())
    };
    match expr {
        Expr::Literal(val) => {
            acc.1 = acc.1.saturating_add(val.to_string().len());
        }
        Expr::Input { name, path } => {
            if let Some(binding) = scope.get(name) {
                match binding {
                    Binding::Zen { zen, .. } => {
                        acc.1 = acc.1.saturating_add(zen.len());
                        for seg in path {
                            acc.1 = acc.1.saturating_add(seg.len()).saturating_add(1);
                        }
                    }
                    Binding::Inline {
                        inline,
                        scope: inner_scope,
                        ..
                    } => {
                        let resolved = resolve_inline_cost_target(inline, path);
                        add(&mut acc, &resolved, inner_scope)?;
                    }
                }
            }
        }
        Expr::Bound { path, .. } => {
            for seg in path {
                acc.1 = acc.1.saturating_add(seg.len()).saturating_add(1);
            }
        }
        Expr::Binary { left, right, .. }
        | Expr::Compare { left, right, .. }
        | Expr::Logical { left, right, .. } => {
            add(&mut acc, left, scope)?;
            add(&mut acc, right, scope)?;
        }
        Expr::Call { args, .. } => {
            for arg in args {
                add(&mut acc, arg, scope)?;
            }
        }
        Expr::Closure {
            func, list, body, ..
        } => {
            acc.1 = acc.1.saturating_add(func.wire_str().len());
            add(&mut acc, list, scope)?;
            add(&mut acc, body, scope)?;
        }
        Expr::Conditional { condition, yes, no } => {
            add(&mut acc, condition, scope)?;
            add(&mut acc, yes, scope)?;
            add(&mut acc, no, scope)?;
        }
        Expr::Coalesce { value, fallback } => {
            add(&mut acc, value, scope)?;
            add(&mut acc, fallback, scope)?;
        }
        Expr::Not(operand) => add(&mut acc, operand, scope)?,
        Expr::List(items) => {
            for item in items {
                add(&mut acc, item, scope)?;
            }
        }
        Expr::Record(fields) => {
            for (name, val) in fields {
                acc.1 = acc.1.saturating_add(name.len());
                add(&mut acc, val, scope)?;
            }
        }
    }
    Ok(acc)
}

fn resolve_inline_cost_target(inline: &Expr, path: &[String]) -> Expr {
    let mut val = inline.clone();
    for (idx, segment) in path.iter().enumerate() {
        match val {
            Expr::Record(fields) => {
                let Some((_, f)) = fields.into_iter().find(|(k, _)| k == segment) else {
                    return Expr::Record(Vec::new());
                };
                val = f;
            }
            Expr::Input {
                name: in_name,
                path: mut in_path,
            } => {
                if let Some(rest) = path.get(idx..) {
                    in_path.extend(rest.iter().cloned());
                }
                return Expr::Input {
                    name: in_name,
                    path: in_path,
                };
            }
            other @ (Expr::Binary { .. }
            | Expr::Call { .. }
            | Expr::Closure { .. }
            | Expr::Conditional { .. }
            | Expr::Coalesce { .. }
            | Expr::Bound { .. }
            | Expr::Literal(_)
            | Expr::Compare { .. }
            | Expr::Logical { .. }
            | Expr::Not(_)
            | Expr::List(_)) => return other,
        }
    }
    val
}

fn temporal_kind_of(e: &Expr, temporal: &BTreeMap<String, TemporalKind>) -> Option<TemporalKind> {
    if let Expr::Input { name, path } = e
        && name == "input"
    {
        return temporal.get(&path.join(".")).copied();
    }
    temporal_kind(e)
}

fn parsed_temporal_compare(
    op: CompareOp,
    left: Expr,
    right: Expr,
    temporal: &BTreeMap<String, TemporalKind>,
) -> Result<Expr, ExprError> {
    let left = parsed_temporal(left, temporal)?;
    let right = parsed_temporal(right, temporal)?;
    if matches!(op, CompareOp::Eq | CompareOp::Ne) {
        return Ok(Expr::compare(op, left, right));
    }
    let left_kind = temporal_kind_of(&left, temporal);
    let right_kind = temporal_kind_of(&right, temporal);
    if let (Some(lk), Some(rk)) = (left_kind, right_kind)
        && lk != rk
    {
        return Err(ExprError::new(
            "NORN-FLOW-TYPE",
            format!(
                "ordering compares a {} with a {}; convert one side explicitly",
                lk.wire_str(),
                rk.wire_str()
            ),
        ));
    }
    let Some(kind) = left_kind.or(right_kind) else {
        return Ok(Expr::compare(op, left, right));
    };
    let parse_side = |side: Expr| -> Expr {
        if temporal_kind(&side).is_some() {
            return side;
        }
        let needs_wrap = (matches!(side, Expr::Input { .. })
            && temporal_kind_of(&side, temporal).is_some())
            || matches!(side, Expr::Literal(Value::String(_)));
        if needs_wrap {
            match kind {
                TemporalKind::Date => Expr::call(CallFn::Date, vec![side]),
                TemporalKind::Timestamp => Expr::call(CallFn::Timestamp, vec![side]),
            }
        } else {
            side
        }
    };
    Ok(Expr::compare(op, parse_side(left), parse_side(right)))
}

/// Rewrites raw date/timestamp comparisons on law subjects into explicit
/// `date(...)` / `timestamp(...)` calls (`parsedTemporal` in `definitions.ts`).
///
/// # Errors
///
/// Returns [`ExprError`] (`NORN-FLOW-TYPE`) if a date is ordered against a timestamp.
pub(crate) fn parsed_temporal(
    expr: Expr,
    temporal: &BTreeMap<String, TemporalKind>,
) -> Result<Expr, ExprError> {
    match expr {
        Expr::Compare { op, left, right } => parsed_temporal_compare(op, *left, *right, temporal),
        Expr::Binary { op, left, right } => Ok(Expr::binary(
            op,
            parsed_temporal(*left, temporal)?,
            parsed_temporal(*right, temporal)?,
        )),
        Expr::Logical { op, left, right } => Ok(Expr::Logical {
            op,
            left: Box::new(parsed_temporal(*left, temporal)?),
            right: Box::new(parsed_temporal(*right, temporal)?),
        }),
        Expr::Not(inner) => Ok(Expr::not(parsed_temporal(*inner, temporal)?)),
        Expr::Conditional { condition, yes, no } => Ok(Expr::conditional(
            parsed_temporal(*condition, temporal)?,
            parsed_temporal(*yes, temporal)?,
            parsed_temporal(*no, temporal)?,
        )),
        Expr::Coalesce { value, fallback } => Ok(Expr::coalesce(
            parsed_temporal(*value, temporal)?,
            parsed_temporal(*fallback, temporal)?,
        )),
        Expr::Call { func, args } => {
            let mut out = Vec::with_capacity(args.len());
            for arg in args {
                out.push(parsed_temporal(arg, temporal)?);
            }
            Ok(Expr::call(func, out))
        }
        Expr::Closure {
            func,
            list,
            binder,
            body,
        } => Ok(Expr::Closure {
            func,
            list: Box::new(parsed_temporal(*list, temporal)?),
            binder,
            body: Box::new(parsed_temporal(*body, temporal)?),
        }),
        Expr::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(parsed_temporal(item, temporal)?);
            }
            Ok(Expr::List(out))
        }
        Expr::Record(fields) => {
            let mut out = Vec::with_capacity(fields.len());
            for (k, v) in fields {
                out.push((k, parsed_temporal(v, temporal)?));
            }
            Ok(Expr::Record(out))
        }
        other @ (Expr::Literal(_) | Expr::Input { .. } | Expr::Bound { .. }) => Ok(other),
    }
}

/// Narrows top-level `self.<prop> != null` conjuncts from a law's `when`
/// condition so the response lambda sees non-null optional links.
#[must_use]
pub(crate) fn narrow_subject_schema(condition: &Expr, schema: &ValueSchema) -> ValueSchema {
    let mut narrowed = BTreeSet::new();
    collect_non_null_guards(condition, &mut narrowed);
    if narrowed.is_empty() {
        return schema.clone();
    }
    let ValueSchema::Object(obj) = schema else {
        return schema.clone();
    };
    let mut properties = obj.properties.clone();
    for field in narrowed {
        if let Some(existing) = properties.get(&field).cloned() {
            properties.insert(field, existing.non_null().clone());
        }
    }
    ValueSchema::object(ObjectSchema::record(properties, &[]))
}

fn collect_non_null_guards(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Logical {
            op: LogicalOp::And,
            left,
            right,
        } => {
            collect_non_null_guards(left, out);
            collect_non_null_guards(right, out);
        }
        Expr::Compare {
            op: CompareOp::Ne,
            left,
            right,
        } => {
            let check = |cand: &Expr, other: &Expr, out: &mut BTreeSet<String>| {
                if matches!(other, Expr::Literal(Value::Null))
                    && let Expr::Input { name, path } = cand
                    && name == "input"
                    && path.len() == 1
                    && let Some(prop) = path.first()
                {
                    out.insert(prop.clone());
                }
            };
            check(left, right, out);
            check(right, left, out);
        }
        Expr::Logical {
            op: LogicalOp::Or, ..
        }
        | Expr::Compare { .. }
        | Expr::Binary { .. }
        | Expr::Call { .. }
        | Expr::Closure { .. }
        | Expr::Conditional { .. }
        | Expr::Coalesce { .. }
        | Expr::Bound { .. }
        | Expr::Literal(_)
        | Expr::Input { .. }
        | Expr::Not(_)
        | Expr::List(_)
        | Expr::Record(_) => {}
    }
}

/// Validates a `snake_case` identifier helper re-exported for expression checks.
#[must_use]
pub(crate) fn is_valid_binding_name(name: &str) -> bool {
    is_snake_ident(name)
}
