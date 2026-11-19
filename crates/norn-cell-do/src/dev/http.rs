//! A bounded HTTP/1.1 transport for the dev host: one request per
//! connection, served in arrival order, over the same dispatch the Durable
//! Object host uses (`norn_cell_do::server`). It carries method, path, query,
//! the `Authorization` header and a bounded body, nothing else.
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::server::{self, Request, Response};
use norn_api::analytics_routes::{EngineQuestions, Transport};
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_cell::engine::NativeEngine;

use super::policy::{DevMetrics, DevPrincipals};

/// Longest accepted request head (request line plus headers), in bytes.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Longest accepted body, in bytes; the action route's own bound is lower.
const MAX_BODY_BYTES: usize = 512 * 1024;

/// The serving state shared with the session: `None` until the first
/// revision is installed.
pub(crate) type Serving = Arc<RwLock<Option<DevHost<NativeEngine>>>>;

/// Serve `listener` until the process exits.
pub(crate) fn serve(
    listener: &TcpListener,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
    engine: Option<&dyn Transport>,
) {
    for stream in listener.incoming().flatten() {
        // A broken connection affects only itself.
        let _ignored = connection(stream, serving, principals, questions, engine);
    }
}

fn connection(
    mut stream: TcpStream,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
    engine: Option<&dyn Transport>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let response = match read_request(&stream) {
        Ok(parsed) => dispatch(&parsed, serving, principals, questions, engine),
        Err(refused) => refusal(&refused),
    };
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        _ => "Status",
    };
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\ncontent-type: application/json\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        response.status,
        response.body.len(),
        response.body
    )?;
    stream.flush()
}

struct Parsed {
    method: String,
    path: String,
    query: String,
    authorization: Option<String>,
    body: String,
}

fn read_request(stream: &TcpStream) -> Result<Parsed, Refusal> {
    let mut reader = BufReader::new(
        stream
            .take(u64::try_from(MAX_HEAD_BYTES.saturating_add(MAX_BODY_BYTES)).unwrap_or(u64::MAX)),
    );
    let mut line = String::new();
    let mut head_bytes = 0_usize;
    let mut next = |reader: &mut BufReader<_>, line: &mut String| -> Result<(), Refusal> {
        line.clear();
        let read = reader
            .read_line(line)
            .map_err(|error| Refusal::invalid(format!("unreadable request: {error}")))?;
        head_bytes = head_bytes.saturating_add(read);
        if read == 0 || head_bytes > MAX_HEAD_BYTES {
            return Err(Refusal::invalid(
                "the request head is missing or exceeds the host bound",
            ));
        }
        Ok(())
    };
    next(&mut reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(Refusal::invalid("the request line is malformed"));
    };
    let (method, target) = (method.to_owned(), target.to_owned());
    let (mut length, mut authorization) = (0_usize, None);
    loop {
        next(&mut reader, &mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                length = value
                    .parse()
                    .map_err(|error| Refusal::invalid(format!("content-length: {error}")))?;
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.to_owned());
            }
        }
    }
    if length > MAX_BODY_BYTES {
        return Err(Refusal::new(
            http::StatusCode::PAYLOAD_TOO_LARGE,
            "request.too_large",
            "the request body exceeds the host bound",
            "a dev host request body is at most 512 KiB",
        ));
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).map_err(|error| {
        Refusal::invalid(format!("the body is shorter than content-length: {error}"))
    })?;
    let body = String::from_utf8(body)
        .map_err(|error| Refusal::invalid(format!("the body is not UTF-8: {error}")))?;
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    Ok(Parsed {
        path: path.to_owned(),
        query: query.to_owned(),
        method,
        authorization,
        body,
    })
}

