//! Same-snapshot `RequiredSources` freshness enforcement and bounded
//! success/error payload construction for `norn.question@1` flow steps.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::compute::{Engine, Freshness, QueryResult, QueryRow, RequiredSources};
use norn_cell::flow_run::QuestionFreshnessGuard;
use norn_ir::metric::FreshnessPolicy;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::flow_turn::StepOutcome;

use crate::refusal::Refusal;

use super::{output_too_large, question_step_codes};

/// Maximum number of source identifiers surfaced in `StaleSource.sources`.
const MAX_STALE_SOURCES: usize = 32;
/// Maximum UTF-8 byte length of any single source identifier in `StaleSource.sources`.
const MAX_SOURCE_ID_BYTES: usize = 128;
/// Maximum UTF-8 byte length of a refusal `code` in `Refused.code`.
const MAX_REFUSAL_CODE_BYTES: usize = 128;
/// Maximum canonical JSON size of any typed failure payload (`StaleSource` /
/// `Denied` / `Refused`), strictly below `MAX_RECEIPT_SUMMARY_BYTES`.
const MAX_ERROR_PAYLOAD_BYTES: usize = 4 * 1024;
/// Fixed conservative overhead for top-level `Answered` metadata keys/syntax.
const BASE_ANSWERED_OVERHEAD_BYTES: usize = 256;
/// Fixed conservative overhead per encoded `rows[]` object.
const PER_ROW_OVERHEAD_BYTES: usize = 72;

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

pub(crate) fn refused_failure(code: &str) -> StepOutcome {
    let bounded_code = truncate_utf8(code, MAX_REFUSAL_CODE_BYTES);
    bound_failure(BTreeMap::from([
        (
            "_tag".to_owned(),
            RuntimeValue::String("Refused".to_owned()),
        ),
        ("code".to_owned(), RuntimeValue::String(bounded_code)),
    ]))
}

pub(crate) fn denied_failure() -> StepOutcome {
    StepOutcome::Failure(BTreeMap::from([(
        "_tag".to_owned(),
        RuntimeValue::String("Denied".to_owned()),
    )]))
}

pub(crate) fn stale_failure(
    freshness: &str,
    complete: bool,
    sources: Vec<String>,
    sources_authorized: bool,
) -> StepOutcome {
    let mut bounded_sources: Vec<RuntimeValue> = if sources_authorized {
        sources
            .into_iter()
            .take(MAX_STALE_SOURCES)
            .map(|id| RuntimeValue::String(truncate_utf8(&id, MAX_SOURCE_ID_BYTES)))
            .collect()
    } else {
        Vec::new()
    };
    loop {
        let wrapped = RuntimeValue::Object(BTreeMap::from([
            (
                "_tag".to_owned(),
                RuntimeValue::String("StaleSource".to_owned()),
            ),
            (
                "code".to_owned(),
                RuntimeValue::String(question_step_codes::STALE_SOURCE.to_owned()),
            ),
            (
                "freshness".to_owned(),
                RuntimeValue::String(freshness.to_owned()),
            ),
            ("complete".to_owned(), RuntimeValue::Bool(complete)),
            (
                "sources".to_owned(),
                RuntimeValue::Array(bounded_sources.clone()),
            ),
        ]));
        if within_error_bound(&wrapped) || bounded_sources.is_empty() {
            return bound_failure_value(wrapped);
        }
        bounded_sources.pop();
    }
}

fn within_error_bound(value: &RuntimeValue) -> bool {
    value
        .canonical_json()
        .is_ok_and(|bytes| bytes.len() <= MAX_ERROR_PAYLOAD_BYTES)
}

fn bound_failure(map: RuntimeObject) -> StepOutcome {
    bound_failure_value(RuntimeValue::Object(map))
}

