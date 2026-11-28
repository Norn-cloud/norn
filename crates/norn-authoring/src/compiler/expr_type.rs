//! Type inference, binding resolution, and contextual assignability for [`Expr`].
//!
//! Split out of `expr.rs` to keep every module under 800 LOC.

use std::collections::{BTreeMap, BTreeSet};

use super::expr::{
    BinaryOp, Binders, Binding, CallFn, ClosureFn, CompareOp, Expr, ExprError, Scope, TemporalKind,
    temporal_kind,
};
use super::expr_render::{render_operand, zen_key};
use super::value_schema::{ObjectSchema, ValueSchema, literal_schema, merge_schemas};

pub(crate) fn is_non_neg_int_index(segment: &str) -> Option<usize> {
    if segment == "0" {
        return Some(0);
    }
    let first = segment.bytes().next()?;
    if !(b'1'..=b'9').contains(&first) || !segment.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    segment.parse::<usize>().ok()
}

pub(crate) fn resolve_binding(
    binding: Option<&Binding>,
    name: &str,
    path: &[String],
) -> Result<(String, ValueSchema), ExprError> {
    let Some(binding) = binding else {
        return Err(ExprError::new(
            "NORN-FLOW-SLICE",
            format!("`{name}` is not bound here"),
        ));
    };
    let mut schema = binding.schema().clone();
    let mut optional = false;
    let is_zen = matches!(binding, Binding::Zen { .. });
    let mut zen_path = match binding {
        Binding::Zen { zen, .. } => zen.clone(),
        Binding::Inline { .. } => String::new(),
    };
    for segment in path {
        let array_index =
            matches!(schema, ValueSchema::Array { .. }) && is_non_neg_int_index(segment).is_some();
        if array_index && is_zen {
            return Err(ExprError::new(
                "NORN-FLOW-SLICE",
                "runtime array indexes are outside the admitted expression profile",
            ));
        }
        if let ValueSchema::Object(obj) = &schema
            && !obj.required.iter().any(|r| r == segment)
        {
            optional = true;
        }
        let member = lookup_schema_member(&schema, segment, array_index);
        let Some(member) = member else {
            let hint = if let ValueSchema::Object(obj) = &schema {
                obj.properties
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                "a record field".to_owned()
            };
            return Err(ExprError::new(
                "NORN-FLOW-SLICE",
                format!("expected a declared field, got {segment} on {name}; did you mean {hint}?"),
            ));
        };
        if array_index {
            zen_path.push('[');
            zen_path.push_str(segment);
            zen_path.push(']');
        } else {
            let key = zen_key(segment)?;
            if !zen_path.is_empty() {
                zen_path.push('.');
            }
            zen_path.push_str(key);
        }
        schema = member;
    }
    if optional {
        schema = schema.into_nullable();
    }
    match binding {
        Binding::Zen { .. } => Ok((zen_path, schema)),
        Binding::Inline { inline, scope, .. } => {
            let rendered = resolve_inline_path(inline, scope, name, path)?;
            Ok((rendered, schema))
        }
    }
}

fn lookup_schema_member(
    schema: &ValueSchema,
    segment: &str,
    array_index: bool,
) -> Option<ValueSchema> {
    match schema {
        ValueSchema::Array { items, .. } if array_index => Some(items.as_ref().clone()),
        ValueSchema::Object(obj) => obj.properties.get(segment).cloned(),
        ValueSchema::String { .. }
        | ValueSchema::Integer { .. }
        | ValueSchema::Number { .. }
        | ValueSchema::Boolean { .. }
        | ValueSchema::Null { .. }
        | ValueSchema::Array { .. }
        | ValueSchema::Nullable(_)
        | ValueSchema::JsonValue { .. } => None,
    }
}

