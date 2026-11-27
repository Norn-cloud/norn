//! Single source of truth for governing-law support across admission, runtime
//! commit/observation evaluation, dataset row lowering, and finite-state proofs.
//!
//! `norn-checker::law_honesty`, `norn-checker::obligations::lifecycle`,
//! `norn-kernel::governing_laws`, and `norn-cell::migration` all delegate to
//! [`classify_law_support`] (and its focused predicates) so admission, honesty
//! receipts, migration validation, and commit execution never drift.

use norn_ir::{LawIR, LawModality, LawResponse, LawRoot, LawSeverity};

/// Runtime commit/observation evaluation support (`norn-kernel::governing_laws`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LawRuntimeSupport {
    /// Enforced on every commit and observation by `norn-kernel::governing_laws`.
    Executable,
    /// Scope (`root` or `objectTypes`) requires B2 effective-scope resolution.
    UnsupportedScope(&'static str),
    /// Temporal modality, timing, scope selector, or response kind is declared-only.
    UnsupportedSemantics(&'static str),
    /// Exception tree or declaration violates structural runtime preconditions.
    Invalid(&'static str),
}

impl LawRuntimeSupport {
    /// Whether `norn-kernel::governing_laws` can evaluate and enforce this law.
    #[must_use]
    pub const fn is_executable(self) -> bool {
        matches!(self, Self::Executable)
    }

    /// Human-readable reason when the law cannot execute on commits.
    #[must_use]
    pub const fn reason(self) -> Option<&'static str> {
        match self {
            Self::Executable => None,
            Self::UnsupportedScope(reason)
            | Self::UnsupportedSemantics(reason)
            | Self::Invalid(reason) => Some(reason),
        }
    }
}

/// Set-based dataset row-lowering support (`MANIFEST-LAWS-1`, `norn-cell::dataset_laws`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LawRowLoweringSupport {
    /// Condition and predicate lower directly to the bounded row-expression tree.
    DirectRowTree,
    /// Runtime-enforced, settling via per-row residue evaluation.
    ResiduePerRow(&'static str),
    /// Not evaluable on datasets because runtime evaluation itself is unavailable.
    Unavailable(&'static str),
}

impl LawRowLoweringSupport {
    /// Wire kebab-case token for JSON projections.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DirectRowTree => "direct-row-tree",
            Self::ResiduePerRow(_) => "residue-per-row",
            Self::Unavailable(_) => "unavailable",
        }
    }
}

/// Static finite-state / lifecycle proof support (`obligations::lifecycle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LawProofSupport {
    /// Admissible by finite-state lifecycle soundness checkers.
    FiniteCheckable,
    /// Enforced at runtime, but outside the current finite-state proof fragment.
    RuntimeOnly(&'static str),
    /// Neither runtime-enforced nor statically checkable (`DeclaredOnly`).
    DeclaredOnly(&'static str),
}

impl LawProofSupport {
    /// Wire kebab-case token for JSON projections.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FiniteCheckable => "finite-checkable",
            Self::RuntimeOnly(_) => "runtime-only",
            Self::DeclaredOnly(_) => "declared-only",
        }
    }
}

/// Complete three-axis support classification for one decoded [`LawIR`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LawSupport {
    /// Commit and observation runtime evaluation support.
    pub runtime: LawRuntimeSupport,
    /// Set-based dataset row-lowering support (`MANIFEST-LAWS-1`).
    pub row_lowering: LawRowLoweringSupport,
    /// Static finite-state proof support.
    pub proof: LawProofSupport,
}

/// Whether `law`'s declared scope (`root` and `object_types`) is supported by
/// single-entity / interface runtime dispatch (`LAW-INTERFACE-1`, prior to B2).
///
/// # Errors
/// Returns a static explanation when `law`'s root or `object_types` requires
/// multi-type or app/pack effective-scope resolution.
pub fn runtime_scope_support(law: &LawIR) -> Result<(), &'static str> {
    if !matches!(&law.scope.root, LawRoot::Entity(name) if *name == law.subject) {
        return Err("only single-entity root laws are enforced on commit");
    }
    if law.scope.object_types != [law.subject.as_str()] {
        return Err("multi-type or non-subject scope objectTypes are not enforced on commit");
    }
    Ok(())
}