fn bound_failure_value(val: RuntimeValue) -> StepOutcome {
    let within = within_error_bound(&val);
    match val {
        RuntimeValue::Object(object) if within => StepOutcome::Failure(object),
        _ => StepOutcome::Failure(BTreeMap::from([
            (
                "_tag".to_owned(),
                RuntimeValue::String("Refused".to_owned()),
            ),
            (
                "code".to_owned(),
                RuntimeValue::String(question_step_codes::OUTPUT_TOO_LARGE.to_owned()),
            ),
        ])),
    }
}

/// Record one offending source/partition identifier into `bad` without
/// allocating beyond `MAX_STALE_SOURCES` entries or `MAX_SOURCE_ID_BYTES` per
/// entry.
fn record_bad_source(bad: &mut BTreeSet<String>, source: &str, partition: Option<&str>) {
    if bad.len() >= MAX_STALE_SOURCES {
        return;
    }
    let entry = match partition {
        Some(part) => truncate_utf8(&format!("{source}/{part}"), MAX_SOURCE_ID_BYTES),
        None => truncate_utf8(source, MAX_SOURCE_ID_BYTES),
    };
    bad.insert(entry);
}

/// Evaluate the `#1905` `RequiredSources` freshness guard strictly against the
/// SAME authenticated snapshot (`result` and `required`) used for the answer
/// evaluation. Any incomplete, missing, quarantined, or lagging required source
/// returns typed `(StaleSource, emitted_sources)` even if `result.freshness`
/// was labeled `Exact`.
pub(crate) fn evaluate_freshness_guard(
    policy: &FreshnessPolicy,
    result: &QueryResult,
    required: &RequiredSources,
    guard: Option<&QuestionFreshnessGuard>,
    sources_authorized: bool,
) -> Option<(StepOutcome, bool)> {
    let require_exact = guard.and_then(|g| g.require_exact).unwrap_or(false);
    let plan_age = match policy {
        FreshnessPolicy::Live { max_age_ms } | FreshnessPolicy::Delayed { max_age_ms } => {
            Some(max_age_ms.0)
        }
        FreshnessPolicy::AsOf { .. } => None,
    };
    let effective_age = match (plan_age, guard.and_then(|g| g.max_age_ms)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };

    let mut incomplete = !result.complete
        || !result.exclusions.is_empty()
        || matches!(result.freshness, Freshness::Incomplete);
    let mut stale = matches!(result.freshness, Freshness::Stale);
    let mut not_exact = require_exact && result.freshness != Freshness::Exact;
    let mut bad = BTreeSet::new();

    for (source, datasets) in &required.0 {
        if datasets.is_empty() {
            incomplete = true;
            record_bad_source(&mut bad, source, None);
            continue;
        }
        let provided = result.context.source_cursors.get(source);
        for dataset in datasets {
            let Some(consumed) = provided.and_then(|parts| parts.get(dataset)) else {
                incomplete = true;
                record_bad_source(&mut bad, source, Some(dataset));
                continue;
            };
            check_watermark(
                result,
                (source, dataset, consumed),
                (require_exact, effective_age),
                (&mut incomplete, &mut stale, &mut not_exact, &mut bad),
            );
        }
    }

    for (source, partitions) in &result.context.source_cursors {
        if partitions.is_empty() {
            incomplete = true;
            record_bad_source(&mut bad, source, None);
            continue;
        }
        for (partition, consumed) in partitions {
            check_watermark(
                result,
                (source, partition, consumed),
                (require_exact, effective_age),
                (&mut incomplete, &mut stale, &mut not_exact, &mut bad),
            );
        }
    }

    check_watermarks_map(
        result,
        (require_exact, effective_age),
        (&mut incomplete, &mut stale, &mut not_exact, &mut bad),
    );

    if !(incomplete || stale || not_exact || !bad.is_empty()) {
        return None;
    }
    let (label, complete_flag) = if incomplete {
        ("incomplete", false)
    } else if stale {
        ("stale", result.complete)
    } else {
        ("fresh", result.complete)
    };
    let emitted = sources_authorized && !bad.is_empty();
    Some((
        stale_failure(
            label,
            complete_flag,
            bad.into_iter().collect(),
            sources_authorized,
        ),
        emitted,
    ))
}

