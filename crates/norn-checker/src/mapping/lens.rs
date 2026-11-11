//! Finite write-back lenses over existing mapping property projections.
//!
//! Exhaustiveness is relative to the declared closed source domain. This
//! evidence grants no cell/connector write authority and proves no concurrency law.
use std::collections::BTreeMap;

use norn_expr::{EvaluationBudget, Expression, Type, Value};
use norn_ir::mapping::{FiniteWriteBackLens, MappingIr};
use norn_ir::source_schema::SourceSchemaIr;

/// The extracted, proved lookups and lens-law decision (#718 U5).
pub mod decision;

/// Reserved mapping extension for finite Boolean write-back projections.
pub const LENS_SLOT: &str = "norn.write-back/0.1.0";

/// A live-checked lens, exhaustive only over its retained finite domain.
/// Construction is private: deserialized assertions never confer admission.
#[derive(Debug)]
pub struct AdmittedLens {
    sources: Vec<BTreeMap<String, bool>>,
    get: Vec<bool>,
    put: Vec<[usize; 2]>,
}

impl AdmittedLens {
    /// Number of source states covered by exhaustive-finite evidence.
    #[must_use]
    pub fn source_states(&self) -> usize {
        self.sources.len()
    }

    /// Apply the checked put-back, refusing source states outside the domain.
    /// Callers must separately enforce mutation authority and concurrency.
    ///
    /// # Errors
    /// Returns unknown for a source outside the admitted domain.
    pub fn put(
        &self,
        source: &BTreeMap<String, bool>,
        view: bool,
    ) -> Result<&BTreeMap<String, bool>, String> {
        decision::put(&self.sources, &self.put, source, view).map_err(refusal)
    }

    /// Read the checked view, refusing source states outside the domain.
    ///
    /// # Errors
    /// Returns unknown for a source outside the admitted domain.
    pub fn get(&self, source: &BTreeMap<String, bool>) -> Result<bool, String> {
        decision::get(&self.sources, &self.get, source).map_err(refusal)
    }
}

/// The same refusal text the lens has always reported.
fn refusal(refusal: decision::Refusal) -> String {
    match refusal {
        decision::Refusal::UnknownSource => "unknown source state".into(),
        decision::Refusal::UnknownPutBack => "unknown put-back".into(),
        decision::Refusal::GetPut(row) => format!("refuted GetPut at source {row}"),
        decision::Refusal::PutGet(row, view) => {
            format!("refuted PutGet at source {row}, view {view}")
        }
        decision::Refusal::PutPut(row, view, last) => {
            format!("refuted PutPut at source {row}, views {view}, {last}")
        }
    }
}

/// Check a declared lens using the mapping's actual Get expression.
///
/// At most 32 states, 16 Boolean fields per state, 128-byte field names,
/// 4096 expression bytes and 1024 evaluation units per state are admitted.
/// `PutPut` is always required. No sampling or solver success is accepted.
///
/// # Errors
/// Malformed, unsupported, unknown or refuted declarations refuse admission.
pub fn admit(mapping: &MappingIr, source: &SourceSchemaIr) -> Result<Option<AdmittedLens>, String> {
    check_declaration(mapping, Some(source))
}

pub(super) fn check_local(mapping: &MappingIr) -> Result<(), String> {
    check_declaration(mapping, None).map(|_| ())
}

