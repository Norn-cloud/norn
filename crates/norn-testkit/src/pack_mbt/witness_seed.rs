//! DX-8 law-example witness seeding for IR-driven MBT object synthesis.

use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::pack_mbt_support::{PropertyType, PropertyTypeDef};

use super::AdmittedPack;
use super::synth::{EffectiveProp, resolve_property_type};

pub(super) fn positive_law_witness_for<'a>(
    pack: &'a AdmittedPack,
    object_type_name: &str,
    seed: u64,
) -> Option<&'a JsonObject> {
    let matching_law_ids: BTreeSet<&str> = pack
        .laws
        .iter()
        .filter(|law| law.subject == object_type_name)
        .map(|law| law.id.as_str())
        .collect();
    if matching_law_ids.is_empty() {
        return None;
    }
    let positives: Vec<&JsonObject> = pack
        .law_examples
        .iter()
        .filter(|ex| {
            ex.expected == super::Verdict::Holds && matching_law_ids.contains(ex.law_id.as_str())
        })
        .map(|ex| &ex.subject)
        .collect();
    if positives.is_empty() {
        return None;
    }
    let idx = usize::try_from(seed)
        .unwrap_or(0)
        .checked_rem(positives.len())
        .unwrap_or(0);
    positives.get(idx).copied()
}

pub(super) fn witness_value_matches_property(
    value: &JsonValue,
    prop: &EffectiveProp<'_>,
    shared: &BTreeMap<&str, &PropertyTypeDef>,
) -> bool {
    if value.is_null() {
        return false;
    }
    let resolved = resolve_property_type(prop.property_type, shared);
    match resolved {
        PropertyType::Boolean {} => matches!(value, JsonValue::Bool(_)),
        PropertyType::Integer { .. } => {
            matches!(value, JsonValue::Number(dec) if dec.to_string().parse::<i64>().is_ok())
        }
        PropertyType::Decimal { .. } => matches!(value, JsonValue::Number(_)),
        PropertyType::Text { .. }
        | PropertyType::Date {}
        | PropertyType::Duration {}
        | PropertyType::Timestamp { .. } => matches!(value, JsonValue::String(_)),
        PropertyType::Enum { values } => {
            matches!(value, JsonValue::String(s) if values.iter().any(|v| v.value == *s))
        }
        PropertyType::Record { .. } => matches!(value, JsonValue::Object(_)),
        PropertyType::JsonExt { .. } => true,
        PropertyType::DecimalBinding { .. } | PropertyType::Ref { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use norn_kernel::cell_host::{JsonObject, JsonValue};
    use norn_kernel::pack_mbt_support::{LawIR, ObjectTypeDef};
    use serde_json::json;

    use crate::pack_mbt::synth::synthesize_object;
    use crate::pack_mbt::{AdmittedPack, LawExampleCase, Verdict};

    #[test]
    fn synthesize_object_seeds_from_positive_law_example_witness() {
        let object_type: ObjectTypeDef = serde_json::from_value(json!({
            "name": "hr.Payslip",
            "properties": [
                { "id": "id", "type": { "kind": "text" } },
                { "id": "base", "type": { "kind": "integer" } },
                { "id": "deduction", "type": { "kind": "integer" } },
                { "id": "net", "type": { "kind": "integer" } }
            ],
            "identity": [{ "name": "primary", "properties": ["id"] }]
        }))
        .expect("payslip type");
        let law: LawIR = serde_json::from_value(json!({
            "lawVersion": "0.1",
            "id": "hr.Payslip.exact_net",
            "owner": "hr",
            "severity": "error",
            "rationale": "net == base - deduction",
            "provenance": { "source": "authoring_sdk", "sourcePath": "hr.ts" },
            "subject": "hr.Payslip",
            "scope": { "root": { "kind": "entity", "id": "hr.Payslip" }, "objectTypes": ["hr.Payslip"] },
            "modality": { "kind": "always" },
            "response": { "kind": "predicate", "expression": "self.net == self.base - self.deduction" }
        }))
        .expect("payslip law");
        let subject: JsonObject = serde_json::from_value(json!({
            "id": "ps_witness",
            "base": 8000,
            "deduction": 500,
            "net": 7500
        }))
        .expect("subject");
        let pack = AdmittedPack {
            name: "hr".to_owned(),
            object_types: vec![object_type.clone()],
            laws: vec![law],
            ..AdmittedPack::default()
        }
        .with_law_examples(vec![LawExampleCase {
            name: "valid payslip".to_owned(),
            law_id: "hr.Payslip.exact_net".to_owned(),
            subject,
            expected: Verdict::Holds,
        }]);

        let synthesized = synthesize_object(&pack, &object_type, "ps_generated_1", 0);
        assert_eq!(
            synthesized.get("id"),
            Some(&JsonValue::String("ps_generated_1".to_owned()))
        );
        assert_eq!(synthesized.get("base"), Some(&JsonValue::from(8000_i64)));
        assert_eq!(
            synthesized.get("deduction"),
            Some(&JsonValue::from(500_i64))
        );
        assert_eq!(synthesized.get("net"), Some(&JsonValue::from(7500_i64)));
    }
}
