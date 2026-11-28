//! OBS-LINK-CONVERGE-1 / OBS-LINK-CONTRIBUTION-1 / OBS-LINK-PENDING-1:
//! durable indexed link-claim frontier (`_norn_link_claim_frontier`, #1028).
use crate::{
    CellError, MAIN_WORLD, StoreReader,
    engine::SqlEngine,
    host::GuardView,
    link_claim_frontier::FrontierRow,
    mapping_bundle::MappingBundle,
    mapping_conflict::{EventDispute, SourceDisputeIndex},
};
use norn_ir::{RuntimeObject, RuntimeValue, mapping::ObservedLinkClaim};
use norn_kernel::{
    CellCommit, CommitModel, CommitView, FactExpectation, FactKey, FactMutation, FactVersion,
    LinkKey, LinkMutation, LinkWrite, ObservationAuthorization, ObservationEvent, TypeCatalog,
};
use std::collections::{BTreeMap, BTreeSet};

type MappingCommit = CellCommit<ObservationEvent, RuntimeObject, RuntimeObject>;

static WIDE_SCOPE_CHECK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Also plan every mapping bundle's links over the wide frontier scope (every
/// record and claim key at both ends) and refuse if the narrowed plan differs.
#[doc(hidden)]
pub fn check_link_plans_against_wide_scope(on: bool) {
    WIDE_SCOPE_CHECK.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// OBS-LINK-DISPUTE-1: the rule of the finding a held edge raises.
pub const LINK_HELD_RULE: &str = "core.boundary.link_held";

/// One frontier claim with its standing metadata.
struct Entry {
    claim: ObservedLinkClaim,
    id: String,
    contribution: String,
    event_keys: [String; 5],
    fresh: bool,
    /// OBS-LINK-DISPUTE-1: an active dispute row covers the claim's source
    /// event now (excluding any dispute being resolved in this commit).
    disputed: bool,
}

/// The kernel's per-contribution standing, recomputed over a frontier group.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Standing {
    Current,
    Superseded,
    Contested,
}

/// Edge changes for one commit and the claims that must back them.
#[derive(Debug, PartialEq)]
pub(crate) struct LinkPlan {
    pub(crate) links: Vec<LinkWrite>,
    pub(crate) fresh: Vec<ObservedLinkClaim>,
    pub(crate) sealed: Vec<ObservedLinkClaim>,
}

/// The keys a frontier read selects on, by the side of a claim they match.
#[derive(Clone, Default)]
pub(crate) struct ClaimScope {
    /// Claims whose source is one of these.
    pub(crate) sources: BTreeSet<String>,
    /// Claims whose target is one of these.
    pub(crate) targets: BTreeSet<String>,
}

