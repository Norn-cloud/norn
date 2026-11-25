//! The wire encoding of [`Role`] (`super::concept_role::ConceptRole`).
//!
//! `concept_role.rs` is also compiled on its own by a standalone Kani
//! harness, where no derive macro crate exists. Its encoding is therefore
//! derived here, on a private mirror with the same name and variants, and
//! the role delegates to it. The exhaustive conversions below keep the two
//! in step: a variant added to one without the other does not compile.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::concept_role::ConceptRole as Role;

/// Semantic role of an ordinary concept, independent of lifecycle presence.
#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ConceptRole {
    /// A persistent thing.
    Thing,
    /// A concept that occurs, runs or terminates in time.
    Happening,
}

impl From<Role> for ConceptRole {
    fn from(role: Role) -> Self {
        match role {
            Role::Thing => Self::Thing,
            Role::Happening => Self::Happening,
        }
    }
}

impl From<ConceptRole> for Role {
    fn from(role: ConceptRole) -> Self {
        match role {
            ConceptRole::Thing => Self::Thing,
            ConceptRole::Happening => Self::Happening,
        }
    }
}

impl Serialize for Role {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ConceptRole::from(*self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        ConceptRole::deserialize(deserializer).map(Self::from)
    }
}

impl JsonSchema for Role {
    fn inline_schema() -> bool {
        ConceptRole::inline_schema()
    }

    fn schema_name() -> Cow<'static, str> {
        ConceptRole::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        ConceptRole::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        ConceptRole::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::Role;

    /// #1723: an item compiled out under `cfg(kani)` breaks every Kani
    /// harness that depends on the crate, before the model checker starts.
    /// The crates those harnesses build never condition source on the
    /// absence of that cfg.
    #[test]
    fn no_source_is_compiled_out_under_cfg_kani() -> std::io::Result<()> {
        let needle = concat!("not(", "kani)");
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut pending: Vec<PathBuf> = ["norn-ir", "norn-expr", "norn-checker"]
            .iter()
            .map(|name| crates.join(name).join("src"))
            .collect();
        let mut scanned = 0_usize;
        let mut offenders = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory)? {
                let path = entry?.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    scanned = scanned.saturating_add(1);
                    if std::fs::read_to_string(&path)?.contains(needle) {
                        offenders.push(path);
                    }
                }
            }
        }
        assert!(scanned > 0, "no source was scanned");
        assert!(offenders.is_empty(), "compiled out under cfg(kani): {offenders:?}");
        Ok(())
    }

    #[test]
    fn the_role_keeps_its_snake_case_wire_spelling() -> Result<(), serde_json::Error> {
        for (role, wire) in [(Role::Thing, "\"thing\""), (Role::Happening, "\"happening\"")] {
            assert_eq!(serde_json::to_string(&role)?, wire);
            assert_eq!(serde_json::from_str::<Role>(wire)?, role);
        }
        assert!(serde_json::from_str::<Role>("\"Thing\"").is_err());
        let schema = serde_json::to_value(schemars::schema_for!(Role))?;
        assert_eq!(schema["title"], "ConceptRole");
        let spellings: Vec<_> = schema["oneOf"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|variant| variant["const"].clone())
            .collect();
        assert_eq!(spellings, ["thing", "happening"]);
        Ok(())
    }
}
