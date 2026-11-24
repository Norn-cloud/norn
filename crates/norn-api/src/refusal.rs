//! Typed refusals: every route answers an error as `{code, category,
//! message, explanation}` with a stable machine code and the kernel
//! taxonomy's category, the discriminant generated SDKs switch on.

use http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;

/// Stable refusal codes of the dev host's own checks. Kernel and checker
/// refusals keep their own codes (`lifecycle.validation.*`, `decision.*`,
/// `commit.*`).
pub mod codes {
    /// The request names no scope.
    pub const SCOPE_MISSING: &str = "scope.missing";
    /// The request's scope is not this cell's scope.
    pub const SCOPE_DENIED: &str = "scope.denied";
    /// The authorizer refused the request.
    pub const AUTHZ_DENIED: &str = "authz.denied";
    /// The object does not exist.
    pub const OBJECT_NOT_FOUND: &str = "object.not_found";
    /// The object type is not in the admitted definitions.
    pub const UNKNOWN_TYPE: &str = "object.unknown_type";
    /// The request body or query is malformed.
    pub const INVALID_REQUEST: &str = "request.invalid";
    /// The cell host failed.
    pub const HOST_ERROR: &str = "host.error";
    /// OPEN-CHECKPOINT-1 (#1551): the cell is verifying its history after an
    /// open from its checkpoint; the read or write waits for it (retryable).
    pub const CELL_VERIFYING: &str = "cell.verifying";
    /// OPEN-CHECKPOINT-1 (#1551): the cell's history did not verify; it is
    /// fenced and serves nothing.
    pub const CELL_FENCED: &str = "cell.fenced";
}

/// What the caller can do about a refusal: the kernel refusal taxonomy's
/// closed category set (`norn_kernel::RefusalCategory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCategory {
    /// The target does not exist for this actor (absent or invisible alike).
    Visibility,
    /// The actor may see the target but may not do this.
    Permission,
    /// Well-formed, but the current state does not allow it.
    Precondition,
    /// State changed concurrently, or a retry does not match the original.
    Conflict,
    /// The request is malformed or names something undeclared.
    Invalid,
    /// A resource limit was exceeded.
    Limit,
    /// The host failed; nothing was committed.
    Internal,
}

impl RefusalCategory {
    /// Its wire name (`snake_case`), as served.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Visibility => "visibility",
            Self::Permission => "permission",
            Self::Precondition => "precondition",
            Self::Conflict => "conflict",
            Self::Invalid => "invalid",
            Self::Limit => "limit",
            Self::Internal => "internal",
        }
    }

    /// The category named `name` ([`Self::wire_name`]).
    #[must_use]
    pub fn from_wire_name(name: &str) -> Option<Self> {
        [
            Self::Visibility,
            Self::Permission,
            Self::Precondition,
            Self::Conflict,
            Self::Invalid,
            Self::Limit,
            Self::Internal,
        ]
        .into_iter()
        .find(|category| category.wire_name() == name)
    }

    /// The category of a host check refused with `status`. The host's own
    /// `422`s are malformed content, so they are `invalid`; a precondition
    /// comes only from a kernel refusal, which carries its own category
    /// through [`Refusal::with_category`].
    #[must_use]
    pub fn of_status(status: StatusCode) -> Self {
        match status.as_u16() {
            401 | 403 => Self::Permission,
            404 => Self::Visibility,
            409 => Self::Conflict,
            413 | 429 => Self::Limit,
            400..=499 => Self::Invalid,
            _ => Self::Internal,
        }
    }
}

/// Version of the public refusal projection, independent of stored receipt formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
pub enum RefusalFormat {
    /// Pinned typed reasons and explicit engine retry semantics.
    #[serde(rename = "norn/refusal@2")]
    V2,
}