fn resolve_inline_path(
    inline: &Expr,
    scope: &Scope,
    name: &str,
    path: &[String],
) -> Result<String, ExprError> {
    let mut expr = inline.clone();
    for (idx, segment) in path.iter().enumerate() {
        match expr {
            Expr::Record(fields) => {
                let Some((_, val)) = fields.into_iter().find(|(k, _)| k == segment) else {
                    let full = std::iter::once(name)
                        .chain(path.iter().map(String::as_str))
                        .collect::<Vec<_>>()
                        .join(".");
                    return Err(ExprError::new(
                        "NORN-FLOW-SLICE",
                        format!("`{full}` reads {segment}, which is not set"),
                    ));
                };
                expr = val;
            }
            Expr::List(items) if is_non_neg_int_index(segment).is_some() => {
                let Some(item) = is_non_neg_int_index(segment)
                    .and_then(|pos| items.get(pos))
                    .cloned()
                else {
                    return Err(ExprError::new(
                        "NORN-FLOW-SLICE",
                        format!("list index {segment} is not set"),
                    ));
                };
                expr = item;
            }
            Expr::Input {
                name: inner_name,
                path: mut inner_path,
            } => {
                if let Some(rest) = path.get(idx..) {
                    inner_path.extend(rest.iter().cloned());
                }
                expr = Expr::Input {
                    name: inner_name,
                    path: inner_path,
                };
                break;
            }
            Expr::Bound {
                binder,
                path: mut inner_path,
            } => {
                if let Some(rest) = path.get(idx..) {
                    inner_path.extend(rest.iter().cloned());
                }
                expr = Expr::Bound {
                    binder,
                    path: inner_path,
                };
                break;
            }
            Expr::Binary { .. }
            | Expr::Call { .. }
            | Expr::Closure { .. }
            | Expr::Conditional { .. }
            | Expr::Coalesce { .. }
            | Expr::Literal(_)
            | Expr::Compare { .. }
            | Expr::Logical { .. }
            | Expr::Not(_)
            | Expr::List(_) => {
                let full = std::iter::once(name)
                    .chain(path.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(".");
                return Err(ExprError::new(
                    "NORN-FLOW-SLICE",
                    format!("`{full}` reads into a non-record value"),
                ));
            }
        }
    }
    render_operand(&expr, scope, &BTreeMap::new())
}

/// Infers the closed [`ValueSchema`] of `expr` in `scope`.
///
/// # Errors
///
/// Returns [`ExprError`] when `expr` fails typing or reads an undeclared path.
pub(crate) fn type_of(expr: &Expr, scope: &Scope) -> Result<ValueSchema, ExprError> {
    infer(expr, scope, &BTreeMap::new())
}

pub(crate) fn infer(
    expr: &Expr,
    scope: &Scope,
    binders: &Binders,
) -> Result<ValueSchema, ExprError> {
    match expr {
        Expr::Binary { op, left, right } => infer_binary(*op, left, right, scope, binders),
        Expr::Call { func, args } => infer_call(*func, args, scope, binders),
        Expr::Closure {
            func,
            list,
            binder,
            body,
        } => infer_closure(*func, list, *binder, body, scope, binders),
        Expr::Conditional { condition, yes, no } => {
            infer_conditional(condition, yes, no, scope, binders)
        }
        Expr::Coalesce { value, fallback } => {
            let val_ty = infer(value, scope, binders)?;
            let fb_ty = infer(fallback, scope, binders)?;
            if matches!(val_ty, ValueSchema::Null { .. }) {
                return Ok(fb_ty);
            }
            merge_schemas(val_ty.non_null(), &fb_ty, "$")
                .map_err(|msg| ExprError::new("NORN-FLOW-EXPRESSION", msg))
        }
        Expr::Literal(val) => literal_schema(val)
            .ok_or_else(|| ExprError::new("NORN-FLOW-EXPRESSION", "literal is not a JSON scalar")),
        Expr::Bound { binder, path } => {
            let binding = binders.get(binder).map(|(zen, schema)| Binding::Zen {
                zen: zen.clone(),
                schema: schema.clone(),
            });
            Ok(resolve_binding(binding.as_ref(), "closure element", path)?.1)
        }
        Expr::Input { name, path } => Ok(resolve_binding(scope.get(name), name, path)?.1),
        Expr::Compare { op, left, right } => infer_compare(*op, left, right, scope, binders),
        Expr::Logical { op, left, right } => {
            let l = infer(left, scope, binders)?;
            let r = infer(right, scope, binders)?;
            if !matches!(l, ValueSchema::Boolean { .. })
                || !matches!(r, ValueSchema::Boolean { .. })
            {
                return Err(ExprError::new(
                    "NORN-FLOW-EXPRESSION",
                    format!(
                        "{} takes booleans, not {} and {}",
                        op.wire_str(),
                        l.type_label(),
                        r.type_label()
                    ),
                ));
            }
            Ok(ValueSchema::boolean())
        }
        Expr::Not(operand) => {
            let ty = infer(operand, scope, binders)?;
            if !matches!(ty, ValueSchema::Boolean { .. }) {
                return Err(ExprError::new(
                    "NORN-FLOW-EXPRESSION",
                    format!("not takes booleans, not {}", ty.type_label()),
                ));
            }
            Ok(ValueSchema::boolean())
        }
        Expr::List(items) => infer_list(items, scope, binders),
        Expr::Record(fields) => infer_record(fields, scope, binders),
    }
}

fn infer_closure(
    func: ClosureFn,
    list: &Expr,
    binder: super::expr::BinderId,
    body: &Expr,
    scope: &Scope,
    binders: &Binders,
) -> Result<ValueSchema, ExprError> {
    let list_schema = infer(list, scope, binders)?;
    let ValueSchema::Array { items, .. } = &list_schema else {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!(
                "{} iterates a list, not {}",
                func.wire_str(),
                list_schema.type_label()
            ),
        ));
    };
    let mut inner = binders.clone();
    inner.insert(binder, (String::new(), items.as_ref().clone()));
    let body_schema = infer(body, scope, &inner)?;
    if func == ClosureFn::Map {
        if matches!(
            body_schema,
            ValueSchema::Null { .. } | ValueSchema::Nullable(_) | ValueSchema::JsonValue { .. }
        ) {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                "a map body must not be null",
            ));
        }
        return Ok(ValueSchema::array(body_schema));
    }
    if !matches!(body_schema, ValueSchema::Boolean { .. }) {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!(
                "a {} body must be Boolean, not {}",
                func.wire_str(),
                body_schema.type_label()
            ),
        ));
    }
    match func {
        ClosureFn::Filter => Ok(list_schema),
        ClosureFn::Count => Ok(ValueSchema::integer()),
        ClosureFn::Some | ClosureFn::All | ClosureFn::None | ClosureFn::One => {
            Ok(ValueSchema::boolean())
        }
        ClosureFn::Map => Ok(ValueSchema::array(body_schema)),
    }
}

