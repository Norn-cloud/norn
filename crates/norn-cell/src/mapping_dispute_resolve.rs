//! Governed `mapping.dispute.resolve` action (#1549, OBS-LINK-DISPUTE-1, FEED-CONFLICT).
//!
//! Resolves one pending `mapping-conflict:` or `mapping-identity:` dispute in
//! a single atomic, receipted commit:
//!
//! - `accept-first`: keeps the originally accepted capture, releases any edges
//!   held by this dispute, and unquarantines affected subjects.
//! - `accept-second-as-correction`: applies the incoming candidate's canonical
//!   record snapshots/deletes and fresh superseding link claims, adding the
//!   candidate's edges and removing old edges that no other source supports.
//! - `reject`: withdraws active link contributions from the disputed source
//!   event (`target: None`), removing rejected-only edges while preserving
//!   undisputed multi-source edges.
//!
//! Immutable conflict/identity evidence remains retained in `_norn_inbox`
//! (marked consumed at the resolution commit sequence, never deleted), so
//! redelivering the same conflicting event does not reopen the dispute while a
//! new distinct conflicting payload opens a fresh dispute.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::mapping::ObservedLinkClaim;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{
    CellCommit, CommitModel, CommitView, DecisionTurn, ObservationAuthorization, ObservationEvent,
    Observed, TypeCatalog,
};
use serde::{Deserialize, Serialize};

use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::host::{GuardView, SqliteCell};
use crate::mapping_bundle::MappingBundle;
use crate::mapping_conflict::MappingConflict;
use crate::mapping_identity::IdentityEvidence;
use crate::mapping_links::{ClaimScope, LinkPlan};
use crate::read::StoreReader;
use crate::store::Consumption;

/// Governed action identifier for mapping dispute resolution (#1549).
pub const MAPPING_DISPUTE_RESOLVE_ACTION: &str = "mapping.dispute.resolve";

/// Operator decision for resolving a mapping dispute (`FEED-CONFLICT` / identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisputeDecision {
    /// Keep the originally accepted capture and release its held edges.
    AcceptFirst,
    /// Apply the second (incoming) candidate as an explicit correction.
    AcceptSecondAsCorrection,
    /// Reject the disputed event and withdraw its active link contributions.
    Reject,
}

impl DisputeDecision {
    /// Canonical wire spelling recorded in `ObservationEvent::DisputeResolved`
    /// and `_norn_dispute_resolution`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AcceptFirst => "accept-first",
            Self::AcceptSecondAsCorrection => "accept-second-as-correction",
            Self::Reject => "reject",
        }
    }

    /// Parse from wire spelling.
    #[must_use]
    pub fn parse_wire(value: &str) -> Option<Self> {
        match value {
            "accept-first" => Some(Self::AcceptFirst),
            "accept-second-as-correction" => Some(Self::AcceptSecondAsCorrection),
            "reject" => Some(Self::Reject),
            _ => None,
        }
    }
}

/// Caller input to [`SqliteCell::resolve_mapping_dispute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisputeResolutionInput {
    /// Inbox observation id of the pending dispute (`mapping-conflict:...` or `mapping-identity:...`).
    pub dispute_id: String,
    /// The operator decision to apply.
    pub decision: DisputeDecision,
    /// Authenticated operator / principal id authorizing the resolution.
    pub actor: String,
    /// Non-empty audit reason recorded in the ledger and `_norn_dispute_resolution`.
    pub reason: String,
}

/// Result returned when [`SqliteCell::resolve_mapping_dispute`] commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisputeResolutionOutcome {
    /// Resolved dispute observation id.
    pub dispute_id: String,
    /// Applied decision.
    pub decision: DisputeDecision,
    /// Commit sequence of the atomic resolution commit.
    pub commit_seq: u64,
    /// Receipt hash of the atomic resolution commit.
    pub receipt_hash: String,
}

/// Unforgeable crate-private proof minted exclusively by
/// [`SqliteCell::resolve_mapping_dispute`] and verified inside the commit
/// transaction by `link_guard::verify_links`. Prevents generic
/// `CellHost::commit`, `SqliteCell::attributed`, `CellHost::commit_observed`,
/// and direct `CellStore::commit` callers from forging a dispute resolution.
#[derive(Clone, Copy)]
pub(crate) struct DisputeResolveProof<'a> {
    dispute_id: &'a str,
    decision: DisputeDecision,
    actor: &'a str,
    reason: &'a str,
}

