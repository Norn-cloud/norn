//! OBS-EVIDENCE-1: NFC storage buckets never establish opaque source identity.
use crate::{
    CellError, CellStore, StoreReader,
    engine::{SqlEngine, params},
    mapping_bundle::{MappingBundle, MappingInput},
    system::INBOX,
};
use norn_ir::{
    DataQualityFinding, DataQualityFindingStatus, DataQualityLineage, DataQualityOutcome,
    DataQualityRuleRef, DataQualitySeverity, DataQualitySubject, RuntimeValue,
};
use norn_kernel::{ObservationId, Observed};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct IdentityEvidence {
    pub(crate) bundle: MappingBundle,
    // Numeric bytes survive the inbox text profile without changing the spelling proof.
    pub(crate) identity: [Vec<u8>; 5],
    pub(crate) finding: DataQualityFinding,
}

pub(crate) fn keys(raw: &RuntimeValue, input: &MappingInput) -> Result<[String; 5], CellError> {
    let text = |name| {
        raw.get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
            .ok_or(CellError::MappingBundleMismatch)
    };
    Ok([
        input.connector_instance.clone(),
        input.partition.clone(),
        text("source")?,
        text("source_id")?,
        text("event_id")?,
    ])
}

fn finding(
    bundle: &MappingBundle,
    identity: &[Vec<u8>; 5],
) -> Result<DataQualityFinding, CellError> {
    let evidence = norn_ir::canonical::domain_hash_hex(
        b"norn.source.identity_guard.v1\0",
        &(
            identity,
            bundle.definitions_version,
            &bundle.definitions_digest,
            &bundle.artifact_pins,
            bundle.raw.get("kind"),
            bundle.raw.get("source_version"),
            bundle.raw.get("payload_digest"),
            bundle.raw.get("redaction").filter(|v| !v.is_null()),
        ),
    )?;
    let mut result = DataQualityFinding::v2(
        DataQualityRuleRef::Builtin {
            id: "core.boundary.import_finding".into(),
            version: "1".into(),
        },
        bundle
            .records
            .iter()
            .map(|r| DataQualitySubject::new(r.object_type.clone(), &r.key))
            .collect(),
        DataQualitySeverity::Error,
        "identity normalized; possible collision",
        DataQualityOutcome::NotSatisfied,
        bundle
            .records
            .iter()
            .map(|r| DataQualityLineage::new(r.provenance.clone(), Vec::new()))
            .collect(),
        bundle.input.recorded_at.clone(),
        bundle.input.recorded_at.clone(),
        DataQualityFindingStatus::Open,
    )?
    .with_evidence_digest(evidence)?;
    for lineage in &mut result.provenance {
        lineage.observation_ids = vec![format!("mapping-identity:{}", result.id)];
    }
    Ok(result)
}

