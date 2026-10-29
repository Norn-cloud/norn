//! Collision-safe allocation for declared business naming series.
//!
//! A generated identifier and its counter are prepared together and written
//! through one [`CellHost::commit`]. Counter reads are guarded by the same
//! compare-and-swap as the created object and action receipt, so a refused or
//! losing concurrent create consumes no number.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use norn_checker::naming_series::{NamingPatternToken, parse_naming_series_pattern};
pub use norn_ir::NamingCounterSeed;
use norn_ir::{
    KeyName, KeySpec, NamingResetPolicy, NamingSeriesIR, ObjectTypeName, PropertyId, RuntimeValue,
    identifier_limits::MAX_OBJECT_KEY_BYTES, runtime_object,
};

use crate::cell_host::{
    CellCommit, CellHost, CommitModel, CommitReceipt, FactExpectation, FactKey, FactMutation,
    FactState, FactVersion, FactWrite, JsonObject,
};
use crate::decision_commit::{DecisionRefusal, FactEvent, TypeCatalog};

/// The kernel-owned store type for one committed naming-series counter.
///
/// This type is part of the persistent host model, but is intentionally not
/// exposed in the tenant's action catalog. Only the action allocator and the
/// validated seed primitive below write it.
pub const NAMING_SERIES_COUNTER_TYPE: &str = "norn.NamingSeriesCounter";

const MAX_NAMING_COUNTER_WIDTH: u8 = 18;
const MAX_COLLISION_PROBES: usize = 128;

/// Atomically install imported naming-series counter snapshots.
///
/// The type catalog must contain the admitted business declarations, not the
/// host's private system types. Every seed is validated against the declared
/// key, scope, period policy, timezone, and counter width. Existing counters
/// cannot be lowered or rebound to a different declaration. Equal seeds are
/// idempotent. All changed counters are committed in one host transaction.
///
/// # Errors
/// Returns a typed decision refusal when a seed does not match its declaration,
/// would lower/rebind a counter, exceeds the counter width, or loses its CAS.
pub fn install_naming_counter_seeds<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    seeds: &[NamingCounterSeed],
) -> Result<Option<CommitReceipt>, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
{
    let mut seen = BTreeSet::new();
    let mut writes = Vec::new();
    let mut events = Vec::new();

    for seed in seeds {
        let (series_owner, key_spec) = declared_key(catalog, &seed.object_type, &seed.key_name)
            .ok_or_else(|| invalid("the seed names no declared naming series"))?;
        if series_owner != &seed.object_type {
            return Err(invalid(
                "the seed object type does not own the declared naming series",
            ));
        }
        let declaration = key_spec
            .naming_series
            .as_ref()
            .ok_or_else(|| invalid("the seed names no declared naming series"))?;
        let generated_property = generated_property(key_spec).map_err(invalid)?;
        let width = counter_width(&declaration.pattern).map_err(invalid)?;
        let max = counter_max(width).ok_or_else(|| invalid("counter width is unsupported"))?;
        if seed.current > max {
            return Err(invalid("the imported counter exceeds the declared width"));
        }
        validate_scope(declaration, &seed.scope).map_err(invalid)?;
        norn_expr::temporal::validate_timezone(&declaration.timezone)
            .map_err(|error| invalid(format!("invalid naming-series timezone: {error}")))?;
        validate_period(declaration, &seed.period).map_err(invalid)?;

        let key = counter_fact_key(series_owner, &seed.key_name, &seed.scope, &seed.period)
            .map_err(invalid)?;
        if !seen.insert(key.clone()) {
            return Err(invalid("the seed contains the same counter bucket twice"));
        }
        let current = host.load(&key).map_err(DecisionRefusal::Host)?;
        let expected_values = counter_values(
            series_owner,
            &seed.key_name,
            declaration,
            &generated_property,
            &seed.scope,
            &seed.period,
            seed.current,
        )
        .map_err(invalid)?;
        if let Some(values) = current.values.as_ref() {
            validate_counter_binding(
                values,
                series_owner,
                &seed.key_name,
                declaration,
                &generated_property,
                &seed.scope,
                &seed.period,
            )
            .map_err(invalid)?;
            let existing = counter_current(values)
                .ok_or_else(|| invalid("the stored counter is malformed"))?;
            if existing > seed.current {
                return Err(invalid("an imported counter cannot lower committed state"));
            }
            if existing == seed.current {
                continue;
            }
        }
        writes.push(FactWrite {
            key: key.clone(),
            expected: current.version,
            mutation: FactMutation::Put {
                values: expected_values,
            },
        });
        events.push(FactEvent::NamingSeriesCounterSeeded {
            counter: key,
            object_type: (*series_owner).clone(),
            key_name: seed.key_name.as_str().to_owned(),
            scope: scope_value(&seed.scope),
            period: seed.period.clone(),
            current: seed.current,
        });
    }

    if writes.is_empty() {
        return Ok(None);
    }
    let commit = CellCommit {
        writes,
        events: events.into_iter().map(Into::into).collect(),
        ..CellCommit::empty()
    };
    host.commit(commit)
        .map(Some)
        .map_err(DecisionRefusal::from_commit)
}