/// A refused request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    /// Versioned projection; stored checker receipts keep their original bytes.
    pub format: RefusalFormat,
    /// Engine-defined retryability, false unless explicitly supplied by the engine.
    pub retryable: bool,
    /// Shared declaration pin and typed operands, disclosed together.
    #[schema(required = true, schema_with = refusal_reason_schema)]
    pub reason: Option<Box<norn_ir::RefusalReason>>,
    /// The HTTP status the refusal is served with.
    #[serde(skip)]
    pub status: u16,
    /// Stable machine code (`scope.denied`, `lifecycle.validation.undeclared_write`, …).
    pub code: String,
    /// What the caller can do about it (the discriminant SDKs switch on).
    pub category: RefusalCategory,
    /// One-line summary.
    pub message: String,
    /// Why, and what would make the request admissible.
    pub explanation: String,
    /// Checker findings for an inadmissible submission (a question or a
    /// definition), each pointing into the submitted document. Absent when
    /// empty. Only ever produced after the caller is authorized for
    /// everything the submission reads, so no finding describes an
    /// ungranted resource.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<RefusalDiagnostic>,
    /// What only some refusals carry, flattened into the refusal: on
    /// `definition.signoff_mismatch`, the host's restatement of the caller's
    /// own admitted payload (`restatement`, `restatementSha256`); on an
    /// internal refusal, its `incidentId` (INCIDENT-1). Absent otherwise.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    #[schema(inline)]
    pub extra: Option<Box<RefusalExtra>>,
}

/// One checker finding in a refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefusalDiagnostic {
    /// Stable registered checker code: the rule that refused.
    pub code: String,
    /// `error`, `warning` or `info`; any error refuses.
    pub severity: String,
    /// Human-readable message; the code is the contract.
    pub message: String,
    /// The law that refused, when a declared law did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub law: Option<String>,
    /// Where in the submitted document, when the check can say.
    #[schema(required = true, nullable = true)]
    pub span: Option<RefusalSpan>,
}

impl RefusalDiagnostic {
    /// Checker findings, each pointer prefixed by `under` (the checked
    /// document's place in the submission).
    #[must_use]
    pub fn from_checker(findings: &[norn_checker::Diagnostic], under: &str) -> Vec<Self> {
        findings
            .iter()
            .map(|finding| Self {
                code: finding.code.as_str().to_owned(),
                severity: match finding.severity {
                    norn_ir::DiagnosticSeverity::Error => "error",
                    norn_ir::DiagnosticSeverity::Warning => "warning",
                    norn_ir::DiagnosticSeverity::Info => "info",
                }
                .to_owned(),
                message: finding.message.clone(),
                law: None,
                span: finding.span.as_ref().map(|span| RefusalSpan {
                    pointer: format!("{under}{}", span.pointer),
                }),
            })
            .collect()
    }

    /// One error-severity finding at `pointer`.
    #[must_use]
    pub fn error(code: &str, message: impl Into<String>, pointer: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            severity: "error".to_owned(),
            message: message.into(),
            law: None,
            span: Some(RefusalSpan {
                pointer: pointer.into(),
            }),
        }
    }
}

/// The words a signer can sign: what the host reads the submission as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefusalRestatement {
    /// The plain-words restatement the host recomputed. Present, with its
    /// digest, only on `definition.signoff_mismatch`.
    #[schema(required = false)]
    pub restatement: String,
    /// Lower-case hex SHA-256 of `restatement`'s UTF-8 bytes.
    #[schema(required = false)]
    pub restatement_sha256: String,
}

/// The rarely present parts of a refusal, boxed so every refusal stays small.
/// Its fields sit flat in the refusal, exactly where the restatement always
/// did, so the wire shape only gains `incidentId` (D84).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefusalExtra {
    /// The plain-words restatement the host recomputed. Present, with its
    /// digest, only on `definition.signoff_mismatch`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, required = false)]
    pub restatement: Option<String>,
    /// Lower-case hex SHA-256 of `restatement`'s UTF-8 bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, required = false)]
    pub restatement_sha256: Option<String>,
    /// INCIDENT-1: the id under which the host logged this internal
    /// refusal's cause. The caller quotes it; the cause is never served.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, required = false)]
    pub incident_id: Option<String>,
    /// The host clock instant (RFC 3339 UTC) at which the cell decided this
    /// refusal, when the boundary supplied a clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, required = false)]
    pub decided_at: Option<String>,
    /// INCIDENT-1: the error chain, for the operator's log only: never
    /// serialized, never compared.
    #[serde(skip)]
    #[schema(ignore)]
    pub cause: Option<String>,
}