fn dispatch(
    parsed: &Parsed,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
    engine: Option<&dyn Transport>,
) -> Response {
    let Ok(guard) = serving.read() else {
        return refusal(&Refusal::host("the serving state is poisoned"));
    };
    let Some(host) = guard.as_ref() else {
        return refusal(&Refusal::new(
            http::StatusCode::CONFLICT,
            "defs.not_provisioned",
            "the project has no admitted revision yet",
            "fix the refused change shown by norn dev; the first admitted revision is served",
        ));
    };
    let now = super::now_ms().checked_div(1000).unwrap_or_default();
    let request = Request {
        method: &parsed.method,
        path: &parsed.path,
        query: &parsed.query,
        authorization: parsed.authorization.as_deref(),
        now,
        body: &parsed.body,
    };
    answer(host, principals, questions, engine, &request)
}

/// One request against the serving cell: what the transport answers once it
/// has parsed the request. Tests drive the dev host through this.
pub(crate) fn answer(
    host: &DevHost<NativeEngine>,
    principals: &DevPrincipals,
    questions: &DevMetrics,
    engine: Option<&dyn Transport>,
    request: &Request<'_>,
) -> Response {
    // DEV-ONE-AUTHORITY-1: only this process installs definitions, from the
    // project's sources; no route that changes them is served here.
    if changes_definitions(request.path) {
        return refusal(&Refusal::new(
            http::StatusCode::CONFLICT,
            "defs.dev_sources_only",
            "norn dev installs definitions from the project's sources",
            "save the project's sources; norn dev admits and installs every change itself",
        ));
    }
    // A recorded export loads as on a hosted cell, decided by the same policy
    // first (CONNECT-AUTH-1), so one loader serves both hosts.
    if let Some(response) = crate::connect::observations(host, principals, request) {
        return response;
    }
    // The hosted host's definition kinds: an approval names hosted roles.
    let kinds = super::policy::dev_kinds();
    // The active revision's metrics, derived before the request runs.
    let questions = match questions.questions(host) {
        Ok(questions) => questions,
        Err(refused) => return refusal(&refused),
    };
    // D102: with an engine linked in, questions and profiles run on it.
    let Some(engine) = engine else {
        return server::handle_all(host, principals, Some(&questions), &kinds, request);
    };
    if let Some(response) = server::analytics(host, principals, engine, None, request) {
        return response;
    }
    let on_engine = EngineQuestions::new(&questions, engine, host.scope());
    server::handle_all(host, principals, Some(&on_engine), &kinds, request)
}

/// The routes that change the installed definitions: the hosted plan/apply
/// pair and the retained-definition install and retire. Admit only checks a
/// definition and stays served.
fn changes_definitions(path: &str) -> bool {
    path == norn_api::cell_definitions::PLAN_PATH
        || path == norn_api::cell_definitions::APPLY_PATH
        || path == "/v1/definitions:install"
        || path
            .strip_prefix("/v1/definitions/")
            .is_some_and(|rest| rest.ends_with(":retire"))
}

fn refusal(refused: &Refusal) -> Response {
    Response {
        status: refused.status,
        body: serde_json::to_string(refused).unwrap_or_else(|_error| String::from("{}")),
    }
}

/// The admitted constitution melt cell the action tests share.
#[cfg(test)]
#[path = "../../../norn-api/tests/support/action_cell.rs"]
#[allow(
    dead_code,
    reason = "the shared fixture module serves the action tests too"
)]
mod action_cell;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dev-host dispatch scenarios"
)]
mod tests {
    //! DEV-REVIEWER-1 through the dev host's own dispatch: the hosted
    //! definition kinds admit an approval over hosted roles, a gated call
    //! waits as one open decision, its requester never decides it, and the
    //! local reviewer's approval commits it as the requester.
    use super::*;
    use crate::dev::policy::{LocalDeveloper, reviewer};
    use norn_api::scope::CellScope;
    use serde_json::{Value, json};

    use super::action_cell::{DERIVES_LOSS, MELT_ACTION, melt_body, melt_store};

    const DEVELOPER: &str = "local:developer";
    const DEVELOPER_TOKEN: &str = "developer-token-0123456789";
    const REVIEWER_TOKEN: &str = "reviewer-token-01234567890";
    const SCOPE: &str = "orgId=org_demo&environmentId=dev";

    struct Dev {
        host: DevHost<NativeEngine>,
        principals: DevPrincipals,
        metrics: DevMetrics,
    }

