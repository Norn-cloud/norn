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

use std::collections::BTreeSet;

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
/// transaction by `link_guard::verify_mapping_links`. Prevents generic
/// `CellHost::commit` and `CellHost::commit_observed` callers from forging a
/// dispute resolution.
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
    fn incoming_bundle(&self) -> &MappingBundle {
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

    pub(crate) fn load_source_conflicts(
        &self,
        pending_only: bool,
    ) -> Result<Vec<(u64, MappingConflict, Option<u64>)>, CellError> {
        let filter = if pending_only {
            " AND consumed_commit_seq IS NULL"
        } else {
            ""
        };
        let rows = self.conn.query(
            &format!(
                "SELECT position, observation_id, observed, consumed_commit_seq FROM {} \
                 WHERE observation_id LIKE 'mapping-conflict:%'{filter} ORDER BY position",
                crate::system::INBOX
            ),
            crate::engine::params![],
        )?;
        let mut conflicts = Vec::new();
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let position = crate::read::to_u64(row.get(0)?, "inbox position")?;
            let stored_id: String = row.get(1)?;
            let encoded: String = row.get(2)?;
            let resolved_seq = row
                .get::<Option<i64>>(3)?
                .map(|seq| crate::read::to_u64(seq, "consumed_commit_seq"))
                .transpose()?;
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
            let Some(dispute) = index.events.get(keys) else {
                return None;
            };
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
                "SELECT i.\"observation_id\", i.\"observed\" FROM \"{}\" i \
                 JOIN \"{}\" r ON r.\"dispute_id\" = i.\"observation_id\" \
                 WHERE r.\"decision\" IN ('accept-second-as-correction', 'reject')",
                crate::system::INBOX,
                crate::link_claim_frontier::DISPUTE_RESOLUTION,
            ),
            crate::engine::params![],
        )?;
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let id: String = row.get(0)?;
            let encoded: String = row.get(1)?;
            let Some((resolved_bundle, _)) =
                crate::mapping_conflict::decode_evidence_with_identity(&id, &encoded)
            else {
                continue;
            };
            let resolved_keys =
                crate::mapping_identity::keys(&resolved_bundle.raw, &resolved_bundle.input)?
                    .map(|k| norn_ir::canonical::nfc(&k));
            if resolved_keys == target_keys {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Load and cryptographically recheck a pending dispute row from `_norn_inbox`.
    pub(crate) fn load_verified_pending_dispute(
        &self,
        dispute_id: &str,
    ) -> Result<(Observed<RuntimeValue>, VerifiedDispute), CellError> {
        if !dispute_id.starts_with("mapping-conflict:")
            && !dispute_id.starts_with("mapping-identity:")
        {
            return Err(CellError::UnsupportedMappingProfile);
        }
        let Some(pending) = self.pending_observation(dispute_id)? else {
            return Err(CellError::NotPending(Consumption::Observation {
                observation_id: dispute_id.to_owned(),
            }));
        };
        let observed: Observed<RuntimeValue> =
            norn_ir::runtime::decode(&pending.observed).map_err(CellError::Encoding)?;
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
            Ok((observed, VerifiedDispute::Conflict(Box::new(conflict))))
        } else {
            let identity: IdentityEvidence =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            self.recheck_identity_evidence(&identity, dispute_id)?;
            Ok((observed, VerifiedDispute::Identity(Box::new(identity))))
        }
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

        let mut fresh_claims: Vec<(ObservedLinkClaim, [String; 5])> = Vec::new();
        match decision {
            DisputeDecision::AcceptFirst => {}
            DisputeDecision::AcceptSecondAsCorrection => {
                // OBS-LINK-CONTRIBUTION-1: incoming correction claims supersede
                // strictly older claims per source-version ordering plus any
                // active claim from THIS disputed event (never erasing a newer
                // active claim such as v3 when resolving a late disputed v2).
                let incoming_version = crate::mapping_links::parse_source_version(
                    incoming,
                    self.bundle_version_kind(incoming)?,
                )?;
                let correction_supersedes =
                    |contribution: &str| -> Result<BTreeSet<String>, CellError> {
                        let mut supersedes =
                            self.compute_claim_supersedes(contribution, &incoming_version, None)?;
                        supersedes.extend(
                            active_event_claims
                                .iter()
                                .filter(|r| r.contribution_id == contribution)
                                .map(|r| r.claim_id.clone()),
                        );
                        Ok(supersedes)
                    };
                let mut covered_contributions = BTreeSet::new();
                for mut claim in incoming.link_claims()? {
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
                // For `kind == "delete"` corrections, withdraw active
                // contributions owned by `incoming`'s source scope on deleted
                // subjects respecting source-version ordering (including
                // earlier v1 claims when a conflicting v2 upsert was pending,
                // without erasing newer v3 claims).
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
                // Withdraw any active claims of the disputed event whose
                // contribution was NOT re-asserted by the incoming correction
                // (e.g. a correction that removes a link or deletes the record).
                let mut withdrawn = BTreeSet::new();
                for row in &active_event_claims {
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
            }
            DisputeDecision::Reject => {
                // Supersede ONLY the active claims belonging to the disputed
                // source event (never an undisputed newer active claim in the
                // same contribution, e.g. current v3 alongside a late contested
                // disputed v2).
                let mut by_contrib: std::collections::BTreeMap<
                    String,
                    Vec<&crate::link_claim_frontier::FrontierRow>,
                > = std::collections::BTreeMap::new();
                for row in &active_event_claims {
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
                    let mut real_surviving = Vec::new();
                    let mut reject_carry_ids = Vec::new();
                    for r in all_active {
                        if supersedes.contains(&r.claim_id) {
                            continue;
                        }
                        let is_reject_carry =
                            (r.claim.observation_id.starts_with("mapping-conflict:")
                                || r.claim.observation_id.starts_with("mapping-identity:"))
                                && self
                                    .dispute_resolution(&r.claim.observation_id)?
                                    .is_some_and(|rec| rec.decision == "reject");
                        if is_reject_carry {
                            reject_carry_ids.push(r.claim_id);
                        } else {
                            real_surviving.push(r);
                        }
                    }
                    // Always supersede prior synthetic `reject` carry-forward
                    // rows so an earlier synthetic `None` (from a 3-way
                    // contested state) never leaves a later unique survivor
                    // permanently contested.
                    supersedes.extend(reject_carry_ids);
                    let surviving_targets: BTreeSet<_> = real_surviving
                        .iter()
                        .map(|r| {
                            r.claim
                                .target
                                .as_ref()
                                .map(|t| (t.object_type.clone(), t.key.clone()))
                        })
                        .collect();
                    let replacement_target = if surviving_targets.len() == 1 {
                        real_surviving.first().and_then(|r| r.claim.target.clone())
                    } else {
                        None
                    };
                    let withdrawal = ObservedLinkClaim {
                        observation_id: dispute_id.to_owned(),
                        source_scope: first.claim.source_scope.clone(),
                        source_type: first.claim.source_type.clone(),
                        source_key: first.claim.source_key.clone(),
                        role: first.claim.role.clone(),
                        target: replacement_target,
                        provenance: first.claim.provenance.clone(),
                        supersedes,
                    };
                    scope.sources.insert(withdrawal.source_key.clone());
                    fresh_claims.push((withdrawal, event_keys.clone()));
                }
            }
        }
        self.plan_observed_links(&scope, fresh_claims, commit, model, Some(dispute_id))
    }
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
        let (observed, dispute) = self
            .store()
            .read()
            .load_verified_pending_dispute(&input.dispute_id)?;
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

        let model = self.store().model().clone();
        let catalog = TypeCatalog::new(model.object_types())
            .with_interfaces(model.interfaces())
            .with_property_types(model.property_types())
            .with_governing_laws(model.law_artifacts());
        let turn = DecisionTurn {
            id: input.dispute_id.clone(),
            now: self.now(),
            world: self.store().world(),
            consumes: Some(observed.consumed()),
        };
        let mut commit = CellCommit::empty().consuming(observed.consumed());
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
            &dispute,
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
        authorization
            .seal_snapshot_event_chunks(&commit)
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        if !authorization.validates(&commit) {
            return Err(CellError::ObservationCommit(Box::new(
                norn_kernel::CommitError::ObservationAuthorization,
            )));
        }
        let references = authorization.reference_reads(&commit).to_vec();
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
}
