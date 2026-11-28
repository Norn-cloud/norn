//! Composable pack manifests and deployment specifications matching
//! `tools/norn-py/norn_py/schema.py` (`PackManifest`, `compose_packs`, `SecretRef`, `DeploymentSpec`).

use std::collections::BTreeMap;

use crate::canonical::JsonValue;
use crate::decimal::format_exact_decimal;
use crate::error::{
    CODE_DEPLOYMENT_ENDPOINT, CODE_DEPLOYMENT_SCOPE, CODE_PACK_CONFLICT, CODE_PACK_CYCLE,
    CODE_PACK_DUPLICATE, CODE_PACK_MISSING_DEP, CODE_RAW_SECRET, CODE_SCHEMA, PyAuthoringError,
    SourceSpan,
};
use crate::schema::types::FnRef;

const FORBIDDEN_SETTING_SECRET_TOKENS: &[&str] = &[
    "secret",
    "password",
    "passwd",
    "token",
    "api_key",
    "private_key",
];

/// Pack category admitted by [`PackManifest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PackCategory {
    /// Foundation pack (`"foundation"`).
    Foundation,
    /// Domain pack (`"domain"`).
    Domain,
    /// Localization overlay (`"localization"`).
    Localization,
    /// Integration pack (`"integration"`).
    Integration,
    /// Views pack (`"views"`).
    Views,
    /// Company overlay (`"company_overlay"`).
    CompanyOverlay,
}

impl PackCategory {
    /// Parses a pack category string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unknown categories.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "foundation" => Ok(Self::Foundation),
            "domain" => Ok(Self::Domain),
            "localization" => Ok(Self::Localization),
            "integration" => Ok(Self::Integration),
            "views" => Ok(Self::Views),
            "company_overlay" => Ok(Self::CompanyOverlay),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported pack category '{raw}'"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Foundation => "foundation",
            Self::Domain => "domain",
            Self::Localization => "localization",
            Self::Integration => "integration",
            Self::Views => "views",
            Self::CompanyOverlay => "company_overlay",
        }
    }
}

/// Conflict-handling policy for [`compose_packs`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PackConflictPolicy {
    /// Fail closed on duplicate artifact IDs across packs (`"fail_closed"`).
    #[default]
    FailClosed,
    /// Later packs override earlier declarations (`"explicit_precedence"`).
    ExplicitPrecedence,
}

impl PackConflictPolicy {
    /// Parses a pack conflict policy string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_PACK_CONFLICT`] on unsupported policies.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "fail_closed" => Ok(Self::FailClosed),
            "explicit_precedence" => Ok(Self::ExplicitPrecedence),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_PACK_CONFLICT,
                format!(
                    "unsupported pack conflict_policy '{raw}' (expected 'fail_closed' or 'explicit_precedence')"
                ),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FailClosed => "fail_closed",
            Self::ExplicitPrecedence => "explicit_precedence",
        }
    }
}

/// Cell host runtime target for [`DeploymentSpec`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CellHostKind {
    /// Rivet cell host (`"rivet"`).
    #[default]
    Rivet,
    /// Local development host (`"local"`).
    Local,
    /// Custom cell host (`"custom"`).
    Custom,
}

impl CellHostKind {
    /// Parses a cell host kind string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported kinds.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "rivet" => Ok(Self::Rivet),
            "local" => Ok(Self::Local),
            "custom" => Ok(Self::Custom),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported cell_host '{raw}' (expected 'rivet', 'local', or 'custom')"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rivet => "rivet",
            Self::Local => "local",
            Self::Custom => "custom",
        }
    }
}

/// Composable Norn Pack manifest matching `norn_py.schema.PackManifest`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackManifest {
    /// Pack identifier (e.g. `"norn.pack.hr"`).
    pub id: String,
    /// Semantic version string.
    pub version: String,
    /// Pack category.
    pub category: PackCategory,
    /// Human-readable pack title.
    pub title: String,
    /// Required dependency pack identifiers.
    pub requires: Vec<String>,
    /// Declared `Concept` dotted names (`__norn_name__`).
    pub entities: Vec<String>,
    /// Declared `LawDecl` identifiers.
    pub laws: Vec<String>,
    /// Declared `MarkingsCatalog` identifiers.
    pub markings: Vec<String>,
    /// Declared external worker handles (`FnRef`).
    pub functions: Vec<FnRef>,
    /// Optional description.
    pub description: Option<String>,
}

