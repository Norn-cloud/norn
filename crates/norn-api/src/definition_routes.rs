//! Retained definitions: Studio dashboards and agents saved in the cell.
//!
//! These are not kernel artifacts (the IR has no kind for them). Each kind
//! has a host validator ([`DefinitionKinds`](crate::definition_routes::DefinitionKinds)) whose context comes only from
//! the host. `:admit` runs it without side effects; `:install` and `:retire`
//! re-admit, then append one revision through the governed definitions
//! install ([`norn_cell::SqliteCell::install_retained_definition`]), which
//! creates a new definitions version, its ledger event and commit receipt
//! atomically, with the kernel set and its checker receipt unchanged.
//!
//! Reads need [`Operation::ReadDefinitions`](crate::authz::Operation::ReadDefinitions); writes need
//! [`Operation::WriteDefinitions`](crate::authz::Operation::WriteDefinitions). Every denied or out-of-scope request gets
//! the one opaque 403, decided before the body is interpreted.

use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::StatusCode;
use norn_cell::engine::SqlEngine;
use norn_cell::retained::{
    RetainedDefinition, RetainedDefinitionChange, RetainedDefinitionRecord, RetainedLifecycle,
    RetainedSignoff, payload_sha256,
};
use norn_cell::{CellError, InstalledDefinitionInput};
use norn_ir::{MetricDefinition, QuestionIR, ResourceLimitProfile, RuntimeValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use utoipa::{IntoParams, ToSchema};

use crate::authz::{AccessRequest, Operation, read_denied};
use crate::host::DevHost;
use crate::question_routes::{AdHocQuestions, check_ad_hoc, disclosed_receipt};
use crate::refusal::{Refusal, RefusalDiagnostic, RefusalRestatement};
use crate::scope::ScopeQuery;

/// The largest canonical payload a definition may carry, in bytes.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// The most tiles a dashboard may hold.
pub const MAX_TILES: usize = 64;
/// The longest title, in characters after trimming.
pub const MAX_TITLE_CHARS: usize = 120;
/// The largest grid coordinate or extent of a tile layout.
pub const MAX_LAYOUT: u64 = 1000;
/// The largest list page.
pub const MAX_LIST_LIMIT: u32 = 100;

/// Stable `definition.*` codes.
pub mod codes {
    /// The admitted payload is refused (422, with diagnostics).
    pub const REFUSED: &str = "definition.refused";
    /// `expectedVersion` is not the id's current version (409).
    pub const VERSION_CONFLICT: &str = "definition.version_conflict";
    /// No definition has this id (or this version of it) (404).
    pub const NOT_FOUND: &str = "definition.not_found";
    /// The kind has no validator in this cell (422).
    pub const KIND_UNKNOWN: &str = "definition.kind_unknown";
    /// The id is in the reserved `core.` or `std.` namespace (400).
    pub const ID_RESERVED: &str = "definition.id_reserved";
    /// The id does not match `^[a-z][a-z0-9._-]{0,127}$` (400).
    pub const ID_INVALID: &str = "definition.id_invalid";
    /// The list cursor is malformed or bound to another listing (400).
    pub const CURSOR_INVALID: &str = "definition.cursor_invalid";
    /// The payload is not the kind's shape.
    pub const INVALID_PAYLOAD: &str = "definition.invalid_payload";
    /// A dashboard or tile title is blank, too long or has control characters.
    pub const TILE_TITLE_INVALID: &str = "definition.tile_title_invalid";
    /// A tile id is blank, too long or repeated.
    pub const TILE_ID_INVALID: &str = "definition.tile_id_invalid";
    /// A tile view is not `chart` (`line`, `bar`, `area`) or `table`.
    pub const VIEW_INVALID: &str = "definition.view_invalid";
    /// A tile layout is not non-negative bounded integers with a positive extent.
    pub const LAYOUT_INVALID: &str = "definition.layout_invalid";
    /// A tile question does not decode as a `QuestionIR`.
    pub const QUESTION_INVALID: &str = "definition.question_invalid";
    /// This cell admits no ad-hoc questions, so a tile cannot be checked.
    pub const QUESTION_UNAVAILABLE: &str = "definition.question_unavailable";
    /// The id is used by a definition of another kind.
    pub const ID_IN_USE: &str = "definition.id_in_use";
    /// The definition is already retired.
    pub const ALREADY_RETIRED: &str = "definition.already_retired";
    /// The sign-off's restatement digest is not the host-recomputed one (422).
    pub const SIGNOFF_MISMATCH: &str = "definition.signoff_mismatch";
    /// This cell cannot restate the payload, so it cannot verify a sign-off (422).
    pub const SIGNOFF_UNAVAILABLE: &str = "definition.signoff_unavailable";
    /// A flow is installed only by signing the server's words for it when
    /// the cell can restate it (422): it changes what the system does.
    pub const SIGNOFF_REQUIRED: &str = "definition.signoff_required";
}

/// One tool binding the host resolved for the org from CONNECT's stored
/// bindings: what an agent's `tools` may name. Named exactly as the browser
/// check names it, so the canvas and the server agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolBinding {
    /// The tool binding id.
    pub id: String,
    /// `gateway:app/tool@version`.
    pub connector: String,
    /// `sha256:<64 lower-case hex>` over the canonical JSON
    /// `{format: "norn.connector-tool-manifest/1", gateway, app, tool,
    /// inputSchema, outputSchema}`, as CONNECT stores it.
    pub manifest_digest: String,
}

impl AgentToolBinding {
    /// A binding whose `connector` and `manifestDigest` follow the naming
    /// contract.
    ///
    /// # Errors
    /// A blank id, a connector that is not `gateway:app/tool@version`, or a
    /// digest that is not `sha256:<64 lower-case hex>`.
    pub fn new(
        id: impl Into<String>,
        connector: impl Into<String>,
        manifest_digest: impl Into<String>,
    ) -> Result<Self, String> {
        let binding = Self {
            id: id.into(),
            connector: connector.into(),
            manifest_digest: manifest_digest.into(),
        };
        if binding.id.trim().is_empty() {
            return Err("a tool binding has a blank id".to_owned());
        }
        if !connector_well_formed(&binding.connector) {
            return Err(format!(
                "tool binding {:?}: connector {:?} is not gateway:app/tool@version",
                binding.id, binding.connector
            ));
        }
        let hex = binding.manifest_digest.strip_prefix("sha256:");
        if !hex.is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(format!(
                "tool binding {:?}: manifestDigest is not sha256:<64 lower-case hex>",
                binding.id
            ));
        }
        Ok(binding)
    }
}