    fn dev() -> Dev {
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let store = melt_store(
            DERIVES_LOSS,
            norn_api::action_admission::prepare_action_model,
        );
        Dev {
            host: DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
                .unwrap()
                .with_authorizer(LocalDeveloper::for_actor(DEVELOPER)),
            principals: DevPrincipals::new(vec![
                (DEVELOPER_TOKEN.into(), DEVELOPER.into()),
                (REVIEWER_TOKEN.into(), reviewer(DEVELOPER)),
            ]),
            metrics: DevMetrics::new([9; 32], scope),
        }
    }

    fn call(
        dev: &Dev,
        token: Option<&str>,
        (method, path, query): (&str, &str, &str),
        body: &str,
    ) -> (u16, Value) {
        let authorization = token.map(|token| format!("Bearer {token}"));
        let response = answer(
            &dev.host,
            &dev.principals,
            &dev.metrics,
            None,
            &Request {
                method,
                path,
                query,
                authorization: authorization.as_deref(),
                now: 1_800_000_000,
                body,
            },
        );
        (
            response.status,
            serde_json::from_str(&response.body).unwrap(),
        )
    }

    fn approval(roles: &[&str]) -> Value {
        json!({ "kind": "approval", "id": "ops.melt_approval", "payload": {
            "action": MELT_ACTION, "decider": { "roles": roles },
            "question": "Convert this melt?", "risk": "high" } })
    }

    /// Installs the approval in process, as norn dev installs from the
    /// project's sources: no network route installs (DEV-ONE-AUTHORITY-1).
    fn gate(dev: &Dev) {
        let mut body = approval(&["admin"]);
        let (status, admitted) = call(
            dev,
            Some(DEVELOPER_TOKEN),
            ("POST", "/v1/definitions:admit", SCOPE),
            &body.to_string(),
        );
        assert_eq!(status, 200, "{admitted}");
        body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
        norn_api::definition_routes::install_definition(
            &dev.host,
            &norn_api::scope::ScopeQuery {
                org_id: Some("org_demo".into()),
                environment_id: Some("dev".into()),
            },
            Some(DEVELOPER),
            &crate::dev::policy::dev_kinds(),
            None,
            &serde_json::from_str(&body.to_string()).unwrap(),
            1_800_000_000_000,
        )
        .unwrap();
    }

