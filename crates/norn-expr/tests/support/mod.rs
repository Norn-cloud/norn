#![allow(dead_code, reason = "shared by several integration-test crates")]
//! Shared fixture loading for the type-environment and checker tests.

use std::collections::BTreeMap;

use norn_expr::{EntityDefinitions, EnvOptions, Type, Value, ZenTypeEnv};
use norn_ir::{
    InterfaceDef, ObjectTypeDef, ObjectTypeExtension, ObjectTypeName, PropertyTypeDef,
    ResourceLimitProfile,
};
use serde::Deserialize;

/// The decoded fixture artifact set.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Model {
    pub(crate) property_types: Vec<PropertyTypeDef>,
    pub(crate) interfaces: Vec<InterfaceDef>,
    pub(crate) object_types: Vec<ObjectTypeDef>,
    pub(crate) extensions: Vec<ObjectTypeExtension>,
}

impl Model {
    pub(crate) fn retail() -> Self {
        serde_json::from_str(include_str!("../fixtures/retail_model.json"))
            .unwrap_or_else(|err| panic!("fixture model does not decode: {err}"))
    }

    pub(crate) fn definitions(&self) -> EntityDefinitions<'_> {
        EntityDefinitions {
            property_types: &self.property_types,
            interfaces: &self.interfaces,
            object_types: &self.object_types,
            extensions: &self.extensions,
        }
    }

    pub(crate) fn env(&self) -> ZenTypeEnv {
        ZenTypeEnv::from_artifact_set(&self.definitions(), EnvOptions::default())
            .unwrap_or_else(|errors| panic!("fixture env does not derive: {errors:?}"))
    }
}

pub(crate) fn name(value: &str) -> ObjectTypeName {
    ObjectTypeName(value.to_owned())
}

/// The retail env with `self` bound to `object_type`.
pub(crate) fn env_for(object_type: &str) -> ZenTypeEnv {
    Model::retail()
        .env()
        .bind_self(&name(object_type))
        .unwrap_or_else(|err| panic!("bind self: {err}"))
}

pub(crate) fn fields<const N: usize>(entries: [(&str, Type); N]) -> BTreeMap<String, Type> {
    entries
        .into_iter()
        .map(|(name, ty)| (name.to_owned(), ty))
        .collect()
}

/// `norn.limits/1` with unlimited decide fuel: for type fixtures that do not
/// test the cost bound.
pub(crate) fn roomy_limits() -> ResourceLimitProfile {
    let mut limits = ResourceLimitProfile::norn_limits_v1();
    limits.decide.fuel_units = u64::MAX;
    limits
}

/// The length of `value`'s compact JSON (the Norn1 canonical form's length:
/// same tokens, sorted keys, no whitespace).
pub(crate) fn slice_bytes(value: &Value) -> u64 {
    fn string(text: &str) -> u64 {
        serde_json::to_string(text).expect("string").len() as u64
    }
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(number) => number.as_str().len() as u64,
        Value::String(text) => string(text),
        Value::Array(items) => {
            2 + items.iter().map(slice_bytes).sum::<u64>() + items.len().saturating_sub(1) as u64
        }
        Value::Object(entries) => {
            2 + entries
                .iter()
                .map(|(key, value)| string(key) + 1 + slice_bytes(value))
                .sum::<u64>()
                + entries.len().saturating_sub(1) as u64
        }
    }
}

/// Whether `value` is within the `norn.limits/1` slice bounds the static cost
/// estimate assumes: at most `max_canonical_bytes`, and every list at most
/// `max_records` long when it holds records.
pub(crate) fn within_slice_limits(value: &Value) -> bool {
    fn lists_ok(value: &Value, max_records: usize) -> bool {
        match value {
            Value::Array(items) => {
                (items.len() <= max_records || !items.iter().any(|i| matches!(i, Value::Object(_))))
                    && items.iter().all(|item| lists_ok(item, max_records))
            }
            Value::Object(entries) => entries.values().all(|item| lists_ok(item, max_records)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
        }
    }
    let limits = ResourceLimitProfile::norn_limits_v1();
    slice_bytes(value) <= limits.slice.max_canonical_bytes
        && lists_ok(value, limits.slice.max_records as usize)
}