/// Whether `connector` is `gateway:app/tool@version`, each part non-blank
/// and free of the separators that delimit the others.
#[must_use]
pub fn connector_well_formed(connector: &str) -> bool {
    let part = |text: &str, banned: &[char]| {
        !text.trim().is_empty() && text.trim() == text && !text.contains(banned)
    };
    let Some((gateway, rest)) = connector.split_once(':') else {
        return false;
    };
    let Some((app, rest)) = rest.split_once('/') else {
        return false;
    };
    let Some((tool, version)) = rest.rsplit_once('@') else {
        return false;
    };
    part(gateway, &[':', '/', '@'])
        && part(app, &[':', '/', '@'])
        && part(tool, &[':', '/'])
        && part(version, &[':', '/', '@'])
}

/// What an agent is admitted against: host-supplied only, never from the
/// request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAdmissionContext {
    /// The model allowlist.
    pub models: BTreeSet<String>,
    /// The org's resolved tool bindings, each built with
    /// [`AgentToolBinding::new`].
    pub bindings: Vec<AgentToolBinding>,
    /// The admitted law ids.
    pub laws: BTreeSet<String>,
    /// The resource-limit profile.
    pub limits: ResourceLimitProfile,
}

/// The agent checker: canonical payload JSON text and the host context in,
/// findings with RFC 6901 pointers into the payload out. Any error-severity
/// finding refuses.
pub type AgentChecker = fn(&str, &AgentAdmissionContext) -> Vec<norn_checker::Diagnostic>;

/// The per-kind validator registry. `dashboard` is built in; `agent` is
/// admitted only once a checker is registered with [`Self::with_agent`].
#[derive(Debug, Default)]
pub struct DefinitionKinds {
    agent: Option<(AgentChecker, AgentAdmissionContext)>,
    restater: Option<Restater>,
    roles: Vec<String>,
    builtins: Option<crate::flow_definitions::Builtins>,
}

/// The deterministic plain-words restatement generator (D59), shared with
/// Studio through the checker: a kind, its admitted canonical payload and
/// the cell's admitted metric declarations (labels and ratio operands are
/// not in a `QuestionIR`) in, the restatement a signer read out. `Err` when
/// the payload has nothing to restate. A sign-off is the SHA-256 of these
/// UTF-8 bytes.
pub type Restater = fn(&str, &str, &[MetricDefinition]) -> Result<String, String>;

/// The checker's restatement (`norn_checker::metric::restate`), the words
/// Studio shows a signer: a saved question's `QuestionIR`, or a dashboard
/// with one line per tile. A flow restates from its IR
/// ([`crate::flow_definitions::restate`]). Other kinds have none.
///
/// # Errors
/// Another kind, or a payload the checker cannot restate.
pub fn checker_restatement(
    kind: &str,
    canonical: &str,
    metrics: &[MetricDefinition],
) -> Result<String, String> {
    use norn_checker::metric::restate::{restate_dashboard, restate_question};
    match kind {
        "question" => {
            let question: QuestionIR =
                serde_json::from_str(canonical).map_err(|error| error.to_string())?;
            restate_question(&question, metrics).map_err(|error| error.to_string())
        }
        "dashboard" => {
            let payload: Value =
                serde_json::from_str(canonical).map_err(|error| error.to_string())?;
            restate_dashboard(&payload, metrics).map_err(|error| error.to_string())
        }
        "flow" => crate::flow_definitions::restate(canonical),
        other => Err(format!("{other} has no restatement")),
    }
}

impl DefinitionKinds {
    /// Declare the roles a binding's `runRole` may name: the cell's own role
    /// table, never the request. Without any, every binding is refused.
    #[must_use]
    pub fn with_roles(mut self, roles: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.roles = roles.into_iter().map(Into::into).collect();
        self
    }

    /// Register the built-in function resolver a flow's performs are
    /// checked against (`norn_builtins::resolve`).
    #[must_use]
    pub fn with_builtins(mut self, resolve: crate::flow_definitions::Builtins) -> Self {
        self.builtins = Some(resolve);
        self
    }

    /// Register the restatement generator that verifies sign-offs.
    #[must_use]
    pub fn with_restater(mut self, restate: Restater) -> Self {
        self.restater = Some(restate);
        self
    }

    /// Register the agent checker with its host context.
    #[must_use]
    pub fn with_agent(mut self, check: AgentChecker, context: AgentAdmissionContext) -> Self {
        self.agent = Some((check, context));
        self
    }
}

fn request_payload_schema() -> utoipa::openapi::schema::Object {
    crate::objects::free_form("The kind's payload.", false)
}

fn admitted_payload_schema() -> utoipa::openapi::schema::Object {
    crate::objects::free_form("The admitted payload, as canonical JSON.", false)
}

/// `POST /v1/definitions:admit`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmitDefinitionRequest {
    /// `dashboard`, `question`, `agent` or `binding`.
    pub kind: String,
    /// The definition id.
    pub id: String,
    /// The kind's payload.
    #[schema(schema_with = request_payload_schema)]
    pub payload: Box<RawValue>,
}

/// `POST /v1/definitions:install`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallDefinitionRequest {
    /// `dashboard`, `question`, `agent` or `binding`.
    pub kind: String,
    /// The definition id.
    pub id: String,
    /// The kind's payload.
    #[schema(schema_with = request_payload_schema)]
    pub payload: Box<RawValue>,
    /// The id's current version; absent requires that the id is unused.
    pub expected_version: Option<u64>,
    /// A sign-off of the payload's plain-words restatement (D59). The host
    /// recomputes the restatement and records the installing principal.
    pub signoff: Option<SignoffRequest>,
}

/// What a signer claims to have read.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignoffRequest {
    /// Lower-case hex SHA-256 of the UTF-8 restatement.
    pub restatement_sha256: String,
}

/// A recorded sign-off: every field set by the host.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignoffDocument {
    /// The installing principal.
    pub signer: String,
    /// When the signed version was installed, in Unix milliseconds.
    pub signed_at_ms: u64,
    /// Lower-case hex SHA-256 of the host-recomputed restatement.
    pub restatement_sha256: String,
}

/// `POST /v1/definitions/{id}:retire`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetireDefinitionRequest {
    /// The id's current version.
    pub expected_version: u64,
}

/// The admission decision.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdmitDefinitionResponse {
    /// True when no finding is an error.
    pub admitted: bool,
    /// Every finding, each pointing into the payload.
    pub diagnostics: Vec<RefusalDiagnostic>,
    /// The plain-words restatement of the admitted payload a signer signs
    /// (D59); null when not admitted or the kind has no restatement.
    #[schema(required = true, nullable = true)]
    pub restatement: Option<String>,
    /// Lower-case hex SHA-256 of `restatement`; null with it.
    #[schema(required = true, nullable = true)]
    pub restatement_sha256: Option<String>,
}