/// Whether `law`'s temporal modality, selectors, response, and exception shape
/// are supported by `norn-kernel::governing_laws` (independent of scope).
///
/// # Errors
/// Returns a static explanation when `law` uses timing, lifecycle/commitment
/// selectors, temporal modalities, or responses not enforced on commit.
pub fn runtime_semantics_support(law: &LawIR) -> Result<(), &'static str> {
    if law.timing.is_some() {
        return Err("timed/deadline laws are not enforced on commit");
    }
    if !law.scope.lifecycle_states.is_empty() {
        return Err("lifecycle-state scoped laws are not enforced on commit");
    }
    if !law.scope.commitments.is_empty() {
        return Err("commitment-scoped laws are not enforced on commit");
    }
    if !matches!(law.modality, LawModality::Always {} | LawModality::Never {}) {
        return Err("temporal modalities (eventually/until) are declared-only");
    }
    match &law.response {
        LawResponse::Predicate { .. } => Ok(()),
        LawResponse::Invariant { .. } => {
            if !law.exceptions.is_empty() {
                Err("invariant responses with exceptions are not enforced on commit")
            } else if matches!(law.modality, LawModality::Always {}) && law.condition.is_none() {
                Ok(())
            } else {
                Err("conditional or non-always invariant responses are declared-only")
            }
        }
        LawResponse::Action { .. }
        | LawResponse::Transition { .. }
        | LawResponse::TranslationPreserves { .. } => {
            Err("action, transition, and translation law responses are declared-only")
        }
    }
}

/// Boolean shorthand for `runtime_scope_support(law).is_ok()`.
#[must_use]
pub fn runtime_scope_supported(law: &LawIR) -> bool {
    runtime_scope_support(law).is_ok()
}

/// Boolean shorthand for `runtime_semantics_support(law).is_ok()`.
#[must_use]
pub fn runtime_semantics_supported(law: &LawIR) -> bool {
    runtime_semantics_support(law).is_ok()
}

/// Classify one decoded [`LawIR`] across runtime execution, dataset row
/// lowering, and finite-state proof support without catalog link metadata.
///
/// Callers with a `TypeCatalog` or link-read analysis should call
/// [`classify_law_support_with_links`] so link-reading laws accurately report
/// `LawRowLoweringSupport::Unavailable` and `LawProofSupport::RuntimeOnly`.
#[must_use]
pub fn classify_law_support(law: &LawIR) -> LawSupport {
    classify_law_support_with_links(law, false)
}

/// Classify one decoded [`LawIR`] across runtime execution, dataset row
/// lowering, and finite-state proof support, incorporating whether the law's
/// expressions read any declared links (`LAW-LINK-1` / `LAW-LINK-MANY-1`).
#[must_use]
pub fn classify_law_support_with_links(law: &LawIR, reads_links: bool) -> LawSupport {
    let runtime = classify_runtime(law);
    let row_lowering = classify_row_lowering(law, runtime, reads_links);
    let proof = classify_proof(law, runtime, reads_links);
    LawSupport {
        runtime,
        row_lowering,
        proof,
    }
}

fn classify_runtime(law: &LawIR) -> LawRuntimeSupport {
    if let Err(reason) = runtime_scope_support(law) {
        return LawRuntimeSupport::UnsupportedScope(reason);
    }
    if let Err(reason) = runtime_semantics_support(law) {
        return LawRuntimeSupport::UnsupportedSemantics(reason);
    }
    if !law.exceptions.is_empty()
        && crate::law_exceptions::check_tree(&law.id, &law.exceptions).is_err()
    {
        return LawRuntimeSupport::Invalid("exception tree fails structural validation");
    }
    LawRuntimeSupport::Executable
}

fn classify_row_lowering(
    law: &LawIR,
    runtime: LawRuntimeSupport,
    reads_links: bool,
) -> LawRowLoweringSupport {
    if let Some(reason) = runtime.reason() {
        return LawRowLoweringSupport::Unavailable(reason);
    }
    if reads_links {
        return LawRowLoweringSupport::Unavailable(
            "a law that reads a link has no dataset semantics",
        );
    }
    if !law.exceptions.is_empty() {
        return LawRowLoweringSupport::ResiduePerRow(
            "excepted predicate laws settle via per-row exception fold",
        );
    }
    match &law.response {
        LawResponse::Predicate { expression } => {
            let cond_ok = law.condition.as_ref().is_none_or(|cond| {
                norn_expr::row_expression(cond.as_str(), "self")
                    .ok()
                    .flatten()
                    .is_some()
            });
            let pred_ok = norn_expr::row_expression(expression.as_str(), "self")
                .ok()
                .flatten()
                .is_some();
            if cond_ok && pred_ok {
                LawRowLoweringSupport::DirectRowTree
            } else {
                LawRowLoweringSupport::ResiduePerRow(
                    "predicate or condition uses constructs outside the row-lowering subset",
                )
            }
        }
        LawResponse::Invariant { .. } => LawRowLoweringSupport::ResiduePerRow(
            "invariant-reference laws delegate to the invariant evaluator",
        ),
        LawResponse::Action { .. }
        | LawResponse::Transition { .. }
        | LawResponse::TranslationPreserves { .. } => {
            LawRowLoweringSupport::Unavailable("non-predicate law response")
        }
    }
}