impl<'a> DisputeResolveProof<'a> {
    pub(crate) const fn dispute_id(&self) -> &'a str {
        self.dispute_id
    }

    pub(crate) const fn decision(&self) -> DisputeDecision {
        self.decision
    }

    pub(crate) const fn actor(&self) -> &'a str {
        self.actor
    }

    pub(crate) const fn reason(&self) -> &'a str {
        self.reason
    }
}

/// Decoded, verified pending dispute evidence ready for planning or verification.
pub(crate) enum VerifiedDispute {
    Conflict(Box<MappingConflict>),
    Identity(Box<IdentityEvidence>),
}

impl VerifiedDispute {
    pub(crate) fn incoming_bundle(&self) -> &MappingBundle {
        match self {
            Self::Conflict(conflict) => &conflict.incoming,
            Self::Identity(identity) => &identity.bundle,
        }
    }

    fn event_keys(&self) -> Result<[String; 5], CellError> {
        let bundle = self.incoming_bundle();
        Ok(crate::mapping_identity::keys(&bundle.raw, &bundle.input)?
            .map(|k| norn_ir::canonical::nfc(&k)))
    }
}

impl<S: SqlEngine> StoreReader<'_, S> {
    /// Every retained conflict (both open and resolved), with its inbox
    /// position and optional resolution commit sequence (#1549).
    ///
    /// # Errors
    /// Corrupt evidence, missing retained pins, or store failure.
    pub fn retained_source_conflicts(
        &self,
    ) -> Result<Vec<(u64, MappingConflict, Option<u64>)>, CellError> {
        self.check_live()?;
        self.load_source_conflicts(false)
    }

    /// Authenticate the prior pre-v13 consuming commit `prev_seq` for a legacy
    /// generically-consumed `dispute_id` against BOTH its durable receipt
    /// (`receipt.body.consumed == Some(Observation { dispute_id })`) and its
    /// hash-verified `_norn_export_journal` record (`storage_version < 13`,
    /// `record.receipt == receipt`, `consumed_input`), returning the
    /// authenticated `Observed` JSON payload. Shared by both live read
    /// verification and `mapping.dispute.resolve` write admission.
    pub(crate) fn authenticated_prior_consumed_observed(
        &self,
        dispute_id: &str,
        prev_seq: u64,
    ) -> Result<RuntimeValue, CellError> {
        let corrupt = |msg: &str| CellError::LedgerCorrupt {
            seq: prev_seq,
            reason: format!("unverified prior consumption for {dispute_id}: {msg}"),
        };
        let Some(prior_receipt) = self.receipt_at(prev_seq)? else {
            return Err(corrupt("missing prior consuming receipt"));
        };
        let expected_consumed = Consumption::Observation {
            observation_id: dispute_id.to_owned(),
        };
        if prior_receipt.body.world != crate::MAIN_WORLD
            || prior_receipt.body.consumed.as_ref() != Some(&expected_consumed)
        {
            return Err(corrupt(
                "prior consuming receipt does not consume this dispute",
            ));
        }
        let has_journal = self
            .conn
            .query_optional(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_export_journal'",
                crate::engine::params![],
                |_row| Ok(()),
            )?
            .is_some();
        if !has_journal {
            return Err(corrupt("missing export journal table"));
        }
        let Some(prior_export) = crate::export_journal::read_record(self.conn, prev_seq)? else {
            return Err(corrupt("missing prior consuming export journal record"));
        };
        if prior_export.receipt != prior_receipt
            || prior_export.storage_version.unwrap_or(1)
                >= crate::link_claim_frontier::FRONTIER_STORAGE_VERSION
        {
            return Err(corrupt(
                "prior consuming export journal disagrees with pre-v13 receipt",
            ));
        }
        let Some(prior_consumed) = prior_export.consumed_input.as_ref() else {
            return Err(corrupt("prior consuming journal missing consumed_input"));
        };
        let crate::export_journal::ConsumedInput::Observation {
            observation_id,
            observed,
            ..
        } = crate::export_journal::decode_observation_input(prior_consumed)
            .map_err(|err| corrupt(&format!("invalid prior consumed input: {err}")))?
        else {
            return Err(corrupt("prior consuming journal is not an observation"));
        };
        if observation_id != dispute_id {
            return Err(corrupt("prior consumed observation id mismatch"));
        }
        Ok(observed)
    }

    pub(crate) fn load_source_conflicts(
        &self,
        pending_only: bool,
    ) -> Result<Vec<(u64, MappingConflict, Option<u64>)>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"position\", \"observation_id\", \"observed\" \
                 FROM \"{}\" \
                 WHERE \"observation_id\" LIKE 'mapping-conflict:%' \
                 ORDER BY \"position\"",
                crate::system::INBOX,
            ),
            crate::engine::params![],
        )?;
        let mut conflicts = Vec::new();
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let position = crate::read::to_u64(row.get(0)?, "inbox position")?;
            let stored_id: String = row.get(1)?;
            let encoded: String = row.get(2)?;
            let resolved_seq = self.verified_dispute_resolution_seq(&stored_id, None)?;
            if pending_only && resolved_seq.is_some() {
                continue;
            }
            let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
            let conflict: MappingConflict =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            self.recheck_mapping_conflict(&conflict)?;
            if observed.id.0 != stored_id
                || stored_id != format!("mapping-conflict:{}", conflict.finding.id)
            {
                return Err(CellError::MappingBundleMismatch);
            }
            conflicts.push((position, conflict, resolved_seq));
        }
        Ok(conflicts)
    }

    /// Active dispute rows covering NFC `keys`, optionally excluding one
    /// dispute row currently being resolved in this commit (`#1549`).
    pub(crate) fn event_dispute_for_keys(
        &self,
        keys: &[String; 5],
        excluding_dispute: Option<&str>,
    ) -> Result<Option<crate::mapping_conflict::EventDispute>, CellError> {
        self.with_source_dispute_index(|index| {
            let dispute = index.events.get(keys)?;
            let Some(exclude) = excluding_dispute else {
                return Some(dispute.clone());
            };
            let rows: BTreeSet<String> = dispute
                .rows
                .iter()
                .filter(|row| row.as_str() != exclude)
                .cloned()
                .collect();
            if rows.is_empty() {
                None
            } else {
                Some(crate::mapping_conflict::EventDispute {
                    position: dispute.position,
                    rows,
                    first_seen: dispute.first_seen.clone(),
                })
            }
        })
    }

    /// Whether NFC `keys` are covered by any active dispute (optionally
    /// excluding one dispute row currently being resolved).
    pub(crate) fn event_keys_disputed(
        &self,
        keys: &[String; 5],
        excluding_dispute: Option<&str>,
    ) -> Result<bool, CellError> {
        Ok(self
            .event_dispute_for_keys(keys, excluding_dispute)?
            .is_some())
    }

    /// Whether `bundle`'s canonical NFC source event `(connector, partition,
    /// source, source_id, event_id)` was already resolved via
    /// `accept-second-as-correction` or `reject` (so neither the original
    /// capture nor a recapture under a newer definition pin can later land and
    /// undo the resolution).
    pub(crate) fn bundle_superseded_by_dispute_resolution(
        &self,
        bundle: &MappingBundle,
    ) -> Result<bool, CellError> {
        let target_keys = crate::mapping_identity::keys(&bundle.raw, &bundle.input)?
            .map(|k| norn_ir::canonical::nfc(&k));
        let rows = self.conn.query(
            &format!(
                "SELECT \"dispute_id\", \"decision\", \"actor\", \"reason\", \
                        \"commit_seq\", \"receipt_hash\" \
                 FROM \"{}\" ORDER BY \"commit_seq\", \"dispute_id\"",
                crate::link_claim_frontier::DISPUTE_RESOLUTION,
            ),
            crate::engine::params![],
        )?;
        let mut superseded = false;
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let rec = crate::mapping_conflict_scoped::decode_dispute_resolution_row(&row)?;
            if !matches!(
                rec.decision.as_str(),
                "accept-second-as-correction" | "reject"
            ) {
                self.verify_dispute_resolution_record(&rec)?;
                continue;
            }
            let resolved_bundle = self.verified_dispute_resolution_bundle(&rec)?;
            if !superseded {
                let resolved_keys =
                    crate::mapping_identity::keys(&resolved_bundle.raw, &resolved_bundle.input)?
                        .map(|k| norn_ir::canonical::nfc(&k));
                if resolved_keys == target_keys {
                    superseded = true;
                }
            }
        }
        Ok(superseded)
    }

    /// Load and cryptographically recheck an unresolved dispute row from
    /// `_norn_inbox`, returning `(observed, dispute, is_pending)`.
    ///
    /// Returns `NotPending` if the dispute does not exist or already has a
    /// recorded governed resolution in `_norn_dispute_resolution`. Pre-v13
    /// generically-consumed rows without a governed resolution are returned
    /// with `is_pending == false` so operators can still resolve them through
    /// `mapping.dispute.resolve` without double-consuming the inbox row.
    pub(crate) fn load_verified_unresolved_dispute(
        &self,
        dispute_id: &str,
    ) -> Result<(Observed<RuntimeValue>, VerifiedDispute, bool), CellError> {
        if !dispute_id.starts_with("mapping-conflict:")
            && !dispute_id.starts_with("mapping-identity:")
        {
            return Err(CellError::UnsupportedMappingProfile);
        }
        let not_pending = || {
            CellError::NotPending(Consumption::Observation {
                observation_id: dispute_id.to_owned(),
            })
        };
        if self
            .verified_dispute_resolution_seq(dispute_id, None)?
            .is_some()
        {
            return Err(not_pending());
        }
        let Some((encoded, consumed_seq)) = self.conn.query_optional(
            &format!(
                "SELECT \"observed\", \"consumed_commit_seq\" FROM \"{}\" WHERE \"observation_id\" = ?1",
                crate::system::INBOX
            ),
            crate::engine::params![dispute_id],
            |row| Ok((row.get::<String>(0)?, row.get::<Option<i64>>(1)?)),
        )? else {
            return Err(not_pending());
        };
        let is_pending = consumed_seq.is_none();
        let observed: Observed<RuntimeValue> = if let Some(prev_i64) = consumed_seq {
            let prev_seq = u64::try_from(prev_i64).map_err(|err| CellError::LedgerCorrupt {
                seq: 0,
                reason: format!("negative consumed_commit_seq for {dispute_id}: {err}"),
            })?;
            let auth_val = self.authenticated_prior_consumed_observed(dispute_id, prev_seq)?;
            norn_ir::runtime::decode(&auth_val).map_err(CellError::Encoding)?
        } else {
            serde_json::from_str(&encoded)?
        };
        if observed.id.0 != dispute_id {
            return Err(CellError::MappingBundleMismatch);
        }
        if dispute_id.starts_with("mapping-conflict:") {
            let conflict: MappingConflict =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            if dispute_id != format!("mapping-conflict:{}", conflict.finding.id) {
                return Err(CellError::MappingBundleMismatch);
            }
            self.recheck_mapping_conflict(&conflict)?;
            Ok((
                observed,
                VerifiedDispute::Conflict(Box::new(conflict)),
                is_pending,
            ))
        } else {
            let identity: IdentityEvidence =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            self.recheck_identity_evidence(&identity, dispute_id)?;
            Ok((
                observed,
                VerifiedDispute::Identity(Box::new(identity)),
                is_pending,
            ))
        }
    }

    /// Recheck a `mapping-conflict:` row directly against a validated pending
    /// first bundle that has not yet been inserted into `_norn_inbox` (e.g.
    /// carried in the same export record's `pending_observations`), without any
    /// SQL write or savepoint.
    pub(crate) fn recheck_mapping_conflict_against(
        &self,
        conflict: &MappingConflict,
        accepted_id: &str,
        accepted_position: u64,
        accepted: &MappingBundle,
    ) -> Result<(), CellError> {
        self.recheck_mapping_bundle(&conflict.incoming)?;
        let keys = crate::mapping_identity::keys(&conflict.incoming.raw, &conflict.incoming.input)?;
        if conflict.accepted_bundle_id != accepted_id
            || crate::mapping_identity::keys(&accepted.raw, &accepted.input)? != keys
        {
            return Err(CellError::MappingBundleMismatch);
        }
        // Verify `accepted` at `accepted_position` strictly precedes every
        // existing `mapping-bundle:` for this source event in `_norn_inbox`,
        // proving it is genuinely the first capture even if a later capture
        // matching `conflict.incoming` is already in `_norn_inbox`.
        let [connector, partition, source, source_id, event_id] = keys;
        let min_existing: Option<i64> = self
            .conn
            .query_optional(
                &format!(
                    "SELECT MIN(\"position\") FROM \"{}\" \
                     WHERE \"observation_id\" LIKE 'mapping-bundle:%' \
                       AND json_extract(\"observed\", '$.input.input.connectorInstance') = ?1 \
                       AND json_extract(\"observed\", '$.input.input.partition') = ?2 \
                       AND json_extract(\"observed\", '$.input.raw.source') = ?3 \
                       AND json_extract(\"observed\", '$.input.raw.source_id') = ?4 \
                       AND json_extract(\"observed\", '$.input.raw.event_id') = ?5",
                    crate::system::INBOX,
                ),
                crate::engine::params![connector, partition, source, source_id, event_id],
                |row| row.get::<Option<i64>>(0),
            )?
            .flatten();
        if let Some(pos) = min_existing
            && crate::read::to_u64(pos, "inbox position")? <= accepted_position
        {
            return Err(CellError::MappingBundleMismatch);
        }
        if build_mapping_conflict(accepted_id.to_owned(), accepted, &conflict.incoming)?.as_ref()
            != Some(conflict)
        {
            return Err(CellError::MappingBundleMismatch);
        }
        Ok(())
    }

    /// Whether `row` is a synthetic carry-forward claim written by a prior
    /// `DisputeDecision::Reject`. Synthetic rejection claims hold no source
    /// version authority of their own and are unconditionally superseded by
    /// any subsequent real claim or subsequent rejection in that contribution.
    pub(crate) fn is_reject_carry_claim(
        &self,
        row: &crate::link_claim_frontier::FrontierRow,
    ) -> Result<bool, CellError> {
        Ok((row.claim.observation_id.starts_with("mapping-conflict:")
            || row.claim.observation_id.starts_with("mapping-identity:"))
            && self
                .dispute_resolution(&row.claim.observation_id)?
                .is_some_and(|rec| rec.decision == "reject"))
    }

    /// Compute the exact [`LinkPlan`] for resolving `dispute` with `decision`
    /// inside a commit whose object writes are already in `commit`.
    pub(crate) fn dispute_resolution_link_plan<E, C, T>(
        &self,
        dispute_id: &str,
        dispute: &VerifiedDispute,
        decision: DisputeDecision,
        commit: &CellCommit<E, C, T>,
    ) -> Result<LinkPlan, CellError> {
        let catalog = TypeCatalog::new(self.model().object_types())
            .with_interfaces(self.model().interfaces())
            .with_property_types(self.model().property_types());
        let model = CommitModel::new(&catalog);
        let event_keys = dispute.event_keys()?;
        let incoming = dispute.incoming_bundle();

        let mut scope = ClaimScope::default();
        let active_event_claims = self.active_claims_for_event_keys(&event_keys)?;
        for row in &active_event_claims {
            scope.sources.insert(row.claim.source_key.clone());
        }
        for record in &incoming.records {
            scope.sources.insert(record.key.clone());
            let key = norn_kernel::FactKey::new(record.object_type.as_str(), &record.key);
            if GuardView(self).concrete(&model.canonical(&key))?.is_none() {
                scope.targets.insert(record.key.clone());
            }
        }
        for delete in &incoming.deletes {
            scope.sources.insert(delete.key.clone());
        }
        if let VerifiedDispute::Conflict(conflict) = dispute
            && let Ok(accepted) = self.inbox_bundle(&conflict.accepted_bundle_id)
        {
            for record in &accepted.records {
                scope.sources.insert(record.key.clone());
            }
            for delete in &accepted.deletes {
                scope.sources.insert(delete.key.clone());
            }
        }

        let fresh_claims = match decision {
            DisputeDecision::AcceptFirst => Vec::new(),
            DisputeDecision::AcceptSecondAsCorrection => self.correction_fresh_claims(
                dispute_id,
                incoming,
                &event_keys,
                &active_event_claims,
                model,
                &mut scope,
            )?,
            DisputeDecision::Reject => self.reject_fresh_claims(
                dispute_id,
                &event_keys,
                &active_event_claims,
                model,
                &mut scope,
            )?,
        };
        self.plan_observed_links(&scope, fresh_claims, commit, model, Some(dispute_id))
    }

    fn correction_fresh_claims(
        &self,
        dispute_id: &str,
        incoming: &MappingBundle,
        event_keys: &[String; 5],
        active_event_claims: &[crate::link_claim_frontier::FrontierRow],
        model: CommitModel<'_, '_>,
        scope: &mut ClaimScope,
    ) -> Result<Vec<(ObservedLinkClaim, [String; 5])>, CellError> {
        let mut fresh_claims = Vec::new();
        // OBS-LINK-CONTRIBUTION-1: incoming correction claims supersede
        // strictly older claims per source-version ordering plus any active
        // claim from THIS disputed event (never erasing a newer active claim).
        let incoming_version = crate::mapping_links::parse_source_version(
            incoming,
            self.bundle_version_kind(incoming)?,
        )?;
        let correction_supersedes = |contribution: &str| -> Result<BTreeSet<String>, CellError> {
            let mut supersedes =
                self.compute_claim_supersedes(contribution, &incoming_version, None)?;
            for row in active_event_claims {
                if row.contribution_id != contribution {
                    continue;
                }
                let is_newer = crate::mapping_delete::parse_verified_frontier_version(self, row)?
                    .and_then(|v| v.try_cmp(&incoming_version).ok())
                    == Some(std::cmp::Ordering::Greater);
                if !is_newer {
                    supersedes.insert(row.claim_id.clone());
                }
            }
            Ok(supersedes)
        };
        let mut covered_contributions = BTreeSet::new();
        let incoming_claims = incoming.link_claims()?;
        let suppressed_by_delete = !incoming.records.is_empty()
            && !incoming_claims.is_empty()
            && crate::mapping_delete::has_newer_or_incomparable_landed_delete(self, incoming)?;
        let stale_delete = !incoming.deletes.is_empty()
            && crate::mapping_delete::has_newer_or_incomparable_landed_source_version(
                self, incoming,
            )?;
        for mut claim in incoming_claims {
            if suppressed_by_delete {
                break;
            }
            claim.observation_id = dispute_id.to_owned();
            let contribution = claim.contribution_id()?;
            covered_contributions.insert(contribution.clone());
            claim.supersedes = correction_supersedes(&contribution)?;
            scope.sources.insert(claim.source_key.clone());
            let exclusive = model
                .link(&claim.source_type, claim.role.as_str())
                .is_none_or(|link| link.cardinality.from == norn_ir::Multiplicity::One);
            if let Some(target) = claim.target.as_ref().filter(|_| exclusive) {
                scope.targets.insert(target.key.clone());
            }
            fresh_claims.push((claim, event_keys.clone()));
        }
        for mut withdrawal in self.delete_withdrawal_claims_for_obs(
            incoming,
            dispute_id,
            &std::collections::BTreeMap::new(),
        )? {
            let contribution = withdrawal.contribution_id()?;
            if !covered_contributions.insert(contribution.clone()) {
                continue;
            }
            withdrawal.supersedes = correction_supersedes(&contribution)?;
            scope.sources.insert(withdrawal.source_key.clone());
            fresh_claims.push((withdrawal, event_keys.clone()));
        }
        if stale_delete || suppressed_by_delete {
            return Ok(fresh_claims);
        }
        let mut withdrawn = BTreeSet::new();
        for row in active_event_claims {
            if covered_contributions.contains(&row.contribution_id)
                || !withdrawn.insert(row.contribution_id.clone())
            {
                continue;
            }
            let supersedes = correction_supersedes(&row.contribution_id)?;
            let withdrawal = ObservedLinkClaim {
                observation_id: dispute_id.to_owned(),
                source_scope: row.claim.source_scope.clone(),
                source_type: row.claim.source_type.clone(),
                source_key: row.claim.source_key.clone(),
                role: row.claim.role.clone(),
                target: None,
                provenance: row.claim.provenance.clone(),
                supersedes,
            };
            scope.sources.insert(withdrawal.source_key.clone());
            fresh_claims.push((withdrawal, event_keys.clone()));
        }
        Ok(fresh_claims)
    }

    fn reject_fresh_claims(
        &self,
        dispute_id: &str,
        event_keys: &[String; 5],
        active_event_claims: &[crate::link_claim_frontier::FrontierRow],
        model: CommitModel<'_, '_>,
        scope: &mut ClaimScope,
    ) -> Result<Vec<(ObservedLinkClaim, [String; 5])>, CellError> {
        let mut fresh_claims = Vec::new();
        let mut by_contrib: BTreeMap<String, Vec<&crate::link_claim_frontier::FrontierRow>> =
            BTreeMap::new();
        for row in active_event_claims {
            by_contrib
                .entry(row.contribution_id.clone())
                .or_default()
                .push(row);
        }
        for (contribution_id, disputed_rows) in by_contrib {
            let Some(first) = disputed_rows.first() else {
                continue;
            };
            let mut supersedes: BTreeSet<String> =
                disputed_rows.iter().map(|r| r.claim_id.clone()).collect();
            let all_active = self.active_claims_for_contribution(&contribution_id)?;
            for r in &all_active {
                if !supersedes.contains(&r.claim_id) && self.is_reject_carry_claim(r)? {
                    supersedes.insert(r.claim_id.clone());
                }
            }
            let surviving = self.surviving_predecessors_after_reject(
                &contribution_id,
                all_active,
                &supersedes,
            )?;
            let mut any_disputed = false;
            for r in &surviving {
                if self
                    .event_dispute_for_keys(&r.event_keys, Some(dispute_id))?
                    .is_some()
                {
                    any_disputed = true;
                    break;
                }
            }
            let surviving_targets: BTreeSet<_> = surviving
                .iter()
                .map(|r| {
                    r.claim
                        .target
                        .as_ref()
                        .map(|t| (t.object_type.clone(), t.key.clone()))
                })
                .collect();
            let replacement_target = if !any_disputed && surviving_targets.len() == 1 {
                surviving.first().and_then(|r| r.claim.target.clone())
            } else {
                None
            };
            let exclusive = model
                .link(&first.claim.source_type, first.claim.role.as_str())
                .is_none_or(|link| link.cardinality.from == norn_ir::Multiplicity::One);
            if let Some(target) = replacement_target.as_ref().filter(|_| exclusive) {
                scope.targets.insert(target.key.clone());
            }
            scope.sources.insert(first.claim.source_key.clone());
            fresh_claims.push((
                ObservedLinkClaim {
                    observation_id: dispute_id.to_owned(),
                    source_scope: first.claim.source_scope.clone(),
                    source_type: first.claim.source_type.clone(),
                    source_key: first.claim.source_key.clone(),
                    role: first.claim.role.clone(),
                    target: replacement_target,
                    provenance: first.claim.provenance.clone(),
                    supersedes,
                },
                event_keys.clone(),
            ));
        }
        Ok(fresh_claims)
    }
}

