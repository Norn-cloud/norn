//! Canonical JSON serialization, `Norn1` compact bytes/hashing, and `irVersion: "0.1"`
//! artifact envelopes matching `tools/norn-py/norn_py/canonical.py` and `norn-canonical`.

use std::collections::BTreeMap;

pub use norn_canonical::{JsonValue, NORN1_DOMAIN_TAG};

use crate::error::{CODE_CANONICAL, PyAuthoringError};

/// Maximum permitted JSON nesting depth matching `norn_py.canonical.MAX_JSON_DEPTH`.
pub const MAX_JSON_DEPTH: usize = 128;

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

/// Install-time scope and provenance options matching `norn_py.canonical.EnvelopeOptions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvelopeOptions {
    /// Explicit install-time scope map (e.g. `orgId`, `environmentId`).
    pub scope: BTreeMap<String, String>,
    /// Logical authoring source path embedded in artifact provenance.
    pub source_path: String,
    /// Optional registry package root (switches lifecycle to `"active"`, source to `"system"`,
    /// and clears scope).
    pub registry_package_path: Option<String>,
}

impl EnvelopeOptions {
    /// Constructs [`EnvelopeOptions`] for a draft authored module.
    #[must_use]
    pub fn new(scope: BTreeMap<String, String>, source_path: impl Into<String>) -> Self {
        Self {
            scope,
            source_path: source_path.into(),
            registry_package_path: None,
        }
    }

    /// Sets the registry package path for system/registry artifact emission.
    #[must_use]
    pub fn with_registry_package_path(mut self, path: impl Into<String>) -> Self {
        self.registry_package_path = Some(path.into());
        self
    }
}

/// Exact artifact dependency pin matching `norn_py.canonical.DependencyPin`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DependencyPin {
    /// Target artifact identifier (e.g. `"core.units"`).
    pub artifact_id: String,
    /// Target artifact semantic version (e.g. `"1.0.0"`).
    pub version: String,
    /// Artifact kind (defaults to `"entity"`).
    pub kind: String,
    /// Whether the dependency is required (defaults to `true`).
    pub required: bool,
}

impl DependencyPin {
    /// Constructs a required `"entity"` dependency pin.
    #[must_use]
    pub fn new(artifact_id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            artifact_id: artifact_id.into(),
            version: version.into(),
            kind: "entity".to_owned(),
            required: true,
        }
    }

    /// Serializes this dependency pin into canonical [`JsonValue`] IR.
    #[must_use]
    pub fn to_ir(&self) -> JsonValue {
        let mut obj = BTreeMap::new();
        obj.insert(
            "artifactId".to_owned(),
            JsonValue::String(self.artifact_id.clone()),
        );
        obj.insert("kind".to_owned(), JsonValue::String(self.kind.clone()));
        obj.insert("required".to_owned(), JsonValue::Bool(self.required));
        obj.insert(
            "version".to_owned(),
            JsonValue::String(self.version.clone()),
        );
        JsonValue::Object(obj)
    }
}

/// Metadata header passed to [`build_envelope`].
#[derive(Clone, Debug)]
pub struct EnvelopeHeader<'a> {
    /// Artifact kind (`"entity"` or `"law"`).
    pub kind: &'a str,
    /// Full artifact identifier (e.g. `"hr.Employee.entity"`).
    pub artifact_id: &'a str,
    /// Artifact namespace (e.g. `"hr"`).
    pub namespace: &'a str,
    /// Human-readable artifact title.
    pub name: &'a str,
    /// Semantic version string.
    pub version: &'a str,
}