fn infer_conditional(
    condition: &Expr,
    yes: &Expr,
    no: &Expr,
    scope: &Scope,
    binders: &Binders,
) -> Result<ValueSchema, ExprError> {
    let cond_ty = infer(condition, scope, binders)?;
    if !matches!(cond_ty, ValueSchema::Boolean { .. }) {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            "conditional expects a boolean condition",
        ));
    }
    let yes_empty = matches!(yes, Expr::List(items) if items.is_empty());
    let no_empty = matches!(no, Expr::List(items) if items.is_empty());
    if yes_empty || no_empty {
        let other = infer(if yes_empty { no } else { yes }, scope, binders)?;
        if !matches!(other, ValueSchema::Array { .. }) {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                "empty list needs an array on the other branch",
            ));
        }
        return Ok(other);
    }
    let l = infer(yes, scope, binders)?;
    let r = infer(no, scope, binders)?;
    merge_schemas(&l, &r, "$").map_err(|msg| ExprError::new("NORN-FLOW-EXPRESSION", msg))
}

fn infer_list(items: &[Expr], scope: &Scope, binders: &Binders) -> Result<ValueSchema, ExprError> {
    let Some(first) = items.first() else {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            "an empty list has no item type; v1 lists are non-empty",
        ));
    };
    let mut item_ty = infer(first, scope, binders)?;
    for item in items.iter().skip(1) {
        let next_ty = infer(item, scope, binders)?;
        item_ty = merge_schemas(&item_ty, &next_ty, "$").map_err(|msg| {
            ExprError::new(
                "NORN-FLOW-EXPRESSION",
                format!("list items disagree: {msg}"),
            )
        })?;
    }
    Ok(ValueSchema::array(item_ty))
}

