//! HOST-READ-3: the object grid query grammar (v1) and its evaluation.
//!
//! The grammar is parsed and validated against the type's effective
//! properties, every violation reported at once with a pointer into the
//! request. Filtering and sorting run over the rows the caller may see,
//! never over values it may not (D67):
//!
//! - a withheld row matches no filter clause, not even `empty`, `present` or
//!   `ne`; a missing value matches only `empty`;
//! - missing and withheld values sort last in both directions, as one tail
//!   group in id order;
//! - nothing value-derived reaches a withheld row's record, the cursor or the
//!   counts.
//!
//! The cursor carries no value: a digest of the type and normalized query,
//! digests of the scope and the caller, the snapshot, and the resume position
//! (a plain listing's last row id, or a digest of it under a sort or filter),
//! within `MAX_CURSOR_BYTES` whatever the id's length.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use norn_ir::{Decimal, PropertyType, RuntimeValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::refusal::{Refusal, RefusalDiagnostic, RefusalSpan};

/// Default and largest page.
pub const DEFAULT_LIMIT: usize = 50;
/// Largest page a caller may ask for.
pub const MAX_LIMIT: usize = 200;
const MAX_SORT_KEYS: usize = 3;
const MAX_CLAUSES: usize = 8;
const MAX_IN_VALUES: usize = 50;
pub(crate) const CURSOR_KEY: &[u8] = b"norn.object-cursor/1";

/// The raw query string fields of `GET /v1/objects/{type}`.
#[derive(Debug, Default, Clone, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ObjectQueryParams {
    /// Page size, 1 to 200; 50 when absent.
    #[param(value_type = Option<u32>, minimum = 1, maximum = 200, nullable = false)]
    pub limit: Option<String>,
    /// Up to three sort keys, `property:asc` or `property:desc`, comma separated.
    /// A property sorts only when it has a comparable type; missing and
    /// withheld values sort last in both directions.
    #[param(nullable = false, example = "name:asc")]
    pub sort: Option<String>,
    /// A JSON document `{"all":[clause, ...]}` of at most eight clauses, each
    /// `{"property", "op", "value"}`. Operators by property type: text `eq`,
    /// `ne`, `in`, `contains`, `prefix`; integer and decimal `eq`, `ne`, `in`,
    /// `lt`, `lte`, `gt`, `gte`, `between`; date, timestamp and duration the
    /// same without `in`; boolean `eq`; enum `eq`, `ne`, `in`; `empty` and
    /// `present` on any comparable type. `in` takes at most 50 values. A
    /// withheld row matches no clause.
    #[param(
        nullable = false,
        example = r#"{"all":[{"property":"name","op":"prefix","value":"A"}]}"#
    )]
    pub filter: Option<String>,
    /// An opaque cursor from a previous page's `next`, valid only for the same
    /// type, query, scope and caller, and only until the cell next commits.
    #[param(nullable = false)]
    pub after: Option<String>,
}

/// What a property's values are compared as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    Text,
    Integer,
    Decimal,
    Date,
    Timestamp,
    Duration,
    Boolean,
    Enum(Vec<String>),
    /// `record`, `json-ext`, `ref`, a decimal binding (an interface slot with no stored value) or a type this cell cannot resolve.
    Opaque,
}

impl Kind {
    fn of(property: Option<&PropertyType>) -> Self {
        match property {
            Some(PropertyType::Text { .. }) => Self::Text,
            Some(PropertyType::Integer { .. }) => Self::Integer,
            Some(PropertyType::Decimal { .. }) => Self::Decimal,
            Some(PropertyType::Date { .. }) => Self::Date,
            Some(PropertyType::Timestamp { .. }) => Self::Timestamp,
            Some(PropertyType::Duration { .. }) => Self::Duration,
            Some(PropertyType::Boolean { .. }) => Self::Boolean,
            Some(PropertyType::Enum { values }) => {
                Self::Enum(values.iter().map(|value| value.value.clone()).collect())
            }
            Some(
                PropertyType::Record { .. }
                | PropertyType::JsonExt { .. }
                | PropertyType::Ref { .. }
                | PropertyType::DecimalBinding { .. },
            )
            | None => Self::Opaque,
        }
    }