fn classify_proof(law: &LawIR, runtime: LawRuntimeSupport, reads_links: bool) -> LawProofSupport {
    if let Some(reason) = runtime.reason() {
        return LawProofSupport::DeclaredOnly(reason);
    }
    if reads_links {
        return LawProofSupport::RuntimeOnly(
            "link-reading laws are outside single-object finite-state checks",
        );
    }
    match &law.response {
        LawResponse::Predicate { .. } => {
            if !law.exceptions.is_empty() {
                LawProofSupport::RuntimeOnly(
                    "excepted predicate laws are runtime-enforced; finite-state proof support pending",
                )
            } else if law.severity != LawSeverity::Error {
                LawProofSupport::RuntimeOnly(
                    "warning-severity laws are outside error-only finite-state checks",
                )
            } else {
                LawProofSupport::FiniteCheckable
            }
        }
        LawResponse::Invariant { .. } => LawProofSupport::RuntimeOnly(
            "invariant-reference laws inherit proof status from their target invariant",
        ),
        LawResponse::Action { .. }
        | LawResponse::Transition { .. }
        | LawResponse::TranslationPreserves { .. } => {
            LawProofSupport::DeclaredOnly("non-predicate law response")
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "fixed valid test laws")]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_law(extra: &serde_json::Value) -> LawIR {
        let mut payload = json!({
            "lawVersion": "0.1",
            "id": "Hr.Shift.MaxSpan",
            "owner": "hr",
            "severity": "error",
            "rationale": "Shifts stay bounded.",
            "provenance": { "source": "authoring_sdk" },
            "subject": "Hr.Shift",
            "scope": {
                "root": { "kind": "entity", "id": "Hr.Shift" },
                "objectTypes": ["Hr.Shift"]
            },
            "modality": { "kind": "always" },
            "response": { "kind": "predicate", "expression": "self.hours <= 8" }
        });
        if let (Some(dst), Some(src)) = (payload.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        serde_json::from_value(payload).expect("valid LawIR")
    }

    #[test]
    fn plain_predicate_law_is_executable_direct_and_finite_checkable() {
        let support = classify_law_support(&sample_law(&json!({})));
        assert_eq!(support.runtime, LawRuntimeSupport::Executable);
        assert_eq!(support.row_lowering, LawRowLoweringSupport::DirectRowTree);
        assert_eq!(support.proof, LawProofSupport::FiniteCheckable);
    }

    #[test]
    fn excepted_predicate_law_is_executable_residue_and_runtime_only_proof() {
        let law = sample_law(&json!({
            "exceptions": [{
                "id": "ramadan",
                "overrides": "Hr.Shift.MaxSpan",
                "condition": "self.is_ramadan == true",
                "rationale": "Seasonal rule",
                "source": { "artifactId": "Hr.Shift.MaxSpan", "pointer": "/payload/exceptions/0" }
            }]
        }));
        let support = classify_law_support(&law);
        assert_eq!(support.runtime, LawRuntimeSupport::Executable);
        assert!(matches!(
            support.row_lowering,
            LawRowLoweringSupport::ResiduePerRow(_)
        ));
        assert!(matches!(support.proof, LawProofSupport::RuntimeOnly(_)));
    }

    #[test]
    fn malformed_exception_tree_is_classified_invalid() {
        let law = sample_law(&json!({
            "exceptions": [{
                "id": "dangling",
                "overrides": "Hr.Shift.MissingParent",
                "condition": "self.is_ramadan == true",
                "rationale": "Broken override target",
                "source": { "artifactId": "Hr.Shift.MaxSpan", "pointer": "/payload/exceptions/0" }
            }]
        }));
        let support = classify_law_support(&law);
        assert!(matches!(support.runtime, LawRuntimeSupport::Invalid(_)));
        assert!(matches!(
            support.row_lowering,
            LawRowLoweringSupport::Unavailable(_)
        ));
        assert!(matches!(support.proof, LawProofSupport::DeclaredOnly(_)));
    }

    #[test]
    fn multi_entity_and_non_entity_roots_are_unsupported_scope() {
        let multi = sample_law(&json!({
            "scope": {
                "root": { "kind": "entity", "id": "Hr.Shift" },
                "objectTypes": ["Hr.Shift", "Hr.Other"]
            }
        }));
        assert!(matches!(
            classify_law_support(&multi).runtime,
            LawRuntimeSupport::UnsupportedScope(_)
        ));
        let app = sample_law(&json!({
            "scope": {
                "root": { "kind": "app", "id": "hr" },
                "objectTypes": ["Hr.Shift"]
            }
        }));
        assert!(matches!(
            classify_law_support(&app).runtime,
            LawRuntimeSupport::UnsupportedScope(_)
        ));
    }

    #[test]
    fn link_reading_law_is_unavailable_for_row_lowering_and_runtime_only_proof() {
        let support = classify_law_support_with_links(&sample_law(&json!({})), true);
        assert_eq!(support.runtime, LawRuntimeSupport::Executable);
        assert!(matches!(
            support.row_lowering,
            LawRowLoweringSupport::Unavailable(_)
        ));
        assert!(matches!(support.proof, LawProofSupport::RuntimeOnly(_)));
    }
}