fn check_watermarks_map(
    result: &QueryResult,
    (require_exact, effective_age): (bool, Option<u64>),
    (incomplete, stale, not_exact, bad): (&mut bool, &mut bool, &mut bool, &mut BTreeSet<String>),
) {
    for (source, partitions) in &result.context.watermarks {
        for (partition, wm) in partitions {
            let Some(at) = wm.heartbeat_ms.filter(|_| wm.complete) else {
                *incomplete = true;
                record_bad_source(bad, source, Some(partition));
                continue;
            };
            check_lag(
                result.context.now_ms,
                at,
                (source, partition),
                (require_exact, effective_age),
                (stale, not_exact, bad),
            );
        }
    }
}

fn check_watermark(
    result: &QueryResult,
    (source, partition, consumed): (&str, &str, &str),
    (require_exact, effective_age): (bool, Option<u64>),
    (incomplete, stale, not_exact, bad): (&mut bool, &mut bool, &mut bool, &mut BTreeSet<String>),
) {
    let wm = result
        .context
        .watermarks
        .get(source)
        .and_then(|map| map.get(partition));
    let Some(at) = wm
        .filter(|w| w.complete && w.observed == consumed)
        .and_then(|w| w.heartbeat_ms)
    else {
        *incomplete = true;
        record_bad_source(bad, source, Some(partition));
        return;
    };
    check_lag(
        result.context.now_ms,
        at,
        (source, partition),
        (require_exact, effective_age),
        (stale, not_exact, bad),
    );
}

fn check_lag(
    now_ms: u64,
    heartbeat_ms: u64,
    (source, partition): (&str, &str),
    (require_exact, effective_age): (bool, Option<u64>),
    (stale, not_exact, bad): (&mut bool, &mut bool, &mut BTreeSet<String>),
) {
    match now_ms.checked_sub(heartbeat_ms) {
        None => {
            *stale = true;
            record_bad_source(bad, source, Some(partition));
        }
        Some(lag) if effective_age.is_some_and(|max_age| lag > max_age) => {
            *stale = true;
            record_bad_source(bad, source, Some(partition));
        }
        Some(lag) if require_exact && lag > 0 => {
            *not_exact = true;
            record_bad_source(bad, source, Some(partition));
        }
        Some(_) => {}
    }
}

const fn engine_str(engine: Engine) -> &'static str {
    match engine {
        Engine::Cell => "cell",
        Engine::Ivm => "ivm",
        Engine::Duckdb => "duckdb",
        Engine::DataFusion => "datafusion",
    }
}

const fn freshness_str(freshness: Freshness) -> &'static str {
    match freshness {
        Freshness::Exact => "exact",
        Freshness::Fresh => "fresh",
        Freshness::Stale => "stale",
        Freshness::Incomplete => "incomplete",
    }
}

fn encode_u64(n: u64) -> Result<RuntimeValue, Refusal> {
    norn_ir::runtime::encode(&n).map_err(Refusal::host)
}

fn encode_query_row(
    bucket_key: &str,
    row: &QueryRow,
    estimated_bytes: &mut usize,
    max_bytes: usize,
) -> Result<RuntimeValue, Refusal> {
    let member_count = row
        .member_count
        .unwrap_or_else(|| u64::try_from(row.members.len()).unwrap_or(u64::MAX));
    *estimated_bytes = estimated_bytes
        .saturating_add(PER_ROW_OVERHEAD_BYTES)
        .saturating_add(bucket_key.len())
        .saturating_add(row.totals.len().saturating_mul(3));
    if *estimated_bytes > max_bytes {
        return Err(output_too_large());
    }
    let mut totals = Vec::with_capacity(row.totals.len());
    for total in &row.totals {
        let text = total.to_string();
        *estimated_bytes = estimated_bytes.saturating_add(text.len());
        if *estimated_bytes > max_bytes {
            return Err(output_too_large());
        }
        totals.push(RuntimeValue::String(text));
    }
    let value = match row.value.as_ref() {
        None => RuntimeValue::Null,
        Some(d) => {
            let text = d.to_string();
            *estimated_bytes = estimated_bytes.saturating_add(text.len());
            if *estimated_bytes > max_bytes {
                return Err(output_too_large());
            }
            RuntimeValue::String(text)
        }
    };
    Ok(RuntimeValue::Object(BTreeMap::from([
        (
            "bucketKey".to_owned(),
            RuntimeValue::String(bucket_key.to_owned()),
        ),
        ("value".to_owned(), value),
        ("totals".to_owned(), RuntimeValue::Array(totals)),
        ("memberCount".to_owned(), encode_u64(member_count)?),
        ("suppressed".to_owned(), RuntimeValue::Bool(row.suppressed)),
    ])))
}

