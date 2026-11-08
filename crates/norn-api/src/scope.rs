//! The cell's tenant scope and the explicit scope every request carries,
//! validated before any data access (fail closed).

use http::StatusCode;
use norn_ir::ScopeRef;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::refusal::{Refusal, codes};

/// The tenant scope one dev cell serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CellScope {
    /// Organization.
    pub org_id: String,
    /// Environment.
    pub environment_id: String,
}

impl CellScope {
    /// The scope string recorded on objects this cell creates and bound to
    /// its store file: `{orgId}/{environmentId}` with `%` and `/` escaped in
    /// each part, so distinct scopes never share a key
    /// (`org/x` + `dev` versus `org` + `x/dev`).
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}", escape(&self.org_id), escape(&self.environment_id))
    }

    /// The scope as an artifact [`ScopeRef`], for visibility checks.
    #[must_use]
    pub fn as_scope_ref(&self) -> ScopeRef {
        ScopeRef {
            org_id: Some(self.org_id.clone()),
            environment_id: Some(self.environment_id.clone()),
            ..ScopeRef::default()
        }
    }

    /// Admit both explicit scope dimensions, exactly matching this cell.
    ///
    /// # Errors
    ///
    /// `scope.missing` or `scope.denied`.
    pub fn admit(&self, org_id: Option<&str>, environment_id: Option<&str>) -> Result<(), Refusal> {
        let (Some(org_id), Some(environment_id)) = (org_id, environment_id) else {
            return Err(Refusal::new(
                StatusCode::BAD_REQUEST,
                codes::SCOPE_MISSING,
                "the request names no scope",
                "every request carries an explicit scope (orgId and environmentId); scope is never inferred",
            ));
        };
        if org_id.trim().is_empty() || environment_id.trim().is_empty() {
            return Err(Refusal::new(
                StatusCode::BAD_REQUEST,
                codes::SCOPE_MISSING,
                "the request scope is blank",
                "orgId and environmentId must be non-blank",
            ));
        }
        if org_id != self.org_id || environment_id != self.environment_id {
            return Err(Refusal::new(
                StatusCode::FORBIDDEN,
                codes::SCOPE_DENIED,
                "the request scope is not authorized",
                "access to the requested scope is denied",
            ));
        }
        Ok(())
    }
}

/// Percent-escape the key separator (and the escape character itself).
fn escape(part: &str) -> String {
    part.replace('%', "%25").replace('/', "%2F")
}

/// The scope of a request, carried in a JSON body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestScope {
    /// Organization.
    #[schema(required = true, nullable = false, value_type = String)]
    pub org_id: Option<String>,
    /// Environment.
    #[schema(required = true, nullable = false, value_type = String)]
    pub environment_id: Option<String>,
}

/// The scope of a read, carried in the query string.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct ScopeQuery {
    /// Organization (required).
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Environment (required).
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
}