/// The sole series attached to an entity key, if the type declares one.
fn declared_key<'a>(
    catalog: &TypeCatalog<'a>,
    object_type: &ObjectTypeName,
    key_name: &KeyName,
) -> Option<(&'a ObjectTypeName, &'a KeySpec)> {
    catalog
        .ancestry(object_type)
        .into_iter()
        .find_map(|definition| {
            definition
                .identity
                .iter()
                .find(|key| &key.name == key_name)
                .map(|key| (&definition.name, key))
        })
}

/// A resolved series declaration and its owning entity key.
pub(crate) struct ResolvedNamingSeries<'a> {
    /// Type that owns the key declaration and its counter partitions.
    pub(crate) owner: &'a ObjectTypeName,
    /// Name of the declaring entity key.
    pub(crate) key_name: &'a KeyName,
    /// Properties that form the declared key.
    pub(crate) key_properties: &'a [PropertyId],
    /// Naming policy attached to the key.
    pub(crate) declaration: &'a NamingSeriesIR,
}

/// Locate the nearest naming-series key and retain its declaring type.
pub(crate) fn type_series<'a>(
    catalog: &TypeCatalog<'a>,
    object_type: &ObjectTypeName,
) -> Option<ResolvedNamingSeries<'a>> {
    catalog
        .ancestry(object_type)
        .into_iter()
        .find_map(|definition| {
            definition.identity.iter().find_map(|key| {
                key.naming_series
                    .as_ref()
                    .map(|series| ResolvedNamingSeries {
                        owner: &definition.name,
                        key_name: &key.name,
                        key_properties: key.properties.as_slice(),
                        declaration: series,
                    })
            })
        })
}

/// A fully checked allocation candidate. The returned counter write must be
/// included in the caller's single action commit with its target and receipt.
pub(crate) struct SeriesCandidate {
    pub(crate) target: FactKey,
    pub(crate) target_state: FactState,
    pub(crate) counter_write: FactWrite,
    pub(crate) skipped_reads: Vec<FactExpectation>,
    pub(crate) event: FactEvent,
}

/// Derive the exact declared scope tuple from effective proposed object values.
pub(crate) fn scope_from_values(
    declaration: &NamingSeriesIR,
    values: &JsonObject,
) -> Result<BTreeMap<PropertyId, String>, String> {
    let mut scope = BTreeMap::new();
    for field in &declaration.scope_fields {
        let Some(RuntimeValue::String(value)) = values.get(field.as_str()) else {
            return Err(format!(
                "scope property {} is missing or is not text",
                field.as_str()
            ));
        };
        scope.insert(field.clone(), value.clone());
    }
    validate_scope(declaration, &scope)?;
    Ok(scope)
}

