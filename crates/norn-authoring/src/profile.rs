use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::AuthoringError;

/// Raw JSON of the pinned `norn-effect` authoring profile.
pub const PROFILE_JSON: &str = include_str!("../../../tools/norn-ts/profile/norn-effect-1.json");

/// Ambient TypeScript declarations for `@norn/authoring`.
pub const INTRINSICS_DTS: &str = include_str!("../../../tools/norn-ts/profile/norn-authoring.d.ts");

/// How an admitted profile symbol may be used in authored TypeScript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolUseKind {
    /// Function or constructor call.
    Call,
    /// Method call on a receiver value.
    Method,
    /// Value reference (for example `Schema.String`).
    Value,
    /// Class `extends` heritage expression (for example `Data.TaggedError`).
    ClassHeritage,
}

/// Grammatical context where an admitted symbol is valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolContext {
    /// Inside a `Norn.flow` body.
    Flow,
    /// Top-level or schema definition position.
    Definition,
    /// Inside an `Effect.retry` schedule expression.
    RetryPolicy,
    /// Admitted in any profile context.
    Any,
}

/// A single pinned dependency entry in the authoring profile.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PackagePin {
    /// Exact pinned package version.
    pub version: String,
    /// Subresource integrity digest recorded in the profile.
    pub integrity: String,
    /// SPDX license identifier.
    pub license: String,
    /// Optional human-readable description of the package's role.
    pub role: Option<String>,
}

/// Pinned package versions recorded in the authoring profile.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ProfilePins {
    /// Pinned `effect` package.
    pub effect: PackagePin,
}

/// Import allowlist metadata declared by the profile.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AllowedImports {
    /// Allowed package specifiers (trailing `/*` permits any subpath).
    pub packages: Vec<String>,
    /// Optional explanatory note.
    pub note: Option<String>,
}

/// Summary and remediation guidance for a `NORN-*` diagnostic code.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DiagnosticDoc {
    /// Short description of the diagnostic.
    pub summary: String,
    /// Remediation hint shown to authors.
    pub fix: String,
}

/// An admitted symbol row in the profile table.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AdmittedSymbol {
    /// Canonical symbol identity (`<module>#<qualified name>`).
    pub symbol: String,
    /// Probe expression path used to verify symbol resolution.
    pub probe: String,
    /// Profile semantic row key (must exist in [`Profile::rows`]).
    pub row: String,
    /// Permitted syntactic usage form.
    #[serde(rename = "use")]
    pub use_kind: SymbolUseKind,
    /// Permitted grammatical context.
    pub context: SymbolContext,
    /// Optional explanatory note.
    pub note: Option<String>,
}

/// An explicitly rejected symbol mapping to a stable diagnostic code.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RejectedSymbol {
    /// Canonical symbol identity (`<module>#<qualified name>`).
    pub symbol: String,
    /// Probe expression path.
    pub probe: String,
    /// Diagnostic code emitted when this symbol is referenced.
    pub code: String,
    /// Optional explanatory note.
    pub note: Option<String>,
}

/// A rejected module prefix mapping to a stable diagnostic code.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RejectedModule {
    /// Module name or trailing-`/` module prefix.
    pub module: String,
    /// Diagnostic code emitted for symbols from this module.
    pub code: String,
    /// Optional explanatory note.
    pub note: Option<String>,
}

/// Documentation of a rejected language construct and its corpus fixtures.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RejectedConstruct {
    /// Human-readable description of the rejected construct.
    pub construct: String,
    /// Diagnostic code expected for the construct.
    pub code: String,
    /// Relative paths of rejection corpus fixtures.
    pub fixtures: Vec<String>,
}

