//! HOST-READ-1 transport: shared services own admission; HTTP preserves their JSON bytes.
use crate::{
    host::DevHost,
    question_routes::{QuestionRuntime, TraceQuery, get_answer, get_numeric_trace},
    refusal::Refusal,
    routes::{DefinitionsQuery, get_defs},
    scope::ScopeQuery,
};
use http::{Method, Request, Response, StatusCode, header};
use norn_cell::engine::SqlEngine;
use serde::Serialize;
use std::collections::BTreeMap;

/// Serve one read against an opened cell. `actor` is resolved by trusted server
/// bootstrap, never from query parameters or token-supplied roles. The returned
/// body must be forwarded unchanged (including from Workers); do not parse and
/// reserialize it in JavaScript.
pub fn dispatch<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    request: &Request<Vec<u8>>,
    actor: Option<&str>,
) -> Response<Vec<u8>> {
    let origin = request.headers().get(header::ORIGIN);
    let allowed_origin = origin.filter(|value| value.to_str().is_ok_and(local_origin));
    let mut response = if origin.is_some() && allowed_origin.is_none() {
        refusal(Refusal::new(
            StatusCode::FORBIDDEN,
            "origin.denied",
            "origin is not allowed",
            "only localhost development origins are allowed",
        ))
    } else if request.method() == Method::OPTIONS {
        response(StatusCode::NO_CONTENT, Vec::new())
    } else if request.method() != Method::GET {
        let mut result = refusal(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "request.method",
            "method is not allowed",
            "this surface accepts reads only",
        ));
        result.headers_mut().insert(
            header::ALLOW,
            header::HeaderValue::from_static("GET, OPTIONS"),
        );
        result
    } else {
        route(host, runtime, request, actor).unwrap_or_else(refusal)
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
        .headers_mut()
        .insert(header::VARY, header::HeaderValue::from_static("Origin"));
    if let Some(origin) = allowed_origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            header::HeaderValue::from_static("GET, OPTIONS"),
        );
    }
    response
}

fn local_origin(origin: &str) -> bool {
    let Ok(uri) = origin.parse::<http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("http")
        && matches!(uri.host(), Some("localhost" | "127.0.0.1" | "[::1]"))
        && uri.path() == "/"
        && uri.query().is_none()
}