impl RefusalExtra {
    /// Carry the host's restatement (`definition.signoff_mismatch`).
    pub fn set_restatement(&mut self, restatement: RefusalRestatement) {
        self.restatement = Some(restatement.restatement);
        self.restatement_sha256 = Some(restatement.restatement_sha256);
    }
}

/// Two refusals that differ only in their cause are the same refusal.
impl PartialEq for RefusalExtra {
    fn eq(&self, other: &Self) -> bool {
        self.restatement == other.restatement
            && self.restatement_sha256 == other.restatement_sha256
            && self.incident_id == other.incident_id
            && self.decided_at == other.decided_at
    }
}

impl Eq for RefusalExtra {}

/// A location in the submitted document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefusalSpan {
    /// RFC 6901 JSON pointer into the submitted document.
    pub pointer: String,
}

impl Refusal {
    /// Opaque diagnostics have no trusted stored-value provenance. Neither an
    /// ordinary read grant nor a broad marking grant proves them revealable.
    pub const UNPROVEN_DETAIL: &str =
        "diagnostic detail is unavailable under the caller's disclosure authority";

    /// Project the entire diagnostic, never fragments matched against values.
    /// Codes, categories and status remain usable even when detail is hidden.
    #[must_use]
    pub fn project_detail(mut self, revealable: bool) -> Self {
        if !revealable
            || matches!(
                self.category,
                RefusalCategory::Visibility | RefusalCategory::Internal
            )
        {
            Self::UNPROVEN_DETAIL.clone_into(&mut self.explanation);
            self.reason = None;
            self.diagnostics.clear();
            if let Some(extra) = self.extra.as_mut() {
                extra.restatement = None;
                extra.restatement_sha256 = None;
            }
        }
        self
    }

    /// A refusal served with `status`.
    #[must_use]
    pub fn new(
        status: StatusCode,
        code: &str,
        message: impl Into<String>,
        explanation: impl Into<String>,
    ) -> Self {
        Self {
            format: RefusalFormat::V2,
            retryable: false,
            reason: None,
            status: status.as_u16(),
            code: code.to_owned(),
            category: RefusalCategory::of_status(status),
            message: message.into(),
            explanation: explanation.into(),
            diagnostics: Vec::new(),
            extra: None,
        }
    }

    /// The same refusal remembering `cause` for the operator's log (an
    /// internal refusal's error chain). Never served to the caller.
    #[must_use]
    pub fn with_cause(mut self, cause: impl std::fmt::Display) -> Self {
        self.extra.get_or_insert_with(Box::default).cause = Some(cause.to_string());
        self
    }

    /// Record the RFC 3339 UTC host clock instant at which this refusal was
    /// decided at the server boundary.
    pub fn set_decided_at(&mut self, decided_at: impl Into<String>) {
        self.extra.get_or_insert_with(Box::default).decided_at = Some(decided_at.into());
    }

    /// The same refusal carrying the host clock instant at which it was
    /// decided.
    #[must_use]
    pub fn with_decided_at(mut self, decided_at: impl Into<String>) -> Self {
        self.set_decided_at(decided_at);
        self
    }

    /// Whether this refusal is an internal failure whose cause the host logs
    /// under an incident id (INCIDENT-1).
    #[must_use]
    pub fn is_internal(&self) -> bool {
        matches!(self.category, RefusalCategory::Internal)
    }

    /// The same refusal carrying checker findings, each pointer prefixed by
    /// `under` (the submitted document's place in the request body).
    #[must_use]
    pub fn with_diagnostics(mut self, findings: &[norn_checker::Diagnostic], under: &str) -> Self {
        self.diagnostics = RefusalDiagnostic::from_checker(findings, under);
        self
    }

    /// The same refusal with the category its source declared (a kernel
    /// refusal's `category()`).
    #[must_use]
    pub fn with_category(mut self, category: RefusalCategory) -> Self {
        self.category = category;
        self
    }