impl PackManifest {
    /// Serializes this pack manifest into canonical [`JsonValue`] IR.
    #[must_use]
    pub fn to_ir(&self) -> JsonValue {
        let mut obj = BTreeMap::new();
        obj.insert(
            "category".to_owned(),
            JsonValue::String(self.category.as_str().to_owned()),
        );
        if let Some(desc) = &self.description {
            obj.insert("description".to_owned(), JsonValue::String(desc.clone()));
        }
        obj.insert(
            "entities".to_owned(),
            JsonValue::Array(
                self.entities
                    .iter()
                    .map(|e| JsonValue::String(e.clone()))
                    .collect(),
            ),
        );
        obj.insert(
            "functions".to_owned(),
            JsonValue::Array(
                self.functions
                    .iter()
                    .map(|f| JsonValue::String(format!("{}@{}", f.id, f.version)))
                    .collect(),
            ),
        );
        obj.insert("id".to_owned(), JsonValue::String(self.id.clone()));
        obj.insert(
            "laws".to_owned(),
            JsonValue::Array(
                self.laws
                    .iter()
                    .map(|l| JsonValue::String(l.clone()))
                    .collect(),
            ),
        );
        obj.insert(
            "markings".to_owned(),
            JsonValue::Array(
                self.markings
                    .iter()
                    .map(|m| JsonValue::String(m.clone()))
                    .collect(),
            ),
        );
        obj.insert(
            "requires".to_owned(),
            JsonValue::Array(
                self.requires
                    .iter()
                    .map(|r| JsonValue::String(r.clone()))
                    .collect(),
            ),
        );
        obj.insert("title".to_owned(), JsonValue::String(self.title.clone()));
        obj.insert(
            "version".to_owned(),
            JsonValue::String(self.version.clone()),
        );
        JsonValue::Object(obj)
    }
}

