//! Source connections and uploads on the Durable Object host (#921).
//!
//! The cell owns both: [`norn_api::sources`] pulls connected systems,
//! [`norn_api::uploads`] signs storage URLs and loads uploaded files. This
//! module routes their requests and plans the host's outbound fetches; the
//! Worker only performs a request the cell planned and hands the response
//! back (a continuation), so no data or credential decision is made in
//! JavaScript.
//!
//! Routes (all `POST`, scoped by `orgId` and `environmentId` like every other):
//! - `/v1/sources/{id}:sync`: sync now; 202 `{"run": …}`.
//! - `/v1/uploads:begin`: `{fileName, contentType, size, prefix?}` → a signed
//!   `PUT` the browser sends the bytes to.
//! - `/v1/uploads/{id}:complete`: the cell reads the object, records its URI
//!   and SHA-256 and profiles it.
//! - `/v1/uploads/{id}:load`: [`LoadRequest`]; installs and loads.
//! - `/v1/uploads:read`: `{"uri"}` or `{"uploadId"}` → a short-lived signed
//!   `GET` of an object under a prefix this cell writes.
//!
//! CONNECT-AUTH-1: every route is decided by the cell's policy as
//! `Operation::SyncSource` before anything else; a denied caller, or a scope
//! with no cell, gets exactly what [`server::outside`] answers the request,
//! so a scope's existence is never disclosed.
//! CONNECT-FETCH-1: every outbound URL is built from server configuration
//! (`NORN_SOURCES`, `NORN_S3_*`): a connection's base URL or the storage
//! endpoint, never a request's or the cell's data.

use std::collections::BTreeMap;

use norn_api::authz::Operation;
use norn_api::definition_routes::authorize;
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::scope::ScopeQuery;
use norn_api::sources::{
    ClaimError, HttpRequest, Sources, Step, Trigger, UploadRun, report_upload,
};
use norn_api::uploads::{self, LoadRequest, S3Config, Store, Table, URL_SECONDS};
use norn_cell::engine::SqlEngine;
use serde_json::{Value, json};

use crate::principal::{PrincipalResolver, RequestScope};
use crate::server::{self, Request, Response};

/// One routed request's answer, and what the host does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    /// The reply, unless the host must fetch first.
    pub response: Response,
    /// A sync to run in the background.
    pub start_sync: Option<String>,
    /// Fetch this, then call [`Connect::resume`] with the token; its answer replaces `response`.
    pub then: Option<(String, HttpRequest)>,
}

impl Routed {
    fn reply(response: Response) -> Self {
        Self {
            response,
            start_sync: None,
            then: None,
        }
    }
}

struct Upload {
    store: Store,
    key: String,
    file_name: String,
    uri: String,
    completed: Option<(String, u64, bool)>,
}

enum Waiting {
    Complete {
        upload: String,
    },
    Load {
        upload: String,
        request: LoadRequest,
        actor: String,
        started_ms: u64,
    },
}

/// The cell's connections, uploads and in-flight continuations.
pub struct Connect {
    sources: Sources,
    uploads_store: Option<S3Config>,
    documents_store: Option<S3Config>,
    uploads: BTreeMap<String, Upload>,
    waiting: BTreeMap<String, Waiting>,
    issued: u64,
}

fn json_response(status: u16, body: &Value) -> Response {
    Response {
        status,
        body: body.to_string(),
    }
}

fn refused(status: u16, code: &str, message: &str) -> Response {
    let refusal = Refusal::new(
        http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::BAD_REQUEST),
        code,
        message,
        message,
    );
    Response {
        status: refusal.status,
        body: serde_json::to_string(&refusal).unwrap_or_else(|_e| String::from("{}")),
    }
}

