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

static WIDE_SCOPE_CHECK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Also plan every mapping bundle's links over the wide journal scope (every
/// record and claim key at both ends) and refuse if the narrowed plan differs.
/// For tests: it makes every plan cost what it did before the narrowing.
#[doc(hidden)]
pub fn check_link_plans_against_wide_scope(on: bool) {
    WIDE_SCOPE_CHECK.store(on, std::sync::atomic::Ordering::Relaxed);
}

std::thread_local! {
    static CLAIM_JOURNAL_V1: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// On this thread, plan new bundles as claim journal v1 did, publishing no
/// withdrawals. For tests: it builds the history an older cell left, to check
/// that it still opens, recomputes and verifies unchanged.
#[doc(hidden)]
pub fn plan_as_claim_journal_v1(on: bool) {
    CLAIM_JOURNAL_V1.with(|v1| v1.set(on));
}
const MAX_CLAIM_BUNDLES: usize = 1000;
const MAX_CLAIM_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CLAIMS: usize = 20_000;

/// One journal claim with its recomputed recorded order.
struct Entry {
    claim: ObservedLinkClaim,
    id: String,
    /// The mapping whose bundle made the claim (its source system).
    mapping: String,
    contribution: String,
    fresh: bool,
    disputed: bool,
    /// SOURCE-RESOLVE-1: a claim on a type resolved across sources from a
    /// bundle whose source record has a higher version consumed. It stays
    /// evidence but has no canonical effect: it adds no edge and takes no
    /// part in a conflict, in whichever order the versions arrived.
    stale: bool,
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

fn source_version(bundle: &MappingBundle) -> Result<&str, CellError> {
    bundle
        .raw
        .get("source_version")
        .and_then(RuntimeValue::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or(CellError::MappingBundleMismatch)
}

/// Whether `o` supersedes `k` under the kernel's rule: a recorded
/// `supersedes` entry of the same contribution, and retained evidence never
/// supersedes a fresh claim.
fn supersedes(o: &Entry, k: &Entry) -> bool {
    o.contribution == k.contribution && o.claim.supersedes.contains(&k.id) && (o.fresh || !k.fresh)
}

/// A journal group as a plan ranks it. A stale claim (SOURCE-RESOLVE-1)
/// takes part only once a claim the group keeps supersedes it: then it can
/// only lose, and its edge is withdrawn. Otherwise it stays evidence with no
/// canonical effect; a stale withdrawal is no exception, so what it superseded
/// is left out with it rather than standing again.
fn ranked_group<'e>(group: &[&'e Entry]) -> Vec<&'e Entry> {
    let mut kept: Vec<bool> = group.iter().map(|e| !e.stale).collect();
    loop {
        let next: Vec<bool> = group
            .iter()
            .zip(&kept)
            .map(|(k, &is)| {
                is || group
                    .iter()
                    .zip(&kept)
                    .any(|(o, &o_kept)| o_kept && supersedes(o, k))
            })
            .collect();
        if next == kept {
            break;
        }
        kept = next;
    }
    group
        .iter()
        .zip(kept)
        .filter_map(|(e, kept)| kept.then_some(*e))
        .collect()
}

/// The claims that decide what a link is: current, and not a withdrawal (a
/// withdrawal asserts nothing, so it never conflicts with another source).
fn deciding<'g, 'e>(
    group: &'g [&'e Entry],
    ranked: &'g [Standing],
) -> impl Iterator<Item = &'e Entry> + 'g {
    group
        .iter()
        .zip(ranked)
        .filter(|(e, s)| **s == Standing::Current && !e.claim.withdrawal)
        .map(|(e, _)| *e)
}

/// OBS-LINK-CONTRIBUTION-1, mirroring the kernel's final-set rule: only a
/// recorded `supersedes` orders two claims of one contribution, retained
/// evidence never supersedes a fresh claim, and unordered rivals are contested.
fn standings(group: &[&Entry], target: impl Fn(&Entry) -> Option<FactKey>) -> Vec<Standing> {
    let superseded: Vec<bool> = group
        .iter()
        .map(|k| group.iter().any(|o| supersedes(o, k)))
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

/// Whether a group's deciding claims come from more than one mapping.
fn rival_sources(group: &[&Entry], ranked: &[Standing]) -> bool {
    let mut mappings = deciding(group, ranked).map(|e| e.mapping.as_str());
    let first = mappings.next();
    mappings.any(|m| Some(m) != first)
}

/// SOURCE-RESOLVE-1: an exclusive (to-one) link that several sources report
/// with different targets. No edge changes for it until they agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkConflict {
    /// The link.
    pub link: String,
    /// Each current claim: (mapping, target key or `None` for a clear,
    /// retained bundle), in mapping order.
    pub claims: Vec<(String, Option<String>, String)>,
}

