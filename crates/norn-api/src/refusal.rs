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
    /// On `definition.signoff_mismatch` only: the host's restatement of the
    /// caller's own admitted payload, flattened into the refusal as
    /// `restatement` and `restatementSha256`. Absent otherwise.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    #[schema(inline)]
    pub restatement: Option<Box<RefusalRestatement>>,
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
    /// The plain-words restatement the host recomputed.
    pub restatement: String,
    /// Lower-case hex SHA-256 of `restatement`'s UTF-8 bytes.
    pub restatement_sha256: String,
}

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
            self.restatement = None;
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
            restatement: None,
        }
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

    /// A host failure (the request changed nothing).
    #[must_use]
    pub fn host(_detail: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::HOST_ERROR,
            "the cell host failed; nothing was committed",
            Self::UNPROVEN_DETAIL,
        )
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