enum Route<'a> {
    Sync(&'a str),
    Begin,
    Complete(&'a str),
    Load(&'a str),
    Read,
}

fn route_of(path: &str) -> Option<Route<'_>> {
    if path == "/v1/uploads:begin" {
        return Some(Route::Begin);
    }
    if path == "/v1/uploads:read" {
        return Some(Route::Read);
    }
    if let Some(rest) = path.strip_prefix("/v1/sources/") {
        let id = rest.strip_suffix(":sync")?;
        return (!id.is_empty() && !id.contains('/')).then_some(Route::Sync(id));
    }
    let rest = path.strip_prefix("/v1/uploads/")?;
    let (id, verb) = rest.split_once(':')?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    match verb {
        "complete" => Some(Route::Complete(id)),
        "load" => Some(Route::Load(id)),
        _ => None,
    }
}

fn scope_of(query: &str) -> ScopeQuery {
    let mut scope = ScopeQuery {
        org_id: None,
        environment_id: None,
    };
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "orgId" => scope.org_id = Some(value.into_owned()),
            "environmentId" => scope.environment_id = Some(value.into_owned()),
            _ => {}
        }
    }
    scope
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Begin {
    file_name: String,
    content_type: String,
    size: u64,
    #[serde(default)]
    prefix: Option<String>,
}

impl Connect {
    /// The connections in `config` (`NORN_SOURCES`) and the server variables
    /// `vars` (a JSON object of the `NORN_SECRET_*` and `NORN_S3_*` values).
    ///
    /// # Errors
    /// Malformed configuration.
    pub fn new(config: &str, vars: &str) -> Result<Self, String> {
        let vars: BTreeMap<String, String> =
            serde_json::from_str(vars).map_err(|e| format!("server variables: {e}"))?;
        let secrets = vars
            .iter()
            .filter(|(name, _)| name.starts_with("NORN_SECRET_"))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        Ok(Self {
            sources: Sources::new(config, secrets)?,
            uploads_store: S3Config::from_vars(&vars, Store::Uploads),
            documents_store: S3Config::from_vars(&vars, Store::Documents),
            uploads: BTreeMap::new(),
            waiting: BTreeMap::new(),
            issued: 0,
        })
    }

    /// SYNC-ONE-1: the next connection due on its schedule, claimed.
    pub fn due(&mut self, now_ms: u64) -> Option<String> {
        self.sources.due(now_ms)
    }

    /// Advance a running sync; the JSON the host reads: `{"request": …}` or `{"done": …}`.
    pub fn step<E: SqlEngine>(
        &mut self,
        host: Option<&DevHost<E>>,
        id: &str,
        response: Option<(u16, &[u8])>,
        now_ms: u64,
    ) -> String {
        let Some(host) = host else {
            return json!({"done": {"outcome": "failed", "error": ["source.no_cell", "the scope has no cell"]}}).to_string();
        };
        match self.sources.step(host, id, response, now_ms) {
            Step::Request(request) => json!({"request": request}).to_string(),
            Step::Done(summary) => json!({"done": summary}).to_string(),
        }
    }

    /// The configured storage of `store`.
    fn store(&self, store: Store) -> Option<&S3Config> {
        match store {
            Store::Uploads => self.uploads_store.as_ref(),
            Store::Documents => self.documents_store.as_ref(),
        }
    }

    fn token(&mut self, now_ms: u64) -> String {
        self.issued = self.issued.saturating_add(1);
        uploads::sha256_hex(format!("{now_ms}/{}", self.issued).as_bytes())
            .chars()
            .take(20)
            .collect()
    }