impl<S: SqlEngine> StoreReader<'_, S> {
    /// SOURCE-RESOLVE-1: the exclusive links of `object_type`/`key` that
    /// several sources currently claim with different targets, as the
    /// journal stands now.
    ///
    /// # Errors
    /// The same evidence/pin/budget errors as `observed_link_claims`.
    pub(crate) fn source_link_conflicts(
        &self,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<LinkConflict>, CellError> {
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces());
        let model = CommitModel::new(&catalog);
        let subject = model.canonical(&FactKey::new(object_type, key));
        let scope = ClaimScope {
            sources: BTreeSet::from([key.to_owned()]),
            targets: BTreeSet::new(),
        };
        let entries = self.claim_journal(&[], Some(&scope))?;
        let mut groups = BTreeMap::<String, Vec<&Entry>>::new();
        for entry in entries.iter().filter(|e| !e.disputed) {
            let source = FactKey::new(entry.claim.source_type.as_str(), &entry.claim.source_key);
            if model.canonical(&source) == subject {
                groups
                    .entry(entry.claim.role.as_str().to_owned())
                    .or_default()
                    .push(entry);
            }
        }
        let target = |e: &Entry| {
            e.claim
                .target
                .as_ref()
                .map(|t| model.canonical(&FactKey::new(t.object_type.as_str(), &t.key)))
        };
        let mut out = Vec::new();
        for (role, group) in groups {
            let exclusive = model
                .link(&subject.object_type, &role)
                .is_some_and(|link| link.cardinality.to == norn_ir::Multiplicity::One);
            let group = ranked_group(&group);
            let ranked = standings(&group, target);
            let current: Vec<_> = deciding(&group, &ranked).collect();
            let targets: BTreeSet<_> = current.iter().map(|e| target(e)).collect();
            if !exclusive || targets.len() < 2 || !rival_sources(&group, &ranked) {
                continue;
            }
            let mut claims: Vec<_> = current
                .iter()
                .map(|e| {
                    (
                        e.mapping.clone(),
                        e.claim.target.as_ref().map(|t| t.key.clone()),
                        e.claim.observation_id.clone(),
                    )
                })
                .collect();
            claims.sort();
            claims.dedup();
            out.push(LinkConflict { link: role, claims });
        }
        Ok(out)
    }

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
        // Claim journal v2: every consumed version of each source record that
        // claimed a scoped source, so the versions that withdraw it are read
        // with the claims they withdraw (a withdrawing version may name none).
        let records = {
            let (record, source) = (crate::system::CLAIM_RECORD, crate::system::SOURCE_RECORD);
            let select = format!(
                "SELECT b.\"commit_seq\", b.\"observation_id\", octet_length(i.\"observed\") FROM \"{record}\" k JOIN \"{source}\" b ON b.\"mapping\" = k.\"mapping\" AND b.\"record_id\" = k.\"record_id\" JOIN \"{INBOX}\" i ON i.\"observation_id\" = b.\"observation_id\""
            );
            match scope.map(|scope| &scope.sources) {
                None => self.conn.query(&select, params![])?,
                Some(keys) => self.conn.query(
                    &format!("{select} WHERE k.\"key\" IN (SELECT value FROM json_each(?1))"),
                    params![serde_json::to_string(keys)?],
                )?,
            }
        };
        let mut selected = BTreeMap::new();
        for rows in [
            side("source", scope.map(|scope| &scope.sources))?,
            side("target", scope.map(|scope| &scope.targets))?,
            records,
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

    /// SOURCE-RESOLVE-1: the resolution table of each of `bundles` (by
    /// digest) whose source record has a higher version consumed.
    fn superseded_tables<'b>(
        &self,
        bundles: impl Iterator<Item = &'b MappingBundle> + Clone,
    ) -> Result<
        BTreeMap<String, std::sync::Arc<norn_checker::mapping::resolution::ResolutionTable>>,
        CellError,
    > {
        let superseded = self.superseded_bundles(
            &bundles
                .clone()
                .map(|b| format!("mapping-bundle:{}", b.digest))
                .collect::<Vec<_>>(),
        )?;
        let mut out = BTreeMap::new();
        for bundle in bundles {
            if superseded.contains(&format!("mapping-bundle:{}", bundle.digest)) {
                let plan = self.mapping_plan(&bundle.input, bundle.definitions_version)?;
                out.insert(
                    bundle.digest.clone(),
                    std::sync::Arc::clone(&plan.resolution),
                );
            }
        }
        Ok(out)
    }

    /// The bundles a journal read recomputes, in consumption order, each with
    /// whether it is fresh and whether its event is disputed.
    fn journal_bundles(
        &self,
        current: &[MappingBundle],
        scope: Option<&ClaimScope>,
    ) -> Result<Vec<(MappingBundle, bool, bool)>, CellError> {
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
            let disputed = self.mapping_event_is_disputed(&bundle)?;
            bundles.push((bundle, false, disputed));
        }
        for bundle in current {
            bundles.push((bundle.clone(), true, false));
        }
        Ok(bundles)
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
        let bundles = self.journal_bundles(current, scope)?;
        let superseded = self.superseded_tables(bundles.iter().map(|(b, ..)| b))?;
        let mut history = BTreeMap::<String, Vec<(String, String)>>::new();
        // Claim journal v2: each contribution's entries (version, entry
        // index), and the contributions of each source record (scope, mapping).
        let mut recorded_entries = BTreeMap::<String, Vec<(String, usize)>>::new();
        let mut by_record = BTreeMap::<(String, String), BTreeSet<String>>::new();
        let mut entries = Vec::new();
        for (bundle, fresh, disputed) in &bundles {
            let version = source_version(bundle)?;
            let resolution = superseded.get(&bundle.digest);
            let mut recorded = Vec::new();
            let claims = bundle.link_claims()?;
            let carried = claims
                .iter()
                .map(ObservedLinkClaim::contribution_id)
                .collect::<Result<BTreeSet<_>, _>>()?;
            let withdrawals = self.withdrawals(
                bundle,
                *fresh,
                version,
                &carried,
                &entries,
                &recorded_entries,
                &by_record,
                &history,
            )?;
            for mut claim in claims.into_iter().chain(withdrawals) {
                let stale =
                    resolution.is_some_and(|table| table.governs(claim.source_type.as_str()));
                let contribution = claim.contribution_id()?;
                claim.supersedes = history
                    .get(&contribution)
                    .into_iter()
                    .flatten()
                    .filter(|(earlier, _)| earlier.as_str() < version)
                    .map(|(_, id)| id.clone())
                    .collect();
                let id = claim.id()?;
                recorded.push((contribution.clone(), id.clone(), entries.len()));
                by_record
                    .entry((claim.source_scope.clone(), bundle.input.mapping.clone()))
                    .or_default()
                    .insert(contribution.clone());
                entries.push(Entry {
                    claim,
                    id,
                    mapping: bundle.input.mapping.clone(),
                    contribution,
                    fresh: *fresh,
                    disputed: *disputed,
                    stale,
                });
            }
            for (contribution, id, index) in recorded {
                recorded_entries
                    .entry(contribution.clone())
                    .or_default()
                    .push((version.to_owned(), index));
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

    /// SOURCE-RESOLVE-1 (claim journal v2): the withdrawals `bundle`
    /// publishes. For each contribution an earlier-recorded, lower version of
    /// its source record (same capture scope and mapping) made, that `bundle`
    /// no longer carries, and whose latest earlier claim is not already a
    /// withdrawal: a claim with no target that the planner then orders after
    /// the contribution's earlier claims, so the kernel sees them superseded.
    /// Only for types resolved across sources, where a stale claim otherwise
    /// keeps its edge. Each is the contribution's own (scope, source, role,
    /// provenance), so it can never touch another source's or contribution's
    /// claim.
    ///
    /// Versioned: a retained bundle publishes a withdrawal only when the
    /// commit that consumed it wrote that claim, so a bundle consumed under
    /// journal v1 recomputes exactly what it published then.
    #[allow(
        clippy::too_many_arguments,
        reason = "the journal state of one recomputation"
    )]
    fn withdrawals(
        &self,
        bundle: &MappingBundle,
        fresh: bool,
        version: &str,
        carried: &BTreeSet<String>,
        entries: &[Entry],
        recorded: &BTreeMap<String, Vec<(String, usize)>>,
        by_record: &BTreeMap<(String, String), BTreeSet<String>>,
        history: &BTreeMap<String, Vec<(String, String)>>,
    ) -> Result<Vec<ObservedLinkClaim>, CellError> {
        if fresh && CLAIM_JOURNAL_V1.with(std::cell::Cell::get) {
            return Ok(Vec::new());
        }
        let record = (bundle.claim_scope()?, bundle.input.mapping.clone());
        let Some(contributions) = by_record.get(&record) else {
            return Ok(Vec::new());
        };
        let mut table = None;
        let mut out = Vec::new();
        for contribution in contributions.difference(carried) {
            // The contribution's latest claim below this version, the last
            // recorded of equal versions.
            let Some(&(_, index)) = recorded
                .get(contribution)
                .into_iter()
                .flatten()
                .filter(|(earlier, _)| earlier.as_str() < version)
                .max_by(|a, b| a.0.cmp(&b.0))
            else {
                continue;
            };
            let latest = &entries
                .get(index)
                .ok_or(CellError::OutOfRange("link claim entry"))?
                .claim;
            if latest.withdrawal {
                continue;
            }
            if table.is_none() {
                table = Some(std::sync::Arc::clone(
                    &self
                        .mapping_plan(&bundle.input, bundle.definitions_version)?
                        .resolution,
                ));
            }
            if !table
                .as_ref()
                .is_some_and(|table| table.governs(latest.source_type.as_str()))
            {
                continue;
            }
            let claim = ObservedLinkClaim {
                observation_id: format!("mapping-bundle:{}", bundle.digest),
                source_scope: latest.source_scope.clone(),
                source_type: latest.source_type.clone(),
                source_key: latest.source_key.clone(),
                role: latest.role.clone(),
                target: None,
                provenance: latest.provenance.clone(),
                supersedes: history
                    .get(contribution)
                    .into_iter()
                    .flatten()
                    .filter(|(earlier, _)| earlier.as_str() < version)
                    .map(|(_, id)| id.clone())
                    .collect(),
                withdrawal: true,
            };
            if fresh
                || self.version(
                    MAIN_WORLD,
                    norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
                    &claim.id()?,
                )? != 0
            {
                out.push(claim);
            }
        }
        Ok(out)
    }

    /// The source keys that consumed versions of `bundles`' source records
    /// claim: a new version may withdraw any of them, so its plan reads them.
    fn record_sources(&self, bundles: &[MappingBundle]) -> Result<BTreeSet<String>, CellError> {
        let mut keys = BTreeSet::new();
        for bundle in bundles {
            let [connector, partition, source, row, _] =
                crate::mapping_identity::keys(&bundle.raw, &bundle.input)?;
            let rows = self.conn.query(
                &format!(
                    "SELECT \"key\" FROM \"{}\" WHERE \"mapping\" = ?1 AND \"record_id\" = json_array(?2, ?3, ?4, ?5)",
                    crate::system::CLAIM_RECORD
                ),
                params![bundle.input.mapping.as_str(), connector, partition, source, row],
            )?;
            for row in rows {
                keys.insert(crate::engine::SqlRow(row).get::<String>(0)?);
            }
        }
        Ok(keys)
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
            let mut keys = self.record_sources(bundles)?;
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
        let mut scope = ClaimScope {
            sources: self.record_sources(bundles)?,
            targets: BTreeSet::new(),
        };
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
        let mut groups = BTreeMap::<_, Vec<&Entry>>::new();
        for entry in entries.iter().filter(|e| !e.disputed) {
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
            let group = &ranked_group(group);
            let ranked = standings(group, target);
            let current: BTreeSet<_> = deciding(group, &ranked).map(&target).collect();
            if current.len() > 1
                && model
                    .link(&source.object_type, role.as_str())
                    .is_some_and(|link| link.cardinality.to == norn_ir::Multiplicity::One)
            {
                // SOURCE-RESOLVE-1: sources that disagree on an exclusive
                // link are an explicit conflict (`source_link_conflicts`):
                // the link changes no edge, and the bundle's other
                // decisions still apply. One source's own rival rows still
                // refuse.
                if rival_sources(group, &ranked) {
                    continue;
                }
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
