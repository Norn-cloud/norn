//! OBS-LINK-CONVERGE-1 / OBS-LINK-PENDING-1: retained bundles are the claim journal.
use crate::{
    CellError, MAIN_WORLD, StoreReader,
    engine::{SqlEngine, params},
    mapping_bundle::MappingBundle,
    system::INBOX,
};
use norn_ir::{RuntimeObject, RuntimeValue, mapping::ObservedLinkClaim};
use norn_kernel::{
    CellCommit, CommitModel, CommitView, FactExpectation, FactKey, FactVersion, LinkKey,
    LinkMutation, LinkWrite, ObservationAuthorization, ObservationEvent, Observed, TypeCatalog,
};
use std::collections::{BTreeMap, BTreeSet};

type MappingCommit = CellCommit<ObservationEvent, RuntimeObject, RuntimeObject>;

impl<S: SqlEngine> StoreReader<'_, S> {
    /// Immutable, recomputed source claims published by consumed mapping bundles.
    /// Missing targets remain represented here; callers enforce source disclosure.
    ///
    /// # Errors
    /// Missing/corrupt pins or an exhausted bounded evidence scan.
    pub fn observed_link_claims(&self) -> Result<Vec<ObservedLinkClaim>, CellError> {
        self.retained_link_claims(None, None, false)
    }

    /// Consumed bundles in commit order, within the journal or naming a scoped key.
    /// The claim budgets bound this selection, so unrelated bundles never exhaust them.
    fn claim_bundles(
        &self,
        scope: Option<&BTreeSet<String>>,
        limit: usize,
    ) -> Result<Vec<String>, CellError> {
        let select = "SELECT consumed_commit_seq, observation_id, octet_length(observed)";
        let order = "ORDER BY consumed_commit_seq, observation_id";
        let journal = format!(
            "FROM {INBOX} WHERE observation_id LIKE 'mapping-bundle:%' AND consumed_commit_seq IS NOT NULL"
        );
        let cap = limit + 1;
        let mut rows = Vec::new();
        if let Some(scope) = scope {
            for key in scope {
                // Select actual claim endpoints, never arbitrary raw/property text.
                rows.extend(self.conn.query(
                    &format!("{select} {journal} AND EXISTS (SELECT 1 FROM json_each(observed, '$.input.records') AS record WHERE (json_extract(record.value, '$.key') = ?1 AND json_array_length(json_extract(record.value, '$.clearedLinks')) > 0) OR EXISTS (SELECT 1 FROM json_each(record.value, '$.links') AS link WHERE json_extract(link.value, '$.key') = ?1 OR json_extract(link.value, '$.targetKey') = ?1)) {order} LIMIT {cap}"),
                    params![key],
                )?);
            }
        } else {
            rows = self
                .conn
                .query(&format!("{select} {journal} {order} LIMIT {cap}"), params![])?;
        }
        let mut selected = BTreeMap::new();
        for row in rows {
            let row = crate::engine::SqlRow(row);
            let order = crate::read::to_u64(row.get(0)?, "claim commit sequence")?;
            let length = crate::read::to_u64(row.get(2)?, "claim evidence bytes")?;
            selected.insert((order, row.get::<String>(1)?), length);
        }
        if selected.len() > limit {
            return Err(CellError::OutOfRange("link claim bundles"));
        }
        if selected.values().fold(0u64, |sum, length| sum.saturating_add(*length)) > 4 * 1024 * 1024
        {
            return Err(CellError::OutOfRange("link claim evidence bytes"));
        }
        Ok(selected.into_keys().map(|(_, id)| id).collect())
    }

    fn retained_link_claims(
        &self,
        current: Option<&MappingBundle>,
        scope: Option<&BTreeSet<String>>,
        latest: bool,
    ) -> Result<Vec<ObservedLinkClaim>, CellError> {
        let ids = self.claim_bundles(scope, 1000)?;
        let mut claims = BTreeMap::new();
        let insert = |claims: &mut BTreeMap<String, ObservedLinkClaim>,
                      claim: ObservedLinkClaim|
         -> Result<(), CellError> {
            let id = if latest {
                claim.contribution_id()?
            } else {
                claim.id()?
            };
            claims.insert(id, claim);
            Ok(())
        };
        for id in ids {
            let encoded: String = self
                .conn
                .query_optional(
                    &format!("SELECT observed FROM {INBOX} WHERE observation_id = ?1"),
                    params![&id],
                    |row| row.get(0),
                )?
                .ok_or(CellError::MappingBundleMismatch)?;
            let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
            let bundle: MappingBundle =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            if observed.id.0 != id || id != format!("mapping-bundle:{}", bundle.digest) {
                return Err(CellError::MappingBundleMismatch);
            }
            self.recheck_mapping_bundle(&bundle)?;
            if latest && self.mapping_event_is_disputed(&bundle)? {
                continue;
            }
            for claim in bundle.link_claims()? {
                insert(&mut claims, claim)?;
            }
        }
        if let Some(bundle) = current {
            for claim in bundle.link_claims()? {
                insert(&mut claims, claim)?;
            }
        }
        if claims.len() > 20_000 {
            return Err(CellError::OutOfRange("observed link claims"));
        }
        Ok(claims.into_values().collect())
    }

    /// Retained claims about one canonical link identity; membership is read separately.
    ///
    /// # Errors
    /// The same evidence/pin/budget errors as `observed_link_claims`.
    pub fn observed_link_provenance(
        &self,
        key: &LinkKey,
    ) -> Result<Vec<ObservedLinkClaim>, CellError> {
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces());
        let model = CommitModel::new(&catalog);
        let key = model.canonical_link(key);
        let scope = BTreeSet::from([key.source.key.clone(), key.target.key.clone()]);
        Ok(self
            .retained_link_claims(None, Some(&scope), false)?
            .into_iter()
            .filter(|claim| {
                claim.target.as_ref().is_some_and(|target| {
                    model.canonical_link(&LinkKey::new(
                        FactKey::new(claim.source_type.as_str(), &claim.source_key),
                        claim.role.as_str(),
                        FactKey::new(target.object_type.as_str(), &target.key),
                    )) == key
                })
            })
            .collect())
    }

    pub(crate) fn mapping_link_plan<E, C, T>(
        &self,
        bundle: &MappingBundle,
        commit: &CellCommit<E, C, T>,
    ) -> Result<(Vec<LinkWrite>, Vec<ObservedLinkClaim>), CellError> {
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces())
            .with_property_types(self.model().property_types());
        // A plan changes only links touching this commit's objects or this bundle's claims.
        let current = bundle.link_claims()?;
        let scope: BTreeSet<String> = bundle
            .records
            .iter()
            .map(|record| record.key.clone())
            .chain(current.iter().map(|claim| claim.source_key.clone()))
            .chain(current.iter().filter_map(|c| c.target.as_ref().map(|t| t.key.clone())))
            .collect();
        let mut claims = self.retained_link_claims(Some(bundle), Some(&scope), true)?;
        let model = CommitModel::new(&catalog);
        let mut intents = BTreeMap::<_, BTreeSet<_>>::new();
        let mut latest = BTreeMap::new();
        for claim in &claims {
            let source = FactKey::new(claim.source_type.as_str(), &claim.source_key);
            let target = claim
                .target
                .as_ref()
                .map(|t| model.canonical(&FactKey::new(t.object_type.as_str(), &t.key)));
            let targets = intents
                .entry((model.canonical(&source), claim.role.clone()))
                .or_default();
            latest.insert(claim.contribution_id()?, target.clone());
            targets.insert(target);
            if targets.len() > 1
                && model
                    .link(&source.object_type, claim.role.as_str())
                    .is_some_and(|link| link.cardinality.to == norn_ir::Multiplicity::One)
            {
                return Err(CellError::UnsupportedMappingProfile);
            }
        }
        let present = |key: &FactKey| {
            model
                .contains_after(commit, &crate::host::GuardView(self), key)
                .map_err(|e| CellError::Encoding(e.to_string()))
        };
        let mut links = BTreeMap::new();
        for claim in &claims {
            let source = FactKey::new(claim.source_type.as_str(), &claim.source_key);
            if !present(&source)? {
                continue;
            }
            if let Some(target) = &claim.target {
                let target = FactKey::new(target.object_type.as_str(), &target.key);
                if !present(&target)? {
                    continue;
                }
                let key = LinkKey::new(source, claim.role.as_str(), target);
                if !self.mapping_link_exists(&key, model)? {
                    let values = model
                        .effective_link_values(
                            commit,
                            &crate::host::GuardView(self),
                            &key,
                            &RuntimeObject::new(),
                        )
                        .map_err(|e| CellError::Encoding(e.to_string()))?;
                    links.insert(
                        model.canonical_link(&key),
                        self.mapping_link_write(key, LinkMutation::Add { values })?,
                    );
                }
            }
        }
        for claim in self.retained_link_claims(None, Some(&scope), false)? {
            let Some(target) = &claim.target else {
                continue;
            };
            let key = LinkKey::new(
                FactKey::new(claim.source_type.as_str(), &claim.source_key),
                claim.role.as_str(),
                FactKey::new(target.object_type.as_str(), &target.key),
            );
            let canonical = model.canonical_link(&key);
            let Some(next) = latest.get(&claim.contribution_id()?) else {
                continue;
            };
            if next.as_ref() == Some(&canonical.target)
                || intents
                    .get(&(canonical.source.clone(), claim.role.clone()))
                    .is_none_or(|targets| targets.contains(&Some(canonical.target.clone())))
                || !self.mapping_link_exists(&key, model)?
            {
                continue;
            }
            links.insert(
                canonical,
                self.mapping_link_write(key, LinkMutation::Remove)?,
            );
            claims.push(claim);
        }
        let mut links: Vec<_> = links.into_values().collect();
        links.sort_by_key(|w| matches!(&w.mutation, LinkMutation::Add { .. }));
        Ok((links, claims))
    }

    fn mapping_link_exists(
        &self,
        key: &LinkKey,
        model: CommitModel<'_, '_>,
    ) -> Result<bool, CellError> {
        Ok(crate::host::GuardView(self)
            .links_of(&key.source)?
            .iter()
            .any(|live| model.canonical_link(live) == model.canonical_link(key)))
    }

    fn mapping_link_write(
        &self,
        key: LinkKey,
        mutation: LinkMutation,
    ) -> Result<LinkWrite, CellError> {
        Ok(LinkWrite {
            expected: FactVersion(self.link_version(
                MAIN_WORLD,
                key.source.object_type.as_str(),
                &key.link,
                &key.source.key,
                key.target.object_type.as_str(),
                &key.target.key,
            )?),
            key,
            mutation,
        })
    }

    pub(crate) fn prepare_mapping_links(
        &self,
        bundle: &MappingBundle,
        catalog: &TypeCatalog<'_>,
        turn: &norn_kernel::DecisionTurn,
        commit: &mut MappingCommit,
        authority: &mut ObservationAuthorization,
    ) -> Result<(), CellError> {
        for claim in bundle.link_claims()? {
            self.prepare_claim_snapshot(catalog, turn, &claim, commit, authority)?;
        }
        let (links, claims) = self.mapping_link_plan(bundle, commit)?;
        commit.links = links;
        let model = CommitModel::new(catalog);
        for claim in claims {
            let source = FactKey::new(claim.source_type.as_str(), &claim.source_key);
            if !commit.links.iter().any(|w| {
                model.canonical(&w.key.source) == model.canonical(&source)
                    && w.key.link == claim.role.as_str()
            }) {
                continue;
            }
            self.prepare_claim_snapshot(catalog, turn, &claim, commit, authority)?;
            let expected =
                FactVersion(self.version(MAIN_WORLD, source.object_type.as_str(), &source.key)?);
            let read = FactExpectation {
                key: source.clone(),
                expected,
            };
            if !commit.writes.iter().any(|w| w.key == source) && !commit.reads.contains(&read) {
                commit.reads.push(read.clone());
            }
            if let Some(target) = &claim.target {
                let key = FactKey::new(target.object_type.as_str(), &target.key);
                if !commit
                    .writes
                    .iter()
                    .any(|w| model.canonical(&w.key) == model.canonical(&key))
                {
                    let read = FactExpectation {
                        expected: FactVersion(self.version(
                            MAIN_WORLD,
                            key.object_type.as_str(),
                            &key.key,
                        )?),
                        key,
                    };
                    if !commit.reads.contains(&read) {
                        commit.reads.push(read);
                    }
                }
            }
            authority
                .seal_observed_link_claim(catalog, &claim, read, commit)
                .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        }
        authority
            .seal_link_reference_reads(commit)
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        Ok(())
    }
    fn prepare_claim_snapshot(
        &self,
        catalog: &TypeCatalog<'_>,
        turn: &norn_kernel::DecisionTurn,
        claim: &ObservedLinkClaim,
        commit: &mut MappingCommit,
        authority: &mut ObservationAuthorization,
    ) -> Result<(), CellError> {
        let key = FactKey::new(norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE, claim.id()?);
        if commit.writes.iter().any(|w| w.key == key) || commit.reads.iter().any(|r| r.key == key) {
            return Ok(());
        }
        let slice = self.mapping_observation_slice(&key)?;
        norn_kernel::snapshot_observe::prepare_observed_link_claim_snapshot(
            catalog, &slice, turn, claim,
        )
        .and_then(|prepared| prepared.append_to(commit, authority))
        .map_err(|e| CellError::ObservationPreparation(Box::new(e)))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "inbox selection fixture")]