fn encode_rows_bounded(
    result: &QueryResult,
    mut estimated_bytes: usize,
    max_bytes: usize,
) -> Result<Vec<RuntimeValue>, Refusal> {
    if result.rows.len().saturating_mul(PER_ROW_OVERHEAD_BYTES) > max_bytes
        || estimated_bytes > max_bytes
    {
        return Err(output_too_large());
    }
    let mut rows = Vec::with_capacity(result.rows.len());
    for (bucket_key, row) in &result.rows {
        rows.push(encode_query_row(
            bucket_key,
            row,
            &mut estimated_bytes,
            max_bytes,
        )?);
    }
    Ok(rows)
}

fn answered_object(
    question_id: &str,
    result: &QueryResult,
    authority_pin: String,
    rows: Vec<RuntimeValue>,
) -> Result<RuntimeValue, Refusal> {
    let scalar_value = if result.rows.len() == 1 {
        result
            .rows
            .values()
            .next()
            .and_then(|row| row.value.as_ref())
            .map_or(RuntimeValue::Null, |d| RuntimeValue::String(d.to_string()))
    } else {
        RuntimeValue::Null
    };
    let revision = RuntimeValue::Object(BTreeMap::from([
        ("seq".to_owned(), encode_u64(result.revision.seq)?),
        (
            "hash".to_owned(),
            RuntimeValue::String(result.revision.hash.clone()),
        ),
        ("commits".to_owned(), encode_u64(result.revision.commits)?),
    ]));
    let row_count = u64::try_from(result.rows.len()).unwrap_or(u64::MAX);
    Ok(RuntimeValue::Object(BTreeMap::from([
        (
            "status".to_owned(),
            RuntimeValue::String("Answered".to_owned()),
        ),
        (
            "question".to_owned(),
            RuntimeValue::String(question_id.to_owned()),
        ),
        (
            "grain".to_owned(),
            RuntimeValue::String(result.grain.0.clone()),
        ),
        (
            "engine".to_owned(),
            RuntimeValue::String(engine_str(result.engine).to_owned()),
        ),
        (
            "freshness".to_owned(),
            RuntimeValue::String(freshness_str(result.freshness).to_owned()),
        ),
        ("complete".to_owned(), RuntimeValue::Bool(result.complete)),
        (
            "queryPin".to_owned(),
            RuntimeValue::String(result.query_pin.clone()),
        ),
        (
            "definitionPin".to_owned(),
            RuntimeValue::String(result.definition_pin.clone()),
        ),
        (
            "authorityPin".to_owned(),
            RuntimeValue::String(authority_pin),
        ),
        (
            "readToken".to_owned(),
            RuntimeValue::String(result.lineage_ref.clone()),
        ),
        ("revision".to_owned(), revision),
        (
            "effectiveAsOf".to_owned(),
            RuntimeValue::String(result.effective_as_of.0.clone()),
        ),
        ("knownAsOf".to_owned(), encode_u64(result.known_as_of)?),
        ("nowMs".to_owned(), encode_u64(result.context.now_ms)?),
        ("rowCount".to_owned(), encode_u64(row_count)?),
        ("value".to_owned(), scalar_value),
        ("rows".to_owned(), RuntimeValue::Array(rows)),
    ])))
}