/// Read the current counter and resolve one available key without reserving it.
/// The caller commits the returned counter CAS together with the create.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the allocator keeps target identity and series owner explicit"
)]
pub(crate) fn prepare_candidate<H>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeName,
    series_owner: &ObjectTypeName,
    key_name: &KeyName,
    key_properties: &[PropertyId],
    declaration: &NamingSeriesIR,
    scope: &BTreeMap<PropertyId, String>,
    now_millis: u64,
) -> Result<SeriesCandidate, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
{
    let generated_property = match key_properties {
        [property] => property.clone(),
        _ => {
            return Err(invalid(
                "the naming series must generate one text key property",
            ));
        }
    };
    validate_scope(declaration, scope).map_err(invalid)?;
    let width = counter_width(&declaration.pattern).map_err(invalid)?;
    let max = counter_max(width).ok_or_else(|| invalid("counter width is unsupported"))?;
    let epoch_millis = now_millis;
    let now_millis = i64::try_from(epoch_millis)
        .map_err(|error| invalid(format!("logical time is out of range: {error}")))?;
    norn_expr::temporal::validate_timezone(&declaration.timezone)
        .map_err(|error| invalid(format!("invalid naming-series timezone: {error}")))?;
    let date = norn_expr::temporal::naming_series_date_at(now_millis, &declaration.timezone)
        .map_err(|error| {
            invalid(format!(
                "naming-series timezone could not resolve the ledger clock: {error}"
            ))
        })?;
    let period = period_for(declaration.reset_policy, date.year, date.month, date.day);
    let counter_key = counter_fact_key(series_owner, key_name, scope, &period).map_err(invalid)?;
    crate::check_fact_key_identity(&counter_key).map_err(DecisionRefusal::from_commit)?;
    let counter_state = host.load(&counter_key).map_err(DecisionRefusal::Host)?;
    let mut current = if let Some(values) = &counter_state.values {
        validate_counter_binding(
            values,
            series_owner,
            key_name,
            declaration,
            &generated_property,
            scope,
            &period,
        )
        .map_err(invalid)?;
        counter_current(values).ok_or_else(|| invalid("the stored counter is malformed"))?
    } else {
        0
    };
    if current > max {
        return Err(invalid("the stored counter exceeds the declared width"));
    }

    let commit_model = CommitModel::new(catalog);
    let mut skipped_reads = Vec::new();
    let mut probes = 0_usize;
    let (value, target_key, target_state) = loop {
        probes = probes
            .checked_add(1)
            .ok_or_else(|| invalid("naming-series collision search exceeded 128 keys"))?;
        if probes > MAX_COLLISION_PROBES {
            return Err(invalid("naming-series collision search exceeded 128 keys"));
        }
        let candidate = current
            .checked_add(1)
            .filter(|candidate| *candidate <= max)
            .ok_or_else(|| invalid("the naming-series counter is exhausted"))?;
        let rendered = render_key(
            declaration,
            scope,
            date.year,
            date.month,
            date.day,
            candidate,
            width,
        )
        .map_err(invalid)?;
        let target_key = FactKey::new(object_type.as_str(), rendered);
        crate::check_fact_key_identity(&target_key).map_err(DecisionRefusal::from_commit)?;
        // Probe through the hierarchy root so a same-key object in any branch
        // is visible, while the committed target retains its concrete type.
        let probe_key = commit_model.canonical(&target_key);
        crate::check_fact_key_identity(&probe_key).map_err(DecisionRefusal::from_commit)?;
        let state = host.load(&probe_key).map_err(DecisionRefusal::Host)?;
        if state.version == FactVersion::ABSENT {
            break (candidate, target_key, state);
        }
        match declaration.gap_policy {
            norn_ir::NamingGapPolicy::GaplessWithinCommit => {
                return Err(invalid(
                    "the next gapless naming-series key is already occupied",
                ));
            }
            norn_ir::NamingGapPolicy::AllowGaps => {
                skipped_reads.push(FactExpectation {
                    key: probe_key,
                    expected: state.version,
                });
                current = candidate;
            }
        }
    };
    let values = counter_values(
        series_owner,
        key_name,
        declaration,
        &generated_property,
        scope,
        &period,
        value,
    )
    .map_err(invalid)?;
    let event = FactEvent::NamingSeriesAllocated {
        key: target_key.clone(),
        counter: counter_key.clone(),
        pattern: declaration.pattern.clone(),
        timezone: declaration.timezone.clone(),
        epoch_millis,
        tzdb_version: norn_expr::temporal::NAMING_SERIES_TZDB_VERSION.to_owned(),
        scope: scope.clone(),
        period,
        value,
        reset_policy: declaration.reset_policy,
        gap_policy: declaration.gap_policy,
    };
    Ok(SeriesCandidate {
        target: target_key,
        target_state,
        counter_write: FactWrite {
            key: counter_key,
            expected: counter_state.version,
            mutation: FactMutation::Put { values },
        },
        skipped_reads,
        event,
    })
}

