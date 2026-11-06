//! A question, and a dashboard of questions, in plain words (D59 sign-off).
//!
//! A person signs off on a question by reading it back in words generated
//! from its IR, never a model's paraphrase. The cell recomputes the same
//! words when it installs the definition and compares their hash with the one
//! signed ([`restatement_sha256`]). One implementation serves both sides:
//! Studio calls it through `norn-checker-wasm`, the cell natively.
//!
//! The wording is a contract fixed by golden fixtures
//! (`fixtures/restatement/cases.json`): a change to it changes every hash.

use std::fmt;

use norn_ir::RuntimeValue;
use norn_ir::metric::{
    FilterOperator, MeasureAggregate, MetricDefinition, MetricFilter, MetricType, QuestionIR,
    QuestionTimeWindow, TimeGrain,
};
use sha2::{Digest, Sha256};

/// Why a question or dashboard cannot be restated.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RestateError {
    /// The question selects a metric the declarations do not hold.
    UnknownMetric(String),
    /// A ratio metric names a numerator or denominator the declarations do
    /// not hold.
    UnknownOperand {
        /// The ratio metric.
        metric: String,
        /// The missing operand.
        operand: String,
    },
    /// A dashboard payload that is not `{title, tiles: [{title, question}]}`.
    Dashboard(String),
}

impl fmt::Display for RestateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownMetric(metric) => write!(f, "no declared metric {metric}"),
            Self::UnknownOperand { metric, operand } => {
                write!(f, "ratio metric {metric} names undeclared metric {operand}")
            }
            Self::Dashboard(detail) => write!(f, "not a dashboard: {detail}"),
        }
    }
}

impl std::error::Error for RestateError {}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn words(name: &str) -> String {
    name.replace('_', " ")
}

fn operator_words(operator: FilterOperator) -> &'static str {
    match operator {
        FilterOperator::Eq => "is",
        FilterOperator::Ne => "is not",
        FilterOperator::Lt => "is below",
        FilterOperator::Le => "is at most",
        FilterOperator::Gt => "is above",
        FilterOperator::Ge => "is at least",
    }
}

fn grain_words(grain: TimeGrain) -> &'static str {
    match grain {
        TimeGrain::Day => "day",
        TimeGrain::Week => "week",
        TimeGrain::Month => "month",
        TimeGrain::Quarter => "quarter",
        TimeGrain::Year => "year",
    }
}

fn aggregate_words(aggregate: MeasureAggregate) -> &'static str {
    match aggregate {
        MeasureAggregate::Sum => "sum of",
        MeasureAggregate::Count => "count of",
        MeasureAggregate::Min => "least",
        MeasureAggregate::Max => "greatest",
        MeasureAggregate::Avg => "average of",
    }
}

fn value_words(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::Bool(flag) => flag.to_string(),
        RuntimeValue::Number(number) => number.canonical(),
        RuntimeValue::String(text) => text.clone(),
        RuntimeValue::Array(items) => items.iter().map(value_words).collect::<Vec<_>>().join(", "),
        RuntimeValue::Object(_) => "an object".to_owned(),
    }
}

fn metric<'a>(metrics: &'a [MetricDefinition], name: &str) -> Option<&'a MetricDefinition> {
    metrics.iter().find(|definition| definition.name == name)
}

/// A metric in words: its label and, where it says how it is computed, how:
/// `Yield (recovered ÷ input)`, `Input (sum of input)`.
fn measure_words(metrics: &[MetricDefinition], name: &str) -> Result<String, RestateError> {
    let definition =
        metric(metrics, name).ok_or_else(|| RestateError::UnknownMetric(name.to_owned()))?;
    let how = match definition.metric_type {
        MetricType::Ratio => {
            let operand = |operand: Option<&String>| -> Result<String, RestateError> {
                let operand = operand.map_or("", String::as_str);
                let declared =
                    metric(metrics, operand).ok_or_else(|| RestateError::UnknownOperand {
                        metric: name.to_owned(),
                        operand: operand.to_owned(),
                    })?;
                Ok(words(
                    declared
                        .type_params
                        .expr
                        .as_deref()
                        .unwrap_or(&declared.label),
                ))
            };
            Some(format!(
                "{} ÷ {}",
                operand(definition.type_params.numerator.as_ref())?,
                operand(definition.type_params.denominator.as_ref())?
            ))
        }
        MetricType::Simple => {
            let aggregate = definition
                .type_params
                .metric_aggregation_params
                .as_ref()
                .map_or(MeasureAggregate::Sum, |params| params.agg);
            let subject = definition
                .type_params
                .expr
                .as_deref()
                .unwrap_or(&definition.label);
            Some(format!("{} {}", aggregate_words(aggregate), words(subject)))
        }
        MetricType::Cumulative | MetricType::Derived | MetricType::Conversion => None,
    };
    Ok(match how {
        Some(how) => format!("{} ({how})", definition.label),
        None => definition.label.clone(),
    })
}