    const fn sortable(&self) -> bool {
        !matches!(self, Self::Opaque)
    }

    fn admits(&self, op: Op) -> bool {
        use Op::{Between, Contains, Empty, Eq, Gt, Gte, In, Lt, Lte, Ne, Prefix, Present};
        if matches!(op, Empty | Present) {
            return !matches!(self, Self::Opaque);
        }
        match self {
            Self::Text => matches!(op, Eq | Ne | In | Contains | Prefix),
            Self::Integer | Self::Decimal => {
                matches!(op, Eq | Ne | In | Lt | Lte | Gt | Gte | Between)
            }
            Self::Date | Self::Timestamp | Self::Duration => {
                matches!(op, Eq | Ne | Lt | Lte | Gt | Gte | Between)
            }
            Self::Boolean => matches!(op, Eq),
            Self::Enum(_) => matches!(op, Eq | Ne | In),
            Self::Opaque => false,
        }
    }
}

/// One comparison operator of the grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Op {
    Eq,
    Ne,
    In,
    Contains,
    Prefix,
    Lt,
    Lte,
    Gt,
    Gte,
    Between,
    Empty,
    Present,
}

/// A comparable value of one kind.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Scalar {
    Text(String),
    Number(Decimal),
    Bool(bool),
    /// Days since 1970-01-01.
    Date(i64),
    /// Nanoseconds since 1970-01-01T00:00:00Z.
    Instant(i128),
    /// Nanoseconds.
    Span(i128),
}

/// A clause's operand, already typed.
#[derive(Debug, Clone)]
pub(crate) enum Operand {
    None,
    One(Scalar),
    Many(Vec<Scalar>),
    Range(Scalar, Scalar),
}

/// One parsed filter clause.
#[derive(Debug, Clone)]
pub(crate) struct Clause {
    pub(crate) property: String,
    pub(crate) kind: Kind,
    pub(crate) op: Op,
    pub(crate) operand: Operand,
    /// The clause as applied, echoed in `query.filter`.
    pub(crate) normalized: Value,
}

/// One sort key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SortKey {
    pub(crate) property: String,
    pub(crate) kind: Kind,
    pub(crate) descending: bool,
}

/// A validated query.
#[derive(Debug, Clone)]
pub(crate) struct ObjectQuery {
    pub(crate) limit: usize,
    pub(crate) sort: Vec<SortKey>,
    pub(crate) filter: Vec<Clause>,
    pub(crate) after: Option<String>,
}

impl ObjectQuery {
    /// The applied sort, as echoed.
    pub(crate) fn sort_document(&self) -> Vec<crate::objects::NornSortKey> {
        self.sort
            .iter()
            .map(|key| crate::objects::NornSortKey {
                property: key.property.clone(),
                direction: if key.descending { "desc" } else { "asc" }.to_owned(),
            })
            .collect()
    }

    /// The applied filter, as echoed; `None` without one.
    pub(crate) fn filter_document(&self) -> Option<Value> {
        (!self.filter.is_empty()).then(|| {
            serde_json::json!({ "all": self.filter.iter().map(|clause| clause.normalized.clone()).collect::<Vec<_>>() })
        })
    }

    /// Whether the query needs every row before the first page.
    pub(crate) fn needs_full_scan(&self) -> bool {
        !self.sort.is_empty() || !self.filter.is_empty()
    }

    /// A digest of the type and the normalized query, bound into cursors.
    pub(crate) fn digest(&self, object_type: &str) -> String {
        let normalized = serde_json::json!({
            "type": object_type,
            "sort": self.sort_document().iter().map(|key| format!("{}:{}", key.property, key.direction)).collect::<Vec<_>>(),
            "filter": self.filter_document(),
        });
        norn_cell::compute::lineage::keyed_lineage_handle(CURSOR_KEY, &normalized.to_string())
    }
}

struct Findings(Vec<RefusalDiagnostic>);