mod tests {
    use super::*;
    use crate::{CellStore, Model};

    fn consumed(store: &CellStore, position: i64, key: &str) {
        let id = format!("mapping-bundle:{position}");
        let observed = serde_json::json!({ "records": [{ "key": key }] }).to_string();
        store
            .conn
            .execute(
                &format!(
                    "INSERT INTO {INBOX} (position, observation_id, observed, consumed_commit_seq) VALUES (?1, ?2, ?3, ?1)"
                ),
                params![position, &id, &observed],
            )
            .expect("consumed bundle");
    }

    #[test]
    fn obs_link_budget_bounds_the_scoped_claim_set_not_the_journal() {
        let store = CellStore::open_in_memory(Model::from_definitions_with_interfaces(&[], &[], &[]).expect("model")).expect("store");
        consumed(&store, 1, "demo/\"quoted\"");
        for position in 2..=1002 {
            consumed(&store, position, &format!("demo/other/{position}"));
        }
        consumed(&store, 1003, "demo/\"quoted\"");
        let reader = store.read();
        let scope = BTreeSet::from(["demo/\"quoted\"".to_owned()]);
        assert_eq!(
            reader.claim_bundles(Some(&scope), 1000).expect("scoped"),
            ["mapping-bundle:1", "mapping-bundle:1003"],
        );
        for refused in [reader.claim_bundles(None, 1000), reader.claim_bundles(Some(&scope), 1)] {
            assert!(matches!(
                refused,
                Err(CellError::OutOfRange("link claim bundles"))
            ));
        }
    }
}