/// The commit that installed a version.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionReceipt {
    /// The commit receipt's hash, for a reader granted Values on every object
    /// type of the cell; every other reader gets its keyed handle
    /// (`norn.receipt/1`, D53), since the receipt commits to the ledger head
    /// and so to every earlier event's values. Null when the cell holds no
    /// handle key.
    #[schema(required = true, nullable = true)]
    pub receipt_hash: Option<String>,
    /// The commit sequence.
    pub commit_seq: u64,
    /// Host time of the commit, in Unix milliseconds.
    pub recorded_at_ms: u64,
}

/// An installed (or retired) version.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstallDefinitionResponse {
    /// Always true.
    pub installed: bool,
    /// The definition id.
    pub id: String,
    /// The new version.
    pub version: u64,
    /// Its commit.
    pub receipt: DefinitionReceipt,
    /// For a flow: its id, version, rule and Run type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<crate::flow_definitions::FlowSummary>,
}

/// `GET /v1/definitions:list` query.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListDefinitionsQuery {
    /// Required organization.
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Required environment.
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// The kind to list.
    pub kind: Option<String>,
    /// `retired` also lists retired definitions.
    pub include: Option<String>,
    /// Page size, 1–100 (default 50).
    pub limit: Option<u32>,
    /// The previous page's cursor, bound to `kind`, `include` and `uses`.
    pub cursor: Option<String>,
    /// With `kind=flow`: only flows whose performs use this binding id.
    pub uses: Option<String>,
}

/// One listed definition, without its payload.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionSummary {
    /// The definition id.
    pub id: String,
    /// Its kind.
    pub kind: String,
    /// Its title.
    pub title: String,
    /// Its current version.
    pub version: u64,
    /// `active` or `retired`.
    pub lifecycle: String,
    /// When its current version was installed, in Unix milliseconds.
    pub updated_at_ms: u64,
    /// With `uses`: the binding version this flow's performs are pinned to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_version: Option<String>,
}

/// One page of definitions, ordered by id.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionPage {
    /// This page.
    pub items: Vec<DefinitionSummary>,
    /// The next page's cursor; null on the last page.
    pub cursor: Option<String>,
}

/// `GET /v1/definitions/{id}` query.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct GetDefinitionQuery {
    /// Required organization.
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Required environment.
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// A past version; absent selects the current one.
    pub version: Option<u64>,
}

/// One version of one definition.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionDocument {
    /// The definition id.
    pub id: String,
    /// Its kind.
    pub kind: String,
    /// This version.
    pub version: u64,
    /// `active` or `retired`.
    pub lifecycle: String,
    /// The admitted payload, as canonical JSON.
    #[schema(schema_with = admitted_payload_schema)]
    pub payload: Box<RawValue>,
    /// The commit that installed this version.
    pub receipt: DefinitionReceipt,
    /// This version's sign-off; null when it was installed unsigned.
    #[schema(required = true, nullable = true)]
    pub signoff: Option<SignoffDocument>,
    /// The restatement of this version's payload, recomputed at read time
    /// against the cell's current declarations; null when the kind has none.
    /// When it no longer hashes to `signoff.restatementSha256`, the words the
    /// signer read have changed meaning since they signed.
    #[schema(required = true, nullable = true)]
    pub restatement: Option<String>,
    /// Lower-case hex SHA-256 of `restatement`; null with it.
    #[schema(required = true, nullable = true)]
    pub restatement_sha256: Option<String>,
    /// For a flow: its id, version, rule and Run type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<crate::flow_definitions::FlowSummary>,
}

/// Whether `id` matches `^[a-z][a-z0-9._-]{0,127}$`.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && id.len() <= 128
        && bytes.all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Decide `operation` for `actor` and admit the explicit scope: the one
/// opaque 403 otherwise. Transports call it before interpreting a body.
/// # Errors
/// A denied, anonymous or out-of-scope request.
pub fn authorize<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    operation: Operation,
) -> Result<(), Refusal> {
    let authorized = host.authorizer.authorize(&AccessRequest {
        scope: host.scope(),
        actor,
        operation,
    });
    let scoped = host
        .scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref());
    if authorized.is_err() || scoped.is_err() || actor.is_none() {
        return Err(read_denied());
    }
    Ok(())
}

/// A write's id: valid, and not a new id in a reserved namespace. An id that
/// already names a retained definition keeps its revisions (D84: existing
/// definitions stay readable, retirable and revisable).
fn check_new_id<E: SqlEngine>(host: &DevHost<E>, id: &str) -> Result<(), Refusal> {
    check_id(id)?;
    if crate::kernel_set::is_reserved(id) {
        let existing = {
            let cell = host.cell()?;
            let reader = cell.store().read();
            reader
                .retained_definition(id, None)
                .map_err(Refusal::host)?
                .is_some()
        };
        if !existing {
            return Err(Refusal::new(
                StatusCode::BAD_REQUEST,
                codes::ID_RESERVED,
                "the definition id is in a reserved namespace",
                "core. and std. name the trusted base; choose another namespace",
            ));
        }
    }
    Ok(())
}

fn check_id(id: &str) -> Result<(), Refusal> {
    if valid_id(id) {
        return Ok(());
    }
    Err(Refusal::new(
        StatusCode::BAD_REQUEST,
        codes::ID_INVALID,
        "the definition id is invalid",
        "an id matches ^[a-z][a-z0-9._-]{0,127}$",
    ))
}

fn kind_unknown(kind: &str) -> Refusal {
    Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        codes::KIND_UNKNOWN,
        "the definition kind is not admitted by this cell",
        format!("this cell admits no definitions of kind {kind:?}"),
    )
}

/// The 422 `definition.refused` carrying `diagnostics`.
pub(crate) fn refused_with(diagnostics: Vec<RefusalDiagnostic>) -> Refusal {
    refused(diagnostics)
}

fn refused(diagnostics: Vec<RefusalDiagnostic>) -> Refusal {
    let mut refusal = Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        codes::REFUSED,
        "the definition is not admissible",
        "each diagnostic names the rule and where in the payload it applies",
    );
    refusal.diagnostics = diagnostics;
    refusal
}

fn version_conflict() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        codes::VERSION_CONFLICT,
        "the definition changed since it was read",
        "reopen the definition and apply the change to its current version",
    )
}

fn not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        codes::NOT_FOUND,
        "no such definition",
        "no definition (or version of it) with this id is installed in the cell",
    )
}

/// A payload admitted by its kind's validator, ready to install.
struct Admitted {
    title: String,
    canonical: String,
    flow: Option<crate::flow_definitions::Flow>,
}