impl Findings {
    /// Record one violation at `pointer`.
    fn push(&mut self, code: &str, message: impl Into<String>, pointer: impl Into<String>) {
        self.0.push(RefusalDiagnostic {
            code: code.to_owned(),
            severity: "error".to_owned(),
            message: message.into(),
            span: Some(RefusalSpan {
                pointer: pointer.into(),
            }),
            law: None,
        });
    }
}

/// A 400 carrying every violation.
fn invalid(findings: Vec<RefusalDiagnostic>) -> Refusal {
    let mut refusal = Refusal::new(
        http::StatusCode::BAD_REQUEST,
        "query.invalid",
        "the object query is not valid",
        "each diagnostic names the rule and where in the request it applies",
    );
    refusal.diagnostics = findings;
    refusal
}

/// A 400 listing `violations`, each `(code, message, pointer)`.
pub(crate) fn refused(violations: Vec<(&str, String, String)>) -> Refusal {
    let mut findings = Findings(Vec::new());
    for (code, message, pointer) in violations {
        findings.push(code, message, pointer);
    }
    invalid(findings.0)
}

/// A 400 for a cursor this query cannot continue from.
pub(crate) fn cursor_invalid() -> Refusal {
    invalid(vec![RefusalDiagnostic {
        code: "query.cursor_invalid".to_owned(),
        severity: "error".to_owned(),
        message: "the cursor does not continue this query for this caller".to_owned(),
        span: Some(RefusalSpan {
            pointer: "/after".to_owned(),
        }),
        law: None,
    }])
}

/// Whether `text` is a property name, `[a-z][a-z0-9_]*`.
fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Parse and validate `params` against `properties`, the type's effective
/// properties. Every violation is reported, each at its pointer.
///
/// # Errors
/// 400 `query.invalid` with one diagnostic per violation.
pub(crate) fn parse(
    params: &ObjectQueryParams,
    properties: &[(String, Option<PropertyType>)],
) -> Result<ObjectQuery, Refusal> {
    let kinds: BTreeMap<&str, Kind> = properties
        .iter()
        .map(|(id, property)| (id.as_str(), Kind::of(property.as_ref())))
        .collect();
    let mut findings = Findings(Vec::new());
    let limit = match params.limit.as_deref() {
        None => DEFAULT_LIMIT,
        Some(text) => match text.parse::<usize>() {
            Ok(limit)
                if (1..=MAX_LIMIT).contains(&limit) && text.bytes().all(|b| b.is_ascii_digit()) =>
            {
                limit
            }
            _ => {
                findings.push(
                    "query.limit_invalid",
                    format!("limit must be 1..={MAX_LIMIT}"),
                    "/limit",
                );
                DEFAULT_LIMIT
            }
        },
    };
    let sort = parse_sort(params.sort.as_deref(), &kinds, &mut findings);
    let filter = parse_filter(params.filter.as_deref(), &kinds, &mut findings);
    if let Some(after) = params.after.as_deref()
        && (after.is_empty() || after.len() > MAX_CURSOR_BYTES)
    {
        findings.push(
            "query.cursor_invalid",
            "the cursor is not one this host issued",
            "/after",
        );
    }
    if findings.0.is_empty() {
        Ok(ObjectQuery {
            limit,
            sort,
            filter,
            after: params.after.clone(),
        })
    } else {
        Err(invalid(findings.0))
    }
}

/// The sort keys of `sort=prop:dir[,…]`, each violation recorded at its pointer.
fn parse_sort(
    text: Option<&str>,
    kinds: &BTreeMap<&str, Kind>,
    findings: &mut Findings,
) -> Vec<SortKey> {
    let Some(text) = text else { return Vec::new() };
    let parts: Vec<&str> = text.split(',').collect();
    if parts.len() > MAX_SORT_KEYS {
        findings.push(
            "query.too_many",
            format!("at most {MAX_SORT_KEYS} sort keys"),
            "/sort",
        );
    }
    let mut keys = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let at = format!("/sort/{index}");
        let Some((property, direction)) = part.split_once(':') else {
            findings.push(
                "query.sort_invalid",
                "each sort key is prop:asc or prop:desc",
                at,
            );
            continue;
        };
        let descending = match direction {
            "asc" => false,
            "desc" => true,
            _ => {
                findings.push("query.sort_invalid", "the direction is asc or desc", at);
                continue;
            }
        };
        if !is_name(property) {
            findings.push(
                "query.sort_invalid",
                "a property name matches [a-z][a-z0-9_]*",
                at,
            );
            continue;
        }
        let Some(kind) = kinds.get(property) else {
            findings.push(
                "query.property_unknown",
                format!("{property} is not a property of this type"),
                at,
            );
            continue;
        };
        if !kind.sortable() {
            findings.push(
                "query.sort_invalid",
                format!("{property} is not sortable"),
                at,
            );
            continue;
        }
        if keys.iter().any(|key: &SortKey| key.property == property) {
            findings.push(
                "query.sort_invalid",
                format!("{property} is already a sort key"),
                at,
            );
            continue;
        }
        keys.push(SortKey {
            property: property.to_owned(),
            kind: kind.clone(),
            descending,
        });
    }
    keys
}

