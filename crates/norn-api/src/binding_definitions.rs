//! The `binding` retained-definition kind: an organization's record that one
//! gateway tool may run from Norn (CONNECT), with its label, fixed inputs and
//! the declared role the call runs as. Credentials stay in the gateway.
//!
//! Payload `{connector: {gateway, origin, app, tool, version}, inputSchema,
//! outputSchema, manifestDigest, label, inputMapping, runRole}`. The host
//! recomputes `manifestDigest` as `sha256:` over the RFC 8785 (JCS) text of
//! `{format: "norn.connector-tool-manifest/1", gateway, app, tool,
//! inputSchema, outputSchema}`, as CONNECT does, and names the connector
//! `gateway:app/tool@version`, the same string the browser check uses.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::definition_routes::AgentToolBinding;
use crate::jcs;
use crate::refusal::RefusalDiagnostic;

/// The tool manifest format a digest is computed over.
pub const MANIFEST_FORMAT: &str = "norn.connector-tool-manifest/1";

/// The binding payload's members; any other is a finding.
const MEMBERS: [&str; 7] = [
    "connector",
    "inputSchema",
    "outputSchema",
    "manifestDigest",
    "label",
    "inputMapping",
    "runRole",
];

/// The longest label, in characters after trimming.
pub const MAX_LABEL_CHARS: usize = 120;

/// Stable `definition.binding_*` codes.
pub mod codes {
    /// The payload is not the binding shape.
    pub const INVALID_PAYLOAD: &str = "definition.binding_invalid_payload";
    /// A connector part is blank or contains a separator.
    pub const CONNECTOR_INVALID: &str = "definition.binding_connector_invalid";
    /// `manifestDigest` is not the host's recomputation.
    pub const DIGEST_MISMATCH: &str = "definition.binding_digest_mismatch";
    /// A `sha256-` version is not the one the digest derives.
    pub const VERSION_MISMATCH: &str = "definition.binding_version_mismatch";
    /// The label is blank, too long or has control characters.
    pub const LABEL_INVALID: &str = "definition.binding_label_invalid";
    /// `runRole` names no role the cell declares.
    pub const ROLE_UNKNOWN: &str = "definition.binding_role_unknown";
    /// An `inputMapping` key is not a property of `inputSchema`.
    pub const MAPPING_UNKNOWN_FIELD: &str = "definition.binding_mapping_unknown_field";
    /// An `inputMapping` entry is not `{fixed: value}` of the property's type.
    pub const MAPPING_INVALID: &str = "definition.binding_mapping_invalid";
    /// A string or member name is not in Unicode Normalization Form C.
    pub const NOT_NFC: &str = "definition.binding_not_nfc";
}

/// `sha256:<hex>` of the JCS text of the binding's tool manifest.
///
/// # Errors
/// A schema number that is not finite as a double.
pub fn manifest_digest(
    gateway: &str,
    app: &str,
    tool: &str,
    input_schema: &Value,
    output_schema: &Value,
) -> Result<String, String> {
    let manifest = serde_json::json!({
        "format": MANIFEST_FORMAT,
        "gateway": gateway,
        "app": app,
        "tool": tool,
        "inputSchema": input_schema,
        "outputSchema": output_schema,
    });
    Ok(format!(
        "sha256:{}",
        norn_cell::retained::payload_sha256(&jcs::canonicalize(&manifest)?)
    ))
}

/// The version a gateway tool without one is named by: `sha256-` and the
/// first 12 hex of its digest.
#[must_use]
pub fn derived_version(digest: &str) -> String {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    format!("sha256-{}", hex.get(..12).unwrap_or(hex))
}

fn error(code: &str, message: impl Into<String>, pointer: impl Into<String>) -> RefusalDiagnostic {
    RefusalDiagnostic::error(code, message, pointer)
}

fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// #1164: every string and member name of `value` is in NFC. A binding is
/// checked and digested as submitted (JCS keeps strings as given) but stored
/// as Norn1, which normalizes them to NFC; refusing any other form keeps the
/// checked, digested and stored manifest one and the same.
fn nfc_findings(value: &Value, at: &str, findings: &mut Vec<RefusalDiagnostic>) {
    match value {
        Value::String(text) if norn_ir::canonical::nfc(text) != *text => findings.push(error(
            codes::NOT_NFC,
            "a string is in Unicode Normalization Form C",
            at,
        )),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                nfc_findings(item, &format!("{at}/{index}"), findings);
            }
        }
        Value::Object(members) => nfc_members(members, at, findings),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn nfc_members(members: &Map<String, Value>, at: &str, findings: &mut Vec<RefusalDiagnostic>) {
    for (key, member) in members {
        let pointer = format!("{at}/{}", pointer_token(key));
        if norn_ir::canonical::nfc(key) != *key {
            findings.push(error(
                codes::NOT_NFC,
                "a member name is in Unicode Normalization Form C",
                pointer.as_str(),
            ));
        }
        nfc_findings(member, &pointer, findings);
    }
}

fn text<'v>(object: &'v Map<String, Value>, key: &str) -> Option<&'v str> {
    object.get(key).and_then(Value::as_str)
}

/// Every finding for a binding payload. `raw` is the submitted payload text,
/// so numbers in its schemas are read exactly as JCS reads them; `roles` are
/// the roles the cell declares.
#[must_use]
pub fn check(raw: &str, roles: &[&str]) -> Vec<RefusalDiagnostic> {
    let mut findings = Vec::new();
    let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(raw) else {
        findings.push(error(codes::INVALID_PAYLOAD, "a binding is an object", ""));
        return findings;
    };
    nfc_members(&payload, "", &mut findings);
    for key in payload
        .keys()
        .filter(|key| !MEMBERS.contains(&key.as_str()))
    {
        findings.push(error(
            codes::INVALID_PAYLOAD,
            format!("unknown member {key:?}"),
            format!("/{}", pointer_token(key)),
        ));
    }
    let connector = payload.get("connector").and_then(Value::as_object);
    let parts = connector.map(|connector| {
        ["gateway", "origin", "app", "tool", "version"].map(|key| (key, text(connector, key)))
    });
    let Some(parts) = parts else {
        findings.push(error(
            codes::CONNECTOR_INVALID,
            "a binding has a connector {gateway, origin, app, tool, version}",
            "/connector",
        ));
        return findings;
    };
    for (key, value) in parts {
        let separators: &[char] = match key {
            "origin" => &[],
            "tool" => &[':', '/'],
            _ => &[':', '/', '@'],
        };
        if !value.is_some_and(|value| {
            !value.trim().is_empty() && value.trim() == value && !value.contains(separators)
        }) {
            findings.push(error(
                codes::CONNECTOR_INVALID,
                format!("connector {key} is a non-blank string without separators"),
                format!("/connector/{key}"),
            ));
        }
    }
    let [(_, gateway), _, (_, app), (_, tool), (_, version)] = parts;
    let input_schema = payload.get("inputSchema").cloned().unwrap_or(Value::Null);
    let output_schema = payload.get("outputSchema").cloned().unwrap_or(Value::Null);
    if !input_schema.is_object() {
        findings.push(error(
            codes::INVALID_PAYLOAD,
            "inputSchema is a JSON Schema object",
            "/inputSchema",
        ));
    }
    if let (Some(gateway), Some(app), Some(tool)) = (gateway, app, tool) {
        match manifest_digest(gateway, app, tool, &input_schema, &output_schema) {
            Err(detail) => findings.push(error(codes::INVALID_PAYLOAD, detail, "/inputSchema")),
            Ok(digest) => {
                if text(&payload, "manifestDigest") != Some(digest.as_str()) {
                    findings.push(error(
                        codes::DIGEST_MISMATCH,
                        format!("the tool manifest digests to {digest}"),
                        "/manifestDigest",
                    ));
                }
                if let Some(version) = version
                    && version.starts_with("sha256-")
                    && version != derived_version(&digest)
                {
                    findings.push(error(
                        codes::VERSION_MISMATCH,
                        format!("a digest-named version is {}", derived_version(&digest)),
                        "/connector/version",
                    ));
                }
            }
        }
    }
    let label_ok = text(&payload, "label").map(str::trim).is_some_and(|label| {
        (1..=MAX_LABEL_CHARS).contains(&label.chars().count())
            && !label.chars().any(char::is_control)
    });
    if !label_ok {
        findings.push(error(
            codes::LABEL_INVALID,
            format!("a label is 1 to {MAX_LABEL_CHARS} characters after trimming, without control characters"),
            "/label",
        ));
    }
    if !text(&payload, "runRole").is_some_and(|role| roles.contains(&role)) {
        findings.push(error(
            codes::ROLE_UNKNOWN,
            format!(
                "runRole names a role the cell declares: {}",
                roles.join(", ")
            ),
            "/runRole",
        ));
    }
    mapping(&payload, &input_schema, &mut findings);
    findings
}

