//! The checker diagnostic model: a stable [`DiagnosticCode`], a
//! [`DiagnosticSeverity`], a message, and a [`Span`] locating the finding.
//!
//! The validators moved from `norn-ir` in T1-CHK-1 still emit the legacy
//! [`norn_ir::Diagnostic`] wire type (dotted `path`), because that type is
//! embedded in IR payloads and engine errors. [`Diagnostic::from_legacy`] maps
//! it onto this model without changing the code, severity, or message text; the
//! dotted path becomes an RFC 6901 JSON pointer. Every code is registered in
//! [`crate::codes`]; [`diagnostics_to_json`] is the stable JSON output (T1-CHK-2).

use std::fmt;

use crate::codes::{self, CodeEntry, severity_str};

pub use norn_ir::{DiagnosticRemediation, DiagnosticSeverity};

/// A stable, machine-readable diagnostic code such as
/// `ir.validation.required_field`.
///
/// Codes are dotted `<area>.<category>.<detail>` identifiers. Emitted codes
/// are part of the checker contract and are never renamed; every one is
/// registered in [`crate::codes::REGISTRY`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    /// Wrap a code string.
    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        Self(code.into())
    }

    /// The code as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.as_str())
    }
}

impl PartialEq<str> for DiagnosticCode {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for DiagnosticCode {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// A byte range in the authoring source a finding came from (for example the
/// TypeScript definition the extractor compiled into IR).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceSpan {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

/// Where a diagnostic applies: an RFC 6901 JSON pointer into the checked
/// document, plus the authoring-source span when the frontend supplied one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    /// RFC 6901 JSON pointer (`""` is the whole document).
    pub pointer: String,
    /// Authoring-source byte range, when known.
    pub source: Option<SourceSpan>,
}

impl Span {
    /// A span that points at `pointer` with no source range.
    #[must_use]
    pub fn pointer(pointer: impl Into<String>) -> Self {
        Self {
            pointer: pointer.into(),
            source: None,
        }
    }

    /// A span whose pointer is built from exact path segments (object keys and
    /// array indices), each escaped per RFC 6901 (`~` → `~0`, `/` → `~1`). A
    /// key containing `.` stays one segment.
    #[must_use]
    pub fn from_segments<I>(segments: I) -> Self
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let mut pointer = String::new();
        for segment in segments {
            pointer.push('/');
            pointer.push_str(&segment.as_ref().replace('~', "~0").replace('/', "~1"));
        }
        Self::pointer(pointer)
    }

    /// Convert a legacy dotted diagnostic path (`payload.resources[0].id`)
    /// into a JSON-pointer span (`/payload/resources/0/id`) without the
    /// document it points into.
    ///
    /// **Limitation:** legacy paths do not escape `.` inside object keys, and
    /// the legacy [`norn_ir::Diagnostic`] carries no segment list, so without
    /// the document every `.` is read as a separator and a key such as
    /// `foo.bar` becomes two segments. Prefer [`Span::from_legacy_path_in`]
    /// whenever the checked document is available; typed payload roots
    /// (T1-IR-2) will emit segments directly.
    #[must_use]
    pub fn from_legacy_path(path: &str) -> Self {
        Self::from_segments(legacy_tokens(path).into_iter().map(LegacyToken::text))
    }

    /// Convert a legacy dotted diagnostic path into a JSON-pointer span by
    /// resolving it against `document`, the JSON the path was reported on.
    ///
    /// At each object the longest run of dot-separated path parts that names
    /// an existing key is taken as one segment, so a key such as `foo.bar` is
    /// one pointer segment. Once the path leaves the document (the reported
    /// member is missing, which is common for "is required" findings), the
    /// remaining parts fall back to one segment per `.`.
    #[must_use]
    pub fn from_legacy_path_in(path: &str, document: &serde_json::Value) -> Self {
        let tokens = legacy_tokens(path);
        let mut segments: Vec<String> = Vec::new();
        let mut node = Some(document);
        let mut rest = tokens.as_slice();
        while let Some((first, tail)) = rest.split_first() {
            if let LegacyToken::Index(index) = first {
                node = node
                    .and_then(serde_json::Value::as_array)
                    .zip(index.parse::<usize>().ok())
                    .and_then(|(items, position)| items.get(position));
                segments.push((*index).to_owned());
                rest = tail;
                continue;
            }
            let run: Vec<&str> = rest
                .iter()
                .map_while(|token| match token {
                    LegacyToken::Key(key) => Some(*key),
                    LegacyToken::Index(_) => None,
                })
                .collect();
            let resolved = node
                .and_then(serde_json::Value::as_object)
                .and_then(|object| {
                    (1..=run.len()).rev().find_map(|parts| {
                        let key = run.get(..parts)?.join(".");
                        object.get(&key).map(|child| (parts, key, child))
                    })
                });
            let (parts, key, child) = match resolved {
                Some((parts, key, child)) => (parts, key, Some(child)),
                None => (1, first.text().to_owned(), None),
            };
            segments.push(key);
            node = child;
            rest = rest.get(parts..).unwrap_or_default();
        }
        Self::from_segments(segments)
    }
}

