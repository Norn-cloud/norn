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
//!
//! Every field that changes what a question means is restated, so two
//! questions that mean different things never share a hash (the property
//! test in `tests/restatement.rs` holds this): the object set and its
//! namespace, the links searched around, each measure with its roll-up,
//! every dimension in order with its links and grain, every filter with its
//! typed value, the window to the instant, the freshness and the comparison.
//! A declared metric reads as its label and how it is computed; the
//! declarations themselves are pinned by the admission the cell repeats.
//! Not restated, because they name rather than mean: the question's `name`
//! and `irVersion`. An inline calculation is refused ([`RestateError::Inline`])
//! rather than restated in part. `calendar` has one policy, ISO/Gregorian
//! UTC; a new policy is a new variant and fails to compile here until it has
//! words.

use std::fmt;

use norn_ir::RuntimeValue;
use norn_ir::metric::{
    CalendarPolicy, DimensionPath, FilterOperator, FreshnessPolicy, MeasureAggregate,
    MetricDefinition, MetricFilter, MetricRollup, MetricType, QuestionComparison, QuestionIR,
    QuestionMetric, QuestionTimeWindow, Reaggregate, TimeGrain,
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
    /// A timestamp that is not `YYYY-MM-DDTHH:MM:SS[.f]Z`.
    Timestamp(String),
    /// A filter value with no canonical form.
    Value(String),
    /// An inline calculation (named by the metric it outputs). Its governance
    /// (currency conversion, deduplication, default filters) changes the
    /// answer and has no words yet, so it is not signed off by a partial
    /// restatement.
    Inline(String),
}

impl fmt::Display for RestateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownMetric(metric) => write!(f, "no declared metric {metric}"),
            Self::UnknownOperand { metric, operand } => {
                write!(f, "ratio metric {metric} names undeclared metric {operand}")
            }
            Self::Dashboard(detail) => write!(f, "not a dashboard: {detail}"),
            Self::Timestamp(timestamp) => write!(f, "not a UTC timestamp: {timestamp}"),
            Self::Value(detail) => write!(f, "not a canonical value: {detail}"),
            Self::Inline(metric) => write!(
                f,
                "inline calculation {metric} has no restatement; select a declared metric"
            ),
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

/// A property reached through links: `metal`, `lot's metal`.
fn path_words(path: &DimensionPath) -> String {
    let mut steps: Vec<String> = path.links.iter().map(|link| words(link.as_str())).collect();
    steps.push(words(path.property.as_str()));
    steps.join("'s ")
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
        MeasureAggregate::CountDistinct => "count of distinct",
        MeasureAggregate::Min => "least",
        MeasureAggregate::Max => "greatest",
        MeasureAggregate::Avg => "average of",
    }
}

fn reaggregate_words(aggregate: Reaggregate) -> &'static str {
    match aggregate {
        Reaggregate::Sum => "total",
        Reaggregate::First => "first value",
        Reaggregate::Last => "last value",
        Reaggregate::Min => "least",
        Reaggregate::Max => "greatest",
    }
}

/// A filter value, typed: a string is quoted (`"5"` is not `5`), a list or
/// an object is written as its canonical JSON, nothing is `no value`.
fn value_words(value: &RuntimeValue) -> Result<String, RestateError> {
    match value {
        RuntimeValue::Null => Ok("no value".to_owned()),
        RuntimeValue::Bool(flag) => Ok(flag.to_string()),
        RuntimeValue::Number(number) => Ok(number.canonical()),
        RuntimeValue::String(_) | RuntimeValue::Array(_) | RuntimeValue::Object(_) => {
            let bytes = value
                .canonical_json()
                .map_err(|error| RestateError::Value(error.to_string()))?;
            String::from_utf8(bytes).map_err(|error| RestateError::Value(error.to_string()))
        }
    }
}

fn metric<'a>(metrics: &'a [MetricDefinition], name: &str) -> Option<&'a MetricDefinition> {
    metrics.iter().find(|definition| definition.name == name)
}