/// Build the bounded `Answered` output, enforcing `MAX_RECEIPT_SUMMARY_BYTES`
/// incrementally before full row encoding and checking exact canonical JSON
/// length without cloning `output`.
pub(crate) fn build_answered_output(
    question_id: &str,
    result: &QueryResult,
) -> Result<StepOutcome, Refusal> {
    let max_bytes =
        usize::try_from(norn_ir::function::MAX_RECEIPT_SUMMARY_BYTES).unwrap_or(usize::MAX);
    let authority_pin = format!(
        "{}@{}",
        result.context.population_pin, result.context.authorization_epoch
    );
    let estimated_bytes = BASE_ANSWERED_OVERHEAD_BYTES
        .saturating_add(question_id.len())
        .saturating_add(result.grain.0.len())
        .saturating_add(result.query_pin.len())
        .saturating_add(result.definition_pin.len())
        .saturating_add(authority_pin.len())
        .saturating_add(result.lineage_ref.len())
        .saturating_add(result.revision.hash.len())
        .saturating_add(result.effective_as_of.0.len());
    let rows = encode_rows_bounded(result, estimated_bytes, max_bytes)?;
    let wrapped = answered_object(question_id, result, authority_pin, rows)?;
    let encoded_size = wrapped.canonical_json().map_err(Refusal::host)?.len();
    if !u64::try_from(encoded_size)
        .is_ok_and(|size| size <= norn_ir::function::MAX_RECEIPT_SUMMARY_BYTES)
    {
        return Err(output_too_large());
    }
    match wrapped {
        RuntimeValue::Object(output) => Ok(StepOutcome::Output(output)),
        _ => Err(Refusal::host("question output is not an object")),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "freshness and payload-bound unit tests"
)]
mod tests {
    use super::*;
    use norn_cell::LedgerHead;
    use norn_cell::compute::{QueryContext, Watermark};
    use norn_ir::{Decimal, ObjectTypeName, UtcTimestamp, WholeNumber};

    fn sample_result(freshness: Freshness, complete: bool, now_ms: u64) -> QueryResult {
        QueryResult {
            query_pin: "qp".to_owned(),
            definition_pin: "dp".to_owned(),
            engine: Engine::Cell,
            revision: LedgerHead::genesis(),
            effective_as_of: UtcTimestamp("2026-08-06T12:00:00Z".to_owned()),
            known_as_of: 1,
            grain: ObjectTypeName("canon.OrderLine".to_owned()),
            lineage_ref: "lr".to_owned(),
            rows: BTreeMap::new(),
            freshness,
            complete,
            context: QueryContext {
                world: "main".to_owned(),
                scope: "org_demo/dev".to_owned(),
                population_pin: "pop".to_owned(),
                authorization_epoch: 1,
                mapping_pin: "mp".to_owned(),
                now_ms,
                source_cursors: BTreeMap::new(),
                watermarks: BTreeMap::new(),
            },
            exclusions: BTreeMap::new(),
            unlinked: 0,
            source_disagreements: None,
        }
    }

    #[test]
    fn missing_required_source_returns_stale_even_when_labeled_exact() {
        let result = sample_result(Freshness::Exact, true, 100_000);
        let required = RequiredSources(BTreeMap::from([(
            "erpnext.rest".to_owned(),
            BTreeSet::from(["Sales Order Item".to_owned()]),
        )]));
        let policy = FreshnessPolicy::Live {
            max_age_ms: WholeNumber(60_000),
        };
        let Some((StepOutcome::Failure(fail), emitted)) =
            evaluate_freshness_guard(&policy, &result, &required, None, true)
        else {
            panic!("expected StaleSource failure when required source is missing");
        };
        assert!(emitted);
        assert_eq!(
            fail.get("_tag").and_then(RuntimeValue::as_str),
            Some("StaleSource")
        );
        assert_eq!(
            fail.get("freshness").and_then(RuntimeValue::as_str),
            Some("incomplete")
        );
        assert_eq!(fail.get("complete"), Some(&RuntimeValue::Bool(false)));
        assert_eq!(
            fail.get("sources"),
            Some(&RuntimeValue::Array(vec![RuntimeValue::String(
                "erpnext.rest/Sales Order Item".to_owned()
            )]))
        );
    }

