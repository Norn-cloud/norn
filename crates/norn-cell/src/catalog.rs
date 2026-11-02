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
    rows: CatalogRows,
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
        let rows = index(&catalog, head.commits)?;
        if store.verified_head()? != head {
            return Err(CatalogReadError::Refused("snapshot changed"));
        }
        Ok(Self {
            rows,
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
        let rows = self
            .rows
            .get(&query.object_type)
            .ok_or(CatalogReadError::Refused("unknown catalog relation"))?;
        let limit = usize::try_from(query.limit)
            .map_err(|_error| CatalogReadError::Refused("invalid limit"))?;
        let mut result = SliceResult {
            items: Vec::new(),
            complete: true,
            commit_seq: self.commit_seq,
        };
        let mut candidate_bytes = 0_u64;
        let mut output_bytes = 0_u64;
        for (bytes, item) in rows.iter().take(limits.max_scan) {
            if result.items.len() >= limit {
                break;
            }
            candidate_bytes = candidate_bytes.saturating_add(*bytes);
            if candidate_bytes > limits.max_candidate_bytes_per_commit {
                result.complete = false;
                break;
            }
            if !query
                .filter
                .iter()
                .all(|(name, value)| item.values.get(name) == Some(value))
            {
                continue;
            }
            let mut access = request.clone();
            access.resource = ResourceInput {
                object_type: query.object_type.clone(),
                id: item.key.clone(),
                scope: scope.clone(),
                attributes: BTreeMap::new(),
            };
            if authz::authorize(pdp, &access).is_err() {
                continue;
            }
            output_bytes = output_bytes.saturating_add(*bytes);
            if output_bytes > limits.max_canonical_bytes {
                result.complete = false;
                break;
            }
            result.items.push(item.clone());
        }
        if result.items.len() < limit && rows.len() > limits.max_scan {
            result.complete = false;
        }
        Ok(result)
    }
}

type CatalogRows = BTreeMap<String, Vec<(u64, SliceItem)>>;
fn index(catalog: &DefinitionCatalog, commit_seq: u64) -> Result<CatalogRows, CatalogReadError> {
    let mut index = BTreeMap::new();
    macro_rules! relations { ($($name:literal => $field:ident),* $(,)?) => { $(
        let mut items = Vec::new();
        for row in &catalog.$field {
            let row = serde_json::to_value(row).map_err(CellError::from)?;
            let key = crate::codec::canonical_json(&identity(&row))?;
            let RuntimeValue::Object(values) = RuntimeValue::from_json(row).map_err(CellError::from)? else {
                return Err(CatalogReadError::Refused("invalid catalog row"));
            };
            let item = SliceItem { object_type:$name.into(), key, version:commit_seq, values };
            let bytes = u64::try_from(crate::codec::canonical_json(&item)?.len()).unwrap_or(u64::MAX);
            items.push((bytes, item));
        }
        items.sort_by(|a, b| a.1.key.cmp(&b.1.key));
        if items.windows(2).any(|rows| matches!(rows, [(_, a), (_, b)] if a.key == b.key)) {
            return Err(CatalogReadError::Refused("ambiguous catalog identity"));
        }
        index.insert($name.into(), items);
    )* }; }
    relations!("norn.catalog.Artifact" => artifacts, "norn.catalog.Type" => types,
        "norn.catalog.Facet" => facets, "norn.catalog.Property" => properties,
        "norn.catalog.Link" => links, "norn.catalog.Transition" => transitions,
        "norn.catalog.State" => states, "norn.catalog.Writes" => writes,
        "norn.catalog.DependsOn" => depends_on, "norn.catalog.Mapping" => mappings,
        "norn.catalog.Law" => laws);
    Ok(index)
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

/// Virtual read descriptors for Cedar projection; never install them as stored types.
///
/// # Errors
/// A descriptor cannot decode as the existing object-type contract.
pub fn resource_types() -> Result<Vec<norn_ir::ObjectTypeDef>, serde_json::Error> {
    "Artifact Type Facet Property Link Transition State Writes DependsOn Mapping Law"
        .split_ascii_whitespace()
        .map(|name| {
            serde_json::from_value(serde_json::json!({"name":format!("norn.catalog.{name}")}))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn_testkit::proptest;
    proptest::proptest! {
        #[test]
        fn exact_member_identity_is_preserved(id in "test.[a-z]{1,12}", version in "[1-9]\\.[0-9]\\.[0-9]", name in "[A-Z][a-z]{0,8}") {
            let at = MemberRef { artifact_id:id, version, member:Member::Type {type_name:format!("test.{name}")} };
            proptest::prop_assert_eq!(identity(&serde_json::json!({"at":at, "role":"thing"})), serde_json::json!(at));
            proptest::prop_assert_eq!(identity(&serde_json::json!({"at":at, "subject":at})), serde_json::json!({"at":at, "subject":at}));
            let mut catalog = norn_checker::definition_catalog::project(&[], &[], &BTreeSet::new()).map_err(|error| proptest::test_runner::TestCaseError::fail(error.to_string()))?;
            catalog.facets = [norn_ir::definition_catalog::ConceptFacet::Props, norn_ir::definition_catalog::ConceptFacet::Access].into_iter().map(|facet| norn_ir::definition_catalog::ConceptFacetRow { concept:at.clone(), facet, member:at.clone() }).collect();
            let indexed = index(&catalog, 0).map_err(|error| proptest::test_runner::TestCaseError::fail(error.to_string()))?;
            proptest::prop_assert_eq!(indexed.get("norn.catalog.Facet").and_then(|rows| rows.first()).and_then(|(_, item)| item.values.get("facet")), Some(&RuntimeValue::String("access".into())));

        }
    }
}
