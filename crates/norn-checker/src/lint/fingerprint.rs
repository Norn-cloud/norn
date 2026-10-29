//! Structural fingerprints of object types (T8-LINT-2), the deterministic
//! basis of the smells and of nearest-concept retrieval (T8-SIM-1 #2604).
//!
//! A fingerprint has two views of one object type:
//!
//! - its **shape**: the sorted multiset of member features with the member
//!   ids removed. A property contributes `property:` and the canonical JSON
//!   of its type and constraints; a link contributes `link:`, its target,
//!   and the canonical JSON of its cardinality. Names, descriptions,
//!   defaults, and computed expressions are not part of the shape. The
//!   [`StructuralFingerprint::digest`] is the hex SHA-256 of the shape, so
//!   two types with the same members under different ids share a digest.
//! - its **member names**: every own property and link id, normalized to
//!   lower-case ASCII letters and digits (`Customer_Name` and `customername`
//!   are one name), the view the source-mirroring smell compares with source
//!   fields.
//!
//! [`shape_overlap`] is the deterministic similarity of two shapes: the size
//! of their multiset intersection and of their multiset union, as integers.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::ObjectTypeDef;
use sha2::{Digest as _, Sha256};

/// Domain separator of the shape digest (bumped if the feature encoding
/// changes).
const SHAPE_DOMAIN: &[u8] = b"norn-lint-shape/v1\n";

/// The structural fingerprint of one object type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralFingerprint {
    /// The member features with ids removed, sorted (a multiset).
    pub shape: Vec<String>,
    /// Hex SHA-256 of the domain separator and the shape, one feature per
    /// line.
    pub digest: String,
    /// The normalized own member ids (see [`normalize_name`]).
    pub member_names: BTreeSet<String>,
}

/// The structural fingerprint of `def`'s own members (inherited members are
/// the supertype's fingerprint).
#[must_use]
pub fn fingerprint(def: &ObjectTypeDef) -> StructuralFingerprint {
    let properties = def.properties.iter().map(|property| {
        let mut shape = serde_json::json!({
            "type": serde_json::to_value(&property.property_type).ok(),
            "constraints": serde_json::to_value(&property.constraints).ok(),
        });
        super::duplicates::strip_descriptions(&mut shape);
        format!("property:{}", canonical(&shape))
    });
    let links = def.links.iter().map(|link| {
        let cardinality = serde_json::to_value(link.cardinality)
            .ok()
            .map_or_else(String::new, |value| canonical(&value));
        format!("link:{}:{cardinality}", link.target.as_str())
    });
    let mut shape: Vec<String> = properties.chain(links).collect();
    shape.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(SHAPE_DOMAIN);
    for feature in &shape {
        hasher.update(feature.as_bytes());
        hasher.update(b"\n");
    }
    let digest = format!("{:x}", hasher.finalize());

    let member_names = def
        .properties
        .iter()
        .map(|property| property.id.as_str())
        .chain(def.links.iter().map(|link| link.id.as_str()))
        .map(normalize_name)
        .collect();
    StructuralFingerprint {
        shape,
        digest,
        member_names,
    }
}

/// A name reduced to lower-case ASCII letters and digits, the form member
/// ids and source field names are compared in (`Customer Name`,
/// `customer_name`, and `customerName` are all `customername`).
#[must_use]
pub fn normalize_name(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The overlap of two shapes as `(shared, union)` multiset sizes: equal
/// shapes give `shared == union`; disjoint shapes give `shared == 0`.
#[must_use]
pub fn shape_overlap(
    left: &StructuralFingerprint,
    right: &StructuralFingerprint,
) -> (usize, usize) {
    fn counts(shape: &[String]) -> BTreeMap<&str, usize> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for feature in shape {
            let count = counts.entry(feature.as_str()).or_default();
            *count = count.saturating_add(1);
        }
        counts
    }
    let left = counts(&left.shape);
    let right = counts(&right.shape);
    let features: BTreeSet<&str> = left.keys().chain(right.keys()).copied().collect();
    features
        .into_iter()
        .fold((0_usize, 0_usize), |(shared, union), feature| {
            let l = left.get(feature).copied().unwrap_or_default();
            let r = right.get(feature).copied().unwrap_or_default();
            (
                shared.saturating_add(l.min(r)),
                union.saturating_add(l.max(r)),
            )
        })
}