    /// Route `request` if it is one of this module's; `None` otherwise.
    pub fn route<E: SqlEngine>(
        &mut self,
        host: Option<&DevHost<E>>,
        principals: &dyn PrincipalResolver,
        request: &Request<'_>,
        now_ms: u64,
    ) -> Option<Routed> {
        let route = route_of(request.path)?;
        let _scope = RequestScope::new(principals);
        let actor = principals.resolve(request.authorization, request.now);
        let target = match &route {
            Route::Sync(id) => (*id).to_owned(),
            Route::Begin | Route::Complete(_) | Route::Load(_) | Route::Read => "upload".to_owned(),
        };
        // CONNECT-AUTH-1: decided first, answered like a scope with no cell.
        let allowed = host.filter(|host| {
            request.method == "POST"
                && authorize(
                    *host,
                    &scope_of(request.query),
                    actor.as_deref(),
                    Operation::SyncSource(target.clone()),
                )
                .is_ok()
        });
        let (Some(host), Some(actor)) = (allowed, actor) else {
            return Some(Routed::reply(server::outside::<E>(request)));
        };
        Some(match route {
            Route::Sync(id) => match self.sources.claim(id, Trigger::Manual, &actor, now_ms) {
                Ok(run) => Routed {
                    response: json_response(202, &json!({"run": run})),
                    start_sync: Some(id.to_owned()),
                    then: None,
                },
                Err(ClaimError::Running) => Routed::reply(refused(
                    409,
                    "source.sync_running",
                    "a sync of this connection is running",
                )),
                Err(ClaimError::Unknown) => {
                    Routed::reply(refused(404, "source.unknown", "no such connection"))
                }
            },
            Route::Begin => Routed::reply(self.begin(host, request.body, now_ms)),
            Route::Complete(id) => {
                self.fetch_object(id, now_ms, |upload| Waiting::Complete { upload })
            }
            Route::Load(id) => match serde_json::from_str::<LoadRequest>(request.body) {
                Ok(load) => {
                    let actor = actor.clone();
                    self.fetch_object(id, now_ms, |upload| Waiting::Load {
                        upload,
                        request: load,
                        actor,
                        started_ms: now_ms,
                    })
                }
                Err(e) => Routed::reply(refused(400, "upload.invalid_request", &e.to_string())),
            },
            Route::Read => Routed::reply(self.read(host, request.body, now_ms)),
        })
    }

    fn begin<E: SqlEngine>(&mut self, host: &DevHost<E>, body: &str, now_ms: u64) -> Response {
        let begin: Begin = match serde_json::from_str(body) {
            Ok(begin) => begin,
            Err(e) => return refused(400, "upload.invalid_request", &e.to_string()),
        };
        // The server picks the bucket by the file's kind; the client never names one.
        let store = Store::for_content_type(&begin.content_type);
        let Some(s3) = self.store(store).cloned() else {
            return refused(
                503,
                "upload.storage_unconfigured",
                "this server has no storage for this kind of file",
            );
        };
        if begin.size > uploads::MAX_OBJECT_BYTES {
            return refused(413, "upload.too_large", "uploads are at most 20 MB");
        }
        let name: String = begin
            .file_name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .take(100)
            .collect();
        let prefix = match begin.prefix.as_deref() {
            None => format!("uploads/{}", host.scope().key()),
            Some(p)
                if p.starts_with("synthetic/")
                    && p.len() <= 100
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
                    && !p.contains("//") =>
            {
                p.trim_end_matches('/').to_owned()
            }
            Some(_) => {
                return refused(
                    400,
                    "upload.invalid_prefix",
                    "a prefix is synthetic/… of letters, digits, '-', '_' and '/'",
                );
            }
        };
        let id = self.token(now_ms);
        // Seeded synthetic objects keep their names, so their URIs are known in
        // advance; a person's upload gets the id in its key.
        let key = if begin.prefix.is_some() {
            format!("{prefix}/{name}")
        } else {
            format!("{prefix}/{id}-{name}")
        };
        let put_url = s3.presign("PUT", &key, now_ms, URL_SECONDS);
        let uri = s3.uri(&key);
        self.uploads.insert(
            id.clone(),
            Upload {
                store,
                key,
                file_name: begin.file_name,
                uri: uri.clone(),
                completed: None,
            },
        );
        json_response(
            200,
            &json!({
                "uploadId": id, "uri": uri, "putUrl": put_url,
                "putHeaders": {"Content-Type": begin.content_type},
                "expiresAt": norn_api::sources::rfc3339(now_ms.saturating_add(u64::from(URL_SECONDS).saturating_mul(1000))),
            }),
        )
    }

