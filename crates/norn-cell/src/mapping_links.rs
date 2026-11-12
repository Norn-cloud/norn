//! OBS-LINK-CONVERGE-1 / OBS-LINK-CONTRIBUTION-1 / OBS-LINK-PENDING-1:
//! retained bundles are the claim journal.
use crate::{
    CellError, MAIN_WORLD, StoreReader,
    engine::{SqlEngine, params},
    host::GuardView,
    mapping_bundle::MappingBundle,
    system::INBOX,
};
use norn_ir::{RuntimeObject, RuntimeValue, mapping::ObservedLinkClaim};
use norn_kernel::{
    CellCommit, CommitModel, CommitView, FactExpectation, FactKey, FactMutation, FactVersion,
    LinkKey, LinkMutation, LinkWrite, ObservationAuthorization, ObservationEvent, Observed,
    TypeCatalog,
};
use std::collections::{BTreeMap, BTreeSet};

type MappingCommit = CellCommit<ObservationEvent, RuntimeObject, RuntimeObject>;
const MAX_CLAIM_BUNDLES: usize = 1000;
const MAX_CLAIM_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CLAIMS: usize = 20_000;

/// One journal claim with its recomputed recorded order.
struct Entry {
    claim: ObservedLinkClaim,
    id: String,
    contribution: String,
    fresh: bool,
    disputed: bool,
}

/// The kernel's per-contribution standing, recomputed over a journal group.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    Current,
    Superseded,
    Contested,
}

/// Edge changes for one bundle and the claims that must back them.
pub(crate) struct LinkPlan {
    pub(crate) links: Vec<LinkWrite>,
    fresh: Vec<ObservedLinkClaim>,
    sealed: Vec<ObservedLinkClaim>,
}

/// The keys a journal read selects on, by the side of a claim they match.
#[derive(Clone, Default)]
pub(crate) struct ClaimScope {
    /// Claims whose source is one of these.
    pub(crate) sources: BTreeSet<String>,
    /// Claims whose target is one of these.
    pub(crate) targets: BTreeSet<String>,
}

impl ClaimScope {
    /// Claims touching these keys at either end.
    fn either(keys: BTreeSet<String>) -> Self {
        Self {
            sources: keys.clone(),
            targets: keys,
        }
    }
}

fn touches(claim: &ObservedLinkClaim, scope: &ClaimScope) -> bool {
    scope.sources.contains(&claim.source_key)
        || claim
            .target
            .as_ref()
            .is_some_and(|t| scope.targets.contains(&t.key))
}

