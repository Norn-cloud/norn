//! The predicate an action's aggregate input reads (#1105).
//!
//! Action preparation resolves each declared aggregate to a predicate read
//! over its type: the filter's input fields become literals and the window's
//! kind comes from the windowed property's declared type. The fold itself is
//! pure (`norn_decisions::fold_aggregate`); the commit only carries the
//! resulting [`crate::PredicateReadExpectation`], whose recheck keeps the
//! folded value valid at commit.

use norn_checker::EffectiveKind;
use norn_ir::{ActionAggregateInput, PropertyType};

use crate::cell_host::{JsonObject, JsonValue};
use crate::decision_commit::TypeCatalog;
use crate::predicate_read::{PredicateQuery, PredicateWindow, PredicateWindowKind};

/// The predicate `aggregate` reads for this call's effective input.
///
/// # Errors
///
/// A reason when an input field has no usable value or the windowed property
/// is not a date or timestamp in this catalog.
pub(crate) fn predicate_query(
    aggregate: &ActionAggregateInput,
    input: &JsonObject,
    catalog: &TypeCatalog<'_>,
) -> Result<PredicateQuery, String> {
    let mut filter = std::collections::BTreeMap::new();
    for (property, field) in &aggregate.filter {
        let value = input
            .get(field)
            .filter(|value| !value.is_null())
            .ok_or_else(|| format!("input field {field:?} has no value"))?;
        filter.insert(property.as_str().to_owned(), value.clone());
    }
    let window = aggregate
        .window
        .as_ref()
        .map(|window| {
            let kind = catalog
                .effective_type(&aggregate.object_type)
                .and_then(|object| object.property(window.property.as_str()))
                .and_then(|property| match &property.kind {
                    EffectiveKind::Base(PropertyType::Date {}) => Some(PredicateWindowKind::Date),
                    EffectiveKind::Base(PropertyType::Timestamp { .. }) => {
                        Some(PredicateWindowKind::Timestamp)
                    }
                    EffectiveKind::Base(_)
                    | EffectiveKind::Registry(_)
                    | EffectiveKind::Unresolved => None,
                })
                .ok_or_else(|| {
                    format!(
                        "window property {:?} is not a date or timestamp",
                        window.property.as_str()
                    )
                })?;
            let edge = |field: &str| {
                input
                    .get(field)
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("window edge {field:?} has no text value"))
            };
            Ok::<_, String>(PredicateWindow {
                property: window.property.as_str().to_owned(),
                kind,
                start: edge(&window.start)?,
                end: edge(&window.end)?,
                start_inclusive: window.start_inclusive,
                end_inclusive: window.end_inclusive,
            })
        })
        .transpose()?;
    Ok(PredicateQuery {
        object_type: aggregate.object_type.clone(),
        filter,
        window,
    })
}
