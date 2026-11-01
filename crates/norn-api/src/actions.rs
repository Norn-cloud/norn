//! One epoch-pinned governed action path for HTTP, MCP and the remote CLI.
use crate::{
    authz::{AccessRequest, Operation},
    host::DevHost,
    refusal::Refusal,
    routes::DefinitionsQuery,
};
use norn_cell::engine::SqlEngine;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionDenial, ActionOutcome, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, DecisionTurn, LogicalTime, Refusable, TypeCatalog, World,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Exact action invocation; the actor comes exclusively from the transport.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionRequest {
    /// Admitted action identity.
    pub action_id: String,
    /// Exact business arguments.
    #[schema(value_type = Object)]
    pub input: RuntimeObject,
    /// Stable retry identity.
    pub idempotency_key: String,
    /// Required admitted revision pin.
    pub version: u64,
}

/// Successful committed or replayed kernel record.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionResponse {
    /// Whether this was an identical retry.
    pub replayed: bool,
    /// Kernel call record without transport reinterpretation.
    #[schema(value_type = Object)]
    pub record: norn_kernel::ActionCallRecord,
}
pub(crate) struct Policy<'a>(pub(crate) &'a dyn crate::authz::Authorizer);
impl ActionAuthorizer for Policy<'_> {
    fn authorize(&self, r: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.0.action(r)
    }
    fn authorize_read(&self, r: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.0.action_read(r)
    }
    fn authorize_read_type(&self, r: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.0.action_read_type(r)
    }
}

/// Dispatch under a single definition epoch through the posting kernel.
///
/// # Errors
/// Scope, policy, revision, argument and kernel refusals change no business state.
#[utoipa::path(post, path = "/v1/actions", request_body = ActionRequest,
    params(("x-norn-org-id" = String, Header, description = "Explicit organization"), ("x-norn-environment-id" = String, Header, description = "Explicit environment")),
    responses((status = 200, body = ActionResponse), (status = 403, body = crate::refusal::Refusal), (status = 409, body = crate::refusal::Refusal), (status = 422, body = crate::refusal::Refusal)))]
pub fn call_action_in_epoch<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &DefinitionsQuery,
    actor: Option<&str>,
    request: ActionRequest,
) -> Result<ActionResponse, Refusal> {
    let _epoch = host.definition_read()?;
    host.scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref())?;
    host.authorizer.authorize(&AccessRequest {
        scope: host.scope(),
        actor,
        operation: Operation::CallAction(request.action_id.clone()),
    })?;
    let actor = actor
        .filter(|a| !a.trim().is_empty())
        .ok_or_else(|| Refusal::invalid("missing authenticated actor"))?;
    let mut cell = host.cell()?;
    let installed = cell
        .store()
        .read()
        .installed_definitions(None)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("missing definitions"))?;
    if installed.version != request.version {
        return Err(Refusal::new(
            http::StatusCode::CONFLICT,
            "defs.stale",
            "definitions changed; refresh the catalog",
            "the action revision pin is stale",
        ));
    }
    let artifacts =
        serde_json::from_str::<Vec<norn_ir::ArtifactEnvelope>>(&installed.canonical_set)
            .map_err(Refusal::host)?;
    let rules = norn_kernel::admit_stock_rules(&artifacts).map_err(Refusal::host)?;
    let model = cell.store().model().clone();
    let catalog = TypeCatalog::new(model.object_types())
        .with_property_types(model.property_types())
        .with_interfaces(model.interfaces())
        .with_stock_rules(&rules);
    let rule = catalog
        .stock_action(&request.action_id)
        .ok_or_else(|| Refusal::invalid("unknown admitted action"))?;
    let id = request
        .input
        .get("id")
        .and_then(RuntimeValue::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| Refusal::invalid("a nonblank voucher id is required"))?;
    if request.input.len() != 1 {
        return Err(Refusal::invalid("unexpected stock action arguments"));
    }
    let now = LogicalTime::from_millis(cell.store().read().clock_floor().map_err(Refusal::host)?);
    cell.advance_to(now);
    let call = rule.call(
        id,
        BTreeMap::from([
            ("orgId".into(), host.scope().org_id.clone()),
            ("environmentId".into(), host.scope().environment_id.clone()),
        ]),
        actor.into(),
        request.idempotency_key.clone(),
        DecisionTurn {
            id: request.idempotency_key,
            now,
            world: World::Main,
            consumes: None,
        },
    );
    let outcome = norn_kernel::commit_action(
        &mut *cell,
        &catalog,
        &call,
        &Policy(host.authorizer.as_ref()),
    )
    .map_err(|error| kernel_refusal(error.refusal()))?;
    Ok(match outcome {
        ActionOutcome::Committed { record, .. } => ActionResponse {
            replayed: false,
            record,
        },
        ActionOutcome::Replayed { record } => ActionResponse {
            replayed: true,
            record,
        },
    })
}

