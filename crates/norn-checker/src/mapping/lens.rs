//! Finite write-back lenses over existing mapping property projections.
//!
//! Exhaustiveness is relative to the declared closed source domain. This
//! evidence grants no cell/connector write authority and proves no concurrency law.
use std::collections::BTreeMap;

use norn_expr::{EvaluationBudget, Expression, Value};
use norn_ir::mapping::MappingIr;
use serde::Deserialize;

/// Reserved mapping extension for finite Boolean write-back projections.
pub const LENS_SLOT: &str = "norn.write-back/0.1.0";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Declaration {
    map: String,
    property: String,
    sources: Vec<BTreeMap<String, bool>>,
    /// For each source row, decimal-string destination indices for false, then true.
    put: Vec<[String; 2]>,
}

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
        let row = self
            .sources
            .iter()
            .position(|s| s == source)
            .ok_or("unknown source state")?;
        let destination = self
            .put
            .get(row)
            .and_then(|r| r.get(usize::from(view)))
            .and_then(|index| self.sources.get(*index))
            .ok_or("unknown put-back")?;
        Ok(destination)
    }

    /// Read the checked view, refusing source states outside the domain.
    ///
    /// # Errors
    /// Returns unknown for a source outside the admitted domain.
    pub fn get(&self, source: &BTreeMap<String, bool>) -> Result<bool, String> {
        self.sources
            .iter()
            .position(|s| s == source)
            .and_then(|row| self.get.get(row).copied())
            .ok_or("unknown source state".into())
    }
}

/// Check a declared lens using the mapping's actual Get expression.
///
/// At most 32 states, 16 Boolean fields per state, 128-byte field names,
/// 4096 expression bytes and 1024 evaluation units per state are admitted.
/// PutPut is always required. No sampling or solver success is accepted.
///
/// # Errors
/// Malformed, unsupported, unknown or refuted declarations refuse admission.
pub fn admit(mapping: &MappingIr) -> Result<Option<AdmittedLens>, String> {
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
    let mut bytes = vec![0_u8; 65_536];
    let mut remaining = bytes.as_mut_slice();
    serde_json::to_writer(&mut remaining, raw)
        .map_err(|_| "unknown: lens declaration exceeds byte bound")?;
    let used = 65_536_usize.saturating_sub(remaining.len());
    let declaration: Declaration =
        serde_json::from_slice(bytes.get(..used).ok_or("unknown: byte bound")?)
            .map_err(|e| e.to_string())?;
    let Declaration {
        map,
        property,
        sources,
        put,
    } = declaration;
    if sources.is_empty() || sources.len() > 32 || put.len() != sources.len() {
        return Err("unknown: invalid finite domain or put table size".into());
    }
    let put = put.into_iter().map(|[no, yes]| {
        Ok([no.parse::<usize>().map_err(|_| "unknown: put index")?,
            yes.parse::<usize>().map_err(|_| "unknown: put index")?])
    }).collect::<Result<Vec<_>, String>>()?;
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
    for (row, source) in lens.sources.iter().enumerate() {
        let current = lens.get(source)?;
        if lens.put(source, current)? != source {
            return Err(format!("refuted GetPut at source {row}"));
        }
        for view in [false, true] {
            let updated = lens.put(source, view)?;
            if lens.get(updated)? != view {
                return Err(format!("refuted PutGet at source {row}, view {view}"));
            }
            for last in [false, true] {
                if lens.put(updated, last)? != lens.put(source, last)? {
                    return Err(format!(
                        "refuted PutPut at source {row}, views {view}, {last}"
                    ));
                }
            }
        }
    }
    Ok(Some(lens))
}
