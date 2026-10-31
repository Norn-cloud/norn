//! Local (T2) checks of `claim` and `ruling` artifacts
//! ([`norn_ir::ruling`], norn#62).
//!
//! These are document rules only: identities and dates are well formed,
//! lists that carry meaning are non-empty, each member reference has the
//! shape its slot needs, and an outcome only names definitions the ruling
//! declares it affects. Whether a referenced member exists at its pinned
//! version, and whether a ruling actually resolves what it claims to, are
//! artifact-set obligations over the definition catalog.

use norn_ir::definition_catalog::{Member, MemberRef};
use norn_ir::ruling::{
    ClaimIr, ClaimStatement, Evidence, EvidenceSource, RevisitTrigger, RulingIr, RulingOutcome,
};
use norn_ir::{ArtifactEnvelope, ArtifactPayload};

use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

/// Stable codes of the local claim and ruling checks.
pub mod codes {
    /// A malformed `ClaimIR` document.
    pub const INVALID_CLAIM: &str = "claim.validation.invalid_document";
    /// A malformed `RulingIR` document.
    pub const INVALID_RULING: &str = "ruling.validation.invalid_document";
}

/// The local findings of a `claim` or `ruling` artifact; empty for any
/// other kind.
#[must_use]
pub fn check_local(artifact: &ArtifactEnvelope) -> Vec<Diagnostic> {
    let payload = serde_json::to_value(artifact.payload.to_json_value()).unwrap_or_default();
    let mut out = Findings {
        artifact,
        code: codes::INVALID_CLAIM,
        found: Vec::new(),
    };
    if let ArtifactPayload::Claim(_) = &artifact.payload {
        match serde_json::from_value::<ClaimIr>(payload) {
            Ok(claim) => out.claim(&claim),
            Err(error) => out.push("/payload", error.to_string()),
        }
    } else if let ArtifactPayload::Ruling(_) = &artifact.payload {
        out.code = codes::INVALID_RULING;
        match serde_json::from_value::<RulingIr>(payload) {
            Ok(ruling) => out.ruling(&ruling),
            Err(error) => out.push("/payload", error.to_string()),
        }
    }
    out.found
}

/// A checker finding in the envelope's legacy wire form.
pub(crate) fn legacy_diagnostic(finding: Diagnostic) -> norn_ir::Diagnostic {
    norn_ir::Diagnostic {
        code: finding.code.as_str().to_owned(),
        severity: finding.severity,
        message: finding.message,
        artifact_id: finding.artifact_id,
        path: finding
            .span
            .map(|span| span.pointer.trim_start_matches('/').replace('/', ".")),
        remediation: finding.remediation,
    }
}

struct Findings<'a> {
    artifact: &'a ArtifactEnvelope,
    code: &'static str,
    found: Vec<Diagnostic>,
}

/// `YYYY-MM-DD` with a plausible month and day.
fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    let number = |range: std::ops::Range<usize>| {
        text.get(range)
            .and_then(|part| part.parse::<u8>().ok())
            .unwrap_or(0)
    };
    bytes.len() == 10
        && digits(0..4)
        && bytes.get(4) == Some(&b'-')
        && digits(5..7)
        && bytes.get(7) == Some(&b'-')
        && digits(8..10)
        && (1..=12).contains(&number(5..7))
        && (1..=31).contains(&number(8..10))
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