    /// Project a kernel refusal under the caller's proven disclosure authority.
    /// DIAG-REDACT-1: codes remain usable; pins and operands are one disclosure unit.
    #[must_use]
    pub fn from_kernel(source: norn_kernel::Refusal, revealable: bool) -> Self {
        use norn_kernel::RefusalCategory as Kernel;
        let (status, category, message) = match source.category {
            Kernel::Visibility => (
                StatusCode::NOT_FOUND,
                RefusalCategory::Visibility,
                "not found",
            ),
            Kernel::Permission => (
                StatusCode::FORBIDDEN,
                RefusalCategory::Permission,
                "the action is not permitted",
            ),
            Kernel::Precondition => (
                StatusCode::UNPROCESSABLE_ENTITY,
                RefusalCategory::Precondition,
                "a declared precondition does not hold",
            ),
            Kernel::Conflict => (
                StatusCode::CONFLICT,
                RefusalCategory::Conflict,
                "the request conflicts with current state",
            ),
            Kernel::Invalid => (
                StatusCode::BAD_REQUEST,
                RefusalCategory::Invalid,
                "the request is invalid",
            ),
            Kernel::Limit => (
                StatusCode::TOO_MANY_REQUESTS,
                RefusalCategory::Limit,
                "a declared limit was exceeded",
            ),
            Kernel::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                RefusalCategory::Internal,
                "the cell could not complete the request",
            ),
        };
        let mut projected =
            Self::new(status, &source.code, message, source.message).with_category(category);
        projected.reason = source.reason.map(Box::new);
        projected.retryable = source.retryable;
        projected.project_detail(revealable)
    }

    /// A malformed request.
    #[must_use]
    pub fn invalid(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self::new(
            StatusCode::BAD_REQUEST,
            codes::INVALID_REQUEST,
            "the request is malformed",
            detail,
        )
    }

    /// Preserve stable compute refusals without exposing storage diagnostics.
    #[must_use]
    pub fn from_compute(error: norn_cell::CellError) -> Self {
        let code = if let norn_cell::CellError::ComputeRefused(code) = error {
            code
        } else if matches!(error, norn_cell::CellError::TemporalLeakage) {
            "core.compute.stale_input"
        } else {
            return Self::host(error);
        };
        if code == "core.projection.answer_moved" {
            let mut refusal = Self::new(
                StatusCode::CONFLICT,
                code,
                "the data changed since this answer",
                "ask the question again for a fresh answer and its trace",
            );
            refusal.retryable = true;
            return refusal;
        }
        if code == "core.compute.bound_exceeded" {
            return Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                code,
                "the question exceeds its admitted bounds",
                "no partial answer is returned",
            );
        }
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            code,
            "the question cannot be evaluated at the requested frontier",
            "the admitted computation or exact-result precondition is not satisfied",
        )
        .with_category(RefusalCategory::Precondition)
    }

    /// Map a [`norn_cell::CellError`] directly to a [`Refusal`], preserving
    /// typed `cell.verifying` (retryable 503) and `cell.fenced` (fail-closed
    /// 503) even when wrapped inside [`norn_cell::CellError::ObservationCommit`].
    #[must_use]
    pub fn from_cell_error(error: &norn_cell::CellError) -> Self {
        if let Some(refusal) = Self::cell_availability_refusal(error) {
            return refusal;
        }
        Self::host(error)
    }

    fn cell_availability_refusal(error: &norn_cell::CellError) -> Option<Self> {
        if matches!(error, norn_cell::CellError::Verifying(_)) {
            return Some(Self::verifying());
        }
        if matches!(error, norn_cell::CellError::Fenced(_)) {
            return Some(Self::fenced(error));
        }
        if let norn_cell::CellError::ObservationCommit(commit) = error
            && let norn_kernel::CommitError::Host(inner) = commit.as_ref()
        {
            return Self::cell_availability_refusal(inner);
        }
        None
    }

    /// Map a kernel [`norn_kernel::CommitError<norn_cell::CellError>`] on a
    /// host-internal commit path to a [`Refusal`], preserving typed
    /// `cell.verifying` and `cell.fenced` instead of returning non-retryable
    /// `500 host.error` (OPEN-CHECKPOINT-1, #1551 round 5).
    #[must_use]
    pub fn from_commit_host_error(error: &norn_kernel::CommitError<norn_cell::CellError>) -> Self {
        if let norn_kernel::CommitError::Host(host_error) = error {
            return Self::from_cell_error(host_error);
        }
        Self::host(format!("{error:?}"))
    }

    /// If an [`norn_kernel::ActionRefusal<norn_cell::CellError>`] wraps a
    /// `cell.verifying` or `cell.fenced` host error, return its typed refusal
    /// directly so kernel commit paths never downgrade it to `500 commit.host`.
    #[must_use]
    pub fn from_action_host_error(
        error: &norn_kernel::ActionRefusal<norn_cell::CellError>,
    ) -> Option<Self> {
        let norn_kernel::ActionRefusal::Decision(norn_kernel::DecisionRefusal::Host(host_error)) =
            error
        else {
            return None;
        };
        Self::cell_availability_refusal(host_error)
    }

    /// Whether this refusal is never recorded as a permanent approval refusal
    /// (internal/visibility/permission or verifying/fenced cell).
    #[must_use]
    pub fn is_unrecorded_approval_refusal(&self) -> bool {
        matches!(
            self.category,
            RefusalCategory::Internal | RefusalCategory::Permission | RefusalCategory::Visibility
        ) || matches!(
            self.code.as_str(),
            codes::CELL_VERIFYING | codes::CELL_FENCED
        )
    }

    /// A host failure (the request changed nothing). A cell still
    /// verifying its history, or fenced by it, answers its own refusal.
    #[must_use]
    pub fn host(detail: impl std::fmt::Display) -> Self {
        let cause = detail.to_string();
        if cause.starts_with(codes::CELL_VERIFYING) {
            return Self::verifying();
        }
        if cause.starts_with(codes::CELL_FENCED) {
            return Self::fenced(cause);
        }
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::HOST_ERROR,
            "the cell host failed; nothing was committed",
            Self::UNPROVEN_DETAIL,
        )
        .with_cause(cause)
    }

    /// OPEN-CHECKPOINT-1 (#1551): the cell opened from its checkpoint and
    /// has not finished verifying the history this request depends on.
    /// Nothing changed; retry.
    #[must_use]
    pub fn verifying() -> Self {
        let mut refusal = Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::CELL_VERIFYING,
            "the cell is verifying its history",
            "this request depends on history the cell is still verifying; nothing was committed; retry",
        )
        .with_category(RefusalCategory::Precondition);
        refusal.retryable = true;
        refusal
    }

    /// OPEN-CHECKPOINT-1 (#1551): the cell's history did not verify, so it
    /// serves nothing until an operator restores it.
    #[must_use]
    pub fn fenced(finding: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::CELL_FENCED,
            "the cell's history did not verify; it serves nothing",
            Self::UNPROVEN_DETAIL,
        )
        .with_cause(finding)
    }

    /// Refuse if the cell's history scrub has fenced it.
    ///
    /// # Errors
    /// [`Self::fenced`] when `status` is [`norn_cell::ScrubStatus::Fenced`].
    pub fn check_scrub(status: norn_cell::ScrubStatus) -> Result<(), Self> {
        if let norn_cell::ScrubStatus::Fenced { finding } = status {
            return Err(Self::fenced(finding));
        }
        Ok(())
    }
}

fn refusal_reason_schema() -> utoipa::openapi::schema::Schema {
    utoipa::openapi::schema::Schema::AnyOf(
        utoipa::openapi::schema::AnyOfBuilder::new()
            .item(utoipa::openapi::Ref::from_schema_name(
                "NornIrRefusalReason",
            ))
            .item(
                utoipa::openapi::schema::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::Type::Null),
            )
            .build(),
    )
}

/// An error and every source under it, outermost first, as one line for the
/// operator's log (INCIDENT-1).
#[must_use]
pub fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut line = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        line.push_str(": ");
        line.push_str(&cause.to_string());
        source = cause.source();
    }
    line
}