fn check_declaration(
    mapping: &MappingIr,
    source_schema: Option<&SourceSchemaIr>,
) -> Result<Option<AdmittedLens>, String> {
    if mapping.extensions.as_ref().is_some_and(|e| {
        e.keys()
            .any(|k| k.starts_with("norn.write-back/") && k != LENS_SLOT)
    }) {
        return Err("unknown: unsupported lens version".into());
    }
    let Some(raw) = mapping.extensions.as_ref().and_then(|e| e.get(LENS_SLOT)) else {
        return Ok(None);
    };
    // The enclosing artifact admission bounds serialized input memory. Bound
    // this extension before decoding a second owned representation.
    // Reject declarations Norn1 cannot canonicalize, but decode the verbatim
    // bytes: canonicalization NFC-normalizes strings and would rewrite keys.
    norn_canonical::canonical_json_of(raw)
        .map_err(|_error| "unknown: lens declaration is not canonical JSON")?;
    let bytes = norn_canonical::serialize_json_verbatim(raw)
        .map_err(|_error| "unknown: lens declaration is not serializable")?;
    (bytes.len() <= 65_536)
        .then_some(())
        .ok_or("unknown: lens declaration exceeds byte bound")?;
    let declaration: FiniteWriteBackLens =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let FiniteWriteBackLens {
        map,
        property,
        sources,
        put,
    } = declaration;
    if sources.is_empty() || sources.len() > 32 || put.len() != sources.len() {
        return Err("unknown: invalid finite domain or put table size".into());
    }
    let put = parse_put(put, sources.len())?;
    for (row, source) in sources.iter().enumerate() {
        if source.is_empty()
            || source.len() > 16
            || source.keys().any(|k| k.len() > 128)
            || sources.iter().take(row).any(|s| s == source)
            || sources.first().is_none_or(|s| !s.keys().eq(source.keys()))
        {
            return Err(
                "unknown: source domain must contain unique, same-shaped Boolean records".into(),
            );
        }
    }
    let map = mapping
        .maps
        .iter()
        .find(|m| m.id == map)
        .ok_or("unknown map")?;
    if map.when.is_some() || map.source.nested_in.is_some() {
        return Err("unknown: guarded or nested lens".into());
    }
    if let Some(schema) = source_schema {
        check_source_schema(
            schema,
            &map.source.dataset,
            sources.first().ok_or("unknown: empty lens source domain")?,
        )?;
    }
    let rule = map
        .properties
        .iter()
        .find(|p| p.property.as_str() == property)
        .ok_or("unknown property projection")?;
    if rule.nested.is_some() || rule.value.as_str().len() > 4096 {
        return Err("unknown: unsupported or oversized Get".into());
    }
    norn_expr::validate_evaluation_source(
        rule.value.as_str(),
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|e| format!("unknown: Get outside bounded expression profile: {e:?}"))?;
    let expression = Expression::compile(rule.value.as_str()).map_err(|e| e.to_string())?;
    let mut get = Vec::new();
    for source in &sources {
        let input = Value::object(
            source
                .iter()
                .map(|(key, value)| (key.clone(), Value::Bool(*value))),
        );
        let evaluation = expression
            .evaluate(
                &Value::object([("input", input)]),
                EvaluationBudget::new(1024),
            )
            .map_err(|e| format!("unknown: {e}"))?;
        let Value::Bool(value) = evaluation.into_value() else {
            return Err("unknown: Get must produce a Boolean".into());
        };
        get.push(value);
    }
    let lens = AdmittedLens { sources, get, put };
    check_laws(&lens)?;
    Ok(Some(lens))
}

fn parse_put(put: Vec<[String; 2]>, source_states: usize) -> Result<Vec<[usize; 2]>, String> {
    let put = put
        .into_iter()
        .map(|[no, yes]| {
            Ok([
                no.parse::<usize>().map_err(|_error| "unknown: put index")?,
                yes.parse::<usize>()
                    .map_err(|_error| "unknown: put index")?,
            ])
        })
        .collect::<Result<Vec<_>, String>>()?;
    if put.iter().flatten().any(|index| *index >= source_states) {
        return Err("unknown put-back".into());
    }
    Ok(put)
}

fn check_source_schema(
    schema: &SourceSchemaIr,
    dataset: &str,
    declared: &BTreeMap<String, bool>,
) -> Result<(), String> {
    let fields = super::input_fields(schema, dataset)
        .ok_or("unknown: lens source dataset is not declared")?;
    // Every record already has identical keys and required Boolean values.
    for (field, ty) in &fields {
        if !declared.contains_key(field) {
            return Err(format!("unknown: lens source field {field:?} is missing"));
        }
        if *ty != Type::Bool {
            return Err(format!(
                "unknown: lens source field {field:?} requires {ty:?}, declared required Boolean"
            ));
        }
    }
    if let Some(field) = declared.keys().find(|field| !fields.contains_key(*field)) {
        return Err(format!(
            "unknown: lens source field {field:?} is undeclared"
        ));
    }
    Ok(())
}

fn check_laws(lens: &AdmittedLens) -> Result<(), String> {
    decision::check_laws(&lens.sources, &lens.get, &lens.put).map_err(refusal)
}