/// The clauses of `filter={"all":[…]}`, each violation recorded at its pointer.
fn parse_filter(
    text: Option<&str>,
    kinds: &BTreeMap<&str, Kind>,
    findings: &mut Findings,
) -> Vec<Clause> {
    let Some(text) = text else { return Vec::new() };
    let Ok(Value::Object(document)) = serde_json::from_str::<Value>(text) else {
        findings.push(
            "query.value_invalid",
            "filter is a JSON object {\"all\": [...]}",
            "/filter",
        );
        return Vec::new();
    };
    if document.keys().any(|key| key != "all") {
        findings.push(
            "query.value_invalid",
            "filter has one member, all",
            "/filter",
        );
    }
    let Some(Value::Array(clauses)) = document.get("all") else {
        findings.push(
            "query.value_invalid",
            "filter.all is an array of clauses",
            "/filter/all",
        );
        return Vec::new();
    };
    if clauses.is_empty() {
        findings.push(
            "query.value_invalid",
            "filter.all holds 1 to 8 clauses",
            "/filter/all",
        );
    }
    if clauses.len() > MAX_CLAUSES {
        findings.push(
            "query.too_many",
            format!("at most {MAX_CLAUSES} clauses"),
            "/filter/all",
        );
    }
    clauses
        .iter()
        .enumerate()
        .filter_map(|(index, clause)| {
            parse_clause(clause, &format!("/filter/all/{index}"), kinds, findings)
        })
        .collect()
}

/// One clause at `at`, typed against its property's kind.
fn parse_clause(
    clause: &Value,
    at: &str,
    kinds: &BTreeMap<&str, Kind>,
    findings: &mut Findings,
) -> Option<Clause> {
    let Value::Object(members) = clause else {
        findings.push(
            "query.value_invalid",
            "a clause is an object {property, op, value?}",
            at.to_owned(),
        );
        return None;
    };
    if members
        .keys()
        .any(|key| !matches!(key.as_str(), "property" | "op" | "value"))
    {
        findings.push(
            "query.value_invalid",
            "a clause has only property, op and value",
            at.to_owned(),
        );
    }
    let property = match members.get("property") {
        Some(Value::String(property)) if is_name(property) => property.clone(),
        _ => {
            findings.push(
                "query.property_unknown",
                "property names a property",
                format!("{at}/property"),
            );
            return None;
        }
    };
    let Some(kind) = kinds.get(property.as_str()).cloned() else {
        findings.push(
            "query.property_unknown",
            format!("{property} is not a property of this type"),
            format!("{at}/property"),
        );
        return None;
    };
    let op = match members.get("op").cloned().map(serde_json::from_value::<Op>) {
        Some(Ok(op)) if kind.admits(op) => op,
        Some(Ok(_)) => {
            findings.push(
                "query.op_invalid",
                format!("this operator does not apply to {property}"),
                format!("{at}/op"),
            );
            return None;
        }
        _ => {
            findings.push(
                "query.op_invalid",
                "op is one of the grammar's operators",
                format!("{at}/op"),
            );
            return None;
        }
    };
    let value = members.get("value");
    let operand = operand(op, value, &kind, &format!("{at}/value"), findings)?;
    let echo = |scalar: &Scalar, original: &Value| match scalar {
        Scalar::Number(number) => Value::String(number.canonical()),
        Scalar::Text(_)
        | Scalar::Bool(_)
        | Scalar::Date(_)
        | Scalar::Instant(_)
        | Scalar::Span(_) => original.clone(),
    };
    let mut normalized = serde_json::json!({ "property": property, "op": op });
    let echoed = match (&operand, value) {
        (Operand::One(one), Some(original)) => Some(echo(one, original)),
        (Operand::Many(many), Some(Value::Array(original))) => Some(Value::Array(
            many.iter().zip(original).map(|(s, o)| echo(s, o)).collect(),
        )),
        (Operand::Range(lo, hi), Some(Value::Array(original))) => Some(Value::Array(
            [lo, hi]
                .into_iter()
                .zip(original)
                .map(|(s, o)| echo(s, o))
                .collect(),
        )),
        _ => None,
    };
    if let (Some(echoed), Some(members)) = (echoed, normalized.as_object_mut()) {
        members.insert("value".to_owned(), echoed);
    }
    Some(Clause {
        property,
        kind,
        op,
        operand,
        normalized,
    })
}

