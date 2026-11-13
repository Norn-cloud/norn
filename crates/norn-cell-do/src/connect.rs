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
//! - `/v1/uploads/{id}:define`: [`LoadRequest`]; installs the generated
//!   definitions only, for a loader that writes the rows itself.
//! - `/v1/uploads/{id}:load`: [`LoadRequest`]; installs and loads.
//! - `/v1/uploads:read`: `{"uri"}` or `{"uploadId"}` → a short-lived signed
//!   `GET` of an object under a prefix this cell writes.
//! - `/v1/observations:load`: a slice of a recorded export
//!   ([`crate::recorded`], at most 500 rows) through the admitted mappings, in
//!   OBS-PAGE-1 pages → counts of the rows' outcomes.
//!
//! CONNECT-AUTH-1: every route is decided by the cell's policy as
//! `Operation::SyncSource` before anything else; a denied caller, or a scope
//! with no cell, gets exactly what [`server::outside`] answers the request,
//! so a scope's existence is never disclosed.
//! CONNECT-FETCH-1: every outbound URL is built from server configuration
//! (`NORN_SOURCES`, `NORN_S3_*`): a connection's base URL or the storage
//! endpoint, never a request's or the cell's data.
//! UPLOAD-ID-1: an upload id carries its store and object key, sealed with the
//! store's secret, so `:complete`, `:load` and `:read` work after the Durable
//! Object restarts; an id resolves only in the scope whose prefix its key names
//! (or `synthetic/`).
//! UPLOAD-RECORD-1: `:complete` keeps what the cell read back (URI, SHA-256,
//! size, the CSV profile, who completed it) as a `source.Upload` object
//! before it answers; after a restart `:load`, `:define` and the bulk loader
//! read it there, and the object read at load time must match its digest
//! (UPLOAD-DIGEST-1). A digest a client sends is only checked, never trusted.

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

#[derive(Clone)]
struct Upload {
    store: Store,
    key: String,
    file_name: String,
    content_type: String,
    uri: String,
    completed: Option<(String, u64, bool)>,
    columns: Vec<uploads::Column>,
}

/// A completed upload, as the cell recorded it: for loaders that read the
/// object themselves (the bulk loader), never its bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRecord {
    /// `s3://<bucket>/<key>`.
    pub uri: String,
    /// SHA-256 of the bytes the cell read at `:complete`.
    pub sha256: String,
    /// Their size.
    pub size: u64,
    /// The declared content type.
    pub content_type: String,
    /// The profiled columns (CSV), empty otherwise.
    pub columns: Vec<uploads::Column>,
    /// The file's name, as uploaded.
    pub file_name: String,
}