    fn fetch_object(
        &mut self,
        id: &str,
        now_ms: u64,
        waiting: impl FnOnce(String) -> Waiting,
    ) -> Routed {
        let Some((s3, upload)) = self
            .uploads
            .get(id)
            .and_then(|u| Some((self.store(u.store)?, u)))
        else {
            return Routed::reply(refused(404, "upload.not_found", "no such upload"));
        };
        let url = s3.presign("GET", &upload.key, now_ms, URL_SECONDS);
        let token = self.token(now_ms);
        self.waiting.insert(token.clone(), waiting(id.to_owned()));
        Routed {
            response: refused(500, "host.error", "the host did not complete the read"),
            start_sync: None,
            then: Some((
                token,
                HttpRequest {
                    method: "GET",
                    url,
                    headers: Vec::new(),
                    body: None,
                },
            )),
        }
    }

    /// A signed `GET` for an object this cell recorded, by upload id, or by
    /// URI under a prefix this cell writes (`uploads/<scope>/`, `synthetic/`).
    fn read<E: SqlEngine>(&self, host: &DevHost<E>, body: &str, now_ms: u64) -> Response {
        let not_found = || refused(404, "upload.not_found", "no such upload");
        let request: Value = serde_json::from_str(body).unwrap_or_default();
        let found = if let Some(id) = request.get("uploadId").and_then(Value::as_str) {
            self.uploads
                .get(id)
                .and_then(|u| Some((self.store(u.store)?, u.key.clone())))
        } else {
            let own = format!("uploads/{}/", host.scope().key());
            let uri = request
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            [Store::Uploads, Store::Documents]
                .into_iter()
                .filter_map(|store| self.store(store))
                .find_map(|s3| {
                    let key = uri.strip_prefix(&format!("s3://{}/", s3.bucket))?;
                    ((key.starts_with(&own) || key.starts_with("synthetic/"))
                        && !key.contains(".."))
                    .then(|| (s3, key.to_owned()))
                })
        };
        let Some((s3, key)) = found else {
            return not_found();
        };
        json_response(
            200,
            &json!({
                "getUrl": s3.presign("GET", &key, now_ms, 300),
                "expiresAt": norn_api::sources::rfc3339(now_ms.saturating_add(300_000)),
            }),
        )
    }

    /// Continue the request whose fetch `token` the host just performed.
    pub fn resume<E: SqlEngine>(
        &mut self,
        host: Option<&DevHost<E>>,
        token: &str,
        status: u16,
        body: &[u8],
        now_ms: u64,
    ) -> Response {
        let (Some(waiting), Some(host)) = (self.waiting.remove(token), host) else {
            return refused(404, "upload.not_found", "no such continuation");
        };
        if status != 200 {
            return refused(
                502,
                "upload.read_failed",
                &format!("the storage answered HTTP {status}"),
            );
        }
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > uploads::MAX_OBJECT_BYTES {
            return refused(413, "upload.too_large", "uploads are at most 20 MB");
        }
        let sha256 = uploads::sha256_hex(body);
        let size = u64::try_from(body.len()).unwrap_or(u64::MAX);
        match waiting {
            Waiting::Complete { upload } => self.complete(&upload, body, &sha256, size),
            Waiting::Load {
                upload,
                request,
                actor,
                started_ms,
            } => self.load(
                host,
                &upload,
                &request,
                &actor,
                body,
                &sha256,
                (started_ms, now_ms),
            ),
        }
    }