/// Canonical JSON text of `payload`.
fn canonical(payload: &RawValue) -> Result<String, Refusal> {
    let value = RuntimeValue::from_json_text(payload.get())
        .map_err(|error| Refusal::invalid(format!("payload: {error}")))?;
    let bytes = value
        .canonical_json()
        .map_err(|error| Refusal::invalid(format!("payload: {error}")))?;
    let text = String::from_utf8(bytes).map_err(Refusal::host)?;
    if text.len() > MAX_PAYLOAD_BYTES {
        return Err(Refusal::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "definition.payload_too_large",
            "the definition payload is too large",
            format!("a payload is at most {MAX_PAYLOAD_BYTES} bytes of canonical JSON"),
        ));
    }
    Ok(text)
}

/// Run `kind`'s validator: the canonical payload and every finding.
#[allow(
    clippy::too_many_arguments,
    reason = "a definition's identity, payload and the host context its validator reads"
)]
fn validate<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
    kinds: &DefinitionKinds,
    questions: Option<&dyn AdHocQuestions<E>>,
    id: &str,
    kind: &str,
    payload: &RawValue,
) -> Result<(Admitted, Vec<RefusalDiagnostic>), Refusal> {
    let canonical = canonical(payload)?;
    let value: Value = serde_json::from_str(&canonical).map_err(Refusal::host)?;
    let mut findings = Vec::new();
    let mut flow = None;
    match kind {
        "flow" => {
            let (flow_findings, checked) = crate::flow_definitions::check(
                id,
                &canonical,
                host.scope(),
                kinds.builtins,
                &active_binding_versions(host)?,
            );
            findings.extend(flow_findings);
            flow = checked;
        }
        "dashboard" => dashboard(host, scope, actor, questions, &value, &mut findings)?,
        // A saved question: a bare QuestionIR, admitted as a tile's is.
        "question" => {
            question_findings(
                host,
                scope,
                actor,
                questions,
                Some(&value),
                "",
                &mut findings,
            )?;
        }
        "agent" => {
            let (check, context) = kinds.agent.as_ref().ok_or_else(|| kind_unknown(kind))?;
            // The tools an agent may name are the cell's installed, active
            // bindings, resolved now; never the request's.
            let context = AgentAdmissionContext {
                bindings: installed_bindings(host)?,
                ..context.clone()
            };
            findings.extend(RefusalDiagnostic::from_checker(
                &check(&canonical, &context),
                "",
            ));
        }
        "binding" => {
            let roles: Vec<&str> = kinds.roles.iter().map(String::as_str).collect();
            findings.extend(crate::binding_definitions::check(payload.get(), &roles));
        }
        other => return Err(kind_unknown(other)),
    }
    // A saved question's title is its QuestionIR name; a binding's its label.
    let title_member = match kind {
        "question" => "name",
        "binding" => "label",
        _ => "title",
    };
    let title = value
        .get(title_member)
        .and_then(Value::as_str)
        .map(|title| title.trim().to_owned())
        .unwrap_or_default();
    let title = flow.as_ref().map_or(title, |flow| {
        flow.summary
            .title
            .clone()
            .unwrap_or_else(|| flow.summary.flow_id.clone())
    });
    Ok((
        Admitted {
            title,
            canonical,
            flow,
        },
        findings,
    ))
}

/// A flow definition's summary, read from its stored payload.
fn flow_summary(definition: &RetainedDefinition) -> Option<crate::flow_definitions::FlowSummary> {
    if definition.kind != "flow" {
        return None;
    }
    let value: Value = serde_json::from_str(&definition.payload).ok()?;
    let flow_id = value.pointer("/flow/id")?.as_str()?;
    let (_, rule_artifact_id) = crate::flow_definitions::owned_ids(flow_id);
    Some(crate::flow_definitions::FlowSummary {
        flow_id: flow_id.to_owned(),
        version: value.pointer("/flow/version")?.as_str()?.to_owned(),
        title: value
            .pointer("/flow/title")
            .and_then(Value::as_str)
            .map(|title| title.trim().to_owned()),
        rule_artifact_id,
        run_type: crate::flow_definitions::run_type(flow_id),
    })
}

/// The current version of each installed, active binding definition.
fn active_binding_versions<E: SqlEngine>(
    host: &DevHost<E>,
) -> Result<std::collections::BTreeMap<String, u64>, Refusal> {
    let cell = host.cell()?;
    binding_versions(&cell.store().read())
}

/// As [`active_binding_versions`], from a reader the caller already holds.
fn binding_versions<E: SqlEngine>(
    reader: &norn_cell::StoreReader<'_, E>,
) -> Result<std::collections::BTreeMap<String, u64>, Refusal> {
    let mut versions = std::collections::BTreeMap::new();
    let mut after: Option<String> = None;
    loop {
        let page = reader
            .retained_definitions("binding", false, after.as_deref(), MAX_LIST_LIMIT)
            .map_err(Refusal::host)?;
        for record in &page {
            versions.insert(record.definition.id.clone(), record.definition.revision);
        }
        if page.len() < usize::try_from(MAX_LIST_LIMIT).unwrap_or(usize::MAX) {
            return Ok(versions);
        }
        after = page.last().map(|record| record.definition.id.clone());
    }
}

/// The cell's installed, non-retired binding definitions as agent tool
/// bindings.
fn installed_bindings<E: SqlEngine>(host: &DevHost<E>) -> Result<Vec<AgentToolBinding>, Refusal> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    let mut bindings = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = reader
            .retained_definitions("binding", false, after.as_deref(), MAX_LIST_LIMIT)
            .map_err(Refusal::host)?;
        for record in &page {
            bindings.push(
                crate::binding_definitions::agent_binding(
                    &record.definition.id,
                    &record.definition.payload,
                )
                .map_err(Refusal::host)?,
            );
        }
        if page.len() < usize::try_from(MAX_LIST_LIMIT).unwrap_or(usize::MAX) {
            return Ok(bindings);
        }
        after = page.last().map(|record| record.definition.id.clone());
    }
}

fn admitted(findings: &[RefusalDiagnostic]) -> bool {
    !findings.iter().any(|finding| finding.severity == "error")
}

/// Members of `value` at `at` as an object, reporting unknown members.
fn members<'v>(
    value: &'v Value,
    at: &str,
    known: &[&str],
    findings: &mut Vec<RefusalDiagnostic>,
) -> Option<&'v serde_json::Map<String, Value>> {
    let Some(object) = value.as_object() else {
        findings.push(RefusalDiagnostic::error(
            codes::INVALID_PAYLOAD,
            "expected an object",
            at,
        ));
        return None;
    };
    for key in object.keys().filter(|key| !known.contains(&key.as_str())) {
        findings.push(RefusalDiagnostic::error(
            codes::INVALID_PAYLOAD,
            format!("unknown member {key:?}"),
            format!("{at}/{}", pointer_token(key)),
        ));
    }
    Some(object)
}

fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn title(value: Option<&Value>, at: &str, findings: &mut Vec<RefusalDiagnostic>) {
    let text = value.and_then(Value::as_str);
    let trimmed = text.map(str::trim);
    let ok = trimmed.is_some_and(|title| {
        let chars = title.chars().count();
        (1..=MAX_TITLE_CHARS).contains(&chars) && !title.chars().any(char::is_control)
    });
    if !ok {
        findings.push(RefusalDiagnostic::error(
            codes::TILE_TITLE_INVALID,
            format!(
                "a title is 1 to {MAX_TITLE_CHARS} characters after trimming, without control characters or newlines"
            ),
            at,
        ));
    }
}

fn dashboard<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
    questions: Option<&dyn AdHocQuestions<E>>,
    value: &Value,
    findings: &mut Vec<RefusalDiagnostic>,
) -> Result<(), Refusal> {
    let Some(object) = members(value, "", &["title", "tiles"], findings) else {
        return Ok(());
    };
    title(object.get("title"), "/title", findings);
    let Some(tiles) = object.get("tiles").and_then(Value::as_array) else {
        findings.push(RefusalDiagnostic::error(
            codes::INVALID_PAYLOAD,
            "tiles is an array",
            "/tiles",
        ));
        return Ok(());
    };
    if tiles.len() > MAX_TILES {
        findings.push(RefusalDiagnostic::error(
            codes::INVALID_PAYLOAD,
            format!("a dashboard has at most {MAX_TILES} tiles"),
            "/tiles",
        ));
        return Ok(());
    }
    let mut ids = BTreeSet::new();
    for (index, tile) in tiles.iter().enumerate() {
        let at = format!("/tiles/{index}");
        let Some(tile) = members(
            tile,
            &at,
            &["id", "title", "question", "view", "layout"],
            findings,
        ) else {
            continue;
        };
        let id = tile.get("id").and_then(Value::as_str);
        if !id.is_some_and(|id| !id.trim().is_empty() && id.len() <= 128 && ids.insert(id)) {
            findings.push(RefusalDiagnostic::error(
                codes::TILE_ID_INVALID,
                "a tile id is a non-blank string of at most 128 bytes, unique in the dashboard",
                format!("{at}/id"),
            ));
        }
        title(tile.get("title"), &format!("{at}/title"), findings);
        view(tile.get("view"), &format!("{at}/view"), findings);
        layout(tile.get("layout"), &format!("{at}/layout"), findings);
        question_findings(
            host,
            scope,
            actor,
            questions,
            tile.get("question"),
            &format!("{at}/question"),
            findings,
        )?;
    }
    Ok(())
}

/// Admit `value` as a `QuestionIR` exactly as `POST /v1/questions:evaluate`
/// does (F2b), its findings' pointers nested under `under`. An ungranted or
/// undeclared read is the one opaque 403.
fn question_findings<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
    questions: Option<&dyn AdHocQuestions<E>>,
    value: Option<&Value>,
    under: &str,
    findings: &mut Vec<RefusalDiagnostic>,
) -> Result<(), Refusal> {
    let question =
        match value.map(|question| serde_json::from_value::<QuestionIR>(question.clone())) {
            None => {
                findings.push(RefusalDiagnostic::error(
                    codes::QUESTION_INVALID,
                    "a question is required",
                    under,
                ));
                return Ok(());
            }
            Some(Err(error)) => {
                findings.push(RefusalDiagnostic::error(
                    codes::QUESTION_INVALID,
                    format!("the question is not a QuestionIR: {error}"),
                    under,
                ));
                return Ok(());
            }
            Some(Ok(question)) => question,
        };
    let Some(runtime) = questions else {
        findings.push(RefusalDiagnostic::error(
            codes::QUESTION_UNAVAILABLE,
            "this cell admits no ad-hoc questions",
            under,
        ));
        return Ok(());
    };
    if let Err(question_findings) = check_ad_hoc(host, scope, actor, &question, runtime)? {
        findings.extend(RefusalDiagnostic::from_checker(&question_findings, under));
    }
    Ok(())
}

fn view(value: Option<&Value>, at: &str, findings: &mut Vec<RefusalDiagnostic>) {
    let Some(view) = value else {
        findings.push(RefusalDiagnostic::error(
            codes::VIEW_INVALID,
            "a tile has a view",
            at,
        ));
        return;
    };
    let Some(object) = members(view, at, &["kind", "chart"], findings) else {
        return;
    };
    let kind = object.get("kind").and_then(Value::as_str);
    let chart = object.get("chart");
    let ok = match (kind, chart) {
        (Some("table" | "chart"), None) => true,
        (Some("chart"), Some(chart)) => {
            matches!(chart.as_str(), Some("line" | "bar" | "area"))
        }
        _ => false,
    };
    if !ok {
        findings.push(RefusalDiagnostic::error(
            codes::VIEW_INVALID,
            "a view is {kind: table} or {kind: chart, chart?: line | bar | area}",
            at,
        ));
    }
}

fn layout(value: Option<&Value>, at: &str, findings: &mut Vec<RefusalDiagnostic>) {
    let Some(layout) = value else {
        findings.push(RefusalDiagnostic::error(
            codes::LAYOUT_INVALID,
            "a tile has a layout",
            at,
        ));
        return;
    };
    let Some(object) = members(layout, at, &["x", "y", "w", "h"], findings) else {
        return;
    };
    for (key, least) in [("x", 0), ("y", 0), ("w", 1), ("h", 1)] {
        let ok = object
            .get(key)
            .and_then(Value::as_u64)
            .is_some_and(|n| (least..=MAX_LAYOUT).contains(&n));
        if !ok {
            findings.push(RefusalDiagnostic::error(
                codes::LAYOUT_INVALID,
                format!("{key} is an integer from {least} to {MAX_LAYOUT}"),
                format!("{at}/{key}"),
            ));
        }
    }
}

/// `POST /v1/definitions:admit`: run the kind's validator. Side-effect free.
/// # Errors
/// A denied caller (403), a malformed request or id (400), an unknown kind
/// (422 `definition.kind_unknown`), or a host failure.
#[utoipa::path(post, path = "/v1/definitions:admit", params(ScopeQuery), request_body = AdmitDefinitionRequest, responses((status = 200, description = "The admission decision.", body = AdmitDefinitionResponse), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 413, description = "The request was refused: over a resource bound.", body = Refusal), (status = 422, description = "The request was refused: the definition is not admissible.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn admit_definition<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    kinds: &DefinitionKinds,
    questions: Option<&dyn AdHocQuestions<E>>,
    request: &AdmitDefinitionRequest,
) -> Result<AdmitDefinitionResponse, Refusal> {
    authorize(host, scope, actor, Operation::WriteDefinitions)?;
    let actor = actor.ok_or_else(read_denied)?;
    check_new_id(host, &request.id)?;
    let (payload, diagnostics) = validate(
        host,
        scope,
        actor,
        kinds,
        questions,
        &request.id,
        &request.kind,
        &request.payload,
    )?;
    let admitted = admitted(&diagnostics);
    let (restatement, restatement_sha256) = if admitted {
        restate(kinds, questions, &request.kind, &payload.canonical)
    } else {
        (None, None)
    };
    Ok(AdmitDefinitionResponse {
        admitted,
        diagnostics,
        restatement,
        restatement_sha256,
    })
}