/// Composes packs conjunctively, failing closed on duplicates, missing dependencies,
/// cycles, or conflicting artifact ownership under [`PackConflictPolicy::FailClosed`].
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_PACK_DUPLICATE`], [`CODE_PACK_MISSING_DEP`],
/// [`CODE_PACK_CYCLE`], or [`CODE_PACK_CONFLICT`].
#[derive(Default)]
struct ComposedOwnership {
    entities: BTreeMap<String, String>,
    laws: BTreeMap<String, String>,
    markings: BTreeMap<String, String>,
    functions: BTreeMap<String, String>,
}

pub fn compose_packs(
    packs: &[PackManifest],
    conflict_policy: PackConflictPolicy,
    span: Option<&SourceSpan>,
) -> Result<JsonValue, PyAuthoringError> {
    let mut by_id: BTreeMap<&str, &PackManifest> = BTreeMap::new();
    for pack in packs {
        if by_id.insert(pack.id.as_str(), pack).is_some() {
            return Err(PyAuthoringError::with_optional_span(
                CODE_PACK_DUPLICATE,
                format!("duplicate pack id '{}'", pack.id),
                span.cloned(),
            ));
        }
    }

    for pack in packs {
        for req in &pack.requires {
            if !by_id.contains_key(req.as_str()) {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_PACK_MISSING_DEP,
                    format!(
                        "pack '{}' requires '{req}', which is not in the composed set",
                        pack.id
                    ),
                    span.cloned(),
                ));
            }
        }
    }

    detect_pack_cycles(packs, &by_id, span)?;
    let owned = collect_pack_ownership(packs, conflict_policy, span)?;

    let mut obj = BTreeMap::new();
    obj.insert(
        "conflictPolicy".to_owned(),
        JsonValue::String(conflict_policy.as_str().to_owned()),
    );
    obj.insert("entities".to_owned(), keys_to_json_array(&owned.entities));
    obj.insert("functions".to_owned(), keys_to_json_array(&owned.functions));
    obj.insert("laws".to_owned(), keys_to_json_array(&owned.laws));
    obj.insert("markings".to_owned(), keys_to_json_array(&owned.markings));
    obj.insert(
        "packs".to_owned(),
        JsonValue::Array(packs.iter().map(PackManifest::to_ir).collect()),
    );
    Ok(JsonValue::Object(obj))
}

fn collect_pack_ownership(
    packs: &[PackManifest],
    conflict_policy: PackConflictPolicy,
    span: Option<&SourceSpan>,
) -> Result<ComposedOwnership, PyAuthoringError> {
    let mut owned = ComposedOwnership::default();
    for pack in packs {
        for entity in &pack.entities {
            register_owned(
                &mut owned.entities,
                entity,
                &pack.id,
                "entity",
                conflict_policy,
                span,
            )?;
        }
        for law_id in &pack.laws {
            register_owned(
                &mut owned.laws,
                law_id,
                &pack.id,
                "law",
                conflict_policy,
                span,
            )?;
        }
        for catalog in &pack.markings {
            register_owned(
                &mut owned.markings,
                catalog,
                &pack.id,
                "markings",
                conflict_policy,
                span,
            )?;
        }
        for func in &pack.functions {
            let key = format!("{}@{}", func.id, func.version);
            register_owned(
                &mut owned.functions,
                &key,
                &pack.id,
                "function",
                conflict_policy,
                span,
            )?;
        }
    }
    Ok(owned)
}

fn detect_pack_cycles(
    packs: &[PackManifest],
    by_id: &BTreeMap<&str, &PackManifest>,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    let mut state: BTreeMap<&str, u8> = BTreeMap::new();
    for start in packs {
        if state.get(start.id.as_str()).copied().unwrap_or(0) != 0 {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(start.id.as_str(), 0)];
        let mut path: Vec<&str> = vec![start.id.as_str()];
        state.insert(start.id.as_str(), 1);

        while let Some((cur_id, idx)) = stack.last().copied() {
            let reqs = by_id
                .get(cur_id)
                .map_or(&[][..], |pack| pack.requires.as_slice());
            if let Some(nxt_str) = reqs.get(idx) {
                if let Some(top) = stack.last_mut() {
                    top.1 = idx.saturating_add(1);
                }
                let nxt = nxt_str.as_str();
                let nxt_state = state.get(nxt).copied().unwrap_or(0);
                if nxt_state == 1 {
                    let start_pos = path.iter().position(|&item| item == nxt).unwrap_or(0);
                    let mut cycle_parts: Vec<&str> = path.get(start_pos..).unwrap_or(&[]).to_vec();
                    cycle_parts.push(nxt);
                    let cycle = cycle_parts.join(" -> ");
                    return Err(PyAuthoringError::with_optional_span(
                        CODE_PACK_CYCLE,
                        format!("cyclic pack dependency detected: {cycle}"),
                        span.cloned(),
                    ));
                }
                if nxt_state == 0 {
                    state.insert(nxt, 1);
                    path.push(nxt);
                    stack.push((nxt, 0));
                }
            } else {
                state.insert(cur_id, 2);
                let _popped_stack = stack.pop();
                let _popped_path = path.pop();
            }
        }
    }
    Ok(())
}

fn register_owned(
    table: &mut BTreeMap<String, String>,
    item_id: &str,
    owner: &str,
    kind: &str,
    policy: PackConflictPolicy,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    if let Some(existing) = table.get(item_id)
        && policy == PackConflictPolicy::FailClosed
    {
        return Err(PyAuthoringError::with_optional_span(
            CODE_PACK_CONFLICT,
            format!("{kind} '{item_id}' declared by both '{existing}' and '{owner}'"),
            span.cloned(),
        ));
    }
    table.insert(item_id.to_owned(), owner.to_owned());
    Ok(())
}

fn keys_to_json_array(table: &BTreeMap<String, String>) -> JsonValue {
    JsonValue::Array(table.keys().map(|k| JsonValue::String(k.clone())).collect())
}

/// External secret slot reference (never embeds a secret literal).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretRef {
    /// Secret slot name (e.g. `"WHATSAPP_VERIFY_TOKEN"`).
    pub name: String,
    /// Optional human-readable description.
    pub description: Option<String>,
}

impl SecretRef {
    /// Serializes this secret reference into canonical [`JsonValue`] IR.
    #[must_use]
    pub fn to_ir(&self) -> JsonValue {
        let mut obj = BTreeMap::new();
        if let Some(desc) = &self.description {
            obj.insert("description".to_owned(), JsonValue::String(desc.clone()));
        }
        obj.insert("name".to_owned(), JsonValue::String(self.name.clone()));
        JsonValue::Object(obj)
    }
}

/// Admitted exact setting value in [`DeploymentSpec::initial_settings`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SettingValue {
    /// String setting.
    Str(String),
    /// Exact integer setting.
    Int(String),
    /// Boolean setting.
    Bool(bool),
    /// Fixed-point exact decimal setting (serialized as string).
    Decimal(String),
}

impl SettingValue {
    /// Constructs a validated decimal setting value.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] if `raw` is not an exact bounded decimal.
    pub fn decimal(raw: &str, span: Option<SourceSpan>) -> Result<Self, PyAuthoringError> {
        Ok(Self::Decimal(format_exact_decimal(raw, span)?))
    }

    /// Serializes this setting value into canonical [`JsonValue`].
    #[must_use]
    pub fn to_ir(&self) -> JsonValue {
        match self {
            Self::Str(s) | Self::Decimal(s) => JsonValue::String(s.clone()),
            Self::Int(n) => JsonValue::Number(n.clone()),
            Self::Bool(b) => JsonValue::Bool(*b),
        }
    }
}

/// Live Norn deployment specification binding packs to a cell host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentSpec {
    /// Deployment identifier.
    pub id: String,
    /// Non-empty organization identifier.
    pub org_id: String,
    /// Non-empty environment identifier.
    pub environment_id: String,
    /// Composed packs bound to this deployment.
    pub packs: Vec<PackManifest>,
    /// Cell host kind (default [`CellHostKind::Rivet`]).
    pub cell_host: CellHostKind,
    /// Validated map from `<fn_id>@<semver>` to endpoint `URL`.
    pub worker_endpoints: BTreeMap<String, String>,
    /// Declared external secret slot references.
    pub secret_refs: Vec<SecretRef>,
    /// Validated initial deployment settings (never secret-like keys or floats).
    pub initial_settings: BTreeMap<String, SettingValue>,
}

impl DeploymentSpec {
    /// Validates all deployment scope, pack-composition, endpoint-`URL`, and secret-hygiene
    /// invariants.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_DEPLOYMENT_SCOPE`],
    /// [`CODE_DEPLOYMENT_ENDPOINT`], [`CODE_RAW_SECRET`], or pack composition error codes.
    pub fn validate(&self, span: Option<&SourceSpan>) -> Result<JsonValue, PyAuthoringError> {
        if self.org_id.trim().is_empty() || self.environment_id.trim().is_empty() {
            return Err(PyAuthoringError::with_optional_span(
                CODE_DEPLOYMENT_SCOPE,
                "DeploymentSpec requires non-empty org_id and environment_id (scope fails closed)",
                span.cloned(),
            ));
        }

        let composed = compose_packs(&self.packs, PackConflictPolicy::FailClosed, span)?;

        for (endpoint_key, url) in &self.worker_endpoints {
            if !is_valid_endpoint_key(endpoint_key) {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_DEPLOYMENT_ENDPOINT,
                    format!(
                        "worker endpoint key '{endpoint_key}' must have form '<fn_id>@<semver>'"
                    ),
                    span.cloned(),
                ));
            }
            if !is_valid_worker_endpoint_url(url) {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_DEPLOYMENT_ENDPOINT,
                    format!(
                        "worker endpoint '{endpoint_key}' must use https:// (or loopback http:// for local dev), got '{url}'"
                    ),
                    span.cloned(),
                ));
            }
        }

        for key in self.initial_settings.keys() {
            let lowered = key.to_ascii_lowercase();
            if FORBIDDEN_SETTING_SECRET_TOKENS
                .iter()
                .any(|tok| lowered.contains(tok))
            {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_RAW_SECRET,
                    format!(
                        "setting '{key}' looks like a secret; declare it in secret_refs instead of initial_settings"
                    ),
                    span.cloned(),
                ));
            }
        }

        Ok(composed)
    }

    /// Validates and serializes this deployment specification into canonical [`JsonValue`] IR.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] if any deployment invariant is violated.
    pub fn to_ir(&self, span: Option<&SourceSpan>) -> Result<JsonValue, PyAuthoringError> {
        let composed = self.validate(span)?;

        let mut settings_map = BTreeMap::new();
        for (k, v) in &self.initial_settings {
            settings_map.insert(k.clone(), v.to_ir());
        }

        let mut scope_map = BTreeMap::new();
        scope_map.insert(
            "environmentId".to_owned(),
            JsonValue::String(self.environment_id.clone()),
        );
        scope_map.insert("orgId".to_owned(), JsonValue::String(self.org_id.clone()));

        let mut endpoints_map = BTreeMap::new();
        for (k, v) in &self.worker_endpoints {
            endpoints_map.insert(k.clone(), JsonValue::String(v.clone()));
        }

        let mut obj = BTreeMap::new();
        obj.insert(
            "cellHost".to_owned(),
            JsonValue::String(self.cell_host.as_str().to_owned()),
        );
        obj.insert("composition".to_owned(), composed);
        obj.insert(
            "deploymentVersion".to_owned(),
            JsonValue::String("0.1".to_owned()),
        );
        obj.insert("id".to_owned(), JsonValue::String(self.id.clone()));
        obj.insert(
            "initialSettings".to_owned(),
            JsonValue::Object(settings_map),
        );
        obj.insert("scope".to_owned(), JsonValue::Object(scope_map));
        obj.insert(
            "secretRefs".to_owned(),
            JsonValue::Array(self.secret_refs.iter().map(SecretRef::to_ir).collect()),
        );
        obj.insert(
            "workerEndpoints".to_owned(),
            JsonValue::Object(endpoints_map),
        );
        Ok(JsonValue::Object(obj))
    }
}