fn infer_record(
    fields: &[(String, Expr)],
    scope: &Scope,
    binders: &Binders,
) -> Result<ValueSchema, ExprError> {
    let mut props = BTreeSet::new();
    let mut ordered = Vec::with_capacity(fields.len());
    for (name, val) in fields {
        let _ = zen_key(name)?;
        if !props.insert(name.as_str()) {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                format!("record key {name} is repeated"),
            ));
        }
        let ty = infer(val, scope, binders)?;
        ordered.push((name.clone(), ty));
    }
    Ok(ValueSchema::object(ObjectSchema::record_ordered(
        ordered,
        &[],
    )))
}

fn infer_binary(
    op: BinaryOp,
    left: &Expr,
    right: &Expr,
    scope: &Scope,
    binders: &Binders,
) -> Result<ValueSchema, ExprError> {
    let l = infer(left, scope, binders)?;
    let r = infer(right, scope, binders)?;
    if op == BinaryOp::In {
        let ValueSchema::Array { items, .. } = &r else {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                "membership expects a value and an array of the same type",
            ));
        };
        if merge_schemas(&l, items, "$").is_err() {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                "membership expects a value and an array of the same type",
            ));
        }
        return Ok(ValueSchema::boolean());
    }
    if op == BinaryOp::Add
        && matches!(l, ValueSchema::String { .. })
        && matches!(r, ValueSchema::String { .. })
    {
        return Ok(ValueSchema::text());
    }
    if !l.is_numeric() || !r.is_numeric() {
        let suffix = if op == BinaryOp::Add {
            " or two strings"
        } else {
            ""
        };
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!(
                "expected numbers{suffix}, got {} and {}",
                l.type_label(),
                r.type_label()
            ),
        ));
    }
    Ok(ValueSchema::number())
}

fn infer_compare(
    op: CompareOp,
    left: &Expr,
    right: &Expr,
    scope: &Scope,
    binders: &Binders,
) -> Result<ValueSchema, ExprError> {
    let l = infer(left, scope, binders)?;
    let r = infer(right, scope, binders)?;
    if matches!(op, CompareOp::Eq | CompareOp::Ne) {
        let null_check = (matches!(l, ValueSchema::Null { .. })
            && matches!(r, ValueSchema::Nullable(_) | ValueSchema::JsonValue { .. }))
            || (matches!(r, ValueSchema::Null { .. })
                && matches!(l, ValueSchema::Nullable(_) | ValueSchema::JsonValue { .. }));
        let same_scalar = l.is_scalar()
            && r.is_scalar()
            && (l.type_label() == r.type_label()
                || matches!(l, ValueSchema::Null { .. })
                || matches!(r, ValueSchema::Null { .. })
                || (l.is_numeric() && r.is_numeric()));
        if !null_check && !same_scalar {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                format!(
                    "{} compares scalars of one type, not {} and {}",
                    op.wire_str(),
                    l.type_label(),
                    r.type_label()
                ),
            ));
        }
    } else if temporal_kind(left).is_some() && temporal_kind(left) == temporal_kind(right) {
        // Same-kind parsed temporal values order semantically.
    } else if !l.is_numeric() || !r.is_numeric() {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!(
                "{} is qualified for numbers only, not {} and {}",
                op.wire_str(),
                l.type_label(),
                r.type_label()
            ),
        ));
    }
    Ok(ValueSchema::boolean())
}