/// Each `inputMapping` entry fixes one `inputSchema` property to a value of
/// its declared `type` (and `enum`, when declared). Other JSON Schema
/// keywords are not judged here.
fn mapping(
    payload: &Map<String, Value>,
    input_schema: &Value,
    findings: &mut Vec<RefusalDiagnostic>,
) {
    let Some(entries) = payload.get("inputMapping") else {
        findings.push(error(
            codes::MAPPING_INVALID,
            "a binding has an inputMapping",
            "/inputMapping",
        ));
        return;
    };
    let Some(entries) = entries.as_object() else {
        findings.push(error(
            codes::MAPPING_INVALID,
            "inputMapping is an object",
            "/inputMapping",
        ));
        return;
    };
    let properties = input_schema.get("properties").and_then(Value::as_object);
    for (field, entry) in entries {
        let at = format!(
            "/inputMapping/{}",
            field.replace('~', "~0").replace('/', "~1")
        );
        let Some(property) = properties.and_then(|properties| properties.get(field)) else {
            findings.push(error(
                codes::MAPPING_UNKNOWN_FIELD,
                format!("{field:?} is not a property of inputSchema"),
                at,
            ));
            continue;
        };
        let fixed = entry
            .as_object()
            .filter(|entry| entry.len() == 1)
            .and_then(|entry| entry.get("fixed"));
        let Some(fixed) = fixed else {
            findings.push(error(
                codes::MAPPING_INVALID,
                "an entry is {fixed: value}",
                at,
            ));
            continue;
        };
        if !fits(property, fixed) {
            findings.push(error(
                codes::MAPPING_INVALID,
                "the fixed value is not of the property's declared type",
                format!("{at}/fixed"),
            ));
        }
    }
}

fn fits(property: &Value, value: &Value) -> bool {
    let declared: BTreeSet<&str> = match property.get("type") {
        Some(Value::String(one)) => BTreeSet::from([one.as_str()]),
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        _ => BTreeSet::new(),
    };
    let typed = declared.is_empty()
        || declared.iter().any(|kind| match *kind {
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => {
                value.is_i64()
                    || value.is_u64()
                    || value
                        .as_f64()
                        .is_some_and(|n| n.fract().abs() < f64::MIN_POSITIVE)
            }
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "null" => value.is_null(),
            _ => false,
        });
    let enumerated = property
        .get("enum")
        .and_then(Value::as_array)
        .is_none_or(|values| values.contains(value));
    typed && enumerated
}

/// The tool binding an installed binding definition names, for the agent
/// context: `gateway:app/tool@version` and its stored digest.
///
/// # Errors
/// The stored payload is not a well-formed binding.
pub fn agent_binding(id: &str, payload: &str) -> Result<AgentToolBinding, String> {
    let value: Value = serde_json::from_str(payload).map_err(|error| error.to_string())?;
    let connector = value
        .get("connector")
        .and_then(Value::as_object)
        .ok_or("binding has no connector")?;
    let part = |key| text(connector, key).ok_or_else(|| format!("binding connector has no {key}"));
    let name = format!(
        "{}:{}/{}@{}",
        part("gateway")?,
        part("app")?,
        part("tool")?,
        part("version")?
    );
    let digest = value
        .get("manifestDigest")
        .and_then(Value::as_str)
        .ok_or("binding has no manifestDigest")?;
    AgentToolBinding::new(id, name, digest)
}