/// Validates `<fn_id>@<major>.<minor>.<patch>` matching
/// `^[a-z][a-z0-9._-]*@[0-9]+\.[0-9]+\.[0-9]+$`.
#[must_use]
pub fn is_valid_endpoint_key(key: &str) -> bool {
    let Some((fn_id, version)) = key.split_once('@') else {
        return false;
    };
    let mut fn_bytes = fn_id.bytes();
    let Some(head) = fn_bytes.next() else {
        return false;
    };
    if !head.is_ascii_lowercase()
        || !fn_bytes.all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
    {
        return false;
    }
    let parts: Vec<&str> = version.split('.').collect();
    let [major, minor, patch] = parts.as_slice() else {
        return false;
    };
    [major, minor, patch]
        .iter()
        .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()))
}

/// Validates worker endpoint `URL`s matching `_is_valid_worker_endpoint_url` in `schema.py`.
#[must_use]
pub fn is_valid_worker_endpoint_url(url: &str) -> bool {
    if url.is_empty()
        || url.chars().any(|ch| {
            let code = u32::from(ch);
            code <= 0x20 || (0x7f..=0x9f).contains(&code) || matches!(ch, '\\' | '?' | '#')
        })
    {
        return false;
    }

    let (is_https, rest) = if let Some(after) = url.strip_prefix("https://") {
        (true, after)
    } else if let Some(after) = url.strip_prefix("http://") {
        (false, after)
    } else {
        return false;
    };

    let authority = rest.split_once('/').map_or(rest, |(auth, _)| auth);
    if authority.is_empty() || authority.contains('@') {
        return false;
    }

    let Some((host, port_str)) = split_authority_host_port(authority) else {
        return false;
    };

    if let Some(p_str) = port_str {
        if p_str.is_empty() || !p_str.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        let Ok(port) = p_str.parse::<u16>() else {
            return false;
        };
        if port == 0 {
            return false;
        }
    }

    if is_https {
        !host.is_empty()
    } else {
        matches!(host, "127.0.0.1" | "localhost" | "::1")
    }
}

fn split_authority_host_port(authority: &str) -> Option<(&str, Option<&str>)> {
    if let Some(after_open) = authority.strip_prefix('[') {
        let (ipv6_host, tail) = after_open.split_once(']')?;
        if ipv6_host.is_empty() {
            return None;
        }
        if tail.is_empty() {
            return Some((ipv6_host, None));
        }
        let port = tail.strip_prefix(':')?;
        return Some((ipv6_host, Some(port)));
    }

    if authority.contains('[') || authority.contains(']') {
        return None;
    }

    if let Some((host, port)) = authority.split_once(':') {
        if port.contains(':') {
            return None;
        }
        Some((host, Some(port)))
    } else {
        Some((authority, None))
    }
}