/// Emits the exact `irVersion: "0.1"` artifact envelope produced by `tools/norn-ts` and `tools/norn-py`.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_CANONICAL`] if the payload violates
/// canonical JSON depth or key constraints.
pub fn build_envelope(
    header: &EnvelopeHeader<'_>,
    payload: BTreeMap<String, JsonValue>,
    options: &EnvelopeOptions,
    dependencies: &[DependencyPin],
) -> Result<JsonValue, PyAuthoringError> {
    let is_registry = options.registry_package_path.is_some();
    let source_path = match &options.registry_package_path {
        Some(raw_root) => {
            let normalized = raw_root.replace('\\', "/");
            let trimmed = normalized.trim_end_matches('/');
            format!("{trimmed}/artifacts/{}.json", header.artifact_id)
        }
        None => options.source_path.clone(),
    };

    let mut scope_map = BTreeMap::new();
    if !is_registry {
        for (k, v) in &options.scope {
            scope_map.insert(k.clone(), JsonValue::String(v.clone()));
        }
    }

    let mut provenance = BTreeMap::new();
    provenance.insert(
        "source".to_owned(),
        JsonValue::String(if is_registry {
            "system".to_owned()
        } else {
            "authoring_sdk".to_owned()
        }),
    );
    provenance.insert("sourcePath".to_owned(), JsonValue::String(source_path));

    let deps = dependencies.iter().map(DependencyPin::to_ir).collect();

    let mut envelope = BTreeMap::new();
    envelope.insert("irVersion".to_owned(), JsonValue::String("0.1".to_owned()));
    envelope.insert(
        "artifactId".to_owned(),
        JsonValue::String(header.artifact_id.to_owned()),
    );
    envelope.insert("kind".to_owned(), JsonValue::String(header.kind.to_owned()));
    envelope.insert(
        "namespace".to_owned(),
        JsonValue::String(header.namespace.to_owned()),
    );
    envelope.insert("name".to_owned(), JsonValue::String(header.name.to_owned()));
    envelope.insert(
        "version".to_owned(),
        JsonValue::String(header.version.to_owned()),
    );
    envelope.insert(
        "lifecycle".to_owned(),
        JsonValue::String(if is_registry {
            "active".to_owned()
        } else {
            "draft".to_owned()
        }),
    );
    envelope.insert("scope".to_owned(), JsonValue::Object(scope_map));
    envelope.insert("dependencies".to_owned(), JsonValue::Array(deps));
    envelope.insert("provenance".to_owned(), JsonValue::Object(provenance));
    envelope.insert("payload".to_owned(), JsonValue::Object(payload));

    let value = JsonValue::Object(envelope);
    validate_json_value(&value, 0, "$")?;
    Ok(value)
}

/// Renders canonical golden-file text matching `tools/norn-ts/src/canonical.ts` and
/// `tools/norn-py/norn_py/canonical.py`: `UTF-16` code-unit sorted keys, two-space
/// indentation, verbatim Unicode (`ensure_ascii=False`), and a trailing newline.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_CANONICAL`] if `value` exceeds
/// [`MAX_JSON_DEPTH`], contains a `"__proto__"` member name, or carries a non-finite number.
pub fn canonical_json(value: &JsonValue) -> Result<String, PyAuthoringError> {
    validate_json_value(value, 0, "$")?;
    let mut out = String::new();
    write_pretty_value(value, 0, &mut out);
    out.push('\n');
    Ok(out)
}

/// Renders compact `Norn1` canonical JSON bytes (zero whitespace, Unicode `NFC`
/// strings/keys, Unicode code-point sorted keys, duplicate-after-`NFC` refusal)
/// directly via `norn_canonical::serialize`.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_CANONICAL`] if `value` violates
/// [`MAX_JSON_DEPTH`], contains `"__proto__"`, has colliding `NFC` keys, or fails
/// `Norn1` canonicalization.
pub fn canonical_norn1_bytes(value: &JsonValue) -> Result<Vec<u8>, PyAuthoringError> {
    validate_json_value(value, 0, "$")?;
    norn_canonical::serialize(value)
        .map_err(|err| PyAuthoringError::unspanned(CODE_CANONICAL, err.to_string()))
}