/// Canonical JSON text (sorted keys, nulls pruned) of `value`, or the empty
/// string if it does not canonicalize.
fn canonical(value: &serde_json::Value) -> String {
    norn_canonical::legacy_canonicalize_value(value)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::panic, reason = "test fixture decoding reports its error")]
mod tests {
    use super::*;

    fn object_type(value: serde_json::Value) -> ObjectTypeDef {
        serde_json::from_value(value).unwrap_or_else(|error| panic!("object type: {error}"))
    }

    #[test]
    fn renamed_members_share_a_shape_digest() {
        let customer = object_type(serde_json::json!({
            "name": "crm.Customer",
            "properties": [
                { "id": "display_name", "type": { "kind": "text" } },
                { "id": "credit_limit", "type": { "kind": "decimal", "scale": 2 } }
            ],
            "links": [{ "id": "region", "target": "geo.Region",
                        "cardinality": { "to": "one", "from": "many" } }]
        }));
        let client = object_type(serde_json::json!({
            "name": "crm.Client",
            "description": "renamed copy",
            "properties": [
                { "id": "limit", "type": { "kind": "decimal", "scale": 2 } },
                { "id": "label", "type": { "kind": "text" } }
            ],
            "links": [{ "id": "area", "target": "geo.Region",
                        "cardinality": { "to": "one", "from": "many" } }]
        }));
        let (a, b) = (fingerprint(&customer), fingerprint(&client));
        assert_eq!(a.digest, b.digest);
        assert_eq!(a.digest.len(), 64);
        assert_eq!(shape_overlap(&a, &b), (3, 3));
        assert!(a.member_names.contains("displayname"));

        let other = object_type(serde_json::json!({
            "name": "crm.Note",
            "properties": [{ "id": "body", "type": { "kind": "text" } }]
        }));
        let c = fingerprint(&other);
        assert_ne!(a.digest, c.digest);
        assert_eq!(shape_overlap(&a, &c), (1, 3));
    }

    #[test]
    fn names_normalize_to_lower_alphanumerics() {
        assert_eq!(normalize_name("Customer Name"), "customername");
        assert_eq!(normalize_name("customer_name"), "customername");
        assert_eq!(normalize_name("customerName2"), "customername2");
    }

    #[test]
    fn shape_ignores_enum_documentation_and_keeps_constraints_and_multiplicity() {
        let plain = serde_json::json!({
            "name": "crm.Customer",
            "properties": [{
                "id": "status", "type": {
                    "kind": "enum", "values": [{"value": "active"}]
                }
            }]
        });
        let a = fingerprint(&object_type(plain.clone()));
        let mut documented = plain.clone();
        if let Some(value) = documented
            .pointer_mut("/properties/0/type/values/0")
            .and_then(serde_json::Value::as_object_mut)
        {
            value.insert(
                "description".to_owned(),
                serde_json::json!("Active customer"),
            );
        }
        assert_eq!(a.digest, fingerprint(&object_type(documented)).digest);

        let mut constrained = plain.clone();
        if let Some(value) = constrained
            .pointer_mut("/properties/0")
            .and_then(serde_json::Value::as_object_mut)
        {
            value.insert(
                "constraints".to_owned(),
                serde_json::json!({"nullable": true}),
            );
        }
        assert_ne!(a.digest, fingerprint(&object_type(constrained)).digest);

        let mut repeated = plain;
        let second = serde_json::json!({
            "id": "category", "type": {"kind": "enum", "values": [{"value": "active"}]}
        });
        if let Some(properties) = repeated
            .get_mut("properties")
            .and_then(serde_json::Value::as_array_mut)
        {
            properties.push(second);
        }
        let b = fingerprint(&object_type(repeated));
        assert_ne!(a.digest, b.digest);
        assert_eq!(shape_overlap(&a, &b), (1, 2));
        assert_eq!(shape_overlap(&b, &a), (1, 2));
    }
}