/// The registered restatement of `canonical` and its digest, against the
/// cell's admitted metric declarations; `(None, None)` when the kind has none.
fn restate<E: SqlEngine>(
    kinds: &DefinitionKinds,
    questions: Option<&dyn AdHocQuestions<E>>,
    kind: &str,
    canonical: &str,
) -> (Option<String>, Option<String>) {
    let metrics = questions.map_or(&[][..], |runtime| runtime.definitions());
    kinds
        .restater
        .and_then(|restater| restater(kind, canonical, metrics).ok())
        .map_or((None, None), |restatement| {
            let digest = payload_sha256(&restatement);
            (Some(restatement), Some(digest))
        })
}

/// The receipt of `record`'s commit as `actor` may see it (#1164, D53): the
/// raw hash only for a reader with whole-world Values, else its keyed handle.
fn receipt<E: SqlEngine>(
    host: &DevHost<E>,
    questions: Option<&dyn AdHocQuestions<E>>,
    actor: &str,
    record: &RetainedDefinitionRecord,
) -> Result<DefinitionReceipt, Refusal> {
    let cell = host.cell()?;
    let raw = cell
        .store()
        .read()
        .receipt_hash_at(record.commit_seq)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("retained definition commit has no receipt"))?;
    let receipt_hash = disclosed_receipt(host, questions, cell.store(), actor, &raw)?;
    Ok(DefinitionReceipt {
        receipt_hash,
        commit_seq: record.commit_seq,
        recorded_at_ms: record.recorded_at,
    })
}

/// The cell's active definitions version.
fn definitions_version<E: SqlEngine>(host: &DevHost<E>) -> Result<u64, Refusal> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    reader
        .installed_definitions(None)
        .map_err(Refusal::host)?
        .map(|active| active.version)
        .ok_or_else(|| Refusal::host("the cell has no installed definitions"))
}

/// Append `definition` (compare-and-set on `expected`, and on the active
/// definitions version when `pinned`) through the governed definitions
/// install, at host time `now_ms`.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one governed append's fixed inputs; the version check, kernel admission and the one governed commit run under one lock"
)]
fn append<E: SqlEngine>(
    host: &DevHost<E>,
    questions: Option<&dyn AdHocQuestions<E>>,
    actor: &str,
    mut definition: RetainedDefinition,
    expected: Option<u64>,
    pinned: Option<u64>,
    signed: Option<(&str, String)>,
    flow: Option<&crate::flow_definitions::Flow>,
    now_ms: u64,
) -> Result<InstallDefinitionResponse, Refusal> {
    let _epoch = host.definition_write()?;
    let record = {
        let mut cell = host.cell()?;
        let (active, current, bindings) = {
            let reader = cell.store().read();
            let active = reader
                .installed_definitions(None)
                .map_err(Refusal::host)?
                .ok_or_else(|| Refusal::host("the cell has no installed definitions"))?;
            let current = reader
                .retained_definition(&definition.id, None)
                .map_err(Refusal::host)?;
            let bindings = match flow {
                Some(_) => binding_versions(&reader)?,
                None => std::collections::BTreeMap::new(),
            };
            (active, current, bindings)
        };
        if current.as_ref().map(|current| current.definition.revision) != expected
            || pinned.is_some_and(|pinned| pinned != active.version)
        {
            return Err(version_conflict());
        }
        if let Some(current) = &current
            && current.definition.kind != definition.kind
        {
            return Err(refused(vec![RefusalDiagnostic {
                code: codes::ID_IN_USE.to_owned(),
                severity: "error".to_owned(),
                message: "the id is used by a definition of another kind".to_owned(),
                law: None,
                span: None,
            }]));
        }
        let installed_at = now_ms.max(cell.now().as_millis());
        definition.signoff = signed.map(|(signer, restatement_sha256)| RetainedSignoff {
            signer: signer.to_owned(),
            signed_at: installed_at,
            restatement_sha256,
        });
        // A flow is installed into the kernel set, admitted against the set
        // active now, under the definitions write lock; anything else
        // carries the active set, receipt and model unchanged.
        let (model, canonical_set, receipt_sha256, admission_receipt) = match flow {
            Some(flow) => {
                // Re-resolved under the lock: a binding retired or revised
                // since validation refuses instead of committing a stale pin.
                crate::flow_definitions::recheck(flow, &bindings)?;
                // Only this flow's own previous revision may be replaced.
                let previously_owned: Vec<String> = match &current {
                    Some(_) => {
                        let (entity, rule) = crate::flow_definitions::owned_ids(&definition.id);
                        vec![entity, rule]
                    }
                    None => Vec::new(),
                };
                let admitted = crate::flow_definitions::rebuild(
                    &active.canonical_set,
                    active.admission_receipt.as_deref(),
                    flow,
                    &previously_owned,
                )?;
                (
                    admitted.model,
                    admitted.canonical_set,
                    admitted.receipt_sha256,
                    Some(admitted.receipt),
                )
            }
            None => (
                cell.store().model().clone(),
                active.canonical_set,
                active.receipt_sha256,
                active.admission_receipt,
            ),
        };
        let input = InstalledDefinitionInput {
            canonical_set,
            registry: active.registry,
            receipt_sha256,
            admission_receipt,
            readmission: None,
            diff: serde_json::json!({
                "added": [], "removed": [], "changed": [], "narrowing": [],
                "retained": {
                    "kind": definition.kind,
                    "id": definition.id,
                    "revision": definition.revision,
                    "lifecycle": definition.lifecycle.as_str(),
                    "payloadSha256": definition.payload_sha256,
                    "signoff": definition.signoff.as_ref().map(|signoff| serde_json::json!({
                        "signer": signoff.signer,
                        "restatementSha256": signoff.restatement_sha256,
                    })),
                },
            }),
            installed_at,
            layout: None,
        };
        let change = RetainedDefinitionChange {
            definition,
            expected_revision: expected,
        };
        let installed =
            match cell.install_retained_definition(model, &input, Some(active.version), &change) {
                Ok(installed) => installed,
                Err(
                    CellError::RetainedDefinitionConflict { .. }
                    | CellError::DefinitionVersionConflict { .. },
                ) => return Err(version_conflict()),
                Err(error) => return Err(Refusal::host(error)),
            };
        RetainedDefinitionRecord {
            definition: change.definition,
            definitions_version: installed.version,
            commit_seq: installed.commit_seq,
            recorded_at: installed.installed_at,
        }
    };
    Ok(InstallDefinitionResponse {
        installed: true,
        id: record.definition.id.clone(),
        version: record.definition.revision,
        receipt: receipt(host, questions, actor, &record)?,
        flow: flow_summary(&record.definition),
    })
}

