//! Source-pinned refusal diagnostics; never Challenge or proof evidence.
use super::finite::{FiniteOracle, PROFILE};
use crate::table_analysis::Unknown;
use norn_expr::schema::Schema;
use std::collections::BTreeSet;

/// Inputs to a refusal diagnostic, never an admission/proof challenge.
pub struct Attempt<'a> {
    /// Exact attempted source bytes, retained even when ordinary admission refuses.
    pub source: &'a [u8],
    /// Exact base bytes.
    pub base: &'a [u8],
    /// Retained semantics/editability authority profile bytes.
    pub profile: &'a [u8],
    /// Explicit closed-world assumptions, including roster snapshot identity.
    pub assumptions: &'a str,
    /// Fixed enumerable universe; relaxing a declaration does not change it.
    pub schema: &'a Schema,
    /// Stable source identity/pointer and admitted Boolean expression per member.
    pub members: &'a [(super::RuleOrigin, String)],
    /// Only input constraints explicitly editable under the retained profile.
    /// This allowlist must exclude protected laws; it grants no editing authority.
    pub editable: &'a BTreeSet<usize>,
    /// Protected laws and fixed inputs, retained independently of editable inputs.
    pub protected: &'a BTreeSet<usize>,
    /// Whole-search oracle query ceiling.
    pub max_calls: usize,
}

/// Pinned refusal report. No conversion to `Challenge`, `EvidencePins` or proof records.
pub struct Report {
    /// SHA-256 of exact attempted source, base and complete analysis profile.
    pub digests: [String; 3],
    /// Diagnostic result only; no admission or proof verdict.
    pub diagnosis: Result<super::diagnosis::Diagnosis, super::diagnosis::Unknown<Unknown>>,
    /// Existing readable diagnostics, with exact source member locations.
    pub diagnostics: Vec<crate::Diagnostic>,
}

/// Reproduce a diagnostic from exact inputs. Ordinary admission remains separate.
#[must_use]
pub fn diagnose_attempt(attempt: &Attempt<'_>) -> Report {
    let digests = attempt_pins(attempt);
    let predicates: Vec<_> = attempt.members.iter().map(|(_, p)| p.as_str()).collect();
    let origins: BTreeSet<_> = attempt.members.iter().map(|(o, _)| o).collect();
    let diagnosis = if origins.len() != attempt.members.len()
        || attempt.members.iter().any(|(o, _)| {
            o.artifact_id.as_deref().is_none_or(str::is_empty)
                || (!o.pointer.is_empty() && !o.pointer.starts_with('/'))
        }) {
        Err(super::diagnosis::Unknown::Oracle(Unknown::Expression(
            "source member identities are incomplete or duplicated".into(),
        )))
    } else if !attempt.editable.is_disjoint(attempt.protected)
        || attempt
            .protected
            .iter()
            .any(|i| *i >= attempt.members.len())
    {
        Err(super::diagnosis::Unknown::InvalidEditable)
    } else {
        FiniteOracle::new(attempt.schema, &predicates)
            .map_err(super::diagnosis::Unknown::Oracle)
            .and_then(|mut oracle| {
                super::diagnosis::diagnose(
                    &mut oracle,
                    predicates.len(),
                    attempt.editable,
                    attempt.max_calls,
                )
            })
    };
    let mut diagnostics = Vec::new();
    let mut emit = |code, message: String, origin: Option<&super::RuleOrigin>| {
        diagnostics.push(super::finding(
            code,
            crate::DiagnosticSeverity::Error,
            message,
            origin,
        ));
    };
    let context = format!(
        "Closed world: {}. Limits [contexts, domain bytes, evaluations, expression fuel] {:?}; at most {} queries and {} declarations. Protected laws remain unchanged. Edit only authorized inputs and resubmit for ordinary admission.",
        attempt.assumptions,
        FiniteOracle::bounds(),
        attempt.max_calls,
        super::diagnosis::MAX_DECLARATIONS
    );
    match &diagnosis {
        Err(error) => emit(
            "consistency.diagnosis.unknown",
            format!("Diagnosis unknown: {error:?}. {context}"),
            None,
        ),
        Ok(result) => {
            for core in &result.cores {
                let labels: Vec<_> = core
                    .iter()
                    .filter_map(|i| attempt.members.get(*i))
                    .map(|(o, p)| {
                        format!(
                            "{}{}: {p}",
                            o.artifact_id.as_deref().unwrap_or(""),
                            o.pointer
                        )
                    })
                    .collect();
                let code = if result.corrections.is_empty() {
                    "consistency.diagnosis.no_editable_repair"
                } else {
                    "consistency.diagnosis.conflict"
                };
                if core.is_empty() {
                    emit(
                        code,
                        format!("The fixed background is inconsistent. {context}"),
                        None,
                    );
                }
                for index in core {
                    emit(
                        code,
                        format!(
                            "These declarations cannot hold together: {}. {context}",
                            labels.join("; ")
                        ),
                        attempt.members.get(*index).map(|(o, _)| o),
                    );
                }
            }
        }
    }
    Report {
        digests,
        diagnosis,
        diagnostics,
    }
}

fn attempt_pins(attempt: &Attempt<'_>) -> [String; 3] {
    use sha2::{Digest, Sha256};
    let hash = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let members: Vec<_> = attempt
        .members
        .iter()
        .map(|(o, p)| serde_json::json!([o.artifact_id, o.pointer, p]))
        .collect();
    let profile = serde_json::json!([
        PROFILE,
        env!("CARGO_PKG_VERSION"),
        crate::receipt::EXPR_PROFILE,
        attempt.profile,
        format!("{:?}", attempt.schema),
        members,
        attempt.assumptions,
        attempt.editable,
        attempt.protected,
        attempt.max_calls,
        super::diagnosis::MAX_DECLARATIONS,
        FiniteOracle::bounds()
    ])
    .to_string();
    [
        hash(attempt.source),
        hash(attempt.base),
        hash(profile.as_bytes()),
    ]
}