    fn complete(&mut self, id: &str, body: &[u8], sha256: &str, size: u64) -> Response {
        let Some(upload) = self.uploads.get_mut(id) else {
            return refused(404, "upload.not_found", "no such upload");
        };
        let table = std::str::from_utf8(body).ok().map(uploads::parse_csv);
        let csv = matches!(table, Some(Ok(_)));
        upload.completed = Some((sha256.to_owned(), size, csv));
        let mut document = serde_json::Map::new();
        document.insert("uploadId".into(), json!(id));
        document.insert("uri".into(), json!(upload.uri));
        document.insert("sha256".into(), json!(sha256));
        document.insert("size".into(), json!(size));
        match table {
            Some(Ok(Table { columns, rows })) => {
                document.insert("rows".into(), json!(rows.len()));
                document.insert("columns".into(), json!(columns));
            }
            Some(Err(reason)) => {
                document.insert("csvError".into(), json!(reason));
            }
            None => {
                document.insert("csvError".into(), json!("the object is not UTF-8 text"));
            }
        }
        json_response(200, &Value::Object(document))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one continuation's whole context"
    )]
    fn load<E: SqlEngine>(
        &mut self,
        host: &DevHost<E>,
        id: &str,
        request: &LoadRequest,
        actor: &str,
        body: &[u8],
        sha256: &str,
        (started_ms, now_ms): (u64, u64),
    ) -> Response {
        let Some(upload) = self.uploads.get(id) else {
            return refused(404, "upload.not_found", "no such upload");
        };
        // UPLOAD-DIGEST-1: the object read now is the one profiled.
        match &upload.completed {
            Some((recorded, _, true)) if recorded == sha256 => {}
            Some((_, _, true)) => {
                return refused(
                    409,
                    "upload.changed",
                    "the object changed since it was completed",
                );
            }
            Some((_, _, false)) => return refused(422, "upload.not_csv", "only CSV uploads load"),
            None => return refused(409, "upload.not_completed", "complete the upload first"),
        }
        let table = match std::str::from_utf8(body)
            .map_err(|e| e.to_string())
            .and_then(uploads::parse_csv)
        {
            Ok(table) => table,
            Err(reason) => return refused(422, "upload.not_csv", &reason),
        };
        let observed_at = norn_api::sources::rfc3339(now_ms);
        let generated = match uploads::generate(&table, request, &upload.uri, sha256, &observed_at)
        {
            Ok(generated) => generated,
            Err(reason) => return refused(422, "upload.refused", &reason),
        };
        let version = match uploads::install(host, &generated, now_ms) {
            Ok(version) => version,
            Err(reason) => return refused(422, "upload.refused", &reason),
        };
        let loaded = match uploads::load_rows(host, &generated, &table, &upload.uri, sha256, now_ms)
        {
            Ok(loaded) => loaded,
            Err(reason) => return refused(422, "upload.refused", &reason),
        };
        let connection = format!("upload.{}", request.type_name.to_ascii_lowercase());
        report_upload(
            host,
            &UploadRun {
                id: &connection,
                label: &upload.file_name,
                uri: &upload.uri,
                sha256,
                mapping: &generated.mapping,
                actor,
                started_ms,
                ended_ms: now_ms,
                counts: (
                    loaded.rows_read,
                    loaded.rows_landed,
                    loaded.rows_quarantined,
                ),
                reasons: loaded.quarantine_reasons.clone(),
            },
        );
        json_response(
            200,
            &json!({
                "connectionId": connection, "objectType": generated.object_type, "definitionsVersion": version,
                "rowsRead": loaded.rows_read, "rowsLanded": loaded.rows_landed,
                "rowsQuarantined": loaded.rows_quarantined, "quarantineReasons": loaded.quarantine_reasons,
            }),
        )
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "route parsing scenarios")]
mod tests {
    use super::*;

    #[test]
    fn only_the_declared_routes_are_this_modules() {
        assert!(matches!(
            route_of("/v1/sources/erpnext.sandbox:sync"),
            Some(Route::Sync("erpnext.sandbox"))
        ));
        assert!(matches!(route_of("/v1/uploads:begin"), Some(Route::Begin)));
        assert!(matches!(route_of("/v1/uploads:read"), Some(Route::Read)));
        assert!(matches!(
            route_of("/v1/uploads/abc:complete"),
            Some(Route::Complete("abc"))
        ));
        assert!(matches!(
            route_of("/v1/uploads/abc:load"),
            Some(Route::Load("abc"))
        ));
        for other in [
            "/v1/sources/a/b:sync",
            "/v1/sources/:sync",
            "/v1/uploads/a/b:load",
            "/v1/uploads/abc:delete",
            "/v1/objects/x",
        ] {
            assert!(route_of(other).is_none(), "{other}");
        }
        let scope = scope_of("orgId=org_a&environmentId=prod&x=1");
        assert_eq!(scope.org_id.as_deref(), Some("org_a"));
        assert_eq!(scope.environment_id.as_deref(), Some("prod"));
    }
}
