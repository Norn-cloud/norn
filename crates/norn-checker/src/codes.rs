//! The diagnostic code registry (T1-CHK-2, #2525).
//!
//! Every code the checker can emit is listed in [`REGISTRY`] with its tier,
//! severity, message template, and the checker version that introduced it.
//! Codes are part of the checker contract: they are never renamed or reused,
//! and a retired code stays registered. `tests/code_registry.rs` scans the
//! crate source so an unregistered code (or a registered code nothing emits)
//! fails the build's tests, and [`conformance_manifest`] is the checked-in
//! `conformance/diagnostic-codes/` consumed by other frontends (#2425).
//!
//! Tiers follow the 2026-08-27 authoring/checker ruling
//! (`knowledge/architecture/design-rulings-2026-08-27-authoring-gateway-checkers.md`).

// CLI/authoring codes use the dedicated cli.* namespace. Their emission
// authority is tools/norn-ts; all other codes require a Rust checker emission.
use norn_ir::DiagnosticSeverity;

/// The envelope declares an IR version this checker does not support.
pub const IR_UNSUPPORTED_VERSION: &str = "ir.validation.unsupported_version";
/// A required envelope field is missing or blank.
pub const IR_REQUIRED_FIELD: &str = "ir.validation.required_field";
/// The envelope (scope, provenance, or JSON values) is malformed.
pub const IR_INVALID_ARTIFACT: &str = "ir.validation.invalid_artifact";
/// A declared dependency is malformed or self-referential.
pub const IR_INVALID_DEPENDENCY: &str = "ir.validation.invalid_dependency";
/// An explicit layout entry took precedence over extracted legacy geometry.
pub const LAYOUT_MERGE_CONFLICT: &str = "layout.validation.merge_conflict";
/// A definition identifier exceeds the admission profile's byte budget.
pub use crate::envelope::IDENTIFIER_TOO_LONG;

/// The checker version that introduced every code registered before the
/// registry existed.
pub const INITIAL_SINCE: &str = "0.1.0";

/// Format tag of the [`conformance_manifest`] document.
pub const MANIFEST_FORMAT: &str = "norn-checker/diagnostic-codes@1";

/// The checker tier a code belongs to (2026-08-27 ruling, "Tiers").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Starlark syntax, names, advisory types, load resolution, limits.
    T0,
    /// Builder construction (arguments, enums, ranges, local ids, CEL parse).
    T1,
    /// Per-artifact IR validation.
    T2,
    /// Artifact-set and graph semantics.
    T3,
    /// BAC plan (compatibility diff, monotonicity, approvals).
    T4,
    /// Activation (re-run T2–T4 on exact bytes, verify plan and evidence).
    T5,
    /// Runtime/commit (input validation, scope, CAS, idempotency, receipts).
    T6,
    /// Gateway/edge.
    T7,
}

impl Tier {
    /// The tier label (`"T2"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::T0 => "T0",
            Self::T1 => "T1",
            Self::T2 => "T2",
            Self::T3 => "T3",
            Self::T4 => "T4",
            Self::T5 => "T5",
            Self::T6 => "T6",
            Self::T7 => "T7",
        }
    }
}

/// One registered diagnostic code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEntry {
    /// The stable dotted code.
    pub code: &'static str,
    /// The checker tier that emits it.
    pub tier: Tier,
    /// The severity every emission carries.
    pub severity: DiagnosticSeverity,
    /// The message shape: the emitted text with `{placeholders}` for the
    /// values each finding interpolates. Emitted messages are human-readable
    /// and may be refined; the code is the contract.
    pub message: &'static str,
    /// The checker version that introduced the code.
    pub since: &'static str,
    /// Supported author action, mutually exclusive with `no_fix`.
    pub fix: Option<&'static str>,
    /// Why an automatic supported repair cannot be offered.
    pub no_fix: Option<&'static str>,
}

include!(concat!(env!("OUT_DIR"), "/diagnostic_registry.rs"));

/// The registry entry for `code`, if it is registered.
#[must_use]
pub fn lookup(code: &str) -> Option<&'static CodeEntry> {
    REGISTRY
        .binary_search_by(|entry| entry.code.cmp(code))
        .ok()
        .and_then(|index| REGISTRY.get(index))
}

/// The serialized severity label (`"error"`), matching the IR wire form.
#[must_use]
pub const fn severity_str(severity: &DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Info => "info",
    }
}

/// The conformance manifest: every registered code with its tier, severity,
/// message template, and `since` version, in registry order. Checked in as
/// `crates/norn-checker/conformance/diagnostic-codes/`.
///
/// The format stays `diagnostic-codes@1`: each code gains one optional member,
/// `fix` or `no_fix`, which is an additive change to an open object (D84).
#[must_use]
pub fn conformance_manifest() -> serde_json::Value {
    let codes: Vec<serde_json::Value> = REGISTRY
        .iter()
        .map(|entry| {
            let mut value = serde_json::json!({
                "code": entry.code,
                "tier": entry.tier.as_str(),
                "severity": severity_str(&entry.severity),
                "message": entry.message,
                "since": entry.since,
            });
            if let (Some(fields), Some(fix)) = (value.as_object_mut(), entry.fix) {
                fields.insert("fix".to_owned(), fix.into());
            }
            if let (Some(fields), Some(reason)) = (value.as_object_mut(), entry.no_fix) {
                fields.insert("no_fix".to_owned(), reason.into());
            }
            value
        })
        .collect();
    serde_json::json!({
        "format": MANIFEST_FORMAT,
        "checkerVersion": env!("CARGO_PKG_VERSION"),
        "codes": codes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sorted_unique_and_well_formed() {
        for pair in REGISTRY.windows(2) {
            if let [left, right] = pair {
                assert!(left.code < right.code, "{} !< {}", left.code, right.code);
            }
        }
        for entry in REGISTRY {
            let segments: Vec<&str> = entry.code.split('.').collect();
            assert_eq!(
                segments.len(),
                3,
                "{} is not <area>.<category>.<detail>",
                entry.code
            );
            assert!(
                segments.iter().all(|segment| !segment.is_empty()
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')),
                "{} is not lower snake case",
                entry.code
            );
            assert!(
                matches!((entry.fix, entry.no_fix), (Some(fix), None) | (None, Some(fix)) if !fix.trim().is_empty()),
                "{} lacks actionable guidance or an explicit no_fix reason",
                entry.code
            );
            assert_ne!(entry.message.len(), 0, "{} has no message", entry.code);
            assert_eq!(lookup(entry.code), Some(entry));
        }
        assert_eq!(lookup("ir.validation.not_a_code"), None);
    }

    #[test]
    fn envelope_constants_are_registered() {
        for code in [
            IR_UNSUPPORTED_VERSION,
            IR_REQUIRED_FIELD,
            IR_INVALID_ARTIFACT,
            IR_INVALID_DEPENDENCY,
        ] {
            assert!(lookup(code).is_some(), "{code}");
        }
    }
}