/// Construct a [`MappingConflict`] between `accepted` and `incoming` when
/// their immutable source event contents differ, or `Ok(None)` if identical.
pub(crate) fn build_mapping_conflict(
    accepted_id: String,
    accepted: &MappingBundle,
    incoming: &MappingBundle,
) -> Result<Option<MappingConflict>, CellError> {
    let old = crate::mapping_conflict::content(accepted)?;
    let new = crate::mapping_conflict::content(incoming)?;
    if old == new {
        return Ok(None);
    }
    let evidence = norn_ir::canonical::domain_hash_hex(
        b"norn.source.conflict.v1\0",
        &(
            &incoming.input.connector_instance,
            &incoming.input.partition,
            crate::mapping_conflict::field(incoming, "source")?,
            crate::mapping_conflict::field(incoming, "source_id")?,
            crate::mapping_conflict::field(incoming, "event_id")?,
            old,
            new,
        ),
    )?;
    let subjects = accepted
        .records
        .iter()
        .chain(&incoming.records)
        .map(|r| norn_ir::DataQualitySubject::new(r.object_type.clone(), &r.key))
        .chain(
            accepted
                .deletes
                .iter()
                .chain(&incoming.deletes)
                .map(|d| norn_ir::DataQualitySubject::new(d.object_type.clone(), &d.key)),
        )
        .collect();
    let observation_ids = vec![
        accepted_id.clone(),
        format!("mapping-bundle:{}", incoming.digest),
    ];
    let lineage = accepted
        .records
        .iter()
        .chain(&incoming.records)
        .map(|r| &r.provenance)
        .chain(
            accepted
                .deletes
                .iter()
                .chain(&incoming.deletes)
                .map(|d| &d.provenance),
        )
        .map(|p| norn_ir::DataQualityLineage::new(p.clone(), observation_ids.clone()))
        .collect();
    let finding = norn_ir::DataQualityFinding::v2(
        norn_ir::DataQualityRuleRef::Builtin {
            id: "core.boundary.import_finding".into(),
            version: "1".into(),
        },
        subjects,
        norn_ir::DataQualitySeverity::Error,
        "conflicting immutable source event content",
        norn_ir::DataQualityOutcome::NotSatisfied,
        lineage,
        incoming.input.recorded_at.clone(),
        incoming.input.recorded_at.clone(),
        norn_ir::DataQualityFindingStatus::Open,
    )?
    .with_evidence_digest(evidence)?;
    Ok(Some(MappingConflict {
        accepted_bundle_id: accepted_id,
        incoming: incoming.clone(),
        finding,
    }))
}