pub(crate) fn generated_property(key: &KeySpec) -> Result<PropertyId, String> {
    match key.properties.as_slice() {
        [property] => Ok(property.clone()),
        _ => Err("the naming series must generate one text key property".to_owned()),
    }
}

pub(crate) fn counter_width(pattern: &str) -> Result<u8, String> {
    let tokens = parse_naming_series_pattern(pattern).map_err(|error| error.to_string())?;
    let mut widths = tokens.iter().filter_map(|token| match token {
        NamingPatternToken::Counter { width } => Some(*width),
        NamingPatternToken::Literal(_)
        | NamingPatternToken::YearFour
        | NamingPatternToken::YearTwo
        | NamingPatternToken::Month
        | NamingPatternToken::Day
        | NamingPatternToken::ScopeField(_) => None,
    });
    let width = widths
        .next()
        .ok_or_else(|| "pattern has no counter token".to_owned())?;
    if widths.next().is_some() || width == 0 || width > MAX_NAMING_COUNTER_WIDTH {
        return Err("pattern must have one counter token of at most 18 digits".to_owned());
    }
    Ok(width)
}

pub(crate) fn counter_max(width: u8) -> Option<u64> {
    10_u64
        .checked_pow(u32::from(width))
        .and_then(|limit| limit.checked_sub(1))
}

fn validate_scope(
    declaration: &NamingSeriesIR,
    scope: &BTreeMap<PropertyId, String>,
) -> Result<(), String> {
    let expected: BTreeSet<_> = declaration.scope_fields.iter().cloned().collect();
    let actual: BTreeSet<_> = scope.keys().cloned().collect();
    if expected != actual {
        return Err("scope fields do not match the declared naming-series scope".to_owned());
    }
    for value in scope.values() {
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(
                "scope values must be non-empty ASCII letters, digits, or underscores".to_owned(),
            );
        }
        if value.len() > MAX_OBJECT_KEY_BYTES {
            return Err("a naming-series scope value exceeds the object-key byte limit".to_owned());
        }
    }
    Ok(())
}

fn validate_period(declaration: &NamingSeriesIR, period: &str) -> Result<(), String> {
    let valid = match declaration.reset_policy {
        NamingResetPolicy::Never => period.is_empty(),
        NamingResetPolicy::Year => {
            digits(period, 4)
                && norn_expr::temporal::validate_date(&format!("{period}-01-01")).is_ok()
        }
        NamingResetPolicy::Month => {
            period.len() == 7
                && period.as_bytes().get(4) == Some(&b'-')
                && period.get(..4).is_some_and(|value| digits(value, 4))
                && period.get(5..).is_some_and(|value| digits(value, 2))
                && period
                    .get(5..)
                    .and_then(|value| value.parse::<u8>().ok())
                    .is_some_and(|month| (1..=12).contains(&month))
                && norn_expr::temporal::validate_date(&format!("{period}-01")).is_ok()
        }
        NamingResetPolicy::Day => norn_expr::temporal::validate_date(period).is_ok(),
    };
    if valid {
        Ok(())
    } else {
        Err("period does not match the declared reset policy".to_owned())
    }
}

fn period_for(policy: NamingResetPolicy, year: u16, month: u8, day: u8) -> String {
    match policy {
        NamingResetPolicy::Never => String::new(),
        NamingResetPolicy::Year => format!("{year:04}"),
        NamingResetPolicy::Month => format!("{year:04}-{month:02}"),
        NamingResetPolicy::Day => format!("{year:04}-{month:02}-{day:02}"),
    }
}