/// The typed operand `op` takes, from the clause's `value`.
fn operand(
    op: Op,
    value: Option<&Value>,
    kind: &Kind,
    pointer: &str,
    findings: &mut Findings,
) -> Option<Operand> {
    match op {
        Op::Empty | Op::Present => {
            if value.is_some() {
                findings.push(
                    "query.value_invalid",
                    "empty and present take no value",
                    pointer.to_owned(),
                );
                return None;
            }
            Some(Operand::None)
        }
        Op::In => {
            let Some(Value::Array(items)) = value else {
                findings.push(
                    "query.value_invalid",
                    "in takes an array of 1 to 50 values",
                    pointer.to_owned(),
                );
                return None;
            };
            if items.is_empty() || items.len() > MAX_IN_VALUES {
                let code = if items.is_empty() {
                    "query.value_invalid"
                } else {
                    "query.too_many"
                };
                findings.push(code, "in takes 1 to 50 values", pointer.to_owned());
                return None;
            }
            let parsed: Vec<Option<Scalar>> = items
                .iter()
                .enumerate()
                .map(|(index, item)| scalar(item, kind, &format!("{pointer}/{index}"), findings))
                .collect();
            parsed
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .map(Operand::Many)
        }
        Op::Between => {
            let Some([lo, hi]) = value
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .and_then(|items| <&[Value; 2]>::try_from(items).ok())
            else {
                findings.push(
                    "query.value_invalid",
                    "between takes [lo, hi]",
                    pointer.to_owned(),
                );
                return None;
            };
            let lo = scalar(lo, kind, &format!("{pointer}/0"), findings);
            let hi = scalar(hi, kind, &format!("{pointer}/1"), findings);
            let (lo, hi) = (lo?, hi?);
            if lo > hi {
                findings.push(
                    "query.value_invalid",
                    "between's lo is at most its hi",
                    pointer.to_owned(),
                );
                return None;
            }
            Some(Operand::Range(lo, hi))
        }
        Op::Eq | Op::Ne | Op::Contains | Op::Prefix | Op::Lt | Op::Lte | Op::Gt | Op::Gte => {
            let Some(value) = value else {
                findings.push(
                    "query.value_invalid",
                    "this operator takes a value",
                    pointer.to_owned(),
                );
                return None;
            };
            scalar(value, kind, pointer, findings).map(Operand::One)
        }
    }
}