impl<S: SqlEngine> SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, S> {
    /// Execute the governed `mapping.dispute.resolve` action (#1549) atomically.
    ///
    /// Hosts must authorize `MAPPING_DISPUTE_RESOLVE_ACTION` for `input.actor`
    /// before invoking this method.
    ///
    /// # Errors
    /// Refuses blank actor/reason, non-pending or unknown disputes, tampered
    /// dispute evidence, definition-version mismatch, or commit admission errors.
    pub fn resolve_mapping_dispute(
        &mut self,
        input: DisputeResolutionInput,
    ) -> Result<DisputeResolutionOutcome, CellError> {
        if input.actor.trim().is_empty() || input.reason.trim().is_empty() {
            return Err(CellError::Encoding(
                "mapping.dispute.resolve requires non-empty actor and reason".to_owned(),
            ));
        }
        let (observed, dispute, is_pending) = self
            .store()
            .read()
            .load_verified_unresolved_dispute(&input.dispute_id)?;
        let incoming = dispute.incoming_bundle();
        if input.decision == DisputeDecision::AcceptSecondAsCorrection {
            if self.store().read().active_definitions_version()?
                != Some(incoming.definitions_version)
            {
                return Err(CellError::MappingBundleMismatch);
            }
            if !incoming.diagnostics.is_empty() {
                return Err(CellError::UnsupportedMappingProfile);
            }
        }

        let (commit, authorization) =
            self.prepare_dispute_resolution_commit(&input, &observed, &dispute, is_pending)?;
        let references = authorization.reference_reads(&commit).to_vec();
        let mut commit = commit;
        authorization.append_findings(&mut commit);
        let proof = DisputeResolveProof {
            dispute_id: &input.dispute_id,
            decision: input.decision,
            actor: &input.actor,
            reason: &input.reason,
        };
        let commit_receipt = self
            .commit_admitted(
                commit,
                crate::host::CommitOptions {
                    authorization: Some(&authorization),
                    references: &references,
                    dispute_resolve: Some(proof),
                    ..Default::default()
                },
            )
            .map_err(|e| CellError::ObservationCommit(Box::new(e)))?;
        let stored_receipt = self
            .store()
            .read()
            .receipt_at(commit_receipt.seq.0)?
            .ok_or_else(|| CellError::Engine("missing dispute resolution receipt".to_owned()))?;
        Ok(DisputeResolutionOutcome {
            dispute_id: input.dispute_id,
            decision: input.decision,
            commit_seq: commit_receipt.seq.0,
            receipt_hash: stored_receipt.hash,
        })
    }