enum Waiting {
    Complete {
        upload: String,
        actor: String,
    },
    Load {
        upload: String,
        request: LoadRequest,
        actor: String,
        started_ms: u64,
        /// `false` for `:define`: install the definitions, write no rows.
        rows: bool,
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

/// An upload id the cell can check without remembering it:
/// `<u|d><hex key>.<seal>`, the store, the object key and the store's seal of both.
fn upload_id(store: Store, key: &str, s3: &S3Config) -> String {
    let tag = match store {
        Store::Uploads => 'u',
        Store::Documents => 'd',
    };
    let seal = s3.seal(&format!("upload/{tag}/{key}"));
    format!("{tag}{}.{seal}", uploads::hex(key.as_bytes()))
}

/// The store and key of a sealed upload id, if its seal checks.
fn unseal<'a>(
    id: &str,
    store_of: impl Fn(Store) -> Option<&'a S3Config>,
) -> Option<(Store, String)> {
    let (head, seal) = id.split_once('.')?;
    let mut chars = head.chars();
    let store = match chars.next()? {
        'u' => Store::Uploads,
        'd' => Store::Documents,
        _ => return None,
    };
    let digits: Vec<u32> = chars.map(|c| c.to_digit(16)).collect::<Option<_>>()?;
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Vec<u8> = digits
        .chunks(2)
        .map(|pair| match pair {
            [high, low] => u8::try_from(high.saturating_mul(16).saturating_add(*low)).ok(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let key = String::from_utf8(bytes).ok()?;
    let s3 = store_of(store)?;
    let expected = upload_id(store, &key, s3);
    let expected = expected.rsplit_once('.').map_or("", |(_, seal)| seal);
    (same(expected.as_bytes(), seal.as_bytes()) && !key.contains("..")).then_some((store, key))
}

/// Whether `a` equals `b`, in time that depends only on their lengths.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// Whether `key` lies under a prefix the cell serving `scope_key` writes.
fn owns(key: &str, scope_key: &str) -> bool {
    key.strip_prefix("uploads/")
        .and_then(|rest| rest.strip_prefix(scope_key))
        .is_some_and(|rest| rest.starts_with('/'))
        || key.starts_with("synthetic/")
}

enum Route<'a> {
    Sync(&'a str),
    Begin,
    Complete(&'a str),
    Define(&'a str),
    Load(&'a str),
    Read,
    Observations,
}

fn route_of(path: &str) -> Option<Route<'_>> {
    if path == "/v1/uploads:begin" {
        return Some(Route::Begin);
    }
    if path == "/v1/uploads:read" {
        return Some(Route::Read);
    }
    if path == "/v1/observations:load" {
        return Some(Route::Observations);
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
        "define" => Some(Route::Define(id)),
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

    /// The upload `id` names, when its object lies under a prefix the cell
    /// serving `scope_key` writes: as remembered, or, after a restart, rebuilt
    /// from the id itself once its seal checks (UPLOAD-ID-1).
    fn resolve(&self, id: &str, scope_key: &str) -> Option<Upload> {
        let upload = if let Some(upload) = self.uploads.get(id) {
            upload.clone()
        } else {
            let (store, key) = unseal(id, |store| self.store(store))?;
            let name = key.rsplit('/').next().unwrap_or_default();
            // A person's upload is `<nonce>-<name>`; the nonce is the cell's.
            let file_name = match name.split_once('-') {
                Some((nonce, rest))
                    if key.starts_with("uploads/")
                        && nonce.len() == 20
                        && nonce.chars().all(|c| c.is_ascii_hexdigit()) =>
                {
                    rest
                }
                _ => name,
            };
            Upload {
                store,
                file_name: file_name.to_owned(),
                content_type: String::new(),
                uri: self.store(store)?.uri(&key),
                key,
                completed: None,
                columns: Vec::new(),
            }
        };
        owns(&upload.key, scope_key).then_some(upload)
    }

    /// The upload `id` names in the cell `host` serves: [`Self::resolve`], its
    /// completion read back from the cell's kept record (UPLOAD-RECORD-1)
    /// when this instance never saw it complete (a restarted host).
    fn resolve_kept<E: SqlEngine>(
        &self,
        host: &DevHost<E>,
        id: &str,
    ) -> Result<Option<Upload>, String> {
        let Some(mut upload) = self.resolve(id, &host.scope().key()) else {
            return Ok(None);
        };
        if upload.completed.is_none()
            && let Some(kept) = norn_api::sources::kept_upload(host, id)?
            && kept.uri == upload.uri
        {
            upload.completed = Some((kept.sha256, kept.size, kept.csv));
            upload.columns = kept.columns;
            upload.content_type = kept.content_type;
            upload.file_name = kept.file_name;
        }
        Ok(Some(upload))
    }

    /// The completed upload `id`, as recorded; `None` before `:complete`.
    #[must_use]
    pub fn upload_record(&self, id: &str) -> Option<UploadRecord> {
        let upload = self.uploads.get(id)?;
        let (sha256, size, _) = upload.completed.clone()?;
        Some(UploadRecord {
            uri: upload.uri.clone(),
            sha256,
            size,
            content_type: upload.content_type.clone(),
            columns: upload.columns.clone(),
            file_name: upload.file_name.clone(),
        })
    }

    /// A signed `GET` of the object at `uri` valid for `seconds`, when `uri`
    /// names a configured bucket; for loaders the cell itself directs.
    #[must_use]
    pub fn signed_get(&self, uri: &str, seconds: u32, now_ms: u64) -> Option<String> {
        [Store::Uploads, Store::Documents]
            .into_iter()
            .filter_map(|store| self.store(store))
            .find_map(|s3| {
                let key = uri.strip_prefix(&format!("s3://{}/", s3.bucket))?;
                (!key.is_empty() && !key.contains(".."))
                    .then(|| s3.presign("GET", key, now_ms, seconds))
            })
    }

    /// SYNC-ONE-1: the next connection due on its schedule, claimed.
    pub fn due(&mut self, now_ms: u64) -> Option<String> {
        self.sources.due(now_ms)
    }

    /// Advance a running sync; the JSON the host reads: `{"request": …}` or `{"done": …}`.
    /// `rows`: the most rows this and later steps map (SYNC-YIELD-2); the
    /// answer says how many this step mapped (`mapped`), for the host's pacing.
    pub fn step<E: SqlEngine>(
        &mut self,
        host: Option<&DevHost<E>>,
        id: &str,
        response: Option<(u16, &[u8])>,
        (now_ms, rows): (u64, Option<usize>),
    ) -> String {
        let Some(host) = host else {
            return json!({"done": {"outcome": "failed", "error": ["source.no_cell", "the scope has no cell"]}}).to_string();
        };
        if let Some(rows) = rows {
            self.sources.pace(id, rows);
        }
        let step = self.sources.step(host, id, response, now_ms);
        let mapped = self.sources.mapped(id);
        match step {
            Step::Request(request) => json!({"request": request, "mapped": mapped}).to_string(),
            Step::Continue => json!({"continue": true, "mapped": mapped}).to_string(),
            Step::Done(summary) => json!({"done": summary, "mapped": mapped}).to_string(),
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
            Route::Begin | Route::Complete(_) | Route::Define(_) | Route::Load(_) | Route::Read => {
                "upload".to_owned()
            }
            Route::Observations => "observations".to_owned(),
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
                Err(ClaimError::Busy(other)) => Routed::reply(refused(
                    409,
                    "source.cell_busy",
                    &format!("{other} is syncing; one sync runs per cell at a time"),
                )),
                Err(ClaimError::Unknown) => {
                    Routed::reply(refused(404, "source.unknown", "no such connection"))
                }
            },
            Route::Begin => Routed::reply(self.begin(host, request.body, now_ms)),
            Route::Complete(id) => {
                let actor = actor.clone();
                self.fetch_object((id, &host.scope().key()), now_ms, |upload| {
                    Waiting::Complete { upload, actor }
                })
            }
            Route::Define(id) | Route::Load(id) => {
                match serde_json::from_str::<LoadRequest>(request.body) {
                    Ok(load) => {
                        let actor = actor.clone();
                        let rows = matches!(route, Route::Load(_));
                        self.fetch_object((id, &host.scope().key()), now_ms, |upload| {
                            Waiting::Load {
                                upload,
                                request: load,
                                actor,
                                started_ms: now_ms,
                                rows,
                            }
                        })
                    }
                    Err(e) => Routed::reply(refused(400, "upload.invalid_request", &e.to_string())),
                }
            }
            Route::Read => Routed::reply(self.read(host, request.body, now_ms)),
            Route::Observations => Routed::reply(load_observations(host, request.body)),
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
        let nonce = self.token(now_ms);
        // Seeded synthetic objects keep their names, so their URIs are known in
        // advance; a person's upload gets the id in its key.
        let key = if begin.prefix.is_some() {
            format!("{prefix}/{name}")
        } else {
            format!("{prefix}/{nonce}-{name}")
        };
        let id = upload_id(store, &key, &s3);
        let put_url = s3.presign("PUT", &key, now_ms, URL_SECONDS);
        let uri = s3.uri(&key);
        self.uploads.insert(
            id.clone(),
            Upload {
                store,
                key,
                file_name: begin.file_name,
                content_type: begin.content_type.clone(),
                columns: Vec::new(),
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
        (id, scope_key): (&str, &str),
        now_ms: u64,
        waiting: impl FnOnce(String) -> Waiting,
    ) -> Routed {
        let Some((s3, upload)) = self
            .resolve(id, scope_key)
            .and_then(|u| Some((self.store(u.store)?.clone(), u)))
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
        let scope_key = host.scope().key();
        let found = if let Some(id) = request.get("uploadId").and_then(Value::as_str) {
            self.resolve(id, &scope_key)
                .and_then(|u| Some((self.store(u.store)?, u.key)))
        } else {
            let own = format!("uploads/{scope_key}/");
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
            Waiting::Complete { upload, actor } => {
                self.complete(host, (&upload, &actor), body, (&sha256, size), now_ms)
            }
            Waiting::Load {
                upload,
                request,
                actor,
                started_ms,
                rows,
            } => self.load(
                host,
                (&upload, rows),
                &request,
                &actor,
                body,
                &sha256,
                (started_ms, now_ms),
            ),
        }
    }

    /// UPLOAD-RECORD-1: what the cell read back is kept in the cell before
    /// `:complete` answers, so the upload loads after the host restarts.
    fn complete<E: SqlEngine>(
        &mut self,
        host: &DevHost<E>,
        (id, actor): (&str, &str),
        body: &[u8],
        (sha256, size): (&str, u64),
        now_ms: u64,
    ) -> Response {
        let Some(mut upload) = self.resolve(id, &host.scope().key()) else {
            return refused(404, "upload.not_found", "no such upload");
        };
        let table = std::str::from_utf8(body).ok().map(uploads::parse_csv);
        let csv = matches!(table, Some(Ok(_)));
        upload.completed = Some((sha256.to_owned(), size, csv));
        if let Some(Ok(table)) = &table {
            upload.columns.clone_from(&table.columns);
        }
        let kept = norn_api::sources::KeptUpload {
            upload_id: id.to_owned(),
            store: upload.store,
            uri: upload.uri.clone(),
            sha256: sha256.to_owned(),
            size,
            content_type: upload.content_type.clone(),
            file_name: upload.file_name.clone(),
            csv,
            columns: upload.columns.clone(),
            owner: actor.to_owned(),
            completed_at: norn_api::sources::rfc3339(now_ms),
        };
        // UPLOAD-RECORD-1: never complete without the record. A cell whose
        // definitions predate source.Upload gets the pack first; one that
        // cannot is refused with a typed reason.
        if let Err(reason) = norn_api::sources::keep_upload(host, &kept, now_ms) {
            return refused(
                409,
                "upload.not_recorded",
                &format!("the upload was read but its record was not kept: {reason}"),
            );
        }
        self.uploads.insert(id.to_owned(), upload.clone());
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
        (id, rows): (&str, bool),
        request: &LoadRequest,
        actor: &str,
        body: &[u8],
        sha256: &str,
        (started_ms, now_ms): (u64, u64),
    ) -> Response {
        let upload = match self.resolve_kept(host, id) {
            Ok(Some(upload)) => upload,
            Ok(None) => return refused(404, "upload.not_found", "no such upload"),
            Err(reason) => return refused(500, "host.error", &reason),
        };
        // UPLOAD-DIGEST-1: the object read now is the one the cell profiled at
        // `:complete` (kept in the cell, UPLOAD-RECORD-1); a digest the client
        // sends is only checked against it, never trusted in its place.
        match &upload.completed {
            Some((recorded, _, true))
                if recorded == sha256
                    && request
                        .sha256
                        .as_ref()
                        .is_none_or(|expected| expected == recorded) => {}
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
        if !rows {
            return json_response(
                200,
                &json!({
                    "objectType": generated.object_type, "definitionsVersion": version,
                    "sourceSchema": generated.source_schema, "mapping": generated.mapping,
                }),
            );
        }
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

/// The most rows one load carries; the host maps them in pages.
const MAX_LOAD_ROWS: usize = 500;

/// `POST /v1/observations:load`: one page of a recorded export, captured and
/// mapped through the cell's admitted mappings exactly as a source sync's
/// rows are (OBS-PAGE-1). No caller supplies facts: a row the mapping
/// refuses stays in the inbox as quarantined evidence, with its reasons.
fn load_observations<E: SqlEngine>(host: &DevHost<E>, body: &str) -> Response {
    let answer = |refusal: &Refusal| Response {
        status: refusal.status,
        body: serde_json::to_string(refusal).unwrap_or_default(),
    };
    let recorded = match serde_json::from_str::<Value>(body)
        .map_err(|error| error.to_string())
        .and_then(|document| crate::recorded::Recorded::parse(&document))
    {
        Ok(recorded) => recorded,
        Err(error) => return answer(&Refusal::invalid(error)),
    };
    if recorded.rows.is_empty() || recorded.rows.len() > MAX_LOAD_ROWS {
        return answer(&Refusal::invalid(format!(
            "a load carries 1 to {MAX_LOAD_ROWS} rows; send a longer export in slices"
        )));
    }
    // A mapping this cell has not admitted refuses before anything is
    // captured, naming the mapping (not a host failure).
    match host.installed_artifact(&recorded.mapping) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return answer(&Refusal::invalid(format!(
                "the mapping {} is not admitted in this cell",
                recorded.mapping
            )));
        }
        Err(refusal) => return answer(&refusal),
    }
    let rows = match recorded.observations() {
        Ok(rows) => rows,
        Err(error) => return answer(&Refusal::invalid(error)),
    };
    let ingested = match host.ingest(&rows) {
        Ok(ingested) => ingested,
        Err(refusal) => return answer(&refusal),
    };
    let (mut landed, mut already, mut quarantined) = (0_usize, 0_usize, 0_usize);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for row in &ingested {
        match &row.outcome {
            norn_cell::MappingOutcome::Landed => landed = landed.saturating_add(1),
            norn_cell::MappingOutcome::AlreadyLanded => already = already.saturating_add(1),
            norn_cell::MappingOutcome::Quarantined(codes) => {
                quarantined = quarantined.saturating_add(1);
                for code in codes {
                    let n = reasons.entry(code.clone()).or_insert(0);
                    *n = n.saturating_add(1);
                }
            }
        }
    }
    json_response(
        200,
        &json!({
            "source": recorded.source,
            "mapping": recorded.mapping,
            "rows": ingested.len(),
            "landed": landed,
            "alreadyLanded": already,
            "quarantined": quarantined,
            "quarantineReasons": reasons,
        }),
    )
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
            route_of("/v1/observations:load"),
            Some(Route::Observations)
        ));
        assert!(matches!(
            route_of("/v1/uploads/abc:complete"),
            Some(Route::Complete("abc"))
        ));
        assert!(matches!(
            route_of("/v1/uploads/abc:load"),
            Some(Route::Load("abc"))
        ));
        assert!(matches!(
            route_of("/v1/uploads/abc:define"),
            Some(Route::Define("abc"))
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

    fn connect(secret: &str) -> Connect {
        let vars = json!({
            "NORN_S3_ENDPOINT": "https://storage.example.test",
            "NORN_S3_BUCKET": "uploads-bucket",
            "NORN_S3_DOCUMENTS_BUCKET": "documents-bucket",
            "NORN_SECRET_S3_ACCESS_KEY_ID": "key-id",
            "NORN_SECRET_S3_SECRET_ACCESS_KEY": secret,
            "NORN_SECRET_S3_DOCUMENTS_ACCESS_KEY_ID": "documents-key-id",
            "NORN_SECRET_S3_DOCUMENTS_SECRET_ACCESS_KEY": secret,
        });
        Connect::new("[]", &vars.to_string()).expect("configuration")
    }

    /// UPLOAD-ID-1: a cell that never saw an upload (a restart) resolves its
    /// id from the seal alone, and only for the scope whose prefix it names.
    #[test]
    fn a_sealed_upload_id_survives_a_restart_and_stays_in_its_scope() {
        let issuing = connect("secret-one");
        let s3 = issuing
            .store(Store::Uploads)
            .expect("uploads store")
            .clone();
        let key = "uploads/org_a/prod/0123456789abcdef0123-orders.csv";
        let id = upload_id(Store::Uploads, key, &s3);

        let restarted = connect("secret-one");
        let upload = restarted.resolve(&id, "org_a/prod").expect("resolves");
        assert_eq!(upload.key, key);
        assert_eq!(upload.file_name, "orders.csv");
        assert_eq!(upload.uri, "s3://uploads-bucket/".to_owned() + key);
        assert!(upload.completed.is_none());

        // Another scope, even one whose key starts the same, never sees it.
        assert!(restarted.resolve(&id, "org_a/pro").is_none());
        assert!(restarted.resolve(&id, "org_b/prod").is_none());
        // A different secret, a changed seal or a changed key does not check.
        assert!(connect("secret-two").resolve(&id, "org_a/prod").is_none());
        let (head, seal) = id.split_once('.').expect("sealed");
        let flipped = if seal.starts_with('0') { '1' } else { '0' };
        let tampered: String = format!("{head}.")
            .chars()
            .chain(std::iter::once(flipped))
            .chain(seal.chars().skip(1))
            .collect();
        assert!(restarted.resolve(&tampered, "org_a/prod").is_none());
        let other = upload_id(Store::Uploads, "uploads/org_b/prod/x.csv", &s3);
        let (_, other_seal) = other.split_once('.').expect("sealed");
        assert!(
            restarted
                .resolve(&format!("{head}.{other_seal}"), "org_a/prod")
                .is_none()
        );
        // The store tag is sealed too: the same key in the documents store.
        let documents = format!("d{}", id.strip_prefix('u').expect("an uploads id"));
        assert!(restarted.resolve(&documents, "org_a/prod").is_none());
        for garbage in ["", ".", "u.", "uzz.00", "x00.00", "u0.00"] {
            assert!(
                restarted.resolve(garbage, "org_a/prod").is_none(),
                "{garbage}"
            );
        }
    }

    #[test]
    fn synthetic_objects_resolve_in_every_scope() {
        let cell = connect("secret-one");
        let s3 = cell
            .store(Store::Documents)
            .expect("documents store")
            .clone();
        let id = upload_id(Store::Documents, "synthetic/scans/receipt-01.pdf", &s3);
        let upload = cell.resolve(&id, "org_z/dev").expect("resolves");
        assert_eq!(upload.file_name, "receipt-01.pdf");
        assert_eq!(
            upload.uri,
            "s3://documents-bucket/synthetic/scans/receipt-01.pdf"
        );
    }
}
