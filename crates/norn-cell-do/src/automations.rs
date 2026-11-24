//! AUTOMATION-1 on the DO host: preview (`POST /v1/automations:preview`),
//! evaluate (`POST /v1/automations/{id}:run`), and read durable run history
//! (`GET /v1/automations/{id}/runs`).

use http::StatusCode;
use norn_api::automations::{
    AutomationPreviewRequest, AutomationRunsQuery, list_automation_runs, preview_automation,
    run_automation,
};
use norn_api::definition_routes::DefinitionKinds;
use norn_api::host::DevHost;
use norn_api::question_routes::{AdHocQuestions, QuestionRuntime};
use norn_api::refusal::Refusal;
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;

use crate::policy::CellObjectAccess;
use crate::server::{Request, Response, document_json, parse};

#[derive(Debug, Clone, Copy)]
enum AutomationsRoute<'p> {
    Preview,
    Run(&'p str),
    Runs(&'p str),
}

fn automations_route(path: &str) -> Option<AutomationsRoute<'_>> {
    if path == "/v1/automations:preview" {
        return Some(AutomationsRoute::Preview);
    }
    let rest = path.strip_prefix("/v1/automations/")?;
    if let Some(id) = rest
        .strip_suffix(":run")
        .filter(|id| !id.is_empty() && !id.contains('/'))
    {
        return Some(AutomationsRoute::Run(id));
    }
    if let Some(id) = rest
        .strip_suffix("/runs")
        .filter(|id| !id.is_empty() && !id.contains('/'))
    {
        return Some(AutomationsRoute::Runs(id));
    }
    None
}

/// Serve `request` when its path is an automation route; `None` otherwise.
pub(crate) fn dispatch<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    questions: Option<&dyn AdHocQuestions<E>>,
    kinds: &DefinitionKinds,
    actor: Option<&str>,
    request: &Request<'_>,
) -> Option<Response> {
    let route = automations_route(request.path)?;
    let result = serve(host, questions, kinds, actor, route, request);
    Some(match result {
        Ok(body) => Response { status: 200, body },
        Err(refused) => crate::incident::respond(refused),
    })
}

fn serve<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    questions: Option<&dyn AdHocQuestions<E>>,
    kinds: &DefinitionKinds,
    actor: Option<&str>,
    route: AutomationsRoute<'_>,
    request: &Request<'_>,
) -> Result<String, Refusal> {
    let expected = match route {
        AutomationsRoute::Preview | AutomationsRoute::Run(_) => "POST",
        AutomationsRoute::Runs(_) => "GET",
    };
    if request.method != expected {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            format!("this automation route takes {expected}"),
        ));
    }
    let now = norn_kernel::LogicalTime::from_millis(request.now.saturating_mul(1000));
    match route {
        AutomationsRoute::Preview => {
            let fields = parse(request.query, &["orgId", "environmentId"])?;
            let scope = ScopeQuery {
                org_id: fields.get("orgId").cloned(),
                environment_id: fields.get("environmentId").cloned(),
            };
            let body: AutomationPreviewRequest =
                serde_json::from_str(request.body).map_err(|error| {
                    Refusal::invalid(format!(
                        "the request body is not a valid automation preview: {error}"
                    ))
                })?;
            let host = host.ok_or_else(norn_api::authz::read_denied)?;
            let roles = kinds.roles();
            document_json(&preview_automation(
                host,
                &scope,
                actor,
                &roles,
                &body,
                &CellObjectAccess,
                now,
            )?)
        }
        AutomationsRoute::Run(id) => {
            let fields = parse(request.query, &["orgId", "environmentId"])?;
            let scope = ScopeQuery {
                org_id: fields.get("orgId").cloned(),
                environment_id: fields.get("environmentId").cloned(),
            };
            let id = percent_encoding::percent_decode_str(id)
                .decode_utf8()
                .map_err(|_bad| Refusal::invalid("an automation path segment is not UTF-8"))?;
            let host = host.ok_or_else(norn_api::authz::read_denied)?;
            let runtime: &dyn QuestionRuntime<E> =
                questions.ok_or_else(norn_api::authz::read_denied)?;
            document_json(&run_automation(
                host,
                &scope,
                actor,
                &id,
                &CellObjectAccess,
                runtime,
                now,
            )?)
        }
        AutomationsRoute::Runs(id) => {
            let mut fields = parse(request.query, &["orgId", "environmentId", "limit"])?;
            let limit = fields
                .remove("limit")
                .map(|v| {
                    v.parse::<u32>()
                        .map_err(|error| Refusal::invalid(format!("limit: {error}")))
                })
                .transpose()?;
            let query = AutomationRunsQuery {
                org_id: fields.remove("orgId"),
                environment_id: fields.remove("environmentId"),
                limit,
            };
            let id = percent_encoding::percent_decode_str(id)
                .decode_utf8()
                .map_err(|_bad| Refusal::invalid("an automation path segment is not UTF-8"))?;
            let host = host.ok_or_else(norn_api::authz::read_denied)?;
            document_json(&list_automation_runs(
                host,
                &query,
                actor,
                &id,
                &CellObjectAccess,
            )?)
        }
    }
}
