//! Loopback fixture host for renderer and transport integration.

use std::{error::Error, io::Write, sync::Arc};

use axum::{extract::Request, middleware::Next};
use norn_api::{
    authz::{AccessRequest, Authorizer, DenyAll},
    host::DevHost,
    refusal::Refusal,
    scope::CellScope,
};
use norn_cell::engine::NativeEngine;
use norn_ir::RuntimeValue;
use norn_kernel::LogicalTime;

struct FixtureReader;

impl Authorizer for FixtureReader {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        if request.actor == Some("fixture-reader") {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }
}

fn router(source: &str, token: String) -> Result<axum::Router, Refusal> {
    let host = DevHost::new(
        NativeEngine::open_in_memory().map_err(Refusal::host)?,
        source,
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        },
        LogicalTime::EPOCH,
    )?
    .with_authorizer(FixtureReader);
    Ok(
        norn_api::routes::router(Arc::new(host)).layer(axum::middleware::from_fn(
            move |mut request: Request, next: Next| {
                let authorized = request
                    .headers()
                    .get("authorization")
                    .and_then(|header| header.to_str().ok())
                    .and_then(|header| header.strip_prefix("Bearer "))
                    == Some(token.as_str());
                request.headers_mut().remove("x-norn-actor");
                if authorized {
                    request.headers_mut().insert(
                        "x-norn-actor",
                        http::HeaderValue::from_static("fixture-reader"),
                    );
                }
                async move { next.run(request).await }
            },
        )),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let token = std::env::var("NORN_DEMO_TOKEN")?;
    if token.trim().is_empty() {
        return Err("NORN_DEMO_TOKEN must not be blank".into());
    }
    let paths = std::env::args().skip(1).collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("usage: fixture-server DEFINITIONS.json [DEFINITIONS.json ...]".into());
    }
    let mut artifacts = Vec::new();
    for path in paths {
        let RuntimeValue::Array(mut definitions) =
            RuntimeValue::from_json_text(std::fs::read_to_string(path)?)?
        else {
            return Err("definitions must be an artifact array".into());
        };
        artifacts.append(&mut definitions);
    }
    let source = String::from_utf8(RuntimeValue::Array(artifacts).canonical_json()?)?;
    let app = router(&source, token).map_err(|refusal| refusal.message)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4174").await?;
    writeln!(
        std::io::stdout(),
        "Fixture definitions: http://127.0.0.1:4174/v1/defs?orgId=org_demo&environmentId=dev"
    )?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    missing_docs,
    reason = "fixture authentication conformance"
)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    #[tokio::test]
    async fn host_fixture_authenticates_before_accepting_actor_headers() {
        let app = router("[]", "fixture-token".into()).expect("fresh admitted tenant");
        for (credential, actor, status) in [
            (None, None, 403),
            (None, Some("fixture-reader"), 403),
            (Some("Bearer wrong"), Some("fixture-reader"), 403),
            (Some("Bearer fixture-token"), None, 200),
        ] {
            let mut request =
                http::Request::builder().uri("/v1/defs?orgId=org_demo&environmentId=dev");
            if let Some(value) = credential {
                request = request.header("authorization", value);
            }
            if let Some(value) = actor {
                request = request.header("x-norn-actor", value);
            }
            let response = app
                .clone()
                .oneshot(request.body(axum::body::Body::empty()).expect("request"))
                .await
                .expect("response");
            assert_eq!(response.status().as_u16(), status);
        }
    }
}