fn route<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    request: &Request<Vec<u8>>,
    actor: Option<&str>,
) -> Result<Response<Vec<u8>>, Refusal> {
    if request.uri().to_string().len() > 16_384 {
        return Err(Refusal::invalid("read URI exceeds the transport limit"));
    }
    let mut query = BTreeMap::new();
    for (key, value) in form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
    {
        if query.insert(key.into_owned(), value.into_owned()).is_some() {
            return Err(Refusal::invalid("duplicate query parameter"));
        }
    }
    let get = |name: &str| query.get(name).cloned();
    let scope = ScopeQuery {
        org_id: get("orgId"),
        environment_id: get("environmentId"),
    };
    if request.uri().path() == "/v1/defs" {
        let query = DefinitionsQuery {
            org_id: scope.org_id,
            environment_id: scope.environment_id,
            version: get("version")
                .map(|value| value.parse())
                .transpose()
                .map_err(|_| Refusal::invalid("invalid definitions version"))?,
        };
        return json(&get_defs(host, &query, actor)?);
    }
    let mut segments = request.uri().path().split('/');
    let route = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    );
    if let (Some(""), Some("v1"), Some("questions"), Some(question), Some(operation), None) = route
    {
        let question = percent_encoding::percent_decode_str(question)
            .decode_utf8()
            .map_err(|_| Refusal::invalid("invalid question identifier"))?;
        return match operation {
            "answer" => json(&get_answer(host, &scope, actor, &question, runtime)?),
            "trace" => json(&get_numeric_trace(
                host,
                &TraceQuery {
                    org_id: scope.org_id,
                    environment_id: scope.environment_id,
                    lineage_ref: get("lineageRef"),
                    bucket: get("bucket"),
                },
                actor,
                &question,
                runtime,
            )?),
            _ => Err(not_found()),
        };
    }
    Err(not_found())
}
fn json(value: &impl Serialize) -> Result<Response<Vec<u8>>, Refusal> {
    serde_json::to_vec(value)
        .map(|bytes| response(StatusCode::OK, bytes))
        .map_err(Refusal::host)
}
fn not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        "request.route_not_found",
        "no such route",
        "the requested read route does not exist",
    )
}
fn refusal(value: Refusal) -> Response<Vec<u8>> {
    let status = StatusCode::from_u16(value.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    match serde_json::to_vec(&value) {
        Ok(bytes) => response(status, bytes),
        Err(_) => response(
            StatusCode::INTERNAL_SERVER_ERROR,
            b"{\"code\":\"host.error\"}".to_vec(),
        ),
    }
}
fn response(status: StatusCode, bytes: Vec<u8>) -> Response<Vec<u8>> {
    let mut response = Response::new(bytes);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "HTTP boundary conformance fixtures"
)]
mod tests {
    use super::*;
    use crate::{authz::ReadCapability, question_routes::QuestionSnapshot, scope::CellScope};
    use norn_cell::{CellStore, Model, ObjectRecord, engine::NativeEngine};
    use norn_checker::metric::compute::QuestionPlan;
    struct Denied;
    impl QuestionRuntime<NativeEngine> for Denied {
        fn plan(&self, _: &str) -> Result<QuestionPlan, Refusal> {
            panic!("denied caller resolved a plan")
        }
        fn snapshot(
            &self,
            _: &CellStore,
            _: &QuestionPlan,
            _: &str,
        ) -> Result<QuestionSnapshot, Refusal> {
            panic!("denied caller resolved a snapshot")
        }
        fn access(
            &self,
            _: &CellScope,
            _: &str,
            _: &str,
            _: ReadCapability,
            _: Option<&ObjectRecord>,
        ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
            Err(Refusal::invalid("private principal mapping failure"))
        }
    }
    fn host() -> DevHost<NativeEngine> {
        DevHost::from_store(
            CellStore::open_in_memory(Model::from_definitions(&[], &[]).expect("model"))
                .expect("store"),
            CellScope {
                org_id: "example".into(),
                environment_id: "dev".into(),
            },
            |_| BTreeMap::new(),
        )
        .expect("host")
    }
    fn request(uri: &str) -> Request<Vec<u8>> {
        Request::builder()
            .uri(uri)
            .body(Vec::new())
            .expect("HTTP request")
    }
    #[test]
    fn trace_required_parameters_are_checked_after_access_without_a_scope_oracle() {
        let host = host();
        let baseline = dispatch(&host, &Denied, &request("/v1/questions/q/answer"), None);
        for route in ["answer", "trace"] {
            for scope in [
                "",
                "?orgId=example&environmentId=dev",
                "?orgId=foreign&environmentId=other",
            ] {
                for actor in [None, Some("denied")] {
                    let actual = dispatch(
                        &host,
                        &Denied,
                        &request(&format!("/v1/questions/q/{route}{scope}")),
                        actor,
                    );
                    assert_eq!(actual.status(), StatusCode::FORBIDDEN);
                    assert_eq!(actual.body(), baseline.body());
                }
            }
        }
    }
    #[test]
    fn cors_is_localhost_only_and_mutations_are_not_dispatched() {
        let host = host();
        for origin in [
            "http://localhost:5173",
            "http://127.0.0.1:3000",
            "http://[::1]:5173",
        ] {
            let mut req = request("/v1/questions/q/trace");
            *req.method_mut() = Method::OPTIONS;
            req.headers_mut()
                .insert(header::ORIGIN, origin.parse().expect("origin"));
            let actual = dispatch(&host, &Denied, &req, None);
            assert_eq!(actual.status(), StatusCode::NO_CONTENT);
            assert_eq!(
                actual
                    .headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .expect("CORS"),
                origin
            );
        }
        for origin in [
            "null",
            "http://localhost.example:5173",
            "https://example.com",
            "http://localhost:5173/path",
        ] {
            let mut req = request("/v1/defs");
            req.headers_mut()
                .insert(header::ORIGIN, origin.parse().expect("origin"));
            let actual = dispatch(&host, &Denied, &req, None);
            assert_eq!(actual.status(), StatusCode::FORBIDDEN);
            assert!(
                !actual
                    .headers()
                    .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            );
        }
        let mut req = request("/v1/questions/q/answer");
        *req.method_mut() = Method::POST;
        assert_eq!(
            dispatch(&host, &Denied, &req, None).status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            dispatch(&host, &Denied, &request("/v1/sql"), None).status(),
            StatusCode::NOT_FOUND
        );
    }
    #[test]
    fn native_raw_json_numbers_are_transported_without_numeric_coercion() {
        let original = "{\"values\":{\"large\":9007199254740993,\"decimal\":123456789.123456789}}";
        let native =
            serde_json::value::RawValue::from_string(original.into()).expect("native exact JSON");
        let actual = json(&native).expect("HTTP response");
        assert_eq!(actual.body(), original.as_bytes());
        assert_eq!(
            actual
                .headers()
                .get(header::CONTENT_TYPE)
                .expect("content type"),
            "application/json"
        );
    }
}