/// Capture non-NFC spelling before the inbox serializes text; refuse promotion, never merge.
pub(crate) fn capture<E: SqlEngine>(
    store: &CellStore<E>,
    raw: &Observed<RuntimeValue>,
    input: &MappingInput,
    version: u64,
) -> Result<Option<(String, bool)>, CellError> {
    let original = keys(&raw.input, input)?;
    let canonical = original.clone().map(|s| norn_ir::canonical::nfc(&s));
    if original == canonical {
        return Ok(None);
    }
    let mut mapped_raw = raw.input.clone();
    let object = mapped_raw
        .as_object_mut()
        .ok_or(CellError::MappingBundleMismatch)?;
    for (name, value) in ["source", "source_id", "event_id"]
        .into_iter()
        .zip(canonical.iter().skip(2))
    {
        object.insert(name.into(), RuntimeValue::from(value.clone()));
    }
    let mut mapped_input = input.clone();
    mapped_input.connector_instance.clone_from(&canonical[0]);
    mapped_input.partition.clone_from(&canonical[1]);
    let bundle = store
        .read()
        .mapping_bundle(&mapped_raw, &mapped_input, version)?;
    let identity = original.map(String::into_bytes);
    let finding = finding(&bundle, &identity)?;
    let id = format!("mapping-identity:{}", finding.id);
    let evidence = IdentityEvidence {
        bundle,
        identity,
        finding,
    };
    let observed = Observed {
        id: ObservationId(id.clone()),
        provenance: raw.provenance.clone(),
        input: norn_ir::runtime::encode(&evidence).map_err(CellError::Encoding)?,
    };
    let inserted = crate::store::inject_observation(
        &store.conn,
        &id,
        &norn_ir::runtime::encode(&observed).map_err(CellError::Encoding)?,
    )?;
    Ok(Some((id, inserted)))
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// OBS-EVIDENCE-1: governed reads cannot consume disputed source subjects.
    pub(crate) fn refuse_disputed_subject(
        &self,
        key: &norn_kernel::FactKey,
    ) -> Result<(), CellError> {
        if self.source_dispute_at(crate::MAIN_WORLD, key.object_type.as_str(), &key.key)? {
            return Err(CellError::SourceIdentityDisputed);
        }
        Ok(())
    }

    /// OBS-EVIDENCE-1: non-main reads cannot omit an unqualified source-evidence overlay.
    pub(crate) fn source_dispute_at(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
    ) -> Result<bool, CellError> {
        let disputed = self.indexed_source_dispute(world, object_type, key)?;
        if disputed && world != crate::MAIN_WORLD {
            return Err(CellError::SourceDisputeWorldUnqualified);
        }
        Ok(disputed)
    }

    /// Recompute identity-dispute evidence against the retained admitted definitions.
    pub(crate) fn recheck_identity_evidence(
        &self,
        guard: &IdentityEvidence,
        stored_id: &str,
    ) -> Result<(), CellError> {
        self.recheck_mapping_bundle(&guard.bundle)?;
        let text: Vec<_> = guard
            .identity
            .iter()
            .map(|b| {
                std::str::from_utf8(b)
                    .map(str::to_owned)
                    .map_err(|_invalid_utf8| CellError::MappingBundleMismatch)
            })
            .collect::<Result<_, _>>()?;
        let normalized: Vec<_> = text.iter().map(|s| norn_ir::canonical::nfc(s)).collect();
        if text == normalized
            || normalized.as_slice() != keys(&guard.bundle.raw, &guard.bundle.input)?
            || guard.finding != finding(&guard.bundle, &guard.identity)?
            || stored_id != format!("mapping-identity:{}", guard.finding.id)
        {
            return Err(CellError::MappingBundleMismatch);
        }
        Ok(())
    }

    /// Every unresolved (pending) identity dispute with its inbox position, in capture order.
    pub(crate) fn identity_evidence(&self) -> Result<Vec<(u64, IdentityEvidence)>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT position, observation_id, observed FROM \"{INBOX}\" \
                 WHERE observation_id LIKE 'mapping-identity:%' \
                 ORDER BY position"
            ),
            params![],
        )?;
        let mut evidence = Vec::new();
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let position = crate::read::to_u64(row.get(0)?, "inbox position")?;
            let stored_id: String = row.get(1)?;
            let encoded: String = row.get(2)?;
            if self
                .verified_dispute_resolution_seq(&stored_id, None)?
                .is_some()
            {
                continue;
            }
            let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
            let guard: IdentityEvidence =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            if observed.id.0 != stored_id {
                return Err(CellError::MappingBundleMismatch);
            }
            self.recheck_identity_evidence(&guard, &stored_id)?;
            evidence.push((position, guard));
        }
        Ok(evidence)
    }

    /// OBS-EVIDENCE-1: an ambiguous NFC bucket grants no convergence authority.
    pub(crate) fn identity_findings(
        &self,
        object_type: &str,
        key: &str,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        self.indexed_source_findings(object_type, key, true)
    }

    /// Active frontier claims associated with `event_keys`, including active
    /// `DisputeDecision::Reject` carry-forward claims whose surviving
    /// predecessor set currently restores a claim from `event_keys` (along with
    /// those carried predecessor rows so a subsequent `Reject` excludes them).
    pub(crate) fn disputed_event_claims(
        &self,
        event_keys: &[String; 5],
    ) -> Result<Vec<crate::link_claim_frontier::FrontierRow>, CellError> {
        let keys_json = serde_json::to_string(event_keys)?;
        let all_event_claims =
            self.query_frontier("WHERE \"event_keys_json\" = ?1", params![keys_json])?;
        let mut out = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut inactive_contribs = std::collections::BTreeSet::new();
        for row in all_event_claims {
            if row.active {
                if seen.insert(row.claim_id.clone()) {
                    out.push(row);
                }
            } else if !self.is_reject_carry_claim(&row)? {
                inactive_contribs.insert(row.contribution_id);
            }
        }
        for contribution_id in inactive_contribs {
            let active = self.active_claims_for_contribution(&contribution_id)?;
            let mut has_carry = false;
            for r in &active {
                if self.is_reject_carry_claim(r)? {
                    has_carry = true;
                    break;
                }
            }
            if !has_carry {
                continue;
            }
            let surviving = self.surviving_predecessors_after_reject(
                &contribution_id,
                active.clone(),
                &std::collections::BTreeSet::new(),
            )?;
            let carried: Vec<_> = surviving
                .into_iter()
                .filter(|r| &r.event_keys == event_keys)
                .collect();
            if carried.is_empty() {
                continue;
            }
            for r in active {
                if self.is_reject_carry_claim(&r)? && seen.insert(r.claim_id.clone()) {
                    out.push(r);
                }
            }
            for r in carried {
                if seen.insert(r.claim_id.clone()) {
                    out.push(r);
                }
            }
        }
        Ok(out)
    }

    /// Whether `row` (or, for a `DisputeDecision::Reject` carry-forward claim,
    /// any surviving predecessor it carries) has a verified source version
    /// strictly greater than `incoming`.
    pub(crate) fn claim_is_newer_than(
        &self,
        row: &crate::link_claim_frontier::FrontierRow,
        incoming: &norn_ir::SourceVersion,
    ) -> Result<bool, CellError> {
        if self.is_reject_carry_claim(row)? {
            let active = self.active_claims_for_contribution(&row.contribution_id)?;
            for pred in self.surviving_predecessors_after_reject(
                &row.contribution_id,
                active,
                &std::collections::BTreeSet::new(),
            )? {
                if crate::mapping_delete::parse_verified_frontier_version(self, &pred)?
                    .and_then(|v| v.try_cmp(incoming).ok())
                    == Some(std::cmp::Ordering::Greater)
                {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        Ok(
            crate::mapping_delete::parse_verified_frontier_version(self, row)?
                .and_then(|v| v.try_cmp(incoming).ok())
                == Some(std::cmp::Ordering::Greater),
        )
    }

    /// Release any held `target: None` reject-carry claim whose surviving
    /// predecessor belongs to `dispute_id`'s event once `AcceptFirst` resolves
    /// that dispute. Returns empty for ordinary `AcceptFirst` resolutions.
    pub(crate) fn accept_first_carry_release_claims(
        &self,
        dispute_id: &str,
        event_keys: &[String; 5],
        active_event_claims: &[crate::link_claim_frontier::FrontierRow],
        model: norn_kernel::CommitModel<'_, '_>,
        scope: &mut crate::mapping_links::ClaimScope,
    ) -> Result<Vec<(norn_ir::mapping::ObservedLinkClaim, [String; 5])>, CellError> {
        let mut by_contrib: std::collections::BTreeMap<
            String,
            Vec<&crate::link_claim_frontier::FrontierRow>,
        > = std::collections::BTreeMap::new();
        for row in active_event_claims {
            if row.active && row.claim.target.is_none() && self.is_reject_carry_claim(row)? {
                by_contrib
                    .entry(row.contribution_id.clone())
                    .or_default()
                    .push(row);
            }
        }
        let mut fresh = Vec::new();
        for (contribution_id, carries) in by_contrib {
            let Some(first) = carries.first() else {
                continue;
            };
            let active = self.active_claims_for_contribution(&contribution_id)?;
            let surviving = self.surviving_predecessors_after_reject(
                &contribution_id,
                active,
                &std::collections::BTreeSet::new(),
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
            let targets: std::collections::BTreeSet<_> = surviving
                .iter()
                .map(|r| {
                    r.claim
                        .target
                        .as_ref()
                        .map(|t| (t.object_type.clone(), t.key.clone()))
                })
                .collect();
            let Some(target) = (!any_disputed && targets.len() == 1)
                .then(|| surviving.first().and_then(|r| r.claim.target.clone()))
                .flatten()
            else {
                continue;
            };
            let exclusive = model
                .link(&first.claim.source_type, first.claim.role.as_str())
                .is_none_or(|link| link.cardinality.from == norn_ir::Multiplicity::One);
            if exclusive {
                scope.targets.insert(target.key.clone());
            }
            scope.sources.insert(first.claim.source_key.clone());
            fresh.push((
                norn_ir::mapping::ObservedLinkClaim {
                    observation_id: dispute_id.to_owned(),
                    source_scope: first.claim.source_scope.clone(),
                    source_type: first.claim.source_type.clone(),
                    source_key: first.claim.source_key.clone(),
                    role: first.claim.role.clone(),
                    target: Some(target),
                    provenance: first.claim.provenance.clone(),
                    supersedes: carries.iter().map(|r| r.claim_id.clone()).collect(),
                },
                event_keys.clone(),
            ));
        }
        Ok(fresh)
    }

    /// Recompute the surviving non-rejected real link claims of `contribution_id`
    /// after excluding `newly_rejected`, all claims superseded by prior
    /// `DisputeDecision::Reject` carry-forward claims, and any claim whose
    /// `event_keys` already carries a verified `DisputeDecision::Reject` at or
    /// after that claim's commit.
    pub(crate) fn surviving_predecessors_after_reject(
        &self,
        contribution_id: &str,
        mut all_claims: Vec<crate::link_claim_frontier::FrontierRow>,
        newly_rejected: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<crate::link_claim_frontier::FrontierRow>, CellError> {
        all_claims.extend(self.inactive_claims_for_contribution(contribution_id)?);
        let mut rejected_ids = newly_rejected.clone();
        let mut supersedes_by_id: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        let mut real_claims = Vec::new();
        for r in all_claims {
            supersedes_by_id.insert(
                r.claim_id.clone(),
                r.claim.supersedes.iter().cloned().collect(),
            );
            if self.is_reject_carry_claim(&r)? {
                rejected_ids.extend(r.claim.supersedes.iter().cloned());
            } else {
                real_claims.push(r);
            }
        }
        let mut rejected_event_commits: std::collections::BTreeMap<[String; 5], u64> =
            std::collections::BTreeMap::new();
        for rec in self.dispute_resolutions()? {
            if rec.decision != "reject" {
                continue;
            }
            let meta = crate::link_claim_frontier::claim_source_meta(
                self.conn,
                Some(self),
                rec.commit_seq,
                None,
                &rec.dispute_id,
                true,
            )?;
            rejected_event_commits
                .entry(meta.event_keys)
                .and_modify(|seq| *seq = (*seq).max(rec.commit_seq))
                .or_insert(rec.commit_seq);
        }
        let remaining: Vec<_> = real_claims
            .into_iter()
            .filter(|r| {
                !rejected_ids.contains(&r.claim_id)
                    && !rejected_event_commits
                        .get(&r.event_keys)
                        .is_some_and(|&rej_seq| rej_seq >= r.commit_seq)
            })
            .collect();
        let mut superseded_by_survivor = std::collections::BTreeSet::new();
        let mut stack: Vec<String> = remaining
            .iter()
            .flat_map(|r| r.claim.supersedes.iter().cloned())
            .collect();
        while let Some(id) = stack.pop() {
            if superseded_by_survivor.insert(id.clone())
                && let Some(parents) = supersedes_by_id.get(&id)
            {
                stack.extend(parents.iter().cloned());
            }
        }
        Ok(remaining
            .into_iter()
            .filter(|r| !superseded_by_survivor.contains(&r.claim_id))
            .collect())
    }
}