/// A UTC timestamp's calendar day: `(day, "Jul", 2026)`.
fn day(timestamp: &str) -> Option<(u32, &'static str, &str)> {
    let year = timestamp.get(0..4)?;
    let month: usize = timestamp.get(5..7)?.parse().ok()?;
    let day: u32 = timestamp.get(8..10)?.parse().ok()?;
    let month = MONTHS.get(month.checked_sub(1)?)?;
    Some((day, month, year))
}

/// A half-open window in words: `from 6 Jul to 26 Oct 2026 (26 Oct not
/// included)`; the start's year is written only when it differs.
#[must_use]
pub fn window_words(window: &QuestionTimeWindow) -> String {
    match (day(window.start.as_str()), day(window.end.as_str())) {
        (Some((start_day, start_month, start_year)), Some((end_day, end_month, end_year))) => {
            let start_year = if start_year == end_year {
                String::new()
            } else {
                format!(" {start_year}")
            };
            format!(
                "from {start_day} {start_month}{start_year} to {end_day} {end_month} {end_year} ({end_day} {end_month} not included)"
            )
        }
        _ => "over no stated window".to_owned(),
    }
}

fn condition_words(filter: &MetricFilter) -> String {
    format!(
        "{} {} {}",
        words(filter.path.property.as_str()),
        operator_words(filter.operator),
        value_words(&filter.value)
    )
}

/// The question in plain words, from its IR and the metric declarations it
/// was admitted with: measures, of what records, how grouped, which records
/// count, over which window.
///
/// # Errors
///
/// A selected metric, or a ratio's operand, that is not declared.
pub fn restate_question(
    question: &QuestionIR,
    metrics: &[MetricDefinition],
) -> Result<String, RestateError> {
    let measures = question
        .metrics
        .iter()
        .map(|selection| measure_words(metrics, &selection.metric))
        .collect::<Result<Vec<_>, _>>()?;
    let measures = if measures.is_empty() {
        "No measure".to_owned()
    } else {
        measures.join(" and ")
    };
    let object_set = question.object_set.as_str();
    let set = words(
        object_set
            .rsplit('.')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("record"),
    );
    let grouping = match question.dimensions.first() {
        None => "in total".to_owned(),
        Some(dimension) => {
            let property = words(dimension.path.property.as_str());
            match dimension.grain {
                Some(grain) => format!("by {} of {property}", grain_words(grain)),
                None => format!("by {property}"),
            }
        }
    };
    let mut parts = vec![format!("{measures} of {set} records"), grouping];
    if !question.filters.is_empty() {
        let conditions = question
            .filters
            .iter()
            .map(condition_words)
            .collect::<Vec<_>>();
        parts.push(format!("where {}", conditions.join(" and ")));
    }
    parts.push(window_words(&question.time_window));
    Ok(parts.join(", "))
}

/// A dashboard in plain words: its title, then `n. <tile title>: <the tile's
/// question restated>` for each tile in order, one per line.
///
/// `payload` is the dashboard definition's payload, `{title, tiles: [{title,
/// question, ...}]}`; members the restatement does not read are ignored.
///
/// # Errors
///
/// A payload of another shape, or a tile whose question cannot be restated.
pub fn restate_dashboard(
    payload: &serde_json::Value,
    metrics: &[MetricDefinition],
) -> Result<String, RestateError> {
    let title = payload
        .get("title")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| RestateError::Dashboard("no title".to_owned()))?;
    let tiles = payload
        .get("tiles")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| RestateError::Dashboard("no tiles array".to_owned()))?;
    let mut lines = vec![title.to_owned()];
    for (number, tile) in (1_usize..).zip(tiles) {
        let tile_title = tile
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Untitled question");
        let question: QuestionIR = tile
            .get("question")
            .cloned()
            .ok_or_else(|| RestateError::Dashboard(format!("tile {number} has no question")))
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|error| RestateError::Dashboard(format!("tile {number}: {error}")))
            })?;
        lines.push(format!(
            "{number}. {tile_title}: {}",
            restate_question(&question, metrics)?
        ));
    }
    Ok(lines.join("\n"))
}

/// The hash a sign-off records: lower-case hex SHA-256 of the restatement's
/// UTF-8 bytes.
#[must_use]
pub fn restatement_sha256(restatement: &str) -> String {
    format!("{:x}", Sha256::digest(restatement.as_bytes()))
}