/// One part of a legacy dotted path: an object key between dots, or an
/// `[index]` suffix.
#[derive(Debug, Clone, Copy)]
enum LegacyToken<'a> {
    Key(&'a str),
    Index(&'a str),
}

impl<'a> LegacyToken<'a> {
    const fn text(self) -> &'a str {
        match self {
            Self::Key(text) | Self::Index(text) => text,
        }
    }
}

fn legacy_tokens(path: &str) -> Vec<LegacyToken<'_>> {
    let mut tokens = Vec::new();
    for dotted in path.split('.') {
        let (key, indices) = dotted.split_once('[').unwrap_or((dotted, ""));
        if !key.is_empty() {
            tokens.push(LegacyToken::Key(key));
        }
        tokens.extend(
            indices
                .split(['[', ']'])
                .filter(|index| !index.is_empty())
                .map(LegacyToken::Index),
        );
    }
    tokens
}

/// One checker finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Stable machine-readable code.
    pub code: DiagnosticCode,
    /// Severity; any [`DiagnosticSeverity::Error`] rejects the input.
    pub severity: DiagnosticSeverity,
    /// Human-readable message.
    pub message: String,
    /// Location of the finding, when the check can name one.
    pub span: Option<Span>,
    /// The artifact the finding belongs to, when checking an artifact set.
    pub artifact_id: Option<String>,
    /// Suggested remediation class.
    pub remediation: Option<DiagnosticRemediation>,
}

impl Diagnostic {
    /// Map a legacy [`norn_ir::Diagnostic`] onto the checker model. Code,
    /// severity, message, artifact id, and remediation are carried over
    /// unchanged; the dotted path becomes a JSON-pointer [`Span`] via
    /// [`Span::from_legacy_path`] (see its dotted-key limitation; prefer
    /// [`Diagnostic::from_legacy_in`] when the document is available).
    #[must_use]
    pub fn from_legacy(legacy: norn_ir::Diagnostic) -> Self {
        Self {
            code: DiagnosticCode(legacy.code),
            severity: legacy.severity,
            message: legacy.message,
            span: legacy.path.as_deref().map(Span::from_legacy_path),
            artifact_id: legacy.artifact_id,
            remediation: legacy.remediation,
        }
    }

    /// [`Diagnostic::from_legacy`], resolving the dotted path against the
    /// JSON `document` it was reported on so keys containing `.` stay one
    /// pointer segment (see [`Span::from_legacy_path_in`]).
    #[must_use]
    pub fn from_legacy_in(legacy: norn_ir::Diagnostic, document: &serde_json::Value) -> Self {
        let span = legacy
            .path
            .as_deref()
            .map(|path| Span::from_legacy_path_in(path, document));
        Self {
            span,
            ..Self::from_legacy(legacy)
        }
    }

    /// Whether this finding rejects the checked input.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity == DiagnosticSeverity::Error
    }

    /// The registry entry for this finding's code (`None` only for a code
    /// the registry does not know, which the registry tests forbid).
    #[must_use]
    pub fn registration(&self) -> Option<&'static CodeEntry> {
        codes::lookup(self.code.as_str())
    }

    /// This finding as one stable JSON object (see [`diagnostics_to_json`]).
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let span = self.span.as_ref().map_or(serde_json::Value::Null, |span| {
            let source = span.source.map_or(
                serde_json::Value::Null,
                |source| serde_json::json!({ "start": source.start, "end": source.end }),
            );
            serde_json::json!({ "pointer": span.pointer, "source": source })
        });
        let remediation = self
            .remediation
            .as_ref()
            .and_then(|remediation| serde_json::to_value(remediation).ok())
            .unwrap_or(serde_json::Value::Null);
        serde_json::json!({
            "code": self.code.as_str(),
            "tier": self.registration().map(|entry| entry.tier.as_str()),
            "severity": severity_str(&self.severity),
            "message": self.message,
            "span": span,
            "artifactId": self.artifact_id,
            "remediation": remediation,
        })
    }
}

