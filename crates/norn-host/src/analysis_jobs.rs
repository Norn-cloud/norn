//! Preregistration metadata contracts; ledger registration and execution follow.
use crate::Refusal;
use crate::analysis_native::ExportPlan;
use norn_ir::runtime::encode;
use serde::{Deserialize, Serialize};

/// Admitted statistical method, with a fixed two-sided zero-effect hypothesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Method {
    /// One predictor, one intercept, one slope hypothesis.
    Ols,
    /// Group zero minus group one, unequal variances.
    Welch,
    /// Chronological forecast; no parameter-inference claim is published.
    Forecast {
        /// Strict evaluation boundary in UTC epoch microseconds.
        boundary: i64,
    },
}

/// A failed diagnostic cannot be waived by a free-text statement. The only
/// initial remedy publishes descriptive estimates with inference removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Remedy {
    /// Failed computed obligations withhold all estimates.
    Block,
    /// Preregistered suppression of p-values, intervals and standard errors.
    DescriptiveOnly {
        /// Analyst's reason, sealed before the run.
        reason: String,
    },
}

/// Preregistered, versioned analysis declaration; contains no executable code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Declaration {
    /// Must be one for this profile.
    pub version: u32,
    /// Stable question identity; one immutable registration/result per question.
    pub question: String,
    /// Authenticated population and explicit temporal reads.
    pub population: ExportPlan,
    /// Fixed method; no search or variants within a registered question.
    pub method: Method,
    /// Alpha as exact basis points (500 = 0.05).
    pub alpha_basis_points: u32,
    /// Analyst's signed statement about independent sampling/observations.
    pub independence: String,
    /// Analyst's confounding/design statement; does not authorize causal wording.
    pub confounding: String,
    /// Fixed response to violated computed assumptions.
    pub remedy: Remedy,
}

impl Declaration {
    /// Check registration metadata and encoded byte bounds.
    /// Source authentication and authorization remain the export host's job.
    ///
    /// # Errors
    /// Invalid version, question, alpha, statement, remedy or encoded size.
    pub fn validate(&self) -> Result<(), Refusal> {
        if self.version != 1
            || self.question.is_empty()
            || self.question.len() > 128
            || !self
                .question
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
            || self.alpha_basis_points == 0
            || self.alpha_basis_points >= 10_000
            || [self.independence.as_str(), self.confounding.as_str()]
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 4096)
            || matches!(&self.remedy, Remedy::DescriptiveOnly { reason } if reason.trim().is_empty() || reason.len() > 4096)
        {
            return Err(Refusal::invalid(
                "analysis declaration or signed assumption bound",
            ));
        }
        let bytes = encode(self)
            .map_err(Refusal::host)?
            .canonical_json()
            .map_err(Refusal::host)?;
        if bytes.len() > 128 * 1024 {
            return Err(Refusal::invalid("analysis declaration byte bound"));
        }
        Ok(())
    }
}