/// How a declared metric is computed, where its type says:
/// `recovered ÷ input`, `sum of input`.
fn declared_how(
    metrics: &[MetricDefinition],
    definition: &MetricDefinition,
) -> Result<Option<String>, RestateError> {
    Ok(match definition.metric_type {
        MetricType::Ratio => {
            let operand = |operand: Option<&String>| -> Result<String, RestateError> {
                let operand = operand.map_or("", String::as_str);
                let declared =
                    metric(metrics, operand).ok_or_else(|| RestateError::UnknownOperand {
                        metric: definition.name.clone(),
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
    })
}

/// A roll-up: `then the greatest across metal and over time`.
fn rollup_words(rollup: &MetricRollup) -> String {
    let aggregate = reaggregate_words(rollup.aggregate);
    let across: Vec<String> = rollup.across.iter().map(path_words).collect();
    match (across.is_empty(), rollup.over_time) {
        (true, false) => format!("then the {aggregate}"),
        (true, true) => format!("then the {aggregate} over time"),
        (false, false) => format!("then the {aggregate} across {}", across.join(" and ")),
        (false, true) => format!(
            "then the {aggregate} across {} and over time",
            across.join(" and ")
        ),
    }
}

/// A selected metric in words: its label and, where known, how it is
/// computed and rolled up: `Yield (recovered ÷ input)`, `Input (sum of
/// input; then the total over time)`.
fn measure_words(
    metrics: &[MetricDefinition],
    selection: &QuestionMetric,
) -> Result<String, RestateError> {
    if selection.inline.is_some() {
        return Err(RestateError::Inline(selection.metric.clone()));
    }
    let definition = metric(metrics, &selection.metric)
        .ok_or_else(|| RestateError::UnknownMetric(selection.metric.clone()))?;
    let label = definition.label.clone();
    let how = declared_how(metrics, definition)?;
    let rollup = selection.reaggregate.as_ref().map(rollup_words);
    Ok(match (how, rollup) {
        (None, None) => label,
        (Some(how), None) => format!("{label} ({how})"),
        (None, Some(rollup)) => format!("{label} ({rollup})"),
        (Some(how), Some(rollup)) => format!("{label} ({how}; {rollup})"),
    })
}

/// A UTC instant, parsed from `YYYY-MM-DDTHH:MM:SS[.f]Z`.
struct Instant<'a> {
    day: u32,
    month: &'static str,
    year: &'a str,
    /// `09:30`, `09:30:15`, `09:30:15.5`; `None` at midnight.
    time: Option<String>,
}

fn instant(timestamp: &str) -> Result<Instant<'_>, RestateError> {
    let malformed = || RestateError::Timestamp(timestamp.to_owned());
    let digits = |range: std::ops::Range<usize>| -> Result<&str, RestateError> {
        timestamp
            .get(range)
            .filter(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or_else(malformed)
    };
    let year = digits(0..4)?;
    let month: usize = digits(5..7)?.parse().ok().ok_or_else(malformed)?;
    let day: u32 = digits(8..10)?.parse().ok().ok_or_else(malformed)?;
    let (hour, minute, second) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    let shape = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if shape
        .iter()
        .any(|&(at, byte)| timestamp.as_bytes().get(at) != Some(&byte))
    {
        return Err(malformed());
    }
    let rest = timestamp.get(19..).ok_or_else(malformed)?;
    let fraction = match rest.strip_suffix('Z').ok_or_else(malformed)? {
        "" => "",
        dotted => dotted
            .strip_prefix('.')
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(malformed)?
            .trim_end_matches('0'),
    };
    let month = month
        .checked_sub(1)
        .and_then(|index| MONTHS.get(index))
        .ok_or_else(malformed)?;
    let time = match (second, fraction) {
        ("00", "") if hour == "00" && minute == "00" => None,
        ("00", "") => Some(format!("{hour}:{minute}")),
        (_, "") => Some(format!("{hour}:{minute}:{second}")),
        _ => Some(format!("{hour}:{minute}:{second}.{fraction}")),
    };
    Ok(Instant {
        day,
        month,
        year,
        time,
    })
}

impl Instant<'_> {
    /// `6 Jul 2026`, `6 Jul 2026 09:30`; `with_time` writes midnight too.
    fn full(&self, with_time: bool) -> String {
        let date = format!("{} {} {}", self.day, self.month, self.year);
        match (&self.time, with_time) {
            (Some(time), _) => format!("{date} {time}"),
            (None, true) => format!("{date} 00:00"),
            (None, false) => date,
        }
    }
}

/// A half-open window in words. Midnight to midnight reads as days: `from
/// 6 Jul to 26 Oct 2026 (26 Oct not included)`, the start's year written
/// only when it differs. Any other bound is written to the instant, both
/// years and both times: `from 6 Jul 2026 09:30 to 26 Oct 2026 00:00 UTC
/// (26 Oct 2026 00:00 not included)`.
///
/// # Errors
///
/// A bound that is not a UTC timestamp.
pub fn window_words(window: &QuestionTimeWindow) -> Result<String, RestateError> {
    let start = instant(window.start.as_str())?;
    let end = instant(window.end.as_str())?;
    if start.time.is_none() && end.time.is_none() {
        let start_year = if start.year == end.year {
            String::new()
        } else {
            format!(" {}", start.year)
        };
        return Ok(format!(
            "from {} {}{start_year} to {} {} {} ({} {} not included)",
            start.day, start.month, end.day, end.month, end.year, end.day, end.month
        ));
    }
    let end_words = end.full(true);
    Ok(format!(
        "from {} to {end_words} UTC ({end_words} not included)",
        start.full(true)
    ))
}

/// A freshness bound: `5 seconds`, `1500 ms`.
fn age_words(max_age_ms: u64) -> String {
    match (max_age_ms.checked_rem(1000), max_age_ms.checked_div(1000)) {
        (Some(0), Some(1)) => "1 second".to_owned(),
        (Some(0), Some(seconds)) => format!("{seconds} seconds"),
        _ => format!("{max_age_ms} ms"),
    }
}

fn freshness_words(freshness: &FreshnessPolicy) -> Result<String, RestateError> {
    Ok(match freshness {
        FreshnessPolicy::Live { max_age_ms } => {
            format!("live data at most {} old", age_words(max_age_ms.0))
        }
        FreshnessPolicy::Delayed { max_age_ms } => {
            format!("delayed data at most {} old", age_words(max_age_ms.0))
        }
        FreshnessPolicy::AsOf { at } => {
            format!("data as of {} UTC", instant(at.as_str())?.full(true))
        }
    })
}

fn comparison_words(comparison: &QuestionComparison) -> String {
    let (kind, question, mapping) = match comparison {
        QuestionComparison::PreviousPeriod => {
            return "compared with the period before".to_owned();
        }
        QuestionComparison::Plan { question, mapping } => ("plan", question, mapping),
        QuestionComparison::Forecast { question, mapping } => ("forecast", question, mapping),
    };
    let matching = if mapping.is_empty() {
        String::new()
    } else {
        let pairs: Vec<String> = mapping
            .iter()
            .map(|pair| format!("{} to {}", pair.metric, pair.reference_metric))
            .collect();
        format!(" (matching {})", pairs.join(" and "))
    };
    format!("compared with the {kind} in question {question}{matching}")
}

fn condition_words(filter: &MetricFilter) -> Result<String, RestateError> {
    Ok(format!(
        "{} {} {}",
        path_words(&filter.path),
        operator_words(filter.operator),
        value_words(&filter.value)?
    ))
}

fn conditions_words(filters: &[MetricFilter]) -> Result<String, RestateError> {
    Ok(filters
        .iter()
        .map(condition_words)
        .collect::<Result<Vec<_>, _>>()?
        .join(" and "))
}

/// The records a question counts: `demo Conversion records`, or, after a
/// search around, `the records reached from demo Conversion records through
/// lot then supplier`.
fn population_words(question: &QuestionIR) -> String {
    let object_set = question.object_set.as_str();
    let (namespace, object_type) = object_set.rsplit_once('.').unwrap_or(("", object_set));
    let start = if namespace.is_empty() {
        format!("{} records", words(object_type))
    } else {
        format!("{} {} records", words(namespace), words(object_type))
    };
    if question.search_around.is_empty() {
        start
    } else {
        let links: Vec<String> = question
            .search_around
            .iter()
            .map(|link| words(link.as_str()))
            .collect();
        format!(
            "the records reached from {start} through {}",
            links.join(" then ")
        )
    }
}

/// The question in plain words, from its IR and the metric declarations it
/// was admitted with: measures, of what records, how grouped, which records
/// count, over which window, how fresh, compared with what.
///
/// # Errors
///
/// A selected metric, or a ratio's operand, that is not declared; an inline
/// calculation; a bound that is not a UTC timestamp; a value with no
/// canonical form.
pub fn restate_question(
    question: &QuestionIR,
    metrics: &[MetricDefinition],
) -> Result<String, RestateError> {
    // The one calendar policy, which the window's words assume; a new
    // policy fails to compile here until it has words.
    let CalendarPolicy::IsoUtc = question.calendar;
    let measures = question
        .metrics
        .iter()
        .map(|selection| measure_words(metrics, selection))
        .collect::<Result<Vec<_>, _>>()?;
    let measures = if measures.is_empty() {
        "No measure".to_owned()
    } else {
        measures.join(" and ")
    };
    let grouping = if question.dimensions.is_empty() {
        "in total".to_owned()
    } else {
        question
            .dimensions
            .iter()
            .map(|dimension| {
                let path = path_words(&dimension.path);
                match dimension.grain {
                    Some(grain) => format!("by {} of {path}", grain_words(grain)),
                    None => format!("by {path}"),
                }
            })
            .collect::<Vec<_>>()
            .join(" and ")
    };
    let mut parts = vec![
        format!("{measures} of {}", population_words(question)),
        grouping,
    ];
    if !question.filters.is_empty() {
        parts.push(format!("where {}", conditions_words(&question.filters)?));
    }
    parts.push(window_words(&question.time_window)?);
    parts.push(freshness_words(&question.freshness)?);
    if let Some(comparison) = &question.comparison {
        parts.push(comparison_words(comparison));
    }
    Ok(parts.join(", "))
}

/// A dashboard in plain words: its title, who authored it when its payload
/// names an origin, then `n. <tile title>: <the tile's question restated>`
/// for each tile in order, one per line.
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
    if let Some(origin) = payload.get("origin") {
        lines.push(origin_words(origin)?);
    }
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

/// Who authored a dashboard version, in words; an assistant's proposal quotes
/// the person's request, so a sign-off covers what was asked as well as what
/// was proposed.
fn origin_words(origin: &serde_json::Value) -> Result<String, RestateError> {
    let text = |key: &str| origin.get(key).and_then(serde_json::Value::as_str);
    match text("by") {
        Some("studio") => Ok("Built in Studio.".to_owned()),
        Some("code") => Ok("Written as code.".to_owned()),
        Some("assistant") => {
            let model = text("model").map_or_else(String::new, |model| format!(" ({model})"));
            Ok(match text("request") {
                Some(request) => {
                    format!("Proposed by the assistant{model} for the request: \"{request}\"")
                }
                None => format!("Proposed by the assistant{model}."),
            })
        }
        _ => Err(RestateError::Dashboard(
            "an origin's by is studio, code or assistant".to_owned(),
        )),
    }
}

/// The hash a sign-off records: lower-case hex SHA-256 of the restatement's
/// UTF-8 bytes.
#[must_use]
pub fn restatement_sha256(restatement: &str) -> String {
    format!("{:x}", Sha256::digest(restatement.as_bytes()))
}
