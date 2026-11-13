//! HOST-READ-7: `GET /v1/objects/{type}:facets`, the exact count of each value
//! of a concept's categorical properties over the caller's authorized
//! snapshot, narrowed by the same filter a grid query takes.
//!
//! The rows counted are the rows a grid query with that filter would list: the
//! collection gate first, then each row on its own (D67). A row whose values
//! are withheld is never counted, and quarantined rows are left out, as on a
//! page. Counts are exact over the rows read; past the visible-row scan bound
//! the facets cover the rows read and say `approximate`.

use std::collections::BTreeMap;

use norn_cell::engine::SqlEngine;
use norn_ir::RuntimeValue;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa::openapi::schema::{Object, Schema, SchemaType};

use crate::{
    host::DevHost,
    object_profile::kind_of,
    object_query::{self, ObjectQueryParams},
    object_routes::{MAX_QUERY_SCAN, ObjectAccess, collection, not_found, scan_listed},
    refusal::Refusal,
    scope::ScopeQuery,
};

/// Most values one facet names; the rows holding the others are counted together.
pub const MAX_FACET_VALUES: usize = 20;

/// The raw query string fields of `GET /v1/objects/{type}:facets`.
#[derive(Debug, Default, Clone, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ObjectFacetParams {
    /// The properties to count, comma separated: each a text, choice or
    /// yes/no property of the type. Every such property when absent.
    #[param(nullable = false, example = "status,unit")]
    pub properties: Option<String>,
    /// The grid query's filter (`{"all":[clause, ...]}`), with its rules: the
    /// facets count the rows a page with this filter lists.
    #[param(nullable = false)]
    pub filter: Option<String>,
}

/// The exact value counts of a concept's categorical properties.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornObjectFacets {
    /// The concept counted; rows may be of its subtypes.
    pub object_type: String,
    /// Rows counted: readable, not held back, and matching the filter.
    pub rows: u64,
    /// Rows the caller may know exist but whose values are withheld; none is
    /// counted, and none matches a filter.
    pub withheld_rows: u64,
    /// Rows held back by source disputes or open quarantining findings.
    pub quarantined_rows: u64,
    /// True when the scan bound was reached: every count is a lower bound.
    pub approximate: bool,
    /// The visible-row scan bound.
    pub scan_limit: u64,
    /// One facet per property asked for, in the order asked (declared order
    /// when none was named).
    pub facets: Vec<NornFacet>,
}

/// One property's values and how many counted rows hold each.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornFacet {
    /// The property id.
    pub property: String,
    /// Its values, most rows first (ties in value order), at most
    /// [`MAX_FACET_VALUES`].
    pub values: Vec<NornFacetValue>,
    /// Counted rows holding a value not listed.
    pub other_rows: u64,
    /// Counted rows without a value.
    pub nulls: u64,
}

/// One value and the counted rows that hold it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornFacetValue {
    #[schema(schema_with = value_schema)]
    /// The value as stored: text or a yes/no.
    pub value: RuntimeValue,
    /// Counted rows holding it.
    pub count: u64,
}