fn infer_call(
    func: CallFn,
    args: &[Expr],
    scope: &Scope,
    binders: &Binders,
) -> Result<ValueSchema, ExprError> {
    let mut arg_types = Vec::with_capacity(args.len());
    for arg in args {
        arg_types.push(infer(arg, scope, binders)?);
    }
    match func {
        CallFn::Concat => {
            if let (
                Some(ValueSchema::Array { items: a, .. }),
                Some(ValueSchema::Array { items: b, .. }),
            ) = (arg_types.first(), arg_types.get(1))
                && arg_types.len() == 2
            {
                let merged = merge_schemas(a, b, "$")
                    .map_err(|msg| ExprError::new("NORN-FLOW-EXPRESSION", msg))?;
                return Ok(ValueSchema::array(merged));
            }
            Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                "concat expects two arrays",
            ))
        }
        CallFn::Sum => {
            if arg_types.len() == 1
                && let Some(ValueSchema::Array { items, .. }) = arg_types.first()
                && items.is_numeric()
            {
                return Ok(ValueSchema::number());
            }
            Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                "sum expects a list of numbers",
            ))
        }
        CallFn::Floor => {
            if arg_types.len() == 1 && arg_types.first().is_some_and(ValueSchema::is_numeric) {
                return Ok(ValueSchema::integer());
            }
            Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                "floor expects a number",
            ))
        }
        CallFn::Len => {
            if arg_types.len() == 1
                && matches!(
                    arg_types.first(),
                    Some(ValueSchema::String { .. } | ValueSchema::Array { .. })
                )
            {
                return Ok(ValueSchema::integer());
            }
            Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                "length expects a string or array",
            ))
        }
        CallFn::Take => {
            if arg_types.len() == 2
                && matches!(arg_types.first(), Some(ValueSchema::String { .. }))
                && matches!(arg_types.get(1), Some(ValueSchema::Integer { .. }))
            {
                return Ok(ValueSchema::text());
            }
            Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                "take expects a string and an integer literal cap",
            ))
        }
        CallFn::DaysBetween
        | CallFn::AddDays
        | CallFn::TimestampBefore
        | CallFn::TimestampAtOrBefore
        | CallFn::SecondsBetween
        | CallFn::AddSeconds
        | CallFn::IntervalsOverlap
        | CallFn::IntervalGapSeconds
        | CallFn::IntervalIntersectionSeconds
        | CallFn::ResolveLocalTimestamp => infer_temporal_call(func, args, &arg_types),
        CallFn::Lower
        | CallFn::Upper
        | CallFn::Trim
        | CallFn::Date
        | CallFn::Timestamp
        | CallFn::Contains
        | CallFn::StartsWith
        | CallFn::EndsWith
        | CallFn::DateBefore => infer_string_call(func, &arg_types),
    }
}

fn infer_temporal_call(
    func: CallFn,
    args: &[Expr],
    arg_types: &[ValueSchema],
) -> Result<ValueSchema, ExprError> {
    match func {
        CallFn::DaysBetween | CallFn::AddDays => infer_date_arith_call(func, args, arg_types),
        CallFn::SecondsBetween | CallFn::AddSeconds => {
            infer_timestamp_arith_call(func, args, arg_types)
        }
        CallFn::TimestampBefore | CallFn::TimestampAtOrBefore => {
            if args.len() != 2
                || !args
                    .iter()
                    .all(|a| temporal_kind(a) == Some(TemporalKind::Timestamp))
            {
                return Err(ExprError::new(
                    "NORN-FLOW-EXPRESSION",
                    format!("{} expects 2 parsed timestamps", func.wire_str()),
                ));
            }
            Ok(ValueSchema::boolean())
        }
        CallFn::IntervalsOverlap
        | CallFn::IntervalGapSeconds
        | CallFn::IntervalIntersectionSeconds => {
            if args.len() != 4
                || !args
                    .iter()
                    .all(|a| temporal_kind(a) == Some(TemporalKind::Timestamp))
            {
                return Err(ExprError::new(
                    "NORN-FLOW-EXPRESSION",
                    format!("{} expects 4 parsed timestamps", func.wire_str()),
                ));
            }
            if func == CallFn::IntervalsOverlap {
                Ok(ValueSchema::boolean())
            } else {
                Ok(ValueSchema::number())
            }
        }
        CallFn::ResolveLocalTimestamp => {
            if arg_types.len() != 3
                || !arg_types
                    .iter()
                    .all(|a| matches!(a, ValueSchema::String { .. }))
            {
                return Err(ExprError::new(
                    "NORN-FLOW-EXPRESSION",
                    "resolve_local_timestamp expects 3 string arguments (localWallTime, ianaTz, dstPolicy)",
                ));
            }
            Ok(ValueSchema::text())
        }
        CallFn::Concat
        | CallFn::Lower
        | CallFn::Upper
        | CallFn::Trim
        | CallFn::Len
        | CallFn::Take
        | CallFn::Contains
        | CallFn::StartsWith
        | CallFn::EndsWith
        | CallFn::DateBefore
        | CallFn::Date
        | CallFn::Timestamp
        | CallFn::Sum
        | CallFn::Floor => Ok(ValueSchema::null_value()),
    }
}