fn render_key(
    declaration: &NamingSeriesIR,
    scope: &BTreeMap<PropertyId, String>,
    year: u16,
    month: u8,
    day: u8,
    counter: u64,
    width: u8,
) -> Result<String, String> {
    let tokens =
        parse_naming_series_pattern(&declaration.pattern).map_err(|error| error.to_string())?;
    let mut output = String::new();
    for token in tokens {
        match token {
            NamingPatternToken::Literal(value) => output.push_str(value),
            NamingPatternToken::YearFour => {
                write!(output, "{year:04}").map_err(|error| error.to_string())?;
            }
            NamingPatternToken::YearTwo => {
                write!(output, "{:02}", year.rem_euclid(100)).map_err(|error| error.to_string())?;
            }
            NamingPatternToken::Month => {
                write!(output, "{month:02}").map_err(|error| error.to_string())?;
            }
            NamingPatternToken::Day => {
                write!(output, "{day:02}").map_err(|error| error.to_string())?;
            }
            NamingPatternToken::ScopeField(field) => {
                let value = scope
                    .iter()
                    .find(|(property, _)| property.as_str() == field)
                    .map(|(_, value)| value.as_str())
                    .ok_or_else(|| format!("scope token {{{field}}} has no declared value"))?;
                output.push_str(value);
            }
            NamingPatternToken::Counter { width: token_width } => {
                if token_width != width {
                    return Err("counter token width changed after validation".to_owned());
                }
                write!(output, "{counter:0width$}", width = usize::from(width))
                    .map_err(|error| error.to_string())?;
            }
        }
        if output.len() > MAX_OBJECT_KEY_BYTES {
            return Err(
                "the rendered naming-series key exceeds the object-key byte limit".to_owned(),
            );
        }
    }
    if output.is_empty() {
        return Err("the naming pattern rendered an empty key".to_owned());
    }
    Ok(output)
}

fn digits(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn counter_fact_key(
    object_type: &ObjectTypeName,
    key_name: &KeyName,
    scope: &BTreeMap<PropertyId, String>,
    period: &str,
) -> Result<FactKey, String> {
    let identity = runtime_object! {
        "objectType" => object_type.as_str(),
        "keyName" => key_name.as_str(),
        "scope" => scope_value(scope),
        "period" => period,
    };
    let canonical = identity
        .canonical_json()
        .map_err(|error| error.to_string())?;
    let digest = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&canonical);
    Ok(FactKey::new(
        NAMING_SERIES_COUNTER_TYPE,
        format!("norn1:{digest}"),
    ))
}

fn scope_value(scope: &BTreeMap<PropertyId, String>) -> RuntimeValue {
    RuntimeValue::Object(
        scope
            .iter()
            .map(|(property, value)| {
                (
                    property.as_str().to_owned(),
                    RuntimeValue::String(value.clone()),
                )
            })
            .collect(),
    )
}

fn counter_values(
    object_type: &ObjectTypeName,
    key_name: &KeyName,
    declaration: &NamingSeriesIR,
    generated_property: &PropertyId,
    scope: &BTreeMap<PropertyId, String>,
    period: &str,
    current: u64,
) -> Result<JsonObject, String> {
    let declaration = norn_ir::runtime::encode(declaration)?;
    let current =
        norn_ir::Decimal::parse(&current.to_string()).map_err(|error| error.to_string())?;
    let values = runtime_object! {
        "objectType" => object_type.as_str(),
        "keyName" => key_name.as_str(),
        "generatedProperty" => generated_property.as_str(),
        "declaration" => declaration,
        "tzdbVersion" => norn_expr::temporal::NAMING_SERIES_TZDB_VERSION,
        "scope" => scope_value(scope),
        "period" => period,
        "current" => RuntimeValue::Number(current),
    };
    values
        .as_object()
        .cloned()
        .ok_or_else(|| "counter values must be an object".to_owned())
}

fn counter_current(values: &JsonObject) -> Option<u64> {
    match values.get("current") {
        Some(RuntimeValue::Number(value)) => value.to_string().parse().ok(),
        _ => None,
    }
}

fn validate_counter_binding(
    values: &JsonObject,
    object_type: &ObjectTypeName,
    key_name: &KeyName,
    declaration: &NamingSeriesIR,
    generated_property: &PropertyId,
    scope: &BTreeMap<PropertyId, String>,
    period: &str,
) -> Result<(), String> {
    let expected = counter_values(
        object_type,
        key_name,
        declaration,
        generated_property,
        scope,
        period,
        0,
    )?;
    for field in [
        "objectType",
        "keyName",
        "generatedProperty",
        "declaration",
        "tzdbVersion",
        "scope",
        "period",
    ] {
        if values.get(field) != expected.get(field) {
            return Err(
                "the stored counter is bound to a different naming-series declaration".to_owned(),
            );
        }
    }
    Ok(())
}

fn invalid<X>(reason: impl Into<String>) -> DecisionRefusal<X> {
    DecisionRefusal::InvalidOutput {
        reason: reason.into(),
    }
}
