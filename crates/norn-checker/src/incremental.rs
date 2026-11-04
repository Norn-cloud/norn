//! Process-local memoisation of structural admission, with explicit global checks.
//!
//! Local jobs read one complete normalized envelope. Every set check retains a
//! whole-set closure: name lookup, absent references and new writers are inputs.
//! This cache does not discharge Challenge goals or create solver evidence.
use std::collections::BTreeMap;

use norn_ir::ArtifactEnvelope;
use serde_json::json;

use crate::obligations::{ChallengeError, exact_artifact_digest, exact_commitment};
use crate::{Diagnostic, EntityArtifactSetContext};

/// Why a job's unchanged structural evidence is available in this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceOrigin {
    /// The checker executed this job, including on extraction failure.
    Rechecked,
    /// Exact closure identity of evidence retained from an earlier admission.
    ReusedFromDigest(String),
}

/// Structural evidence provenance; never a proof or an assurance promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEvidence {
    /// Artifact id for a local job, or `None` for all global set checks.
    pub artifact_id: Option<String>,
    /// Exact closure digest; absent when extraction failed.
    pub closure_digest: Option<String>,
    /// Reuse versus execution in this admission.
    pub origin: EvidenceOrigin,
}

/// Ordinary findings and a separate audit of job execution/reuse.
#[derive(Debug, Clone)]
pub struct Admission {
    /// Identical findings, in identical order, to the full structural checker.
    pub findings: Vec<Diagnostic>,
    /// Only jobs actually consumed by this admission; removed jobs disappear.
    pub evidence: Vec<JobEvidence>,
}

#[derive(Debug, Clone)]
struct Entry {
    digest: String,
    findings: Vec<Diagnostic>,
}

/// A trusted controller's structural evidence cache, limited to the current set.
///
/// Tool digest must identify the complete executing checker/solver build. Policy
/// and assumptions are immutable for this cache; create a new cache on change.
/// Stored diagnostics are private and cannot be supplied by an untrusted worker.
pub struct AdmissionCache {
    profile: String,
    local: BTreeMap<String, Entry>,
    global: Option<Entry>,
    jobs: Vec<JobEvidence>,
}

impl AdmissionCache {
    /// Bind the executing implementation, policy and assumptions. These are exact
    /// bytes, not display labels; no portable proof identity is minted here.
    #[must_use]
    pub fn new(tool_sha256: &[u8; 32], policy: &str, assumptions: &str) -> Self {
        Self {
            profile: format!("norn.incremental-structural/1:{tool_sha256:?}:{policy:?}:{assumptions:?}"),
            local: BTreeMap::new(),
            global: None,
            jobs: Vec::new(),
        }
    }

    /// Execute exactly the changed/new local jobs and all affected global jobs.
    /// Input digests are reconstructed, so callers cannot under-report edits.
    /// Extraction failure discards prior evidence and runs the full checker.
    pub fn check(
        &mut self,
        artifacts: &[ArtifactEnvelope],
        context: &EntityArtifactSetContext<'_>,
    ) -> Admission {
        let Ok(digest) = self.global_digest(artifacts, context) else {
            self.local.clear();
            self.global = None;
            self.jobs.clear();
            return Admission {
                findings: crate::check_artifact_set(artifacts, context),
                evidence: vec![JobEvidence {
                    artifact_id: None,
                    closure_digest: None,
                    origin: EvidenceOrigin::Rechecked,
                }],
            };
        };
        if let Some(entry) = &self.global
            && entry.digest == digest
        {
            let mut evidence = self.jobs.clone();
            for job in &mut evidence {
                if let Some(digest) = &job.closure_digest {
                    job.origin = EvidenceOrigin::ReusedFromDigest(digest.clone());
                }
            }
            evidence.push(global_evidence(&digest, true));
            return Admission { findings: entry.findings.clone(), evidence };
        }
        let mut evidence = Vec::new();
        let mut next = BTreeMap::new();
        let mut local = |artifact: &ArtifactEnvelope| {
            // This callback receives the same normalized envelope as full admission.
            let key = exact_artifact_digest(artifact).and_then(|hash| {
                exact_commitment(b"local-admission", &[self.profile.as_bytes(), hash.as_bytes()])
            });
            let old = self.local.get(&artifact.artifact_id);
            let reused = key.as_ref().is_ok_and(|key| old.is_some_and(|e| e.digest == *key));
            let findings = if reused {
                old.map(|entry| entry.findings.clone()).unwrap_or_default()
            } else {
                crate::check_artifact(artifact)
            };
            if let Ok(key) = &key {
                next.insert(artifact.artifact_id.clone(), Entry { digest: key.clone(), findings: findings.clone() });
            }
            evidence.push(JobEvidence {
                artifact_id: Some(artifact.artifact_id.clone()),
                closure_digest: key.as_ref().ok().cloned(),
                origin: if reused { EvidenceOrigin::ReusedFromDigest(key.unwrap_or_default()) } else { EvidenceOrigin::Rechecked },
            });
            findings
        };
        let findings = crate::artifact_set::check_artifact_set_using(artifacts, context, &mut local);
        self.jobs.clone_from(&evidence);
        self.local = next;
        self.global = Some(Entry { digest: digest.clone(), findings: findings.clone() });
        evidence.push(global_evidence(&digest, false));
        Admission { findings, evidence }
    }

    fn global_digest(
        &self,
        artifacts: &[ArtifactEnvelope],
        context: &EntityArtifactSetContext<'_>,
    ) -> Result<String, ChallengeError> {
        // Preserve layer membership and order: diagnostics depend on both. Duplicate
        // identities cannot collapse, nor can moving an artifact into the base.
        let hashes = |pool: &[ArtifactEnvelope]| pool.iter().map(exact_artifact_digest).collect::<Result<Vec<_>, _>>();
        let body = json!({
            "profile": self.profile,
            "candidate": hashes(artifacts)?,
            "base": hashes(context.base)?,
            "registry": context.registry,
            "tenant": context.tenant.map(|t| (t.tenant_id, &t.namespace.0)),
        });
        let bytes = norn_canonical::serialize_json_verbatim(&body).map_err(|_error| ChallengeError::Closure)?;
        exact_commitment(b"global-admission", &[&bytes])
    }
}

fn global_evidence(digest: &str, reused: bool) -> JobEvidence {
    JobEvidence {
        artifact_id: None,
        closure_digest: Some(digest.to_owned()),
        origin: if reused { EvidenceOrigin::ReusedFromDigest(digest.to_owned()) } else { EvidenceOrigin::Rechecked },
    }
}