/// Format tag of the [`diagnostics_to_json`] document.
pub const DIAGNOSTICS_FORMAT: &str = "norn-checker/diagnostics@1";

/// The stable JSON output for a set of findings — the document `norn check
/// --format json` prints once the CLI lands (`norn-cli`):
///
/// ```json
/// { "format": "norn-checker/diagnostics@1", "checkerVersion": "0.1.0",
///   "diagnostics": [{ "code": "…", "tier": "T2", "severity": "error",
///     "message": "…", "span": { "pointer": "/irVersion", "source": null },
///     "artifactId": "…", "remediation": "upgrade_ir" }] }
/// ```
///
/// Every key is always present (`null` when absent); diagnostics keep the
/// order the checker produced them in.
#[must_use]
pub fn diagnostics_to_json(diagnostics: &[Diagnostic]) -> serde_json::Value {
    serde_json::json!({
        "format": DIAGNOSTICS_FORMAT,
        "checkerVersion": env!("CARGO_PKG_VERSION"),
        "diagnostics": diagnostics.iter().map(Diagnostic::to_json).collect::<Vec<_>>(),
    })
}

impl From<norn_ir::Diagnostic> for Diagnostic {
    fn from(legacy: norn_ir::Diagnostic) -> Self {
        Self::from_legacy(legacy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_paths_become_json_pointers() {
        assert_eq!(Span::from_legacy_path("irVersion").pointer, "/irVersion");
        assert_eq!(
            Span::from_legacy_path("payload.resources[0].id").pointer,
            "/payload/resources/0/id"
        );
        assert_eq!(
            Span::from_legacy_path("baseSeals[1][2]").pointer,
            "/baseSeals/1/2"
        );
        assert_eq!(Span::from_legacy_path("a~b.c/d").pointer, "/a~0b/c~1d");
        assert_eq!(Span::from_legacy_path("").pointer, "");
    }

    #[test]
    fn dotted_keys_stay_one_segment_when_resolved_against_the_document() {
        let document = serde_json::json!({
            "payload": {
                "fields": { "foo.bar": { "type": "text" }, "a/b~c": {} },
                "resources": [{ "id": "x", "retail.sku": 1 }]
            }
        });
        assert_eq!(
            Span::from_legacy_path_in("payload.fields.foo.bar.type", &document).pointer,
            "/payload/fields/foo.bar/type"
        );
        assert_eq!(
            Span::from_legacy_path_in("payload.resources[0].retail.sku", &document).pointer,
            "/payload/resources/0/retail.sku"
        );
        assert_eq!(
            Span::from_legacy_path_in("payload.fields.a/b~c", &document).pointer,
            "/payload/fields/a~1b~0c"
        );
        // Missing members (e.g. "is required") fall back to one segment per `.`.
        assert_eq!(
            Span::from_legacy_path_in("payload.fields.missing.child", &document).pointer,
            "/payload/fields/missing/child"
        );
        // Without the document the dotted key splits: the documented limitation.
        assert_eq!(
            Span::from_legacy_path("payload.fields.foo.bar").pointer,
            "/payload/fields/foo/bar"
        );
        assert_eq!(
            Span::from_segments(["payload", "fields", "foo.bar"]).pointer,
            "/payload/fields/foo.bar"
        );
    }

    #[test]
    fn legacy_diagnostics_keep_code_severity_and_message() {
        let legacy = norn_ir::Diagnostic {
            code: codes::IR_REQUIRED_FIELD.to_string(),
            severity: DiagnosticSeverity::Error,
            message: "namespace is required".to_string(),
            artifact_id: Some("crm.Account".to_string()),
            path: Some("namespace".to_string()),
            remediation: Some(DiagnosticRemediation::FixDefinition),
        };
        let mapped = Diagnostic::from(legacy);
        assert_eq!(mapped.code, codes::IR_REQUIRED_FIELD);
        assert!(mapped.is_error());
        assert_eq!(mapped.message, "namespace is required");
        assert_eq!(mapped.span, Some(Span::pointer("/namespace")));
        assert_eq!(mapped.artifact_id.as_deref(), Some("crm.Account"));
        assert_eq!(
            mapped.remediation,
            Some(DiagnosticRemediation::FixDefinition)
        );
    }
}