fn source_version(bundle: &MappingBundle) -> Result<&str, CellError> {
    bundle
        .raw
        .get("source_version")
        .and_then(RuntimeValue::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or(CellError::MappingBundleMismatch)
}

/// OBS-LINK-CONTRIBUTION-1, mirroring the kernel's final-set rule: only a
/// recorded `supersedes` orders two claims of one contribution, retained
/// evidence never supersedes a fresh claim, and unordered rivals are contested.
fn standings(group: &[&Entry], target: impl Fn(&Entry) -> Option<FactKey>) -> Vec<Standing> {
    let superseded: Vec<bool> = group
        .iter()
        .map(|k| {
            group.iter().any(|o| {
                o.contribution == k.contribution
                    && o.claim.supersedes.contains(&k.id)
                    && (o.fresh || !k.fresh)
            })
        })
        .collect();
    group
        .iter()
        .zip(&superseded)
        .map(|(k, &gone)| {
            let rival = group.iter().zip(&superseded).any(|(o, &o_gone)| {
                o.contribution == k.contribution && target(o) != target(k) && !o_gone
            });
            match (gone, rival) {
                (true, _) => Standing::Superseded,
                (false, true) => Standing::Contested,
                (false, false) => Standing::Current,
            }
        })
        .collect()
}

impl<S: SqlEngine> StoreReader<'_, S> {
    /// Immutable, recomputed source claims published by consumed mapping bundles.
    /// Missing targets remain represented here; callers enforce source disclosure.
    ///
    /// # Errors
    /// Missing/corrupt pins or an exhausted bounded evidence scan.
    pub fn observed_link_claims(&self) -> Result<Vec<ObservedLinkClaim>, CellError> {
        let entries = self.claim_journal(None, None)?;
        Ok(Self::distinct(entries.into_iter()))
    }

    fn distinct(entries: impl Iterator<Item = Entry>) -> Vec<ObservedLinkClaim> {
        let claims: BTreeMap<_, _> = entries.map(|e| (e.id, e.claim)).collect();
        claims.into_values().collect()
    }

    /// Consumed bundles in commit order, within the journal or naming a scoped key.
    /// The claim budgets bound this selection, so unrelated bundles never exhaust them.
    fn claim_bundles(
        &self,
        scope: Option<&ClaimScope>,
        limit: usize,
    ) -> Result<Vec<String>, CellError> {
        let selected = self.with_source_dispute_index(|index| {
            let mut selected = BTreeMap::new();
            for (side, keys) in [
                (&index.claim_sources, scope.map(|scope| &scope.sources)),
                (&index.claim_targets, scope.map(|scope| &scope.targets)),
            ] {
                for (key, entries) in side {
                    if keys.is_none_or(|keys| keys.contains(key)) {
                        selected.extend(
                            entries
                                .iter()
                                .map(|(identity, bytes)| (identity.clone(), *bytes)),
                        );
                    }
                }
            }
            selected
        })?;
        if selected.len() > limit {
            return Err(CellError::OutOfRange("link claim bundles"));
        }
        if selected
            .values()
            .fold(0u64, |sum, length| sum.saturating_add(*length))
            > MAX_CLAIM_BYTES
        {
            return Err(CellError::OutOfRange("link claim evidence bytes"));
        }
        Ok(selected.into_keys().map(|(_, id)| id).collect())
    }

    fn claim_bundle(&self, id: &str) -> Result<MappingBundle, CellError> {
        let encoded: String = self
            .conn
            .query_optional(
                &format!("SELECT observed FROM {INBOX} WHERE observation_id = ?1"),
                params![id],
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
        Ok(bundle)
    }

    /// Recomputed claims in consumption order, then `current` as fresh evidence.
    /// A claim supersedes every earlier-consumed claim of its contribution with a
    /// lower source version, so retained identities recompute exactly and a late
    /// older occurrence records no order over a newer one. With a scope, the read
    /// closes over touching claims' sources, and only their source/role groups return.
    fn claim_journal(
        &self,
        current: Option<&MappingBundle>,
        scope: Option<&ClaimScope>,
    ) -> Result<Vec<Entry>, CellError> {
        let mut cache = BTreeMap::new();
        let selection = match scope {
            None => None,
            Some(scope) => {
                let mut closed = scope.clone();
                for id in self.claim_bundles(Some(scope), MAX_CLAIM_BUNDLES)? {
                    let bundle = self.claim_bundle(&id)?;
                    for claim in bundle.link_claims()? {
                        if touches(&claim, scope) {
                            closed.sources.insert(claim.source_key);
                        }
                    }
                    cache.insert(id, bundle);
                }
                for claim in current
                    .map(MappingBundle::link_claims)
                    .transpose()?
                    .into_iter()
                    .flatten()
                {
                    if touches(&claim, scope) {
                        closed.sources.insert(claim.source_key);
                    }
                }
                Some(closed)
            }
        };
        let mut bundles = Vec::new();
        for id in self.claim_bundles(selection.as_ref(), MAX_CLAIM_BUNDLES)? {
            let bundle = match cache.remove(&id) {
                Some(bundle) => bundle,
                None => self.claim_bundle(&id)?,
            };
            let disputed = self.mapping_event_is_disputed(&bundle)?;
            bundles.push((bundle, false, disputed));
        }
        if let Some(bundle) = current {
            bundles.push((bundle.clone(), true, false));
        }
        let mut history = BTreeMap::<String, Vec<(String, String)>>::new();
        let mut entries = Vec::new();
        for (bundle, fresh, disputed) in &bundles {
            let version = source_version(bundle)?;
            let mut recorded = Vec::new();
            for mut claim in bundle.link_claims()? {
                let contribution = claim.contribution_id()?;
                claim.supersedes = history
                    .get(&contribution)
                    .into_iter()
                    .flatten()
                    .filter(|(earlier, _)| earlier.as_str() < version)
                    .map(|(_, id)| id.clone())
                    .collect();
                let id = claim.id()?;
                recorded.push((contribution.clone(), id.clone()));
                entries.push(Entry {
                    claim,
                    id,
                    contribution,
                    fresh: *fresh,
                    disputed: *disputed,
                });
            }
            for (contribution, id) in recorded {
                history
                    .entry(contribution)
                    .or_default()
                    .push((version.to_owned(), id));
            }
        }
        if entries.len() > MAX_CLAIMS {
            return Err(CellError::OutOfRange("observed link claims"));
        }
        if let Some(scope) = scope {
            let groups: BTreeSet<_> = entries
                .iter()
                .filter(|e| touches(&e.claim, scope))
                .map(|e| (e.claim.source_key.clone(), e.claim.role.clone()))
                .collect();
            entries
                .retain(|e| groups.contains(&(e.claim.source_key.clone(), e.claim.role.clone())));
        }
        Ok(entries)
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
        let scope =
            ClaimScope::either(BTreeSet::from([key.source.key.clone(), key.target.key.clone()]));
        let entries = self.claim_journal(None, Some(&scope))?;
        Ok(Self::distinct(entries.into_iter().filter(|e| {
            e.claim.target.as_ref().is_some_and(|target| {
                model.canonical_link(&LinkKey::new(
                    FactKey::new(e.claim.source_type.as_str(), &e.claim.source_key),
                    e.claim.role.as_str(),
                    FactKey::new(target.object_type.as_str(), &target.key),
                )) == key
            })
        })))
    }

    /// The exact edge changes the kernel's final claim set will authorize.
    pub(crate) fn mapping_link_plan<E, C, T>(
        &self,
        bundle: &MappingBundle,
        commit: &CellCommit<E, C, T>,
    ) -> Result<LinkPlan, CellError> {
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces())
            .with_property_types(self.model().property_types());
        let model = CommitModel::new(&catalog);
        // A plan changes only links touching this commit's objects or this
        // bundle's claims. Sources: every record and every claim source, so
        // each changed (source, role) group is read whole. Targets, only where
        // another source's claim can change something:
        // - a claim's target when the link admits one source per target
        //   (`from: one`): there claims to the same target compete;
        // - a record's object that is absent before this commit: claims that
        //   waited on it as a target (OBS-LINK-PENDING-1) may now resolve.
        // A group reached only through a shared `from: many` target, or
        // through a target that was already present, keeps its standings
        // (`standings` is per (source, role)) and its endpoint presence, so it
        // writes no link and is never sealed: leaving it out changes no plan,
        // while reading it cost time linear in the target's other claims.
        let mut scope = ClaimScope::default();
        for record in &bundle.records {
            scope.sources.insert(record.key.clone());
            let key = FactKey::new(record.object_type.as_str(), &record.key);
            if GuardView(self).concrete(&model.canonical(&key))?.is_none() {
                scope.targets.insert(record.key.clone());
            }
        }
        for claim in bundle.link_claims()? {
            let exclusive = model
                .link(&claim.source_type, claim.role.as_str())
                .is_none_or(|link| link.cardinality.from == norn_ir::Multiplicity::One);
            if let Some(target) = claim.target.filter(|_| exclusive) {
                scope.targets.insert(target.key);
            }
            scope.sources.insert(claim.source_key);
        }
        let entries = self.claim_journal(Some(bundle), Some(&scope))?;
        let fact = |object_type: &norn_ir::ObjectTypeName, key: &str| {
            model.canonical(&FactKey::new(object_type.as_str(), key))
        };
        let target = |e: &Entry| {
            e.claim
                .target
                .as_ref()
                .map(|t| fact(&t.object_type, &t.key))
        };
        let mut groups = BTreeMap::<_, Vec<&Entry>>::new();
        for entry in entries.iter().filter(|e| !e.disputed) {
            let source = fact(&entry.claim.source_type, &entry.claim.source_key);
            groups
                .entry((source, entry.claim.role.clone()))
                .or_default()
                .push(entry);
        }
        let present = |key: &FactKey| self.present_after(model, commit, key);
        let mut links = BTreeMap::new();
        let mut sealed = Vec::new();
        for ((source, role), group) in &groups {
            let ranked = standings(group, target);
            let current: BTreeSet<_> = group
                .iter()
                .zip(&ranked)
                .filter(|(_, s)| **s == Standing::Current)
                .map(|(e, _)| target(e))
                .collect();
            if current.len() > 1
                && model
                    .link(&source.object_type, role.as_str())
                    .is_some_and(|link| link.cardinality.to == norn_ir::Multiplicity::One)
            {
                return Err(CellError::UnsupportedMappingProfile);
            }
            let before = links.len();
            for (entry, standing) in group.iter().zip(&ranked) {
                let Some(to) = target(entry) else { continue };
                let key = LinkKey::new(source.clone(), role.as_str(), to.clone());
                let exists = self.mapping_link_exists(&key, model)?;
                let supported = group
                    .iter()
                    .zip(&ranked)
                    .any(|(o, s)| *s != Standing::Superseded && target(o).as_ref() == Some(&to));
                let mutation = match standing {
                    Standing::Current if !exists && present(source)? && present(&to)? => {
                        LinkMutation::Add {
                            values: model
                                .effective_link_values(
                                    commit,
                                    &GuardView(self),
                                    &key,
                                    &RuntimeObject::new(),
                                )
                                .map_err(|e| CellError::Encoding(e.to_string()))?,
                        }
                    }
                    Standing::Superseded if exists && !supported => LinkMutation::Remove,
                    Standing::Current | Standing::Superseded | Standing::Contested => continue,
                };
                links.insert(
                    model.canonical_link(&key),
                    self.mapping_link_write(key, mutation)?,
                );
            }
            if links.len() > before {
                sealed.extend(group.iter().map(|e| e.claim.clone()));
            }
        }
        let mut links: Vec<_> = links.into_values().collect();
        links.sort_by_key(|w| matches!(&w.mutation, LinkMutation::Add { .. }));
        let fresh = entries
            .into_iter()
            .filter(|e| e.fresh)
            .map(|e| e.claim)
            .collect();
        Ok(LinkPlan {
            links,
            fresh,
            sealed,
        })
    }

    /// OBS-LINK-PENDING-1: endpoint presence as admission resolves it, after `commit`.
    fn present_after<E, C, T>(
        &self,
        model: CommitModel<'_, '_>,
        commit: &CellCommit<E, C, T>,
        key: &FactKey,
    ) -> Result<bool, CellError> {
        let canonical = model.canonical(key);
        let concrete = match commit
            .writes
            .iter()
            .find(|w| model.canonical(&w.key) == canonical)
        {
            Some(write) => matches!(write.mutation, FactMutation::Put { .. })
                .then(|| write.key.object_type.clone()),
            None => GuardView(self).concrete(&canonical)?,
        };
        Ok(concrete.is_some_and(|concrete| model.is_a(&concrete, &key.object_type)))
    }

    fn mapping_link_exists(
        &self,
        key: &LinkKey,
        model: CommitModel<'_, '_>,
    ) -> Result<bool, CellError> {
        Ok(GuardView(self)
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

    /// Writes this bundle's claims and prepares every claim of each changed
    /// source/role group, so the kernel decides standing over the whole group.
    pub(crate) fn prepare_mapping_links(
        &self,
        bundle: &MappingBundle,
        catalog: &TypeCatalog<'_>,
        turn: &norn_kernel::DecisionTurn,
        commit: &mut MappingCommit,
        authority: &mut ObservationAuthorization,
    ) -> Result<(), CellError> {
        let plan = self.mapping_link_plan(bundle, commit)?;
        for claim in &plan.fresh {
            self.prepare_claim_snapshot(catalog, turn, claim, commit, authority)?;
        }
        commit.links = plan.links;
        let model = CommitModel::new(catalog);
        let expectation = |key: FactKey| -> Result<FactExpectation, CellError> {
            Ok(FactExpectation {
                expected: FactVersion(self.version(
                    MAIN_WORLD,
                    key.object_type.as_str(),
                    &key.key,
                )?),
                key,
            })
        };
        for claim in plan.sealed {
            self.prepare_claim_snapshot(catalog, turn, &claim, commit, authority)?;
            let source = FactKey::new(claim.source_type.as_str(), &claim.source_key);
            let read = expectation(source.clone())?;
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
                    let target = expectation(key)?;
                    if !commit.reads.contains(&target) {
                        commit.reads.push(target);
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
        // The index reads consumed bundle records that declare a link.
        let observed = serde_json::json!({
            "input": { "records": [{ "key": key, "links": [{ "key": key, "targetKey": "demo/target" }] }] }
        })
        .to_string();
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
        let store = CellStore::open_in_memory(
            Model::from_definitions_with_interfaces(&[], &[], &[]).expect("model"),
        )
        .expect("store");
        consumed(&store, 1, "demo/\"quoted\"");
        for position in 2..=1002 {
            consumed(&store, position, &format!("demo/other/{position}"));
        }
        consumed(&store, 1003, "demo/\"quoted\"");
        let reader = store.read();
        let scope = BTreeSet::from(["demo/\"quoted\"".to_owned()]);
        assert_eq!(
            reader
                .claim_bundles(Some(&scope), MAX_CLAIM_BUNDLES)
                .expect("scoped"),
            ["mapping-bundle:1", "mapping-bundle:1003"],
        );
        for refused in [
            reader.claim_bundles(None, MAX_CLAIM_BUNDLES),
            reader.claim_bundles(Some(&scope), 1),
        ] {
            assert!(matches!(
                refused,
                Err(CellError::OutOfRange("link claim bundles"))
            ));
        }
    }
}
