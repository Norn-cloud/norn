//! OBS-LINK-CONVERGE-1 / OBS-LINK-PENDING-1: retained bundles are the claim journal.
use crate::{
    CellError, MAIN_WORLD, StoreReader,
    engine::{SqlEngine, params},
    mapping_bundle::MappingBundle,
    system::INBOX,
};
use norn_ir::{RuntimeObject, RuntimeValue, mapping::ObservedLinkClaim};
use norn_kernel::{
    CellCommit, CommitModel, FactExpectation, FactKey, FactMutation, FactVersion, LinkKey,
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
        self.retained_link_claims(None, false)
    }

    fn retained_link_claims(
        &self,
        current: Option<&MappingBundle>,
        latest: bool,
    ) -> Result<Vec<ObservedLinkClaim>, CellError> {
        let lengths = self.conn.query(&format!("SELECT observation_id, octet_length(observed) FROM {INBOX} WHERE observation_id LIKE 'mapping-bundle:%' AND consumed_commit_seq IS NOT NULL ORDER BY consumed_commit_seq, observation_id LIMIT 1001"), params![])?;
        if lengths.len() > 1000 {
            return Err(CellError::OutOfRange("link claim bundles"));
        }
        let mut bytes = 0u64;
        let mut ids = Vec::new();
        for row in lengths {
            let row = crate::engine::SqlRow(row);
            bytes = bytes.saturating_add(crate::read::to_u64(row.get(1)?, "claim evidence bytes")?);
            if bytes > 4 * 1024 * 1024 {
                return Err(CellError::OutOfRange("link claim evidence bytes"));
            }
            ids.push(row.get::<String>(0)?);
        }
        let mut claims = BTreeMap::new();
        let insert = |claims: &mut BTreeMap<String, ObservedLinkClaim>,
                      claim: ObservedLinkClaim|
         -> Result<(), CellError> {
            let id = if latest {
                let mapping = claim.provenance.mapping_version.as_str();
                let mapping_id = mapping.split_once('@').map_or(mapping, |(id, _)| id);
                norn_ir::canonical::domain_hash_hex(
                    b"norn.link.contribution.v1\0",
                    &(
                        &claim.source_type,
                        &claim.source_key,
                        &claim.role,
                        &claim.provenance.source,
                        mapping_id,
                    ),
                )?
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
            for record in &bundle.records {
                for claim in record.link_claims(&id) {
                    insert(&mut claims, claim)?;
                }
            }
        }
        if let Some(bundle) = current {
            let id = format!("mapping-bundle:{}", bundle.digest);
            for record in &bundle.records {
                for claim in record.link_claims(&id) {
                    insert(&mut claims, claim)?;
                }
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
        Ok(self
            .observed_link_claims()?
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
        let claims = self.retained_link_claims(Some(bundle), true)?;
        let model = CommitModel::new(&catalog);
        let mut intents = BTreeMap::<_, BTreeSet<_>>::new();
        for claim in &claims {
            let source = FactKey::new(claim.source_type.as_str(), &claim.source_key);
            let target = claim
                .target
                .as_ref()
                .map(|t| model.canonical(&FactKey::new(t.object_type.as_str(), &t.key)));
            let targets = intents
                .entry((model.canonical(&source), claim.role.clone()))
                .or_default();
            targets.insert(target);
            if targets.len() > 1
                && model
                    .link(&source.object_type, claim.role.as_str())
                    .is_some_and(|link| link.cardinality.to == norn_ir::Multiplicity::One)
            {
                return Err(CellError::UnsupportedMappingProfile);
            }
        }
        let present = |key: &FactKey| -> Result<bool, CellError> {
            if let Some(write) = commit
                .writes
                .iter()
                .find(|w| model.canonical(&w.key) == model.canonical(key))
            {
                return Ok(matches!(write.mutation, FactMutation::Put { .. }));
            }
            Ok(self
                .object(MAIN_WORLD, key.object_type.as_str(), &key.key)?
                .is_some())
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
                if self
                    .link(
                        MAIN_WORLD,
                        key.source.object_type.as_str(),
                        &key.link,
                        &key.source.key,
                        &key.target.key,
                    )?
                    .is_none()
                {
                    let expected = FactVersion(self.link_version(
                        MAIN_WORLD,
                        key.source.object_type.as_str(),
                        &key.link,
                        &key.source.key,
                        key.target.object_type.as_str(),
                        &key.target.key,
                    )?);
                    links.insert(
                        model.canonical_link(&key),
                        LinkWrite {
                            key,
                            expected,
                            mutation: LinkMutation::Add {
                                values: catalog
                                    .effective_link_values(
                                        &claim.source_type,
                                        claim.role.as_str(),
                                        &RuntimeObject::new(),
                                    )
                                    .map_err(|e| CellError::Encoding(e.to_string()))?,
                            },
                        },
                    );
                }
            } else {
                for old in
                    self.links_touching(MAIN_WORLD, source.object_type.as_str(), &source.key)?
                {
                    if model.canonical(&FactKey::new(&old.source_type, &old.source))
                        != model.canonical(&source)
                        || old.link != claim.role.as_str()
                    {
                        continue;
                    }
                    let key = LinkKey::new(
                        source.clone(),
                        claim.role.as_str(),
                        FactKey::new(old.target_type, old.target),
                    );
                    let expected = FactVersion(self.link_version(
                        MAIN_WORLD,
                        key.source.object_type.as_str(),
                        &key.link,
                        &key.source.key,
                        key.target.object_type.as_str(),
                        &key.target.key,
                    )?);
                    links.insert(
                        model.canonical_link(&key),
                        LinkWrite {
                            key,
                            expected,
                            mutation: LinkMutation::Remove,
                        },
                    );
                }
            }
        }
        Ok((links.into_values().collect(), claims))
    }

    pub(crate) fn prepare_mapping_links(
        &self,
        bundle: &MappingBundle,
        catalog: &TypeCatalog<'_>,
        turn: &norn_kernel::DecisionTurn,
        commit: &mut MappingCommit,
        authority: &mut ObservationAuthorization,
    ) -> Result<(), CellError> {
        for claim in bundle
            .records
            .iter()
            .flat_map(|record| record.link_claims(&turn.id))
        {
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