    #[allow(clippy::type_complexity, reason = "internal commit preparation pair")]
    fn prepare_dispute_resolution_commit(
        &self,
        input: &DisputeResolutionInput,
        observed: &Observed<RuntimeValue>,
        dispute: &VerifiedDispute,
        is_pending: bool,
    ) -> Result<
        (
            CellCommit<ObservationEvent, RuntimeObject, RuntimeObject>,
            ObservationAuthorization,
        ),
        CellError,
    > {
        let incoming = dispute.incoming_bundle();
        let model = self.store().model().clone();
        let catalog = TypeCatalog::new(model.object_types())
            .with_interfaces(model.interfaces())
            .with_property_types(model.property_types())
            .with_governing_laws(model.law_artifacts());
        let consumed = is_pending.then(|| observed.consumed());
        let turn = DecisionTurn {
            id: input.dispute_id.clone(),
            now: self.now(),
            world: self.store().world(),
            consumes: consumed.clone(),
        };
        let mut commit = match consumed {
            Some(c) => CellCommit::empty().consuming(c),
            None => CellCommit::empty(),
        };
        let mut authorization = ObservationAuthorization::empty();
        let reader = self.store().read();
        if input.decision == DisputeDecision::AcceptSecondAsCorrection {
            reader.prepare_mapping_records(
                incoming,
                &catalog,
                &turn,
                &mut commit,
                &mut authorization,
            )?;
        }
        let plan = reader.dispute_resolution_link_plan(
            &input.dispute_id,
            dispute,
            input.decision,
            &commit,
        )?;
        reader.apply_link_plan(plan, &catalog, &turn, &mut commit, &mut authorization)?;
        commit.events.push(ObservationEvent::DisputeResolved {
            dispute_id: input.dispute_id.clone(),
            decision: input.decision.as_str().to_owned(),
            actor: input.actor.clone(),
            reason: input.reason.clone(),
        });
        authorization
            .refresh_link_findings(&catalog, &mut commit, &GuardView(&reader))
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        if is_pending {
            authorization
                .seal_snapshot_event_chunks(&commit)
                .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        }
        if !authorization.validates(&commit) {
            return Err(CellError::ObservationCommit(Box::new(
                norn_kernel::CommitError::ObservationAuthorization,
            )));
        }
        Ok((commit, authorization))
    }
}