fn value_schema() -> Schema {
    let mut schema = Object::with_type(SchemaType::AnyValue);
    schema.description = Some("A stored value: text or a boolean.".to_owned());
    Schema::Object(schema)
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// The values of one property over `rows`, most rows first, the tail counted together.
fn facet(property: &str, rows: &[&BTreeMap<String, RuntimeValue>]) -> NornFacet {
    let mut held: BTreeMap<String, (RuntimeValue, u64)> = BTreeMap::new();
    let mut nulls: u64 = 0;
    for values in rows {
        match values.get(property) {
            None | Some(RuntimeValue::Null) => nulls = nulls.saturating_add(1),
            Some(value) => {
                let entry = held
                    .entry(format!("{value:?}"))
                    .or_insert_with(|| (value.clone(), 0));
                entry.1 = entry.1.saturating_add(1);
            }
        }
    }
    let mut ordered: Vec<(String, (RuntimeValue, u64))> = held.into_iter().collect();
    ordered.sort_by(|(left, (_, a)), (right, (_, b))| b.cmp(a).then_with(|| left.cmp(right)));
    let other_rows = ordered
        .iter()
        .skip(MAX_FACET_VALUES)
        .fold(0_u64, |sum, (_, (_, n))| sum.saturating_add(*n));
    NornFacet {
        property: property.to_owned(),
        values: ordered
            .into_iter()
            .take(MAX_FACET_VALUES)
            .map(|(_, (value, count))| NornFacetValue { value, count })
            .collect(),
        other_rows,
        nulls,
    }
}

/// HOST-READ-7: the exact value counts of `object_type`'s categorical
/// properties over the caller's authorized snapshot, narrowed by a filter.
///
/// # Errors
/// A denied gate (the opaque 403), an undeclared type (404), an invalid filter
/// or a property that is not a declared text, choice or yes/no property (400
/// `query.invalid`, every violation at its pointer), a commit while the
/// snapshot was read (409 `object.snapshot_moved`, retryable), or a host failure.
#[utoipa::path(get, path = "/v1/objects/{type}:facets", tag = "crate::object_routes", params(ScopeQuery, ObjectFacetParams, ("type" = String, Path, description = "The concept to count.")), responses((status = 200, description = "The value counts over the caller's authorized snapshot.", body = NornObjectFacets), (status = 400, description = "The request was refused as invalid (`query.invalid`): the filter does not hold, or a property is not a declared text, choice or yes/no property, with every violation as a diagnostic.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 409, description = "The snapshot moved while it was read; retry.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn get_object_facets<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    params: &ObjectFacetParams,
    access: &dyn ObjectAccess,
) -> Result<NornObjectFacets, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let properties = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        if !reader.model().has_type(object_type) {
            return Err(not_found());
        }
        reader
            .model()
            .effective_property_types(object_type)
            .map_err(Refusal::host)?
    };
    let query = object_query::parse(
        &ObjectQueryParams {
            filter: params.filter.clone(),
            ..ObjectQueryParams::default()
        },
        &properties,
    )?;
    let categorical = |id: &str| {
        properties
            .iter()
            .any(|(name, kind)| name == id && matches!(kind_of(kind.as_ref()), "text" | "boolean"))
    };
    let asked: Vec<String> = match params.properties.as_deref() {
        None => properties
            .iter()
            .filter(|(name, _)| categorical(name))
            .map(|(name, _)| name.clone())
            .collect(),
        Some(list) => {
            let names: Vec<String> = list.split(',').map(|name| name.trim().to_owned()).collect();
            let violations: Vec<(&str, String, String)> = names
                .iter()
                .filter(|name| !categorical(name))
                .map(|name| {
                    (
                        "query.property_unknown",
                        format!("{name} is not a text, choice or yes/no property of {object_type}"),
                        "/properties".to_owned(),
                    )
                })
                .collect();
            if !violations.is_empty() {
                return Err(object_query::refused(violations));
            }
            names
        }
    };
    let scan = scan_listed(
        host,
        access,
        &actor,
        object_type,
        (None, usize::MAX, MAX_QUERY_SCAN),
    )?;
    let counted: Vec<&BTreeMap<String, RuntimeValue>> = scan
        .listed
        .iter()
        .filter(|listed| {
            listed.readable && object_query::matches(&query.filter, Some(&listed.row.values))
        })
        .map(|listed| &listed.row.values)
        .collect();
    let withheld = scan.listed.iter().filter(|listed| !listed.readable).count();
    Ok(NornObjectFacets {
        object_type: object_type.to_owned(),
        rows: count(counted.len()),
        withheld_rows: count(withheld),
        quarantined_rows: scan.quarantined,
        approximate: !scan.exhausted,
        scan_limit: count(MAX_QUERY_SCAN),
        facets: asked
            .iter()
            .map(|property| facet(property, &counted))
            .collect(),
    })
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "facet assertions"
)]
mod tests {
    use super::*;

    fn row(status: Option<&str>) -> BTreeMap<String, RuntimeValue> {
        status
            .map(|status| (String::from("status"), RuntimeValue::String(status.into())))
            .into_iter()
            .collect()
    }

    #[test]
    fn values_come_most_rows_first_ties_in_value_order_and_the_tail_is_counted_together() {
        let mut rows: Vec<BTreeMap<String, RuntimeValue>> = Vec::new();
        for (status, n) in [("open", 3), ("closed", 5), ("held", 3)] {
            rows.extend((0..n).map(|_| row(Some(status))));
        }
        rows.push(row(None));
        let refs: Vec<&BTreeMap<String, RuntimeValue>> = rows.iter().collect();
        let found = facet("status", &refs);
        let names: Vec<(String, u64)> = found
            .values
            .iter()
            .map(|v| (format!("{:?}", v.value), v.count))
            .collect();
        assert_eq!(names[0].1, 5);
        assert!(names[1].0 < names[2].0, "ties keep value order: {names:?}");
        assert_eq!(found.nulls, 1);
        assert_eq!(found.other_rows, 0);

        let many: Vec<BTreeMap<String, RuntimeValue>> = (0..MAX_FACET_VALUES + 3)
            .map(|n| row(Some(&format!("v{n:03}"))))
            .collect();
        let refs: Vec<&BTreeMap<String, RuntimeValue>> = many.iter().collect();
        let found = facet("status", &refs);
        assert_eq!(found.values.len(), MAX_FACET_VALUES);
        assert_eq!(found.other_rows, 3);
    }
}
