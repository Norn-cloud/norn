//! GRAPH-META-1: ordinary bounded, kernel-authorized reads of virtual catalog rows.
use crate::engine::SqlEngine;
use crate::reactive::{ReactiveLimits, SliceItem, SliceQuery, SliceResult};
use crate::{CellError, CellStore};
use norn_ir::definition_catalog::{DefinitionCatalog, Member, MemberRef};
use norn_ir::{ArtifactEnvelope, PropertyTypeName, RuntimeValue};
use norn_kernel::authz::{self, AccessRequest, Pdp, ResourceInput, ScopeKey};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One immutable projection of installed, admitted definitions.
pub struct CatalogSnapshot {
    catalog: DefinitionCatalog,
    commit_seq: u64,
}

/// Catalog read refusal; there is no catalog mutation interface.
#[derive(Debug, thiserror::Error)]
pub enum CatalogReadError {
    /// Unknown relation, unsupported binding, wrong access action, or over-profile query.
    #[error("catalog read refused: {0}")]
    Refused(&'static str),
    /// Installed definitions or their projection could not be read.
    #[error(transparent)]
    Store(#[from] CellError),
    /// A catalog value is not exact canonical runtime data.
    #[error(transparent)]
    Value(#[from] norn_ir::RuntimeValueError),
}

impl CatalogSnapshot {
    /// Project active installed definitions at one verified revision.
    ///
    /// # Errors
    /// Missing/corrupt definitions or a concurrent commit.
    pub fn read_active<E: SqlEngine>(
        store: &CellStore<E>,
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Result<Self, CatalogReadError> {
        let head = store.verified_head()?;
        let reader = store.read();
        let installed = reader
            .installed_definitions(None)?
            .ok_or(CatalogReadError::Refused("no active definitions"))?;
        if installed.model_hash != reader.model().model_hash() {
            return Err(CatalogReadError::Refused("active model mismatch"));
        }
        let artifacts: Vec<ArtifactEnvelope> =
            serde_json::from_str(&installed.canonical_set).map_err(CellError::from)?;
        let catalog = norn_checker::definition_catalog::project(&[], &artifacts, registry)
            .map_err(|_error| CatalogReadError::Refused("catalog projection failed"))?;
        if store.verified_head()? != head {
            return Err(CatalogReadError::Refused("snapshot changed"));
        }
        Ok(Self {
            catalog,
            commit_seq: head.commits,
        })
    }

    /// Read virtual rows with `norn.read` and kernel PDP in the trusted cell scope.
    /// Member keys preserve canonical `MemberRef`; edge keys preserve endpoints.
    ///
    /// # Errors
    /// Unsupported/over-profile queries refuse; exhausted bounds mark incomplete.
    pub fn read(
        &self,
        query: &SliceQuery,
        limits: &ReactiveLimits,
        pdp: &Pdp,
        request: &AccessRequest,
        scope: &ScopeKey,
    ) -> Result<SliceResult, CatalogReadError> {
        if query.binding.is_some()
            || query.limit > limits.max_records
            || request.action_id != "norn.read"
            || request.capability != "norn.read"
            || request.scope.as_ref() != Some(scope)
            || request.binding.is_some()
        {
            return Err(CatalogReadError::Refused("bounded norn.read required"));
        }
        let rows = self.rows(&query.object_type)?;
        let mut result = SliceResult {
            items: Vec::new(),
            complete: rows.len() <= limits.max_scan,
            commit_seq: self.commit_seq,
        };
        let mut candidate_bytes = 0_u64;
        let mut output_bytes = 0_u64;
        for row in rows.take(limits.max_scan) {
            if result.items.len()
                >= usize::try_from(query.limit)
                    .map_err(|_error| CatalogReadError::Refused("invalid limit"))?
            {
                break;
            }
            let row = row.map_err(CellError::from)?;
            let id = identity(&row);
            let key = String::from_utf8(RuntimeValue::from_json(id)?.canonical_json()?)
                .map_err(|_error| CatalogReadError::Refused("invalid canonical identity"))?;
            let value = RuntimeValue::from_json(row)?;
            candidate_bytes = candidate_bytes
                .saturating_add(u64::try_from(value.canonical_json()?.len()).unwrap_or(u64::MAX));
            if candidate_bytes > limits.max_candidate_bytes_per_commit {
                result.complete = false;
                break;
            }
            let RuntimeValue::Object(values) = value else {
                return Err(CatalogReadError::Refused("invalid catalog row"));
            };
            if !query
                .filter
                .iter()
                .all(|(name, value)| values.get(name) == Some(value))
            {
                continue;
            }
            let mut access = request.clone();
            access.resource = ResourceInput {
                object_type: query.object_type.clone(),
                id: key.clone(),
                scope: scope.clone(),
                attributes: BTreeMap::new(),
            };
            if authz::authorize(pdp, &access).is_err() {
                continue;
            }
            let item = SliceItem {
                object_type: query.object_type.clone(),
                key,
                version: self.commit_seq,
                values,
            };
            output_bytes = output_bytes.saturating_add(
                u64::try_from(crate::codec::canonical_json(&item)?.len()).unwrap_or(u64::MAX),
            );
            if output_bytes > limits.max_canonical_bytes {
                result.complete = false;
                break;
            }
            result.items.push(item);
        }
        Ok(result)
    }

    fn rows(&self, name: &str) -> Result<Rows<'_>, CatalogReadError> {
        macro_rules! relation { ($($name:literal => $field:ident),* $(,)?) => { match name {
            $($name => Ok(Box::new(self.catalog.$field.iter().map(serde_json::to_value))) ,)*
            _ => Err(CatalogReadError::Refused("unknown catalog relation")),
        } }; }
        relation!("norn.catalog.Artifact" => artifacts, "norn.catalog.Type" => types,
            "norn.catalog.Facet" => facets, "norn.catalog.Property" => properties,
            "norn.catalog.Link" => links, "norn.catalog.Transition" => transitions,
            "norn.catalog.State" => states, "norn.catalog.Writes" => writes,
            "norn.catalog.DependsOn" => depends_on, "norn.catalog.Mapping" => mappings,
            "norn.catalog.Law" => laws)
    }
}

fn identity(row: &Value) -> Value {
    if row.get("subject").is_some() {
        return row.clone();
    }
    if let Some(at) = row.get("at") {
        return at.clone();
    }
    if serde_json::from_value::<MemberRef>(row.clone()).is_ok() {
        return row.clone();
    }
    match (row.get("artifactId"), row.get("version")) {
        (Some(Value::String(id)), Some(Value::String(version))) => serde_json::json!(MemberRef {
            artifact_id: id.clone(),
            version: version.clone(),
            member: Member::Artifact {}
        }),
        _ => row.clone(),
    }
}

type Rows<'a> = Box<dyn ExactSizeIterator<Item = Result<Value, serde_json::Error>> + 'a>;

/// Virtual read descriptors for Cedar projection; never install them as stored types.
///
/// # Errors
/// A descriptor cannot decode as the existing object-type contract.
pub fn resource_types() -> Result<Vec<norn_ir::ObjectTypeDef>, serde_json::Error> {
    [
        "Artifact",
        "Type",
        "Facet",
        "Property",
        "Link",
        "Transition",
        "State",
        "Writes",
        "DependsOn",
        "Mapping",
        "Law",
    ]
    .into_iter()
    .map(|name| serde_json::from_value(serde_json::json!({"name":format!("norn.catalog.{name}")})))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    proptest::proptest! {
        #[test]
        fn exact_member_identity_is_preserved(id in "test.[a-z]{1,12}", version in "[1-9]\\.[0-9]\\.[0-9]", name in "[A-Z][a-z]{0,8}") {
            let at = MemberRef { artifact_id:id, version, member:Member::Type {type_name:format!("test.{name}")} };
            proptest::prop_assert_eq!(identity(&serde_json::json!({"at":at.clone(), "role":"thing"})), serde_json::json!(at.clone()));
            proptest::prop_assert_eq!(identity(&serde_json::json!({"at":at.clone(), "subject":at.clone()})), serde_json::json!({"at":at.clone(), "subject":at}));
        }
    }
}