/// Machine-readable Norn TypeScript authoring profile.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Profile {
    /// Profile identifier (for example `norn-effect/2`).
    pub profile: String,
    /// Optional status note.
    pub status: Option<String>,
    /// Optional specification path.
    pub spec: Option<String>,
    /// Optional description of the symbol identity convention.
    pub identity: Option<String>,
    /// Pinned reference package versions.
    pub pins: ProfilePins,
    /// Module specifier of the Norn intrinsics declaration (`@norn/authoring`).
    #[serde(rename = "intrinsicsModule")]
    pub intrinsics_module: String,
    /// Package import allowlist.
    #[serde(rename = "allowedImports")]
    pub allowed_imports: AllowedImports,
    /// Diagnostic code dictionary.
    pub diagnostics: BTreeMap<String, DiagnosticDoc>,
    /// Semantic lowering rows referenced by [`Self::admitted`].
    pub rows: BTreeMap<String, String>,
    /// Admitted profile symbols.
    pub admitted: Vec<AdmittedSymbol>,
    /// Explicitly rejected profile symbols.
    pub rejected: Vec<RejectedSymbol>,
    /// Explicitly rejected module prefixes.
    #[serde(rename = "rejectedModules")]
    pub rejected_modules: Vec<RejectedModule>,
    /// Fallback diagnostic code for unlisted symbols from profile modules.
    #[serde(rename = "defaultCode")]
    pub default_code: String,
    /// Reserved intrinsic names planned for later milestones.
    #[serde(default, rename = "reservedIntrinsics")]
    pub reserved_intrinsics: BTreeMap<String, String>,
    /// Catalog of rejected constructs and corpus fixtures.
    #[serde(rename = "rejectedConstructs")]
    pub rejected_constructs: Vec<RejectedConstruct>,
}

impl Profile {
    /// Loads and structurally validates the embedded authoring profile.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidProfile`] if the embedded JSON fails to
    /// deserialize, references an unknown row or diagnostic code, or declares
    /// duplicate or conflicting symbol entries.
    pub fn load() -> Result<Self, AuthoringError> {
        Self::parse(PROFILE_JSON)
    }

    /// Parses and structurally validates a profile JSON document.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidProfile`] if the JSON is malformed or
    /// violates the profile's referential invariants.
    pub fn parse(json: &str) -> Result<Self, AuthoringError> {
        let profile: Self = serde_json::from_str(json)
            .map_err(|err| AuthoringError::InvalidProfile(err.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    /// Looks up an admitted symbol entry by canonical symbol identity.
    #[must_use]
    pub fn admitted_entry(&self, symbol_id: &str) -> Option<&AdmittedSymbol> {
        self.admitted.iter().find(|entry| entry.symbol == symbol_id)
    }

    /// Returns the stable diagnostic code for a non-admitted symbol from a profile module.
    #[must_use]
    pub fn code_for(&self, symbol_id: &str) -> &str {
        if let Some(rejected) = self.rejected.iter().find(|entry| entry.symbol == symbol_id) {
            return rejected.code.as_str();
        }
        let module = symbol_id
            .split_once('#')
            .map_or(symbol_id, |(module, _)| module);
        for entry in &self.rejected_modules {
            let matched = if entry.module.ends_with('/') {
                module.starts_with(entry.module.as_str())
            } else {
                module == entry.module
            };
            if matched {
                return entry.code.as_str();
            }
        }
        self.default_code.as_str()
    }

    fn validate(&self) -> Result<(), AuthoringError> {
        let require_code = |code: &str, context: &str| -> Result<(), AuthoringError> {
            if self.diagnostics.contains_key(code) {
                Ok(())
            } else {
                Err(AuthoringError::InvalidProfile(format!(
                    "{context}: unknown diagnostic code `{code}`"
                )))
            }
        };

        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for entry in &self.admitted {
            if !self.rows.contains_key(entry.row.as_str()) {
                return Err(AuthoringError::InvalidProfile(format!(
                    "admitted `{}`: unknown row `{}`",
                    entry.symbol, entry.row
                )));
            }
            if !seen.insert(entry.symbol.as_str()) {
                return Err(AuthoringError::InvalidProfile(format!(
                    "duplicate symbol `{}`",
                    entry.symbol
                )));
            }
        }

        for entry in &self.rejected {
            require_code(&entry.code, &format!("rejected `{}`", entry.symbol))?;
            if !seen.insert(entry.symbol.as_str()) {
                return Err(AuthoringError::InvalidProfile(format!(
                    "symbol both admitted and rejected or duplicated: `{}`",
                    entry.symbol
                )));
            }
        }

        for entry in &self.rejected_modules {
            require_code(&entry.code, &format!("rejectedModules `{}`", entry.module))?;
        }
        for entry in &self.rejected_constructs {
            require_code(
                &entry.code,
                &format!("rejectedConstructs `{}`", entry.construct),
            )?;
        }
        require_code(&self.default_code, "defaultCode")?;
        Ok(())
    }
}