    #[test]
    fn no_route_changes_the_definitions_on_the_dev_host() {
        let dev = dev();
        let mut body = approval(&["admin"]);
        let (_, admitted) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("POST", "/v1/definitions:admit", SCOPE),
            &body.to_string(),
        );
        body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
        for path in [
            norn_api::cell_definitions::PLAN_PATH,
            norn_api::cell_definitions::APPLY_PATH,
            "/v1/definitions:install",
            "/v1/definitions/ops.melt_approval:retire",
        ] {
            let (status, refused) = call(
                &dev,
                Some(DEVELOPER_TOKEN),
                ("POST", path, SCOPE),
                &body.to_string(),
            );
            assert_eq!(
                (status, &refused["code"]),
                (409, &json!("defs.dev_sources_only")),
                "{path}: {refused}"
            );
        }
        let query = format!("{SCOPE}&kind=approval");
        let (status, listed) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("GET", "/v1/definitions:list", &query),
            "",
        );
        assert_eq!(status, 200, "{listed}");
        assert!(
            !listed.to_string().contains("ops.melt_approval"),
            "nothing was installed: {listed}"
        );
    }

    fn pending(dev: &Dev, token: &str) -> Value {
        let query = format!("{SCOPE}&state=pending");
        let (status, page) = call(dev, Some(token), ("GET", "/v1/decisions", &query), "");
        assert_eq!(status, 200, "{page}");
        page
    }

    fn melt(dev: &Dev) -> u16 {
        let path = "/v1/objects/constitution.Melt/melt-7";
        call(dev, Some(DEVELOPER_TOKEN), ("GET", path, SCOPE), "").0
    }

    #[test]
    fn an_approval_waits_on_the_dev_host_until_the_reviewer_decides() {
        let dev = dev();
        gate(&dev);
        let body = melt_body("melt-7", "12", "10", "dev-1");
        let action = format!("/v1/actions/{MELT_ACTION}");
        let (status, parked) = call(&dev, Some(DEVELOPER_TOKEN), ("POST", &action, SCOPE), &body);
        assert_eq!(status, 202, "{parked}");
        assert_eq!(parked["outcome"], json!("pending"));
        let id = parked["decision"]["id"].as_str().unwrap().to_owned();
        assert_eq!(melt(&dev), 404, "a waiting call commits nothing");

        // The requester never decides their own call (#618).
        assert_eq!(pending(&dev, DEVELOPER_TOKEN)["total"], json!(0));
        let decide = format!("/v1/decisions/{id}:decide");
        let choice = json!({ "choice": "approve", "idempotencyKey": "approve-1" }).to_string();
        let (status, own) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("POST", &decide, SCOPE),
            &choice,
        );
        assert_eq!(status, 404, "{own}");

        // Without a session token nothing is listed or decided.
        let (status, _) = call(&dev, None, ("POST", &decide, SCOPE), &choice);
        assert_ne!(status, 200);

        assert_eq!(pending(&dev, REVIEWER_TOKEN)["total"], json!(1));
        let (status, decided) = call(
            &dev,
            Some(REVIEWER_TOKEN),
            ("POST", &decide, SCOPE),
            &choice,
        );
        assert_eq!(status, 200, "{decided}");
        assert_eq!(decided["call"], json!({ "committed": true }), "{decided}");
        assert_eq!(melt(&dev), 200);

        // The approved call committed as its requester, never as the
        // reviewer who approved it: the version's writer names the developer.
        let path = "/v1/objects/constitution.Melt/melt-7/history";
        let (status, history) = call(&dev, Some(DEVELOPER_TOKEN), ("GET", path, SCOPE), "");
        assert_eq!(status, 200, "{history}");
        let writer = &history["versions"][0]["writer"];
        assert_eq!(
            (&writer["kind"], &writer["actionId"], &writer["actor"]),
            (&json!("action"), &json!(MELT_ACTION), &json!(DEVELOPER)),
            "{history}"
        );
    }

    #[test]
    fn an_approval_admits_against_the_hosted_roles() {
        let dev = dev();
        let (status, refused) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("POST", "/v1/definitions:admit", SCOPE),
            &approval(&["auditor"]).to_string(),
        );
        assert_eq!(
            (status, &refused["admitted"]),
            (200, &json!(false)),
            "{refused}"
        );
        assert_eq!(
            refused["diagnostics"][0]["code"],
            json!("definition.approval_role_unknown")
        );
        let (status, admitted) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("POST", "/v1/definitions:admit", SCOPE),
            &approval(&["admin"]).to_string(),
        );
        assert_eq!(
            (status, &admitted["admitted"]),
            (200, &json!(true)),
            "{admitted}"
        );
    }

    #[test]
    fn a_load_without_a_session_token_is_answered_as_outside() {
        let dev = dev();
        let body = json!({ "profile": "synthetic-erpnext-three/1", "snapshot": [] }).to_string();
        let path = "/v1/observations:load";
        let anonymous = call(&dev, None, ("POST", path, SCOPE), &body);
        let outside = crate::server::outside::<NativeEngine>(&Request {
            method: "POST",
            path,
            query: SCOPE,
            authorization: None,
            now: 1_800_000_000,
            body: &body,
        });
        assert_eq!(
            anonymous,
            (outside.status, serde_json::from_str(&outside.body).unwrap())
        );
        // The developer reaches the loader, which refuses a load with no rows
        // before it looks up the mapping. A real load is driven end to end in
        // norn-ts `three_systems_dev_load`.
        let (status, refused) = call(&dev, Some(DEVELOPER_TOKEN), ("POST", path, SCOPE), &body);
        assert_eq!(status, 400, "{refused}");
        assert!(
            refused["explanation"]
                .as_str()
                .is_some_and(|detail| detail.contains("at least one row")),
            "{refused}"
        );
    }
}
