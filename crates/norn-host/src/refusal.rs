//! Typed refusals: every route answers an error as `{code, category,
//! message, explanation}` with a stable machine code and the kernel
//! taxonomy's category (#2949), the discriminant generated SDKs switch on
//! (#2455).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use norn_cell::CellError;
use norn_kernel::CommitError;
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
/// closed category set (`norn_kernel::RefusalCategory`, #2949).
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

impl From<norn_kernel::RefusalCategory> for RefusalCategory {
    fn from(category: norn_kernel::RefusalCategory) -> Self {
        match category {
            norn_kernel::RefusalCategory::Visibility => Self::Visibility,
            norn_kernel::RefusalCategory::Permission => Self::Permission,
            norn_kernel::RefusalCategory::Precondition => Self::Precondition,
            norn_kernel::RefusalCategory::Conflict => Self::Conflict,
            norn_kernel::RefusalCategory::Invalid => Self::Invalid,
            norn_kernel::RefusalCategory::Limit => Self::Limit,
            norn_kernel::RefusalCategory::Internal => Self::Internal,
        }
    }
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

/// A refused request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
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
}

impl Refusal {
    /// Opaque diagnostics have no trusted stored-value provenance. Neither an
    /// ordinary read grant nor a broad marking grant proves them revealable.
    pub(crate) const UNPROVEN_DETAIL: &str =
        "diagnostic detail is unavailable under the caller's disclosure authority";

    /// Project the entire diagnostic, never fragments matched against values.
    /// Codes, categories and status remain usable even when detail is hidden.
    pub(crate) fn project_detail(mut self, revealable: bool) -> Self {
        if !revealable {
            Self::UNPROVEN_DETAIL.clone_into(&mut self.explanation);
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
            status: status.as_u16(),
            code: code.to_owned(),
            category: RefusalCategory::of_status(status),
            message: message.into(),
            explanation: explanation.into(),
        }
    }

    /// The same refusal with the category its source declared (a kernel
    /// refusal's `category()`).
    #[must_use]
    pub fn with_category(mut self, category: RefusalCategory) -> Self {
        self.category = category;
        self
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

    /// Preserve the projection resource limit across a kernel commit wrapper.
    #[must_use]
    pub fn commit(error: CommitError<CellError>) -> Self {
        let refusal = match &error {
            CommitError::TraversalLimitExceeded { .. } => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                error.code(),
                "the relation proof exceeds its traversal budget",
                error.to_string(),
            )
            .with_category(error.category().into()),
            CommitError::Host(CellError::ProjectionBudgetExceeded(detail)) => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "identity.snapshot.limit",
                "the cell identity snapshot exceeds its read bound",
                detail,
            ),
            CommitError::Host(CellError::IdentityInputInvalid(detail)) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "identity.input.invalid",
                "identity input is invalid",
                detail,
            ),
            CommitError::OwnedRowsProtected { .. }
            | CommitError::DuplicateKey { .. }
            | CommitError::NotPending { .. }
            | CommitError::Conflict { .. }
            | CommitError::DuplicateLink { .. }
            | CommitError::IdentifierTooLong { .. }
            | CommitError::LinkConflict { .. }
            | CommitError::LinkReadConflict { .. }
            | CommitError::UnsupportedLinkRead { .. }
            | CommitError::DuplicateLinkRead { .. }
            | CommitError::LinkReadTooLarge { .. }
            | CommitError::InvalidLinkRead { .. }
            | CommitError::DanglingLink { .. }
            | CommitError::LinkTarget { .. }
            | CommitError::LinkCardinality { .. }
            | CommitError::LinkRequired { .. }
            | CommitError::TypeMismatch { .. }
            | CommitError::EffectiveValues { .. }
            | CommitError::AccountingPosting { .. }
            | CommitError::InvariantViolation { .. }
            | CommitError::InvariantBudgetExceeded { .. }
            | CommitError::InvariantModel { .. }
            | CommitError::ObservationAuthorization
            | CommitError::EventLimitExceeded { .. }
            | CommitError::Overflow
            | CommitError::Host(_) => Self::host(error),
        };
        refusal.project_detail(false)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{Refusal, RefusalCategory};
    use axum::http::StatusCode;

    #[test]
    fn host_refusals_take_their_category_from_status_and_kernel_refusals_keep_theirs() {
        let malformed = Refusal::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "action.invalid_input",
            "m",
            "e",
        );
        assert_eq!(malformed.category, RefusalCategory::Invalid);
        assert_eq!(
            Refusal::new(StatusCode::NOT_FOUND, "object.not_found", "m", "e").category,
            RefusalCategory::Visibility
        );
        assert_eq!(
            Refusal::new(StatusCode::FORBIDDEN, "authz.denied", "m", "e").category,
            RefusalCategory::Permission
        );
        assert_eq!(Refusal::host("boom").category, RefusalCategory::Internal);
        let kernel = malformed.with_category(norn_kernel::RefusalCategory::Precondition.into());
        assert_eq!(kernel.category, RefusalCategory::Precondition);
    }
}

#[cfg(test)]
mod traversal_tests {
    #[test]
    fn traversal_limit_projects_stable_limit_code_and_category() -> Result<(), serde_json::Error> {
        use axum::response::IntoResponse as _;
        for reason in [
            "1000-record commit budget",
            "frontier budget",
            "adjacency read budget",
            "commit byte budget",
        ] {
            let error = norn_kernel::CommitError::<norn_cell::CellError>::TraversalLimitExceeded {
                key: norn_kernel::FactKey::new("location.Space", "leaf"),
                reason: reason.to_owned(),
            };
            let refusal = super::Refusal::commit(error);
            let json = serde_json::to_value(&refusal)?;
            assert_eq!(
                json.get("code").and_then(serde_json::Value::as_str),
                Some("commit.traversal_limit_exceeded")
            );
            assert_eq!(
                json.get("category").and_then(serde_json::Value::as_str),
                Some("limit")
            );
            assert_eq!(
                refusal.into_response().status(),
                axum::http::StatusCode::PAYLOAD_TOO_LARGE
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod opaque_diagnostic_tests {
    use super::*;

    #[test]
    fn stored_cell_values_never_become_unproven_host_explanations() {
        let secret = "stored-classified-value";
        for error in [
            CellError::TransitionNotAllowed {
                transition: "submit".into(),
                state: secret.into(),
            },
            CellError::BindingMismatch {
                key: "tenant".into(),
                expected: "caller".into(),
                found: secret.into(),
            },
            CellError::Engine(secret.into()),
            CellError::Encoding(secret.into()),
            CellError::HistoricalValueUnproven(secret.into()),
            CellError::DefinitionMigrationRefused(secret.into()),
            CellError::IdentityInputInvalid(secret.into()),
            CellError::ProjectionBudgetExceeded(secret.into()),
        ] {
            let refusal = Refusal::host(error);
            assert_eq!(refusal.code, codes::HOST_ERROR);
            assert_eq!(refusal.category, RefusalCategory::Internal);
            assert_eq!(refusal.explanation, Refusal::UNPROVEN_DETAIL);
        }
    }
}