/// Parse a filter value of `kind`; values are JSON strings except booleans.
fn scalar(value: &Value, kind: &Kind, pointer: &str, findings: &mut Findings) -> Option<Scalar> {
    let bad = |findings: &mut Findings, message: &str| {
        findings.push(
            "query.value_invalid",
            message.to_owned(),
            pointer.to_owned(),
        );
        None
    };
    match (kind, value) {
        (Kind::Boolean, Value::Bool(flag)) => Some(Scalar::Bool(*flag)),
        (Kind::Boolean, _) => bad(findings, "a boolean value is true or false"),
        (_, Value::String(text)) => match parse_text(kind, text) {
            Some(parsed) => Some(parsed),
            None => bad(
                findings,
                match kind {
                    Kind::Integer => "an integer value is a string of digits",
                    Kind::Decimal => "a decimal value is exact decimal text",
                    Kind::Date => "a date value is YYYY-MM-DD",
                    Kind::Timestamp => "a timestamp value is RFC 3339 with an offset",
                    Kind::Duration => "a duration value is ISO 8601 (PnDTnHnMnS)",
                    Kind::Enum(_) => "an enum value is one of the declared values",
                    Kind::Text | Kind::Boolean | Kind::Opaque => {
                        "this value does not fit the property"
                    }
                },
            ),
        },
        _ => bad(findings, "values are JSON strings, except booleans"),
    }
}

/// Parse `text` as `kind`: the filter value grammar, and how stored values are read.
pub(crate) fn parse_text(kind: &Kind, text: &str) -> Option<Scalar> {
    match kind {
        Kind::Text => Some(Scalar::Text(text.to_owned())),
        Kind::Enum(values) => values
            .iter()
            .any(|value| value == text)
            .then(|| Scalar::Text(text.to_owned())),
        Kind::Integer => {
            let digits = text.strip_prefix('-').unwrap_or(text);
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| Decimal::parse(text).ok())
                .flatten()
                .map(Scalar::Number)
        }
        Kind::Decimal => {
            let unsigned = text.strip_prefix('-').unwrap_or(text);
            let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, "0"));
            (!whole.is_empty()
                && !fraction.is_empty()
                && whole
                    .bytes()
                    .chain(fraction.bytes())
                    .all(|b| b.is_ascii_digit()))
            .then(|| Decimal::parse(text).ok())
            .flatten()
            .map(Scalar::Number)
        }
        Kind::Date => date_days(text).map(Scalar::Date),
        Kind::Timestamp => instant_nanos(text).map(Scalar::Instant),
        Kind::Duration => duration_nanos(text).map(Scalar::Span),
        Kind::Boolean | Kind::Opaque => None,
    }
}

/// A stored value as the property's kind: `None` when missing or not readable as it.
pub(crate) fn stored(value: Option<&RuntimeValue>, kind: &Kind) -> Option<Scalar> {
    match (kind, value?) {
        (Kind::Boolean, RuntimeValue::Bool(flag)) => Some(Scalar::Bool(*flag)),
        (Kind::Integer | Kind::Decimal, RuntimeValue::Number(number)) => {
            Some(Scalar::Number(*number))
        }
        (_, RuntimeValue::String(text)) => parse_text(kind, text),
        _ => None,
    }
}