/// `POST /v1/definitions:install`: re-admit, then install the next version
/// through the governed definitions install.
/// # Errors
/// As [`admit_definition`]; an inadmissible payload (422 `definition.refused`
/// with diagnostics); a stale `expectedVersion` (409
/// `definition.version_conflict`).
#[utoipa::path(post, path = "/v1/definitions:install", params(ScopeQuery), request_body = InstallDefinitionRequest, responses((status = 200, description = "The installed version.", body = InstallDefinitionResponse), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 409, description = "The request was refused: the definition changed since it was read.", body = Refusal), (status = 413, description = "The request was refused: over a resource bound.", body = Refusal), (status = 422, description = "The request was refused: the definition is not admissible.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn install_definition<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    kinds: &DefinitionKinds,
    questions: Option<&dyn AdHocQuestions<E>>,
    request: &InstallDefinitionRequest,
    now_ms: u64,
) -> Result<InstallDefinitionResponse, Refusal> {
    authorize(host, scope, actor, Operation::WriteDefinitions)?;
    let actor = actor.ok_or_else(read_denied)?;
    check_new_id(host, &request.id)?;
    // An agent is admitted against the bindings of this definitions version;
    // it commits only if no definition was written since.
    let pinned = if request.kind == "agent" {
        Some(definitions_version(host)?)
    } else {
        None
    };
    let (payload, diagnostics) = validate(
        host,
        scope,
        actor,
        kinds,
        questions,
        &request.id,
        &request.kind,
        &request.payload,
    )?;
    if !admitted(&diagnostics) {
        return Err(refused(diagnostics));
    }
    // Fable S7: nothing checks a sign-off when a run starts, so a flow is
    // installed only once a signer signs its words. A cell that cannot
    // restate a flow cannot install one (fail closed).
    if request.kind == "flow" && request.signoff.is_none() {
        return Err(if kinds.restater.is_some() {
            Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                codes::SIGNOFF_REQUIRED,
                "a flow is installed by signing the server's words for it",
                "admit the flow, read its restatement, and install it with signoff.restatementSha256",
            )
        } else {
            Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                codes::SIGNOFF_UNAVAILABLE,
                "the sign-off cannot be verified",
                "this cell has no restatement generator, so it cannot install a flow",
            )
        });
    }
    let signed = request
        .signoff
        .as_ref()
        .map(|claimed| {
            // The same declarations the payload's questions were admitted against.
            let metrics = questions.map_or(&[][..], |runtime| runtime.definitions());
            verify_signoff(kinds, &request.kind, &payload.canonical, metrics, claimed)
        })
        .transpose()?
        .map(|restatement_sha256| (actor, restatement_sha256));
    let revision = request
        .expected_version
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(version_conflict)?;
    append(
        host,
        questions,
        actor,
        RetainedDefinition {
            kind: request.kind.clone(),
            id: request.id.clone(),
            revision,
            lifecycle: RetainedLifecycle::Active,
            title: payload.title,
            payload_sha256: payload_sha256(&payload.canonical),
            payload: payload.canonical,
            signoff: None,
        },
        request.expected_version,
        pinned,
        signed,
        payload.flow.as_ref(),
        now_ms,
    )
}

/// Recompute the restatement of an admitted payload and check the signer's
/// claimed digest against it; returns the recomputed digest.
fn verify_signoff(
    kinds: &DefinitionKinds,
    kind: &str,
    canonical: &str,
    metrics: &[MetricDefinition],
    claimed: &SignoffRequest,
) -> Result<String, Refusal> {
    let unavailable = |detail: String| {
        Refusal::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            codes::SIGNOFF_UNAVAILABLE,
            "the sign-off cannot be verified",
            detail,
        )
    };
    let restate = kinds
        .restater
        .ok_or_else(|| unavailable("this cell has no restatement generator".to_owned()))?;
    let restatement = restate(kind, canonical, metrics).map_err(unavailable)?;
    let recomputed = payload_sha256(&restatement);
    if recomputed != claimed.restatement_sha256 {
        // The caller's own admitted payload, restated: it discloses nothing
        // the caller did not submit, and lets them sign these exact words.
        let mut refusal = Refusal::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            codes::SIGNOFF_MISMATCH,
            "the sign-off does not match the definition",
            "the restatement the signer read is not the one this definition restates to; read the returned restatement and sign it",
        );
        refusal.restatement = Some(Box::new(RefusalRestatement {
            restatement,
            restatement_sha256: recomputed,
        }));
        return Err(refusal);
    }
    Ok(recomputed)
}

/// `POST /v1/definitions/{id}:retire`: record a retired version; history
/// stays readable.
/// # Errors
/// A denied caller (403), an unknown id (404 `definition.not_found`), a stale
/// `expectedVersion` (409), an already retired definition (422).
#[utoipa::path(post, path = "/v1/definitions/{id}:retire", params(("id" = String, Path, description = "The definition id."), ScopeQuery), request_body = RetireDefinitionRequest, responses((status = 200, description = "The retired version.", body = InstallDefinitionResponse), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 409, description = "The request was refused: the definition changed since it was read.", body = Refusal), (status = 422, description = "The request was refused: the definition is not admissible.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn retire_definition<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    questions: Option<&dyn AdHocQuestions<E>>,
    id: &str,
    request: &RetireDefinitionRequest,
    now_ms: u64,
) -> Result<InstallDefinitionResponse, Refusal> {
    authorize(host, scope, actor, Operation::WriteDefinitions)?;
    let actor = actor.ok_or_else(read_denied)?;
    check_id(id)?;
    let current = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        reader
            .retained_definition(id, None)
            .map_err(Refusal::host)?
            .ok_or_else(not_found)?
    };
    if current.definition.revision != request.expected_version {
        return Err(version_conflict());
    }
    if current.definition.kind == "flow" {
        // A retired flow keeps its artifacts; retiring is refused while a
        // run may be live, and without the run ledger's count none can be
        // ruled out.
        return Err(Refusal::new(
            StatusCode::CONFLICT,
            crate::flow_definitions::codes::RUNS_LIVE,
            "the flow may have live runs",
            "live runs cannot be ruled out until the run ledger reports them; retire when it does",
        ));
    }
    if current.definition.lifecycle == RetainedLifecycle::Retired {
        return Err(refused(vec![RefusalDiagnostic {
            code: codes::ALREADY_RETIRED.to_owned(),
            severity: "error".to_owned(),
            message: "the definition is already retired".to_owned(),
            law: None,
            span: None,
        }]));
    }
    let mut definition = current.definition;
    definition.revision = definition
        .revision
        .checked_add(1)
        .ok_or_else(version_conflict)?;
    definition.lifecycle = RetainedLifecycle::Retired;
    definition.signoff = None;
    append(
        host,
        questions,
        actor,
        definition,
        Some(request.expected_version),
        None,
        None,
        None,
        now_ms,
    )
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    k: String,
    r: bool,
    a: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    u: Option<String>,
}