    #[test]
    fn lagging_required_source_returns_stale_even_when_labeled_exact() {
        let mut result = sample_result(Freshness::Exact, true, 200_000);
        result.context.source_cursors.insert(
            "erpnext.rest".to_owned(),
            BTreeMap::from([("orders".to_owned(), "c1".to_owned())]),
        );
        result.context.watermarks.insert(
            "erpnext.rest".to_owned(),
            BTreeMap::from([(
                "orders".to_owned(),
                Watermark {
                    observed: "c1".to_owned(),
                    heartbeat_ms: Some(100_000),
                    complete: true,
                },
            )]),
        );
        let required = RequiredSources(BTreeMap::from([(
            "erpnext.rest".to_owned(),
            BTreeSet::from(["orders".to_owned()]),
        )]));
        let policy = FreshnessPolicy::Live {
            max_age_ms: WholeNumber(60_000),
        };
        let Some((StepOutcome::Failure(fail), emitted)) =
            evaluate_freshness_guard(&policy, &result, &required, None, true)
        else {
            panic!("expected StaleSource when required watermark exceeds max_age_ms");
        };
        assert!(emitted);
        assert_eq!(
            fail.get("_tag").and_then(RuntimeValue::as_str),
            Some("StaleSource")
        );
        assert_eq!(
            fail.get("freshness").and_then(RuntimeValue::as_str),
            Some("stale")
        );
    }

    #[test]
    fn stale_source_redacts_when_unauthorized_and_bounds_when_authorized() {
        let sources: Vec<String> = (0..60)
            .map(|i| format!("source-{i:02}-{}", "x".repeat(260)))
            .collect();
        let StepOutcome::Failure(redacted) = stale_failure("stale", true, sources.clone(), false)
        else {
            panic!("expected Failure");
        };
        assert_eq!(
            redacted.get("sources"),
            Some(&RuntimeValue::Array(Vec::new()))
        );

        let StepOutcome::Failure(bounded) = stale_failure("stale", true, sources, true) else {
            panic!("expected Failure");
        };
        let Some(RuntimeValue::Array(items)) = bounded.get("sources") else {
            panic!("expected sources array");
        };
        assert!(items.len() <= MAX_STALE_SOURCES);
        assert!(!items.is_empty());
        for item in items {
            let text = item.as_str().unwrap();
            assert!(text.len() <= MAX_SOURCE_ID_BYTES);
        }
        let encoded = RuntimeValue::Object(bounded).canonical_json().unwrap();
        assert!(encoded.len() <= MAX_ERROR_PAYLOAD_BYTES);
    }

    #[test]
    fn build_answered_output_enforces_receipt_summary_bound() {
        let small = sample_result(Freshness::Exact, true, 10_000);
        assert!(matches!(
            build_answered_output("q.small", &small),
            Ok(StepOutcome::Output(_))
        ));

        let mut huge = sample_result(Freshness::Exact, true, 10_000);
        for i in 0..600 {
            huge.rows.insert(
                format!("bucket-{i:04}-{}", "k".repeat(48)),
                QueryRow {
                    bucket: RuntimeValue::String(format!("b-{i}")),
                    totals: vec!["42".parse::<Decimal>().unwrap()],
                    value: Some("42".parse::<Decimal>().unwrap()),
                    members: BTreeMap::new(),
                    suppressed: false,
                    null_counts: Vec::new(),
                    member_count: Some(1),
                    membership_digest: None,
                },
            );
        }
        let err = build_answered_output("q.huge", &huge).unwrap_err();
        assert_eq!(err.code, question_step_codes::OUTPUT_TOO_LARGE);
    }
}