/// Days from 1970-01-01 for a proleptic Gregorian `YYYY-MM-DD`.
fn date_days(text: &str) -> Option<i64> {
    if text.len() != 10 || text.get(4..5) != Some("-") || text.get(7..8) != Some("-") {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: i64 = text.get(5..7)?.parse().ok()?;
    let day: i64 = text.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "remainders by non-zero constants"
)]
const fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Howard Hinnant's days-from-civil.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "a four-digit year, a month and a day: every term is far inside i64"
)]
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Nanoseconds since the epoch for RFC 3339 with an explicit offset (`Z` or `±HH:MM`).
#[allow(
    clippy::arithmetic_side_effects,
    reason = "two-digit clock fields and a four-digit year: every term is far inside i128"
)]
fn instant_nanos(text: &str) -> Option<i128> {
    let (date, rest) = text.split_once(['T', 't'])?;
    let days = date_days(date)?;
    let offset_at = rest.rfind(['Z', 'z', '+', '-'])?;
    let (clock, offset) = rest.split_at(offset_at);
    let offset_seconds: i128 = match offset {
        "Z" | "z" => 0,
        _ => {
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let (hours, minutes) = offset.get(1..)?.split_once(':')?;
            if hours.len() != 2 || minutes.len() != 2 {
                return None;
            }
            let (hours, minutes): (i128, i128) = (hours.parse().ok()?, minutes.parse().ok()?);
            if hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let (hms, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut parts = hms.split(':');
    let (hour, minute, second) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || [hour, minute, second].iter().any(|part| part.len() != 2) {
        return None;
    }
    let (hour, minute, second): (i128, i128, i128) = (
        hour.parse().ok()?,
        minute.parse().ok()?,
        second.parse().ok()?,
    );
    if hour > 23
        || minute > 59
        || second > 60
        || fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let nanos: i128 = if fraction.is_empty() {
        0
    } else {
        format!("{fraction:0<9}").parse().ok()?
    };
    let seconds = i128::from(days) * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some(seconds * 1_000_000_000 + nanos)
}

/// Nanoseconds of an ISO 8601 duration `P[nW|nD][T[nH][nM][n[.n]S]]`; years
/// and months have no fixed length and refuse.
fn duration_nanos(text: &str) -> Option<i128> {
    let rest = text.strip_prefix('P')?;
    if rest.is_empty() {
        return None;
    }
    let (date, time) = rest
        .split_once('T')
        .map_or((rest, None), |(d, t)| (d, Some(t)));
    if time.is_some_and(str::is_empty) {
        return None;
    }
    let mut total: i128 = 0;
    let mut number = String::new();
    for c in date.chars() {
        match c {
            '0'..='9' => number.push(c),
            'W' | 'D' if !number.is_empty() => {
                let unit: i128 = if c == 'W' { 604_800 } else { 86_400 };
                total = number
                    .parse::<i128>()
                    .ok()?
                    .checked_mul(unit.checked_mul(1_000_000_000)?)
                    .and_then(|part| total.checked_add(part))?;
                number.clear();
            }
            _ => return None,
        }
    }
    if !number.is_empty() {
        return None;
    }
    for c in time.unwrap_or_default().chars() {
        match c {
            '0'..='9' | '.' => number.push(c),
            'H' | 'M' | 'S' if !number.is_empty() => {
                let unit: i128 = match c {
                    'H' => 3600,
                    'M' => 60,
                    _ => 1,
                };
                let (whole, fraction) = number.split_once('.').unwrap_or((&number, ""));
                if (c != 'S' && !fraction.is_empty()) || fraction.len() > 9 {
                    return None;
                }
                let nanos: i128 = if fraction.is_empty() {
                    0
                } else {
                    format!("{fraction:0<9}").parse().ok()?
                };
                total = whole
                    .parse::<i128>()
                    .ok()?
                    .checked_mul(unit.checked_mul(1_000_000_000)?)
                    .and_then(|part| part.checked_add(nanos))
                    .and_then(|part| total.checked_add(part))?;
                number.clear();
            }
            _ => return None,
        }
    }
    number.is_empty().then_some(total)
}

/// One row as the query sees it: `None` when its values are withheld.
pub(crate) type RowValues<'a> = Option<&'a BTreeMap<String, RuntimeValue>>;

/// Whether a row matches every clause (D67: a withheld row matches none).
pub(crate) fn matches(filter: &[Clause], values: RowValues<'_>) -> bool {
    filter.iter().all(|clause| {
        let Some(values) = values else { return false };
        let value = stored(values.get(&clause.property), &clause.kind);
        match (&clause.op, &clause.operand, value) {
            (Op::Empty, _, value) => value.is_none(),
            (Op::Present, _, value) => value.is_some(),
            (Op::Eq, Operand::One(want), Some(have)) => have == *want,
            (Op::Ne, Operand::One(want), Some(have)) => have != *want,
            (Op::In, Operand::Many(set), Some(have)) => set.contains(&have),
            (Op::Lt, Operand::One(want), Some(have)) => have < *want,
            (Op::Lte, Operand::One(want), Some(have)) => have <= *want,
            (Op::Gt, Operand::One(want), Some(have)) => have > *want,
            (Op::Gte, Operand::One(want), Some(have)) => have >= *want,
            (Op::Between, Operand::Range(lo, hi), Some(have)) => *lo <= have && have <= *hi,
            (Op::Contains, Operand::One(Scalar::Text(want)), Some(Scalar::Text(have))) => {
                have.contains(want.as_str())
            }
            (Op::Prefix, Operand::One(Scalar::Text(want)), Some(Scalar::Text(have))) => {
                have.starts_with(want.as_str())
            }
            _ => false,
        }
    })
}

/// Order two rows by `sort`, then id: missing and withheld values last in
/// both directions, one tail group in id order.
pub(crate) fn compare(
    sort: &[SortKey],
    a: (&str, RowValues<'_>),
    b: (&str, RowValues<'_>),
) -> Ordering {
    for key in sort {
        let left =
            a.1.and_then(|values| stored(values.get(&key.property), &key.kind));
        let right =
            b.1.and_then(|values| stored(values.get(&key.property), &key.kind));
        let order = match (left, right) {
            (Some(left), Some(right)) if key.descending => right.cmp(&left),
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.0.cmp(b.0)
}

/// Longest `after` the host accepts. It covers the largest cursor the host
/// issues: the fixed-size header (digests and the snapshot, at most a few
/// hundred bytes) and a plain listing's resume id of up to
/// [`norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES`] bytes, base64url.
pub const MAX_CURSOR_BYTES: usize = 6_144;

/// What a cursor binds: no value of any row. The header is fixed-size: the
/// type and query bind through `q`, the scope and the caller through digests.
/// The resume position `p` travels outside the JSON, so it is never escaped.
/// A plain listing resumes by the last row's id, and a sort or filter by a
/// digest of it, which keeps that cursor small beside a large filter.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) v: u8,
    pub(crate) q: String,
    pub(crate) s: String,
    pub(crate) a: String,
    pub(crate) c: u64,
    pub(crate) r: Option<(u64, u64)>,
    #[serde(skip)]
    pub(crate) p: String,
}

impl Cursor {
    /// The opaque wire form: base64url of the header's JSON, `.`, and
    /// base64url of the position's UTF-8.
    pub(crate) fn encode(&self) -> String {
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_string(self).unwrap_or_default()),
            URL_SAFE_NO_PAD.encode(self.p.as_bytes()),
        )
    }

    /// The cursor `text` encodes, if it is a version-2 cursor.
    pub(crate) fn decode(text: &str) -> Option<Self> {
        let (header, position) = text.split_once('.')?;
        let mut cursor = serde_json::from_slice::<Self>(&URL_SAFE_NO_PAD.decode(header).ok()?)
            .ok()
            .filter(|cursor| cursor.v == 2)?;
        cursor.p = String::from_utf8(URL_SAFE_NO_PAD.decode(position).ok()?).ok()?;
        Some(cursor)
    }

    /// The digest a cursor binds the caller by.
    pub(crate) fn actor_digest(actor: &str) -> String {
        norn_cell::compute::lineage::keyed_lineage_handle(CURSOR_KEY, &format!("actor:{actor}"))
    }

    /// The digest a cursor binds the scope by.
    pub(crate) fn scope_digest(scope: &str) -> String {
        norn_cell::compute::lineage::keyed_lineage_handle(CURSOR_KEY, &format!("scope:{scope}"))
    }

    /// The position a sort or filter resumes after: a digest of the row id.
    pub(crate) fn row_digest(id: &str) -> String {
        norn_cell::compute::lineage::keyed_lineage_handle(CURSOR_KEY, &format!("row:{id}"))
    }
}

#[cfg(test)]
mod cursor_tests {
    #![allow(clippy::unwrap_used, reason = "cursor bound")]
    use super::{Cursor, MAX_CURSOR_BYTES};

    /// The largest cursor the host can issue round-trips and fits the bound
    /// it accepts: a maximal resume id of four-byte characters, and the
    /// largest snapshot numbers.
    #[test]
    fn the_largest_issued_cursor_fits_the_accepted_bound() {
        let digest = Cursor::actor_digest("any");
        let id = "\u{1F600}".repeat(norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES / 4);
        let cursor = Cursor {
            v: 2,
            q: digest.clone(),
            s: digest.clone(),
            a: digest,
            c: u64::MAX,
            r: Some((u64::MAX, u64::MAX)),
            p: id,
        };
        let wire = cursor.encode();
        assert!(wire.len() <= MAX_CURSOR_BYTES, "{} bytes", wire.len());
        assert_eq!(Cursor::decode(&wire).unwrap(), cursor);
    }
}