fn infer_date_arith_call(
    func: CallFn,
    args: &[Expr],
    arg_types: &[ValueSchema],
) -> Result<ValueSchema, ExprError> {
    if args.len() != 2 {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!("{} expects 2 arguments", func.wire_str()),
        ));
    }
    let date_count = if func == CallFn::DaysBetween { 2 } else { 1 };
    let dates_ok = args.get(..date_count).is_some_and(|slice| {
        slice
            .iter()
            .all(|a| temporal_kind(a) == Some(TemporalKind::Date))
    });
    if !dates_ok {
        let ctor = if func == CallFn::DaysBetween {
            "daysBetween"
        } else {
            "addDays"
        };
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!(
                "{} expects parsed dates; build it with {ctor}",
                func.wire_str()
            ),
        ));
    }
    if func == CallFn::AddDays {
        let second = arg_types.get(1);
        if !matches!(second, Some(ValueSchema::Integer { .. })) {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                format!(
                    "add_days adds a whole number of days, not {}",
                    second.map_or_else(|| "unknown".to_owned(), ValueSchema::type_label)
                ),
            ));
        }
        return Ok(ValueSchema::text());
    }
    Ok(ValueSchema::integer())
}

fn infer_timestamp_arith_call(
    func: CallFn,
    args: &[Expr],
    arg_types: &[ValueSchema],
) -> Result<ValueSchema, ExprError> {
    if args.len() != 2 {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!("{} expects 2 arguments", func.wire_str()),
        ));
    }
    let ts_count = if func == CallFn::SecondsBetween { 2 } else { 1 };
    let ts_ok = args.get(..ts_count).is_some_and(|slice| {
        slice
            .iter()
            .all(|a| temporal_kind(a) == Some(TemporalKind::Timestamp))
    });
    if !ts_ok {
        let ctor = if func == CallFn::SecondsBetween {
            "secondsBetween"
        } else {
            "addSeconds"
        };
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!(
                "{} expects parsed timestamps; build it with {ctor}",
                func.wire_str()
            ),
        ));
    }
    if func == CallFn::AddSeconds {
        let second = arg_types.get(1);
        if !matches!(second, Some(ValueSchema::Integer { .. })) {
            return Err(ExprError::new(
                "NORN-FLOW-EXPRESSION",
                format!(
                    "add_seconds adds a whole number of seconds, not {}",
                    second.map_or_else(|| "unknown".to_owned(), ValueSchema::type_label)
                ),
            ));
        }
        return Ok(ValueSchema::text());
    }
    Ok(ValueSchema::number())
}

fn infer_string_call(func: CallFn, arg_types: &[ValueSchema]) -> Result<ValueSchema, ExprError> {
    let arity = if matches!(
        func,
        CallFn::Lower | CallFn::Upper | CallFn::Trim | CallFn::Date | CallFn::Timestamp
    ) {
        1
    } else {
        2
    };
    if arg_types.len() != arity
        || !arg_types
            .iter()
            .all(|a| matches!(a, ValueSchema::String { .. }))
    {
        return Err(ExprError::new(
            "NORN-FLOW-EXPRESSION",
            format!("{} expects {arity} string arguments", func.wire_str()),
        ));
    }
    if arity == 1 {
        Ok(ValueSchema::text())
    } else {
        Ok(ValueSchema::boolean())
    }
}