impl Findings<'_> {
    fn push(&mut self, pointer: &str, message: impl Into<String>) {
        self.found.push(Diagnostic {
            code: DiagnosticCode::new(self.code),
            severity: DiagnosticSeverity::Error,
            message: message.into(),
            span: Some(Span::pointer(pointer)),
            artifact_id: Some(self.artifact.artifact_id.clone()),
            remediation: Some(DiagnosticRemediation::FixDefinition),
        });
    }

    fn member(&mut self, pointer: &str, member: &MemberRef) {
        if blank(&member.artifact_id) || blank(&member.version) {
            self.push(
                pointer,
                "a member reference names an exact artifact id and version",
            );
        }
    }

    fn evidence(&mut self, pointer: &str, evidence: &[Evidence]) {
        for (index, item) in evidence.iter().enumerate() {
            let at = format!("{pointer}/{index}/source");
            let pinned = match &item.source {
                EvidenceSource::Artifact {
                    artifact_id,
                    version,
                    sha256,
                } => !blank(artifact_id) && !blank(version) && is_sha256(sha256),
                EvidenceSource::Upstream {
                    system,
                    release,
                    locator,
                } => !blank(system) && !blank(release) && !blank(locator),
            };
            if !pinned {
                self.push(
                    &at,
                    "evidence is pinned: an artifact by id, version and lower-case SHA-256, or an upstream file by system, release and locator",
                );
            }
        }
    }

    fn claim(&mut self, claim: &ClaimIr) {
        if blank(&claim.id) {
            self.push("/payload/id", "a claim has a nonblank id");
        }
        if claim.evidence.is_empty() {
            self.push(
                "/payload/evidence",
                "a claim cites at least one piece of evidence",
            );
        }
        self.evidence("/payload/evidence", &claim.evidence);
        match &claim.statement {
            ClaimStatement::DerivedState { state, from } => {
                self.member("/payload/statement/state", state);
                if !matches!(state.member, Member::State { .. }) {
                    self.push(
                        "/payload/statement/state/member",
                        "a derived-state claim names a lifecycle state",
                    );
                }
                if from.is_empty() {
                    self.push(
                        "/payload/statement/from",
                        "a derived state names the members it is derived from",
                    );
                }
                for (index, member) in from.iter().enumerate() {
                    self.member(&format!("/payload/statement/from/{index}"), member);
                }
            }
            ClaimStatement::Divergence { witness } => {
                self.member("/payload/statement/witness/target", &witness.target);
                if !matches!(witness.target.member, Member::Property { .. }) {
                    self.push(
                        "/payload/statement/witness/target/member",
                        "a divergence is over one property",
                    );
                }
                for (side, observation) in [("left", &witness.left), ("right", &witness.right)] {
                    if blank(&observation.mapping.artifact_id)
                        || blank(&observation.mapping.version)
                        || blank(&observation.map)
                    {
                        self.push(
                            &format!("/payload/statement/witness/{side}"),
                            "an observation names an exact mapping and an object map",
                        );
                    }
                }
                if witness.left == witness.right {
                    self.push(
                        "/payload/statement/witness",
                        "a divergence needs two different observations",
                    );
                }
            }
        }
    }

    fn ruling(&mut self, ruling: &RulingIr) {
        if blank(&ruling.id) {
            self.push("/payload/id", "a ruling has a nonblank id");
        }
        if ruling.resolves.is_empty() {
            self.push("/payload/resolves", "a ruling resolves at least one claim");
        }
        if ruling.affects.is_empty() {
            self.push(
                "/payload/affects",
                "a ruling names the definitions it governs",
            );
        }
        for (index, member) in ruling.affects.iter().enumerate() {
            self.member(&format!("/payload/affects/{index}"), member);
        }
        if blank(&ruling.decided_by.principal) {
            self.push("/payload/decidedBy/principal", "a ruling names its decider");
        }
        if !is_date(&ruling.decided_at) {
            self.push("/payload/decidedAt", "decidedAt is a YYYY-MM-DD date");
        }
        self.evidence("/payload/evidence", &ruling.evidence);
        let own = ruling.supersedes.as_ref().is_some_and(|key| {
            key.artifact_id == self.artifact.artifact_id && key.version == self.artifact.version
        });
        if own {
            self.push("/payload/supersedes", "a ruling cannot supersede itself");
        }
        self.outcome("/payload/outcome", &ruling.outcome, &ruling.affects);
        for (index, alternative) in ruling.alternatives.iter().enumerate() {
            if alternative.outcome == ruling.outcome {
                self.push(
                    &format!("/payload/alternatives/{index}/outcome"),
                    "an alternative differs from the outcome",
                );
            }
        }
    }

    /// The definitions an outcome names are among those the ruling affects.
    fn outcome(&mut self, pointer: &str, outcome: &RulingOutcome, affects: &[MemberRef]) {
        let mut named: Vec<(String, &MemberRef)> = Vec::new();
        match outcome {
            RulingOutcome::Choose { definition } => {
                named.push((format!("{pointer}/definition"), definition));
            }
            RulingOutcome::Parameterise {
                parameter, set_by, ..
            } => {
                named.push((format!("{pointer}/parameter"), parameter));
                if set_by.is_empty() {
                    self.push(
                        &format!("{pointer}/setBy"),
                        "a parameter names who may set it",
                    );
                }
            }
            RulingOutcome::Scoped { cases } => {
                if cases.len() < 2 {
                    self.push(
                        &format!("{pointer}/cases"),
                        "a scoped outcome has two or more cases",
                    );
                }
                for (index, case) in cases.iter().enumerate() {
                    named.push((
                        format!("{pointer}/cases/{index}/definition"),
                        &case.definition,
                    ));
                }
            }
            RulingOutcome::Defer { revisit } => {
                if let RevisitTrigger::Date { on } = revisit
                    && !is_date(on)
                {
                    self.push(
                        &format!("{pointer}/revisit/on"),
                        "a revisit date is YYYY-MM-DD",
                    );
                }
            }
            RulingOutcome::Reject {} => {}
        }
        for (at, member) in named {
            if !affects.contains(member) {
                self.push(&at, "an outcome names only definitions the ruling affects");
            }
        }
    }
}