fn kernel_refusal(safe: norn_kernel::Refusal) -> Refusal {
    use crate::refusal::RefusalCategory as A;
    use norn_kernel::RefusalCategory as K;
    let (status, category) = match safe.category {
        K::Visibility => (http::StatusCode::NOT_FOUND, A::Visibility),
        K::Permission => (http::StatusCode::FORBIDDEN, A::Permission),
        K::Precondition => (http::StatusCode::UNPROCESSABLE_ENTITY, A::Precondition),
        K::Conflict => (http::StatusCode::CONFLICT, A::Conflict),
        K::Invalid => (http::StatusCode::BAD_REQUEST, A::Invalid),
        K::Limit => (http::StatusCode::TOO_MANY_REQUESTS, A::Limit),
        K::Internal => (http::StatusCode::INTERNAL_SERVER_ERROR, A::Internal),
    };
    Refusal::new(status, &safe.code, safe.message.clone(), safe.message).with_category(category)
}

#[cfg(feature = "http-host")]
pub(crate) fn router<E: SqlEngine + Send + 'static>() -> axum::Router<std::sync::Arc<DevHost<E>>> {
    use axum::{extract::State, response::IntoResponse};
    type Host<E> = State<std::sync::Arc<DevHost<E>>>;
    fn scope(headers: &http::HeaderMap) -> DefinitionsQuery {
        DefinitionsQuery {
            org_id: headers
                .get("x-norn-org-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
            environment_id: headers
                .get("x-norn-environment-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
            version: None,
        }
    }
    async fn dispatch<E: SqlEngine + Send + 'static>(
        State(host): Host<E>,
        headers: http::HeaderMap,
        body: axum::body::Bytes,
        mcp: bool,
    ) -> axum::response::Response {
        let result = tokio::task::spawn_blocking(move || {
            let actor = headers.get("x-norn-actor").and_then(|v| v.to_str().ok());
            let value: serde_json::Value = serde_json::from_slice(&body).map_err(|e| Refusal::invalid(e.to_string()))?;
            let id = value.get("id").cloned().unwrap_or(serde_json::Value::Null);
            if mcp && value["jsonrpc"] != "2.0" { return Err(Refusal::invalid("expected JSON-RPC 2.0")); }
            if mcp && value["method"] == "tools/list" {
                let document = crate::routes::get_defs(&host, &scope(&headers), actor)?;
                let schema = serde_json::json!({"type":"object","required":["actionId","input","idempotencyKey","version"],"properties":{"actionId":{"type":"string"},"input":{"type":"object"},"idempotencyKey":{"type":"string"},"version":{"type":"integer","const":document.version}},"additionalProperties":false});
                return Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"call_action","description":"Invoke an admitted action through the governed kernel","inputSchema":schema}]}}));
            }
            let request = if mcp {
                if value["method"] != "tools/call" || value["params"]["name"] != "call_action" { return Err(Refusal::invalid("unknown MCP method or tool")); }
                let mut args = value["params"]["arguments"].clone();
                let key = &value["params"]["_meta"]["norn/idempotencyKey"];
                if args.get("idempotencyKey").is_some_and(|v| v != key) { return Err(Refusal::invalid("conflicting retry identities")); }
                args["idempotencyKey"] = key.clone();
                serde_json::from_value(args)
            } else { serde_json::from_value(value) }.map_err(|e| Refusal::invalid(e.to_string()))?;
            let result = call_action_in_epoch(&host, &scope(&headers), actor, request);
            if mcp {
                let (failed, document) = match result { Ok(v) => (false, serde_json::to_value(v).map_err(Refusal::host)?), Err(e) => (true, serde_json::to_value(e).map_err(Refusal::host)?) };
                Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"isError":failed,"structuredContent":document,"content":[{"type":"text","text":document.to_string()}]}}))
            } else { result.and_then(|v| serde_json::to_value(v).map_err(Refusal::host)) }
        }).await.map_err(Refusal::host).and_then(std::convert::identity);
        let mut response = match result {
            Ok(value) => axum::Json(value).into_response(),
            Err(error) => (
                http::StatusCode::from_u16(error.status)
                    .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR),
                axum::Json(error),
            )
                .into_response(),
        };
        response.headers_mut().insert(
            http::header::CACHE_CONTROL,
            http::HeaderValue::from_static("no-store"),
        );
        response
    }
    async fn api<E: SqlEngine + Send + 'static>(
        host: Host<E>,
        headers: http::HeaderMap,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        dispatch(host, headers, body, false).await
    }
    async fn mcp<E: SqlEngine + Send + 'static>(
        host: Host<E>,
        headers: http::HeaderMap,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        dispatch(host, headers, body, true).await
    }
    axum::Router::new()
        .route("/v1/actions", axum::routing::post(api::<E>))
        .route("/mcp", axum::routing::post(mcp::<E>))
}