impl ClaimScope {
    /// Claims touching these keys at either end.
    pub(crate) fn either(keys: BTreeSet<String>) -> Self {
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

pub(crate) fn parse_source_version(
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

/// Verify `row`'s frontier metadata against `_norn_export_journal` + `_norn_receipt`.
pub(crate) fn parse_verified_frontier_version<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    row: &FrontierRow,
) -> Result<Option<norn_ir::SourceVersion>, CellError> {
    let meta = crate::link_claim_frontier::claim_source_meta(
        reader.conn,
        Some(reader),
        row.commit_seq,
        &row.claim.observation_id,
        true,
    )?;
    if meta.source_version != row.source_version
        || meta.version_kind != row.version_kind
        || meta.event_keys != row.event_keys
    {
        return Err(CellError::LedgerCorrupt {
            seq: row.commit_seq,
            reason: format!("frontier metadata mismatch for {}", row.claim_id),
        });
    }
    Ok((!row.source_version.trim().is_empty())
        .then(|| norn_ir::SourceVersion::parse(row.version_kind, &row.source_version).ok())
        .flatten())
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
    /// Immutable source claims published by committed mapping bundles and
    /// dispute resolutions, ordered deterministically by claim identity.
    ///
    /// # Errors
    /// Store or decoding failures.
    pub fn observed_link_claims(&self) -> Result<Vec<ObservedLinkClaim>, CellError> {
        self.check_live()?;
        let rows = self.all_frontier_claims()?;
        Ok(Self::distinct_claims(
            rows.into_iter().map(|r| (r.claim_id, r.claim)),
        ))
    }

    fn distinct_claims(
        claims: impl Iterator<Item = (String, ObservedLinkClaim)>,
    ) -> Vec<ObservedLinkClaim> {
        let map: BTreeMap<_, _> = claims.collect();
        map.into_values().collect()
    }

    /// Differential check (#1028): verify that `_norn_link_claim_frontier`
    /// matches immutable `_norn_fact_values` field-for-field and that its
    /// `active = 1` subset agrees with a full-history fold directly over
    /// immutable `_norn_fact_values`.
    ///
    /// # Errors
    /// Returns [`CellError::Encoding`] if the frontier diverges from immutable
    /// `_norn_fact_values`, `_norn_ledger`, or the full history fold.
    pub fn verify_frontier_equivalence(&self) -> Result<(), CellError> {
        self.check_live()?;
        if crate::link_claim_frontier::frontier_has_drift(self)? {
            return Err(CellError::Encoding(
                "link-claim frontier drifted from immutable _norn_fact_values or _norn_ledger"
                    .to_owned(),
            ));
        }
        let (immutable_claims, _) =
            crate::link_claim_frontier::decode_claims_from_fact_values(self.conn, Some(self))?;
        let active_rows = self.all_frontier_claims()?;
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces());
        let model = CommitModel::new(&catalog);
        let fact = |object_type: &norn_ir::ObjectTypeName, key: &str| {
            model.canonical(&FactKey::new(object_type.as_str(), key))
        };
        let all_entries: Vec<Entry> = immutable_claims
            .into_iter()
            .map(|(id, _, _, claim)| {
                let contribution = claim.contribution_id()?;
                Ok(Entry {
                    claim,
                    id,
                    contribution,
                    event_keys: Default::default(),
                    fresh: false,
                    disputed: false,
                })
            })
            .collect::<Result<_, CellError>>()?;
        let active_entries: Vec<Entry> = active_rows
            .into_iter()
            .filter(|r| r.active)
            .map(|row| Entry {
                claim: row.claim,
                id: row.claim_id,
                contribution: row.contribution_id,
                event_keys: row.event_keys,
                fresh: false,
                disputed: false,
            })
            .collect();
        let target = |e: &Entry| {
            e.claim
                .target
                .as_ref()
                .map(|t| fact(&t.object_type, &t.key))
        };
        let full_groups = group_entries_by_source_role(&all_entries, model);
        let active_groups = group_entries_by_source_role(&active_entries, model);
        for (group_key, full_group) in &full_groups {
            let full_non_sup: BTreeMap<String, Standing> = full_group
                .iter()
                .zip(standings(full_group, target))
                .filter(|(_, s)| *s != Standing::Superseded)
                .map(|(e, s)| (e.id.clone(), s))
                .collect();
            let active_group = active_groups.get(group_key).map_or(&[][..], Vec::as_slice);
            let active_non_sup: BTreeMap<String, Standing> = active_group
                .iter()
                .zip(standings(active_group, target))
                .filter(|(_, s)| *s != Standing::Superseded)
                .map(|(e, s)| (e.id.clone(), s))
                .collect();
            if full_non_sup != active_non_sup {
                return Err(CellError::Encoding(format!(
                    "frontier standings diverge from full journal for {:?}: active={active_non_sup:?}, full={full_non_sup:?}",
                    group_key
                )));
            }
        }
        Ok(())
    }

    /// Derive fresh claims for `bundles` (both upsert link claims and
    /// `kind == "delete"` withdrawal claims), filling each claim's `supersedes`
    /// from the indexed frontier (O(active) on monotonic updates; consulting
    /// inactive predecessors for that contribution only on out-of-order late
    /// arrivals).
    fn derive_bundle_fresh_claims(
        &self,
        bundles: &[MappingBundle],
    ) -> Result<Vec<(ObservedLinkClaim, [String; 5])>, CellError> {
        let mut batch_history =
            BTreeMap::<String, Vec<(norn_ir::SourceVersion, String, ObservedLinkClaim)>>::new();
        let mut fresh_out = Vec::new();
        for bundle in bundles {
            let kind = self.bundle_version_kind(bundle)?;
            let version = parse_source_version(bundle, kind)?;
            let event_keys = crate::mapping_identity::keys(&bundle.raw, &bundle.input)?
                .map(|k| norn_ir::canonical::nfc(&k));
            let mut raw_claims = bundle.link_claims()?;
            if !bundle.deletes.is_empty() {
                raw_claims.extend(self.delete_withdrawal_claims(bundle, &batch_history)?);
            }
            let mut recorded = Vec::new();
            for mut claim in raw_claims {
                let contribution = claim.contribution_id()?;
                let supersedes = self.compute_claim_supersedes(
                    &contribution,
                    &version,
                    batch_history.get(&contribution),
                )?;
                claim.supersedes = supersedes;
                let id = claim.id()?;
                recorded.push((contribution, id, claim.clone()));
                fresh_out.push((claim, event_keys.clone()));
            }
            for (contribution, id, claim) in recorded {
                batch_history
                    .entry(contribution)
                    .or_default()
                    .push((version.clone(), id, claim));
            }
        }
        Ok(fresh_out)
    }

    /// Emit `target: None` withdrawal claims for every active contribution
    /// owned by `bundle`'s source scope on the subjects deleted by `bundle`.
    fn delete_withdrawal_claims(
        &self,
        bundle: &MappingBundle,
        batch_history: &BTreeMap<String, Vec<(norn_ir::SourceVersion, String, ObservedLinkClaim)>>,
    ) -> Result<Vec<ObservedLinkClaim>, CellError> {
        let obs_id = format!("mapping-bundle:{}", bundle.digest);
        self.delete_withdrawal_claims_for_obs(bundle, &obs_id, batch_history)
    }

    /// Compute `supersedes` for a fresh claim of `contribution` at `version`.
    /// On monotonic updates (`version > all active claims`), only the active
    /// frontier is inspected (`O(1)`). On out-of-order late arrivals
    /// (`version <= some active claim`), inactive predecessors of that
    /// contribution are checked to record the maximal earlier predecessor(s).
    pub(crate) fn compute_claim_supersedes(
        &self,
        contribution: &str,
        version: &norn_ir::SourceVersion,
        batch_prior: Option<&Vec<(norn_ir::SourceVersion, String, ObservedLinkClaim)>>,
    ) -> Result<BTreeSet<String>, CellError> {
        let active = self.active_claims_for_contribution(contribution)?;
        let mut supersedes = BTreeSet::new();
        let mut all_active_smaller = true;
        for row in &active {
            if self.is_reject_carry_claim(row)? {
                supersedes.insert(row.claim_id.clone());
                continue;
            }
            let Some(earlier) = parse_verified_frontier_version(self, row)? else {
                continue;
            };
            match earlier.try_cmp(version) {
                Ok(std::cmp::Ordering::Less) => {
                    supersedes.insert(row.claim_id.clone());
                }
                Ok(_) => {
                    all_active_smaller = false;
                }
                Err(_) => return Err(CellError::MappingBundleMismatch),
            }
        }
        if let Some(prior) = batch_prior {
            for (earlier, id, _) in prior {
                match earlier.try_cmp(version) {
                    Ok(std::cmp::Ordering::Less) => {
                        supersedes.insert(id.clone());
                    }
                    Ok(_) => {
                        all_active_smaller = false;
                    }
                    Err(_) => return Err(CellError::MappingBundleMismatch),
                }
            }
        }
        if !all_active_smaller {
            // Late out-of-order arrival: include the maximal inactive claims
            // strictly older than `version` so the late claim records its
            // order over earlier history without superseding newer active claims.
            let inactive = self.inactive_claims_for_contribution(contribution)?;
            let mut older = Vec::new();
            for row in inactive {
                if self.is_reject_carry_claim(&row)? {
                    continue;
                }
                let Some(earlier) = parse_verified_frontier_version(self, &row)? else {
                    continue;
                };
                if earlier
                    .try_cmp(version)
                    .map_err(|_err| CellError::MappingBundleMismatch)?
                    == std::cmp::Ordering::Less
                {
                    older.push(row);
                }
            }
            let covered_by_older: BTreeSet<&str> = older
                .iter()
                .flat_map(|row| row.claim.supersedes.iter().map(String::as_str))
                .collect();
            for row in &older {
                if !covered_by_older.contains(row.claim_id.as_str()) {
                    supersedes.insert(row.claim_id.clone());
                }
            }
        }
        Ok(supersedes)
    }

    /// OBS-LINK-DISPUTE-1: open held-link findings from `key`.
    pub(crate) fn held_link_findings(
        &self,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<norn_ir::DataQualityFinding>, CellError> {
        if !self.indexed_source_dispute(MAIN_WORLD, object_type, key)? {
            return Ok(Vec::new());
        }
        self.held_link_findings_with(object_type, key, None)
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
        self.held_link_findings_with(object_type, key, Some(index))
    }

    fn held_link_findings_with(
        &self,
        object_type: &str,
        key: &str,
        index: Option<&SourceDisputeIndex>,
    ) -> Result<Vec<norn_ir::DataQualityFinding>, CellError> {
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces());
        let model = CommitModel::new(&catalog);
        let subject = model.canonical(&FactKey::new(object_type, key));
        let active_rows = self
            .active_frontier_for_scope(&BTreeSet::from([subject.key.clone()]), &BTreeSet::new())?;
        let entries = match index {
            Some(idx) => active_rows
                .into_iter()
                .map(|row| Entry {
                    disputed: idx.events.contains_key(&row.event_keys),
                    claim: row.claim,
                    id: row.claim_id,
                    contribution: row.contribution_id,
                    event_keys: row.event_keys,
                    fresh: false,
                })
                .collect(),
            None => self.frontier_rows_to_entries(active_rows, None)?,
        };
        let fact =
            |ty: &norn_ir::ObjectTypeName, k: &str| model.canonical(&FactKey::new(ty.as_str(), k));
        let target = |e: &Entry| {
            e.claim
                .target
                .as_ref()
                .map(|t| fact(&t.object_type, &t.key))
        };
        let present = |k: &FactKey| -> Result<bool, CellError> {
            Ok(GuardView(self)
                .concrete(&model.canonical(k))?
                .is_some_and(|c| model.is_a(&c, &k.object_type)))
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
                let dispute = match index {
                    Some(idx) => idx.events.get(&entry.event_keys).cloned(),
                    None => self.event_dispute_for_keys(&entry.event_keys, None)?,
                };
                let Some(dispute) = dispute else { continue };
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
    /// Store or encoding failures.
    pub fn observed_link_provenance(
        &self,
        key: &LinkKey,
    ) -> Result<Vec<ObservedLinkClaim>, CellError> {
        self.check_live()?;
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces());
        let model = CommitModel::new(&catalog);
        let key = model.canonical_link(key);
        let rows =
            self.frontier_claims_for_endpoints(&key.source.key, &key.link, &key.target.key)?;
        Ok(Self::distinct_claims(rows.into_iter().filter_map(|row| {
            let matches = row.claim.target.as_ref().is_some_and(|target| {
                model.canonical_link(&LinkKey::new(
                    FactKey::new(row.claim.source_type.as_str(), &row.claim.source_key),
                    row.claim.role.as_str(),
                    FactKey::new(target.object_type.as_str(), &target.key),
                )) == key
            });
            matches.then_some((row.claim_id, row.claim))
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
        let fresh_claims = self.derive_bundle_fresh_claims(bundles)?;
        let scope = self.plan_scope(bundles, &fresh_claims, model)?;
        let plan = self.plan_observed_links(&scope, fresh_claims.clone(), commit, model, None)?;
        if WIDE_SCOPE_CHECK.load(std::sync::atomic::Ordering::Relaxed) {
            let mut keys = BTreeSet::new();
            for bundle in bundles {
                for record in &bundle.records {
                    keys.insert(record.key.clone());
                }
                for delete in &bundle.deletes {
                    keys.insert(delete.key.clone());
                }
            }
            for (claim, _) in &fresh_claims {
                keys.insert(claim.source_key.clone());
                keys.extend(claim.target.as_ref().map(|t| t.key.clone()));
            }
            let wide = self.plan_observed_links(
                &ClaimScope::either(keys),
                fresh_claims,
                commit,
                model,
                None,
            )?;
            if wide != plan {
                return Err(CellError::Encoding(format!(
                    "the narrowed link plan differs from the wide one: {plan:?} != {wide:?}"
                )));
            }
        }
        Ok(plan)
    }

    /// The frontier scope a bundle's plan reads.
    fn plan_scope(
        &self,
        bundles: &[MappingBundle],
        fresh_claims: &[(ObservedLinkClaim, [String; 5])],
        model: CommitModel<'_, '_>,
    ) -> Result<ClaimScope, CellError> {
        let mut scope = ClaimScope::default();
        for record in bundles.iter().flat_map(|b| &b.records) {
            scope.sources.insert(record.key.clone());
            let key = FactKey::new(record.object_type.as_str(), &record.key);
            if GuardView(self).concrete(&model.canonical(&key))?.is_none() {
                scope.targets.insert(record.key.clone());
            }
        }
        for delete in bundles.iter().flat_map(|b| &b.deletes) {
            scope.sources.insert(delete.key.clone());
        }
        for (claim, _) in fresh_claims {
            let exclusive = model
                .link(&claim.source_type, claim.role.as_str())
                .is_none_or(|link| link.cardinality.from == norn_ir::Multiplicity::One);
            if let Some(target) = claim.target.as_ref().filter(|_| exclusive) {
                scope.targets.insert(target.key.clone());
            }
            scope.sources.insert(claim.source_key.clone());
        }
        Ok(scope)
    }

    fn frontier_rows_to_entries(
        &self,
        rows: Vec<FrontierRow>,
        excluding_dispute: Option<&str>,
    ) -> Result<Vec<Entry>, CellError> {
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let disputed = self.event_keys_disputed(&row.event_keys, excluding_dispute)?;
            entries.push(Entry {
                claim: row.claim,
                id: row.claim_id,
                contribution: row.contribution_id,
                event_keys: row.event_keys,
                fresh: false,
                disputed,
            });
        }
        Ok(entries)
    }

    /// Shared frontier link planner used by both mapping commits and
    /// `mapping.dispute.resolve` (#1028, #1549).
    pub(crate) fn plan_observed_links<E, C, T>(
        &self,
        scope: &ClaimScope,
        fresh_claims: Vec<(ObservedLinkClaim, [String; 5])>,
        commit: &CellCommit<E, C, T>,
        model: CommitModel<'_, '_>,
        excluding_dispute: Option<&str>,
    ) -> Result<LinkPlan, CellError> {
        let entries = self.collect_frontier_entries(scope, fresh_claims, excluding_dispute)?;

        let fact = |object_type: &norn_ir::ObjectTypeName, key: &str| {
            model.canonical(&FactKey::new(object_type.as_str(), key))
        };
        let target = |e: &Entry| {
            e.claim
                .target
                .as_ref()
                .map(|t| fact(&t.object_type, &t.key))
        };
        let groups = group_entries_by_source_role(&entries, model);
        let present = |key: &FactKey| self.present_after(model, commit, key);
        let mut live: BTreeMap<FactKey, BTreeSet<LinkKey>> = BTreeMap::new();
        let mut links = BTreeMap::new();
        let mut sealed = Vec::new();
        for ((source, role), group) in &groups {
            let ranked = standings(group, target);
            let current: BTreeSet<_> = group
                .iter()
                .zip(&ranked)
                .filter(|(_, s)| **s == Standing::Current)
                .filter_map(|(e, _)| target(e))
                .collect();
            if current.len() > 1
                && model
                    .link(&source.object_type, role.as_str())
                    .is_some_and(|link| link.cardinality.to == norn_ir::Multiplicity::One)
            {
                return Err(CellError::UnsupportedMappingProfile);
            }
            let before = links.len();
            // OBS-LINK-DISPUTE-1: a contribution with ANY non-superseded
            // disputed claim stays frozen even if a sibling non-disputed
            // carry-forward claim in the same contribution agrees on its target.
            let disputed_contributions: BTreeSet<&str> = group
                .iter()
                .zip(&ranked)
                .filter(|(e, s)| **s != Standing::Superseded && e.disputed)
                .map(|(e, _)| e.contribution.as_str())
                .collect();
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
                        if !entry.disputed
                            && !disputed_contributions.contains(entry.contribution.as_str())
                            && !exists
                            && present(source)?
                            && present(&to)? =>
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
        let v = self.link_version(
            MAIN_WORLD,
            key.source.object_type.as_str(),
            &key.link,
            &key.source.key,
            key.target.object_type.as_str(),
            &key.target.key,
        )?;
        Ok(LinkWrite {
            expected: FactVersion(v),
            key,
            mutation,
        })
    }

    /// Writes this bundle's claims and prepares active frontier claims.
    pub(crate) fn prepare_mapping_links(
        &self,
        bundles: &[MappingBundle],
        catalog: &TypeCatalog<'_>,
        turn: &norn_kernel::DecisionTurn,
        commit: &mut MappingCommit,
        authority: &mut ObservationAuthorization,
    ) -> Result<(), CellError> {
        let plan = self.mapping_link_plan(bundles, commit)?;
        self.apply_link_plan(plan, catalog, turn, commit, authority)
    }

    /// Seal a computed [`LinkPlan`] into `commit` and `authority`.
    pub(crate) fn apply_link_plan(
        &self,
        plan: LinkPlan,
        catalog: &TypeCatalog<'_>,
        turn: &norn_kernel::DecisionTurn,
        commit: &mut MappingCommit,
        authority: &mut ObservationAuthorization,
    ) -> Result<(), CellError> {
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

    fn collect_frontier_entries(
        &self,
        scope: &ClaimScope,
        fresh_claims: Vec<(ObservedLinkClaim, [String; 5])>,
        excluding_dispute: Option<&str>,
    ) -> Result<Vec<Entry>, CellError> {
        let mut closed_sources = scope.sources.clone();
        for (claim, _) in &fresh_claims {
            if touches(claim, scope) {
                closed_sources.insert(claim.source_key.clone());
            }
        }
        let active_rows = self.active_frontier_for_scope(&closed_sources, &scope.targets)?;
        let mut entries = self.frontier_rows_to_entries(active_rows, excluding_dispute)?;
        let mut existing_ids: BTreeSet<String> = entries.iter().map(|e| e.id.clone()).collect();
        for (claim, event_keys) in fresh_claims {
            let id = claim.id()?;
            if !existing_ids.insert(id.clone()) {
                continue;
            }
            let contribution = claim.contribution_id()?;
            let disputed = self.event_keys_disputed(&event_keys, excluding_dispute)?;
            entries.push(Entry {
                claim,
                id,
                contribution,
                event_keys,
                fresh: true,
                disputed,
            });
        }
        let touched_groups: BTreeSet<_> = entries
            .iter()
            .filter(|e| touches(&e.claim, scope))
            .map(|e| (e.claim.source_key.clone(), e.claim.role.clone()))
            .collect();
        entries.retain(|e| {
            touched_groups.contains(&(e.claim.source_key.clone(), e.claim.role.clone()))
        });
        Ok(entries)
    }
}

fn group_entries_by_source_role<'a>(
    list: &'a [Entry],
    model: CommitModel<'_, '_>,
) -> BTreeMap<(FactKey, norn_ir::LinkId), Vec<&'a Entry>> {
    let mut groups = BTreeMap::<_, Vec<&'a Entry>>::new();
    for entry in list {
        let source = model.canonical(&FactKey::new(
            entry.claim.source_type.as_str(),
            &entry.claim.source_key,
        ));
        groups
            .entry((source, entry.claim.role.clone()))
            .or_default()
            .push(entry);
    }
    groups
}