fn cursor_invalid() -> Refusal {
    Refusal::new(
        StatusCode::BAD_REQUEST,
        codes::CURSOR_INVALID,
        "the cursor is not valid for this listing",
        "pass back a cursor exactly as a listing of the same kind and include returned it",
    )
}

/// `GET /v1/definitions:list`: the current version of each definition of a
/// kind, ordered by id, retired ones only with `include=retired`.
/// # Errors
/// A denied caller (403), a missing kind, a bad limit or cursor (400).
#[utoipa::path(get, path = "/v1/definitions:list", params(ListDefinitionsQuery), responses((status = 200, description = "One page of definitions.", body = DefinitionPage), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
#[allow(
    clippy::too_many_lines,
    reason = "the query's checks, the filtered page walk and the bound cursor stay together"
)]
pub fn list_definitions<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ListDefinitionsQuery,
    actor: Option<&str>,
) -> Result<DefinitionPage, Refusal> {
    let scope = ScopeQuery {
        org_id: query.org_id.clone(),
        environment_id: query.environment_id.clone(),
    };
    authorize(host, &scope, actor, Operation::ReadDefinitions)?;
    let kind = query
        .kind
        .as_deref()
        .ok_or_else(|| Refusal::invalid("kind is required"))?;
    let include_retired = match query.include.as_deref() {
        None => false,
        Some("retired") => true,
        Some(other) => {
            return Err(Refusal::invalid(format!(
                "include: unknown value {other:?}"
            )));
        }
    };
    let limit = query.limit.unwrap_or(50);
    if !(1..=MAX_LIST_LIMIT).contains(&limit) {
        return Err(Refusal::invalid(format!("limit is 1 to {MAX_LIST_LIMIT}")));
    }
    let after = query
        .cursor
        .as_deref()
        .map(|cursor| {
            let bytes = URL_SAFE_NO_PAD
                .decode(cursor)
                .map_err(|_malformed| cursor_invalid())?;
            let cursor: Cursor =
                serde_json::from_slice(&bytes).map_err(|_malformed| cursor_invalid())?;
            if cursor.k != kind || cursor.r != include_retired || cursor.u != query.uses {
                return Err(cursor_invalid());
            }
            Ok(cursor.a)
        })
        .transpose()?;
    if query.uses.is_some() && kind != "flow" {
        return Err(Refusal::invalid("uses filters kind=flow"));
    }
    let _epoch = host.definition_read()?;
    let wanted = usize::try_from(limit.saturating_add(1)).unwrap_or(usize::MAX);
    let mut rows: Vec<(RetainedDefinitionRecord, Option<String>)> = Vec::new();
    {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let mut after = after;
        // With `uses`, pages are read until enough flows match: the filter is
        // the same perform extraction the install check runs.
        loop {
            let page = reader
                .retained_definitions(kind, include_retired, after.as_deref(), MAX_LIST_LIMIT)
                .map_err(Refusal::host)?;
            let exhausted = page.len() < usize::try_from(MAX_LIST_LIMIT).unwrap_or(usize::MAX);
            after = page.last().map(|record| record.definition.id.clone());
            for record in page {
                let pinned = match &query.uses {
                    None => None,
                    Some(binding) => {
                        match crate::flow_definitions::pinned_version(
                            &record.definition.payload,
                            binding,
                        ) {
                            Some(version) => Some(version),
                            None => continue,
                        }
                    }
                };
                rows.push((record, pinned));
            }
            if exhausted || rows.len() >= wanted {
                break;
            }
        }
    }
    let more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let cursor = if more {
        rows.last()
            .map(|(last, _)| {
                norn_ir::canonical::canonical_json_of(&Cursor {
                    k: kind.to_owned(),
                    r: include_retired,
                    a: last.definition.id.clone(),
                    u: query.uses.clone(),
                })
                .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
                .map_err(Refusal::host)
            })
            .transpose()?
    } else {
        None
    };
    Ok(DefinitionPage {
        items: rows
            .into_iter()
            .map(|(row, pinned_version)| DefinitionSummary {
                id: row.definition.id,
                kind: row.definition.kind,
                title: row.definition.title,
                version: row.definition.revision,
                lifecycle: row.definition.lifecycle.as_str().to_owned(),
                updated_at_ms: row.recorded_at,
                pinned_version,
            })
            .collect(),
        cursor,
    })
}

/// `GET /v1/definitions/{id}`: one version of one definition, the current
/// one unless `version` names a past one.
/// # Errors
/// A denied caller (403), an invalid id (400), an unknown id or version (404
/// `definition.not_found`).
#[utoipa::path(get, path = "/v1/definitions/{id}", params(("id" = String, Path, description = "The definition id."), GetDefinitionQuery), responses((status = 200, description = "The definition version.", body = DefinitionDocument), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn get_definition<E: SqlEngine>(
    host: &DevHost<E>,
    query: &GetDefinitionQuery,
    actor: Option<&str>,
    kinds: &DefinitionKinds,
    questions: Option<&dyn AdHocQuestions<E>>,
    id: &str,
) -> Result<DefinitionDocument, Refusal> {
    let scope = ScopeQuery {
        org_id: query.org_id.clone(),
        environment_id: query.environment_id.clone(),
    };
    authorize(host, &scope, actor, Operation::ReadDefinitions)?;
    let actor = actor.ok_or_else(read_denied)?;
    check_id(id)?;
    let _epoch = host.definition_read()?;
    let record = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        reader
            .retained_definition(id, query.version)
            .map_err(Refusal::host)?
            .ok_or_else(not_found)?
    };
    let receipt = receipt(host, questions, actor, &record)?;
    let (restatement, restatement_sha256) = restate(
        kinds,
        questions,
        &record.definition.kind,
        &record.definition.payload,
    );
    Ok(DefinitionDocument {
        flow: flow_summary(&record.definition),
        restatement,
        restatement_sha256,
        id: record.definition.id,
        kind: record.definition.kind,
        version: record.definition.revision,
        lifecycle: record.definition.lifecycle.as_str().to_owned(),
        signoff: record.definition.signoff.map(|signoff| SignoffDocument {
            signer: signoff.signer,
            signed_at_ms: signoff.signed_at,
            restatement_sha256: signoff.restatement_sha256,
        }),
        payload: RawValue::from_string(record.definition.payload).map_err(Refusal::host)?,
        receipt,
    })
}