/// Computes the lower-case hex `SHA-256` digest of `NORN1_DOMAIN_TAG || norn1_bytes`.
#[must_use]
pub fn norn1_sha256_bytes(norn1_bytes: &[u8]) -> String {
    norn_canonical::HashProfile::Norn1.hash_canonical_bytes(norn1_bytes)
}

/// Computes the lower-case hex `SHA-256` digest of `NORN1_DOMAIN_TAG || canonical_norn1_bytes(value)`.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_CANONICAL`] if `Norn1` canonicalization fails.
pub fn canonical_norn1_sha256(value: &JsonValue) -> Result<String, PyAuthoringError> {
    let bytes = canonical_norn1_bytes(value)?;
    Ok(norn1_sha256_bytes(&bytes))
}

fn validate_json_value(
    value: &JsonValue,
    depth: usize,
    path: &str,
) -> Result<(), PyAuthoringError> {
    if depth > MAX_JSON_DEPTH {
        return Err(PyAuthoringError::unspanned(
            CODE_CANONICAL,
            format!("{path}: nesting depth exceeds {MAX_JSON_DEPTH}"),
        ));
    }
    match value {
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::String(_) => Ok(()),
        JsonValue::Number(raw) => {
            if matches!(
                raw.as_str(),
                "NaN" | "+NaN" | "-NaN" | "Infinity" | "+Infinity" | "-Infinity"
            ) {
                return Err(PyAuthoringError::unspanned(
                    CODE_CANONICAL,
                    format!("{path}: non-finite number {raw}"),
                ));
            }
            Ok(())
        }
        JsonValue::Array(items) => {
            let next_depth = depth.saturating_add(1);
            for (idx, item) in items.iter().enumerate() {
                validate_json_value(item, next_depth, &format!("{path}[{idx}]"))?;
            }
            Ok(())
        }
        JsonValue::Object(map) => {
            let next_depth = depth.saturating_add(1);
            for (key, member) in map {
                if key == "__proto__" {
                    return Err(PyAuthoringError::unspanned(
                        CODE_CANONICAL,
                        format!("{path}.{key}: __proto__ is not a portable JSON member name"),
                    ));
                }
                validate_json_value(member, next_depth, &format!("{path}.{key}"))?;
            }
            Ok(())
        }
    }
}

fn write_pretty_value(value: &JsonValue, indent_level: usize, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(true) => out.push_str("true"),
        JsonValue::Bool(false) => out.push_str("false"),
        JsonValue::Number(raw) => out.push_str(raw),
        JsonValue::String(text) => write_json_string(text, out),
        JsonValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            let child_indent = indent_level.saturating_add(1);
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push_str(",\n");
                }
                push_indent(child_indent, out);
                write_pretty_value(item, child_indent, out);
            }
            out.push('\n');
            push_indent(indent_level, out);
            out.push(']');
        }
        JsonValue::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut entries: Vec<(&str, &JsonValue)> =
                map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            entries.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));

            out.push_str("{\n");
            let child_indent = indent_level.saturating_add(1);
            for (idx, (key, val)) in entries.into_iter().enumerate() {
                if idx > 0 {
                    out.push_str(",\n");
                }
                push_indent(child_indent, out);
                write_json_string(key, out);
                out.push_str(": ");
                write_pretty_value(val, child_indent, out);
            }
            out.push('\n');
            push_indent(indent_level, out);
            out.push('}');
        }
    }
}

fn push_indent(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_json_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let code = u32::from(c);
                let hi = usize::try_from((code >> 4) & 0x0f).unwrap_or(0);
                let lo = usize::try_from(code & 0x0f).unwrap_or(0);
                out.push_str("\\u00");
                if let Some(&b) = HEX_LOWER.get(hi) {
                    out.push(char::from(b));
                }
                if let Some(&b) = HEX_LOWER.get(lo) {
                    out.push(char::from(b));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
