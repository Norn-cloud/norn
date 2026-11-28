//! OBS-LINK-CONVERGE-1 / OBS-LINK-CONTRIBUTION-1 / OBS-LINK-PENDING-1:
//! retained bundles are the claim journal.
use crate::{
    CellError, MAIN_WORLD, StoreReader,
    engine::{SqlEngine, params},
    host::GuardView,
    mapping_bundle::MappingBundle,
    mapping_conflict::{EventDispute, SourceDisputeIndex},
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

static WIDE_SCOPE_CHECK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Also plan every mapping bundle's links over the wide journal scope (every
/// record and claim key at both ends) and refuse if the narrowed plan differs.
/// For tests: it makes every plan cost what it did before the narrowing.
#[doc(hidden)]
pub fn check_link_plans_against_wide_scope(on: bool) {
    WIDE_SCOPE_CHECK.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// OBS-LINK-DISPUTE-1: the rule of the finding a held edge raises.
pub const LINK_HELD_RULE: &str = "core.boundary.link_held";
const MAX_CLAIM_BUNDLES: usize = 1000;
const MAX_CLAIM_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CLAIMS: usize = 20_000;

/// One journal claim with its recomputed recorded order.
struct Entry {
    claim: ObservedLinkClaim,
    id: String,
    contribution: String,
    fresh: bool,
    /// OBS-LINK-DISPUTE-1: a dispute row (conflict or identity) covers the
    /// claim's source event now; never for a fresh claim, whose bundle
    /// `check_mapping_bundle` refuses when disputed.
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
#[derive(Debug, PartialEq)]
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

fn parse_source_version(
    bundle: &MappingBundle,
    kind: norn_ir::VersionKind,
) -> Result<norn_ir::SourceVersion, CellError> {
    let raw_version = bundle
        .raw
        .get("source_version")
        .and_then(RuntimeValue::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or(CellError::MappingBundleMismatch)?;
    norn_ir::SourceVersion::parse(kind, raw_version)
        .map_err(|_err| CellError::MappingBundleMismatch)
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
        let entries = self.claim_journal(&[], None)?;
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
        // Bundles touching the scope, by the side of the claim, from the
        // endpoints recorded when each bundle was consumed.
        let side = |name: &str,
                    keys: Option<&BTreeSet<String>>|
         -> Result<Vec<Vec<crate::engine::SqlValue>>, CellError> {
            let table = crate::system::CLAIM_ENDPOINT;
            match keys {
                None => Ok(self.conn.query(
                    &format!("SELECT \"commit_seq\", \"observation_id\", \"bytes\" FROM \"{table}\" WHERE \"side\" = ?1"),
                    params![name],
                )?),
                Some(keys) => Ok(self.conn.query(
                    &format!("SELECT \"commit_seq\", \"observation_id\", \"bytes\" FROM \"{table}\" WHERE \"side\" = ?1 AND \"key\" IN (SELECT value FROM json_each(?2))"),
                    params![name, serde_json::to_string(keys)?],
                )?),
            }
        };
        let mut selected = BTreeMap::new();
        for rows in [
            side("source", scope.map(|scope| &scope.sources))?,
            side("target", scope.map(|scope| &scope.targets))?,
        ] {
            for row in rows {
                let row = crate::engine::SqlRow(row);
                let seq = crate::read::to_u64(row.get(0)?, "claim sequence")?;
                let id: String = row.get(1)?;
                let bytes = crate::read::to_u64(row.get(2)?, "claim bytes")?;
                selected.insert((seq, id), bytes);
            }
        }
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
        // A row whose exact text rechecked under the verified install gives
        // the same result again (MAPPING-PLAN-1's byte-equal reuse).
        if let Some(cache) = self.mapping_plans
            && let Some((seen, bundle)) = cache.borrow().claims.get(id)
            && *seen == encoded
        {
            return Ok(bundle.clone());
        }
        let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
        let bundle: MappingBundle =
            norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
        if observed.id.0 != id || id != format!("mapping-bundle:{}", bundle.digest) {
            return Err(CellError::MappingBundleMismatch);
        }
        self.recheck_mapping_bundle(&bundle)?;
        if let Some(cache) = self.mapping_plans {
            let mut cache = cache.borrow_mut();
            if cache.claims.len() >= crate::mapping_bundle::MAX_CACHED_CLAIM_BUNDLES {
                cache.claims.clear();
            }
            cache
                .claims
                .insert(id.to_owned(), (encoded, bundle.clone()));
        }
        Ok(bundle)
    }

    /// Recomputed claims in consumption order, then `current` as fresh evidence.
    /// A claim supersedes every earlier-consumed claim of its contribution with a
    /// lower source version, so retained identities recompute exactly and a late
    /// older occurrence records no order over a newer one. With a scope, the read
    /// closes over touching claims' sources, and only their source/role groups return.
    fn claim_journal(
        &self,
        current: &[MappingBundle],
        scope: Option<&ClaimScope>,
    ) -> Result<Vec<Entry>, CellError> {
        self.claim_journal_with_dispute(current, scope, |bundle| {
            self.mapping_event_is_disputed(bundle)
        })
    }

    fn claim_journal_with_dispute(
        &self,
        current: &[MappingBundle],
        scope: Option<&ClaimScope>,
        mut is_disputed: impl FnMut(&MappingBundle) -> Result<bool, CellError>,
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
                    .iter()
                    .map(MappingBundle::link_claims)
                    .collect::<Result<Vec<_>, _>>()?
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
            let disputed = is_disputed(&bundle)?;
            bundles.push((bundle, false, disputed));
        }
        for bundle in current {
            bundles.push((bundle.clone(), true, false));
        }
        let mut history = BTreeMap::<String, Vec<(norn_ir::SourceVersion, String)>>::new();
        let mut entries = Vec::new();
        for (bundle, fresh, disputed) in &bundles {
            let kind = self.bundle_version_kind(bundle)?;
            let version = parse_source_version(bundle, kind)?;
            let mut recorded = Vec::new();
            for mut claim in bundle.link_claims()? {
                let contribution = claim.contribution_id()?;
                let mut supersedes = BTreeSet::new();
                if let Some(prior) = history.get(&contribution) {
                    for (earlier, id) in prior {
                        if earlier
                            .try_cmp(&version)
                            .map_err(|_err| CellError::MappingBundleMismatch)?
                            == std::cmp::Ordering::Less
                        {
                            supersedes.insert(id.clone());
                        }
                    }
                }
                claim.supersedes = supersedes;
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
                    .push((version.clone(), id));
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

    /// OBS-LINK-DISPUTE-1: one open finding per edge a dispute holds back
    /// from `key`: the current claims from `key` whose source event is
    /// disputed, whose endpoints are both present and whose edge is absent,
    /// so the freeze withheld their `Add`. Claims of several contributions
    /// for one edge make one finding, with each claim's lineage. Derived from
    /// the retained evidence, like the dispute findings themselves; it lasts
    /// as long as the dispute does. Read only for a subject that is in
    /// dispute, so an undisputed object's findings cost no journal read. The
    /// target is a subject: hosts disclose the finding only to a caller who
    /// may see both endpoints, as for the link itself.
    ///
    /// # Errors
    /// The same evidence/pin/budget errors as `observed_link_claims`.
    pub(crate) fn held_link_findings(
        &self,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<norn_ir::DataQualityFinding>, CellError> {
        if !self.indexed_source_dispute(MAIN_WORLD, object_type, key)? {
            return Ok(Vec::new());
        }
        self.held_link_findings_with_lookup(object_type, key, |bundle| {
            self.mapping_event_dispute(bundle)
        })
    }

    pub(crate) fn held_link_findings_in_index(
        &self,
        object_type: &str,
        key: &str,
        index: &SourceDisputeIndex,
    ) -> Result<Vec<norn_ir::DataQualityFinding>, CellError> {
        let roots = self.source_dispute_roots(MAIN_WORLD, object_type, key)?;
        if !index.has_subject(roots.as_deref(), key) {
            return Ok(Vec::new());
        }
        self.held_link_findings_with_lookup(object_type, key, |bundle| index.event_dispute(bundle))
    }

    fn held_link_findings_with_lookup(
        &self,
        object_type: &str,
        key: &str,
        mut event_dispute: impl FnMut(&MappingBundle) -> Result<Option<EventDispute>, CellError>,
    ) -> Result<Vec<norn_ir::DataQualityFinding>, CellError> {
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces());
        let model = CommitModel::new(&catalog);
        let subject = model.canonical(&FactKey::new(object_type, key));
        let scope = ClaimScope {
            sources: BTreeSet::from([subject.key.clone()]),
            targets: BTreeSet::new(),
        };
        let entries = self.claim_journal_with_dispute(&[], Some(&scope), |bundle| {
            Ok(event_dispute(bundle)?.is_some())
        })?;
        let fact = |object_type: &norn_ir::ObjectTypeName, key: &str| {
            model.canonical(&FactKey::new(object_type.as_str(), key))
        };
        let target = |e: &Entry| {
            e.claim
                .target
                .as_ref()
                .map(|t| fact(&t.object_type, &t.key))
        };
        let present = |key: &FactKey| -> Result<bool, CellError> {
            Ok(GuardView(self)
                .concrete(&model.canonical(key))?
                .is_some_and(|concrete| model.is_a(&concrete, &key.object_type)))
        };
        let mut groups = BTreeMap::<_, Vec<&Entry>>::new();
        for entry in &entries {
            let source = fact(&entry.claim.source_type, &entry.claim.source_key);
            if source == subject {
                groups
                    .entry((source, entry.claim.role.clone()))
                    .or_default()
                    .push(entry);
            }
        }
        // Each held edge, by canonical link: its claims and their disputes.
        let mut held = BTreeMap::<LinkKey, (LinkKey, Vec<(&Entry, EventDispute)>)>::new();
        let mut live = BTreeMap::new();
        for ((source, role), group) in &groups {
            for (entry, standing) in group.iter().zip(standings(group, target)) {
                let Some(to) = target(entry) else { continue };
                if !entry.disputed || standing != Standing::Current {
                    continue;
                }
                let link = LinkKey::new(source.clone(), role.as_str(), to.clone());
                if self.mapping_link_exists(&link, model, &mut live)?
                    || !present(source)?
                    || !present(&to)?
                {
                    continue;
                }
                let bundle = self.claim_bundle(&entry.claim.observation_id)?;
                let Some(dispute) = event_dispute(&bundle)? else {
                    continue;
                };
                held.entry(model.canonical_link(&link))
                    .or_insert_with(|| (link, Vec::new()))
                    .1
                    .push((entry, dispute));
            }
        }
        let mut findings = Vec::with_capacity(held.len());
        for (link, claims) in held.into_values() {
            findings.extend(Self::held_link_finding(&link, &claims)?);
        }
        findings.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(findings)
    }

    /// The one held-link finding for `link`, from the disputed claims that
    /// hold it; `None` when there are none.
    fn held_link_finding(
        link: &LinkKey,
        claims: &[(&Entry, EventDispute)],
    ) -> Result<Option<norn_ir::DataQualityFinding>, CellError> {
        let rows: BTreeSet<String> = claims
            .iter()
            .flat_map(|(_, dispute)| dispute.rows.iter().cloned())
            .collect();
        let Some(first_seen) = claims
            .iter()
            .min_by_key(|(_, dispute)| dispute.position)
            .map(|(_, dispute)| dispute.first_seen.clone())
        else {
            return Ok(None);
        };
        let cause = rows
            .iter()
            .map(|row| match row.split_once(':') {
                Some(("mapping-conflict", id)) => format!("conflict {id}"),
                Some((_, id)) => format!("identity {id}"),
                None => row.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let lineage = claims
            .iter()
            .map(|(entry, dispute)| {
                norn_ir::DataQualityLineage::new(
                    entry.claim.provenance.clone(),
                    std::iter::once(entry.claim.observation_id.clone())
                        .chain(dispute.rows.iter().cloned())
                        .collect(),
                )
            })
            .collect();
        let ids: BTreeSet<&str> = claims.iter().map(|(entry, _)| entry.id.as_str()).collect();
        let finding = norn_ir::DataQualityFinding::v2(
            norn_ir::DataQualityRuleRef::Builtin {
                id: LINK_HELD_RULE.into(),
                version: "1".into(),
            },
            vec![
                norn_ir::DataQualitySubject::new(link.source.object_type.clone(), &link.source.key),
                norn_ir::DataQualitySubject::new(link.target.object_type.clone(), &link.target.key),
            ],
            norn_ir::DataQualitySeverity::Warning,
            format!(
                "link {} -{}-> {} held: record in dispute ({cause})",
                link.source.key, link.link, link.target.key
            ),
            norn_ir::DataQualityOutcome::NotSatisfied,
            lineage,
            first_seen.clone(),
            first_seen,
            norn_ir::DataQualityFindingStatus::Open,
        )?
        .with_evidence_digest(norn_ir::canonical::domain_hash_hex(
            b"norn.link.held.v2\0",
            &(&ids, &rows),
        )?)?;
        Ok(Some(finding))
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
        let scope = ClaimScope::either(BTreeSet::from([
            key.source.key.clone(),
            key.target.key.clone(),
        ]));
        let entries = self.claim_journal(&[], Some(&scope))?;
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
        bundles: &[MappingBundle],
        commit: &CellCommit<E, C, T>,
    ) -> Result<LinkPlan, CellError> {
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces())
            .with_property_types(self.model().property_types());
        let model = CommitModel::new(&catalog);
        let plan =
            self.link_plan_over(bundles, commit, model, &self.plan_scope(bundles, model)?)?;
        // Equivalence check for the narrowed scope (tests turn it on): the same
        // plan over the wide scope, every record and claim key at both ends.
        if WIDE_SCOPE_CHECK.load(std::sync::atomic::Ordering::Relaxed) {
            let mut keys = BTreeSet::new();
            for bundle in bundles {
                for record in &bundle.records {
                    keys.insert(record.key.clone());
                }
                for claim in bundle.link_claims()? {
                    keys.insert(claim.source_key);
                    keys.extend(claim.target.map(|t| t.key));
                }
            }
            let wide = self.link_plan_over(bundles, commit, model, &ClaimScope::either(keys))?;
            if wide != plan {
                return Err(CellError::Encoding(format!(
                    "the narrowed link plan differs from the wide one: {plan:?} != {wide:?}"
                )));
            }
        }
        Ok(plan)
    }

    /// The journal scope a bundle's plan reads.
    fn plan_scope(
        &self,
        bundles: &[MappingBundle],
        model: CommitModel<'_, '_>,
    ) -> Result<ClaimScope, CellError> {
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
        for record in bundles.iter().flat_map(|b| &b.records) {
            scope.sources.insert(record.key.clone());
            let key = FactKey::new(record.object_type.as_str(), &record.key);
            if GuardView(self).concrete(&model.canonical(&key))?.is_none() {
                scope.targets.insert(record.key.clone());
            }
        }
        for claim in bundles
            .iter()
            .map(MappingBundle::link_claims)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
        {
            let exclusive = model
                .link(&claim.source_type, claim.role.as_str())
                .is_none_or(|link| link.cardinality.from == norn_ir::Multiplicity::One);
            if let Some(target) = claim.target.filter(|_| exclusive) {
                scope.targets.insert(target.key);
            }
            scope.sources.insert(claim.source_key);
        }
        Ok(scope)
    }

    /// The plan over one journal scope.
    fn link_plan_over<E, C, T>(
        &self,
        bundles: &[MappingBundle],
        commit: &CellCommit<E, C, T>,
        model: CommitModel<'_, '_>,
        scope: &ClaimScope,
    ) -> Result<LinkPlan, CellError> {
        let entries = self.claim_journal(bundles, Some(scope))?;
        let fact = |object_type: &norn_ir::ObjectTypeName, key: &str| {
            model.canonical(&FactKey::new(object_type.as_str(), key))
        };
        let target = |e: &Entry| {
            e.claim
                .target
                .as_ref()
                .map(|t| fact(&t.object_type, &t.key))
        };
        // OBS-LINK-DISPUTE-1: a dispute freezes the claims it covers. They
        // keep their standing (they still supersede older claims of their
        // contribution, contest rivals and hold their edge) but authorize no
        // new edge until review settles the event. A dispute is captured
        // without a commit, so it must not change any group's standings:
        // dropping the disputed claims here revived the claims they had
        // superseded, and whichever later commit happened to read the group
        // applied that, so the end state depended on batching (#1490).
        // The freeze reads the disputes that exist when this commit runs, on
        // every path: no positional dating. Paged delivery commits at the
        // same dispute state as one by one because a page never captures a
        // dispute row after a record it commits (`inject_mapping_page`
        // refuses, and the host delivers that page's records one at a time).
        let mut groups = BTreeMap::<_, Vec<&Entry>>::new();
        for entry in &entries {
            let source = fact(&entry.claim.source_type, &entry.claim.source_key);
            groups
                .entry((source, entry.claim.role.clone()))
                .or_default()
                .push(entry);
        }
        let present = |key: &FactKey| self.present_after(model, commit, key);
        // Each source's live links, read once per plan: a source's journal
        // can hold every claim of every earlier version of its records.
        let mut live: BTreeMap<FactKey, BTreeSet<LinkKey>> = BTreeMap::new();
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
                let exists = self.mapping_link_exists(&key, model, &mut live)?;
                let supported = group
                    .iter()
                    .zip(&ranked)
                    .any(|(o, s)| *s != Standing::Superseded && target(o).as_ref() == Some(&to));
                let mutation = match standing {
                    Standing::Current
                        if !entry.disputed && !exists && present(source)? && present(&to)? =>
                    {
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
            .find(|w| w.key.key == canonical.key && model.canonical(&w.key) == canonical)
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
        live: &mut BTreeMap<FactKey, BTreeSet<LinkKey>>,
    ) -> Result<bool, CellError> {
        let links = match live.entry(key.source.clone()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                GuardView(self)
                    .links_of(&key.source)?
                    .iter()
                    .map(|link| model.canonical_link(link))
                    .collect(),
            ),
        };
        Ok(links.contains(&model.canonical_link(key)))
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
        bundles: &[MappingBundle],
        catalog: &TypeCatalog<'_>,
        turn: &norn_kernel::DecisionTurn,
        commit: &mut MappingCommit,
        authority: &mut ObservationAuthorization,
    ) -> Result<(), CellError> {
        let plan = self.mapping_link_plan(bundles, commit)?;
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
                if !commit.writes.iter().any(|w| {
                    w.key.key == key.key && model.canonical(&w.key) == model.canonical(&key)
                }) {
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
        // Consumption records the bundle's claim endpoints in the same write.
        store
            .conn
            .execute(
                &crate::system::claim_endpoints_of("i.\"observation_id\" = ?1"),
                params![&id],
            )
            .expect("claim endpoints");
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
        let scope = ClaimScope::either(BTreeSet::from(["demo/\"quoted\"".to_owned()]));
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
