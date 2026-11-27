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
//!   `PUT` the browser sends the bytes to, always under `uploads/<scope>/`
//!   (a `prefix` names a folder there, UPLOAD-SCOPE-1).
//! - `/v1/uploads/{id}:complete`: the cell reads the object, records its URI
//!   and SHA-256 and profiles it.
//! - `/v1/uploads/{id}:define`: [`LoadRequest`]; installs the generated
//!   definitions only, for a loader that writes the rows itself.
//! - `/v1/uploads/{id}:load`: [`LoadRequest`]; installs and loads.
//! - `/v1/uploads:read`: `{"uri"}` or `{"uploadId"}` → a short-lived signed
//!   `GET` of an object under a prefix this cell writes.
//! - `/v1/observations:load`: a slice of a recorded export
//!   ([`crate::recorded`], at most 100 rows and 8 page commits) through the
//!   admitted mappings, in OBS-PAGE-1 pages → counts of the rows' outcomes;
//!   a larger slice is refused 413 `observations.too_large`.
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
//! (or a read-only host fixture, `fixtures/`).
//! UPLOAD-SCOPE-1: every key a tenant creates is under its own
//! `uploads/<scope>/`; `fixtures/` holds the operator's shared fixtures,
//! which every scope may read and none may write (no `PUT` is ever signed
//! there, under these rules or the ones before them). A bare `synthetic/`
//! key is a legacy tenant write (the old rules signed `PUT`s there for any
//! scope), so it is read by no scope, by URI or by sealed id.
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
use norn_api::scope::{CellScope, ScopeQuery};
use norn_api::sources::{
    ClaimError, HttpRequest, Sources, Step, Trigger, UploadRun, report_upload,
};
use norn_api::uploads::{self, LoadRequest, S3Config, Store, Table, URL_SECONDS};
use norn_cell::engine::SqlEngine;
use serde_json::{Value, json};

use crate::principal::{PrincipalResolver, RequestScope};
use crate::server::{self, Request, Response};

mod held;
use held::{Held, Upload};

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
    uploads: Held,
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
    crate::incident::respond(refusal)
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

/// Whether `key` lies under the prefix the cell serving `scope_key` writes.
fn owns(key: &str, scope_key: &str) -> bool {
    key.strip_prefix("uploads/")
        .and_then(|rest| rest.strip_prefix(scope_key))
        .is_some_and(|rest| rest.starts_with('/'))
        && !key.contains("..")
}

/// The operator-only namespace of shared fixtures (UPLOAD-SCOPE-1): no
/// tenant has ever been able to sign a write under it.
const FIXTURES: &str = "fixtures/";

/// Whether `key` is one of the operator's shared fixtures (UPLOAD-SCOPE-1):
/// readable from every scope, writable from none. A legacy bare
/// `synthetic/` key is not one: any tenant could write there.
fn fixture(key: &str) -> bool {
    key.starts_with(FIXTURES) && !key.contains("..")
}

/// Whether the cell serving `scope_key` may read `key`: its own, or a fixture.
fn readable(key: &str, scope_key: &str) -> bool {
    owns(key, scope_key) || fixture(key)
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

/// Whether `path` is one of the source and upload routes [`Connect::route`]
/// serves (TENANT-ISOLATION-1: the membership gate answers an outsider there
/// as these routes do).
pub(crate) fn serves(path: &str) -> bool {
    route_of(path).is_some()
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

/// OLAP-01: names in the connection secret namespace that deployment-wide
/// secrets used to have, or might be given: the object store's keys (now
/// `NORN_S3_*`), the analytics signing key (now `NORN_OLAP_SIGNING_KEY`) and
/// the retired analytics token. A connector names its secret freely and the
/// pull host sends it to a source endpoint, so a deployment secret in this
/// namespace would be a tenant's to read.
const RESERVED_SECRETS: &[&str] = &[
    "NORN_SECRET_S3_",
    "NORN_SECRET_OLAP_",
    "NORN_SECRET_DUCKDB_",
];

/// OLAP-01: the secrets a connector's auth may resolve (`secretRef`, through
/// [`norn_api::sources::secret_env`]): the `NORN_SECRET_*` variables, which
/// hold connection secrets only. Storage keys and the analytics signing key
/// live outside that namespace and are never in this map, whatever name a
/// connector asks for.
///
/// # Errors
/// A variable under a reserved deployment-secret name: the deployment still
/// uses an old name, and the cell refuses rather than expose it.
fn connection_secrets(vars: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    if let Some(name) = vars.keys().find(|name| {
        RESERVED_SECRETS
            .iter()
            .any(|prefix| name.starts_with(prefix))
    }) {
        return Err(format!(
            "{name} is a deployment secret in the connection secret namespace, where a \
             connector could read it; use NORN_S3_* for storage keys and \
             NORN_OLAP_SIGNING_KEY for the analytics signing key"
        ));
    }
    Ok(vars
        .iter()
        .filter(|(name, _)| name.starts_with("NORN_SECRET_"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect())
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
    /// The connections in `config` (`NORN_SOURCES`) that `scope` owns
    /// (SOURCE-SCOPE-1) and the server variables `vars` (a JSON object of the
    /// `NORN_SECRET_*`, `NORN_S3_*` and `NORN_SOURCE_ALLOWED_HOSTS` values).
    ///
    /// # Errors
    /// Malformed configuration, or a deployment secret under a connection
    /// secret's name (`NORN_SECRET_S3_*`, `NORN_SECRET_OLAP_*`, `NORN_SECRET_DUCKDB_*`).
    pub fn new(scope: &CellScope, config: &str, vars: &str) -> Result<Self, String> {
        let vars: BTreeMap<String, String> =
            serde_json::from_str(vars).map_err(|e| format!("server variables: {e}"))?;
        let secrets = connection_secrets(&vars)?;
        let allowed = vars.get(norn_api::sources::ALLOWED_HOSTS_VAR);
        let allowed = allowed.map_or("", String::as_str);
        Ok(Self {
            sources: Sources::with_allowed_hosts(scope, config, &secrets, allowed)?,
            uploads_store: S3Config::from_vars(&vars, Store::Uploads),
            documents_store: S3Config::from_vars(&vars, Store::Documents),
            uploads: Held::default(),
            waiting: BTreeMap::new(),
            issued: 0,
        })
    }

    /// Scoped `(connection_id -> mapping_id)` bindings from `NORN_SOURCES` for QUERY-FRESH-1.
    #[must_use]
    pub fn configured_mappings(&self) -> BTreeMap<String, String> {
        self.sources
            .connections()
            .iter()
            .filter(|conn| !conn.mapping.is_empty())
            .map(|conn| (conn.id.clone(), conn.mapping.clone()))
            .collect()
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
        readable(&upload.key, scope_key).then_some(upload)
    }

    /// Whether `id` is an upload id this host issued (sealed, UPLOAD-ID-1),
    /// readable from the cell serving `scope_key`, for the object at exactly
    /// `uri`: what a loader checks a kept record against before it signs
    /// anything (S5), since a record names a URI but only the seal is the
    /// host's.
    #[must_use]
    pub fn issued_upload(&self, id: &str, scope_key: &str, uri: &str) -> bool {
        self.resolve(id, scope_key)
            .is_some_and(|upload| upload.uri == uri)
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

    /// MANIFEST-LAND-1: a signed `PUT` of the uploads-bucket object `key` the
    /// cell lands a dataset file at, valid for `seconds`, and its URI.
    #[must_use]
    pub fn signed_put(&self, key: &str, seconds: u32, now_ms: u64) -> Option<(String, String)> {
        let s3 = self.store(Store::Uploads)?;
        (!key.is_empty() && !key.contains(".."))
            .then(|| (s3.presign("PUT", key, now_ms, seconds), s3.uri(key)))
    }

    /// SYNC-ONE-1: the next connection due on its schedule, claimed.
    pub fn due(&mut self, now_ms: u64) -> Option<String> {
        self.sources.due(now_ms)
    }

    /// TIMER-ALARM-1: read the schedule back from the cell's durable sync
    /// records once per host instance (see [`norn_api::sources::Sources::restore_schedule`]).
    ///
    /// # Errors
    /// The cell could not be read.
    pub fn restore_schedule<E: SqlEngine>(&mut self, host: &DevHost<E>) -> Result<(), String> {
        self.sources.restore_schedule(host)
    }

    /// TIMER-ALARM-1: when the next scheduled sync falls due, at or after `now_ms`.
    #[must_use]
    pub fn next_sync_due(&self, now_ms: u64) -> Option<u64> {
        self.sources.next_due(now_ms)
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
            Route::Begin => Routed::reply(self.begin(&host.scope().key(), request.body, now_ms)),
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
            Route::Read => Routed::reply(self.read(&host.scope().key(), request.body, now_ms)),
            Route::Observations => {
                Routed::reply(load_observations(host, request.body, &HOSTED_LOAD))
            }
        })
    }

    fn begin(&mut self, scope_key: &str, body: &str, now_ms: u64) -> Response {
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
        // UPLOAD-SCOPE-1: a caller's prefix is a folder of its own scope's
        // uploads, never a key outside it (never a bare `synthetic/` key, and
        // never `fixtures/`, the operator's).
        let prefix = match begin.prefix.as_deref() {
            None => format!("uploads/{scope_key}"),
            Some(p)
                if p.starts_with("synthetic/")
                    && p.len() <= 100
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
                    && !p.contains("//") =>
            {
                format!("uploads/{scope_key}/{}", p.trim_end_matches('/'))
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
        // Named synthetic objects keep their names (in this scope), so their
        // URIs are known in advance; a person's upload gets the id in its key.
        let key = if begin.prefix.is_some() {
            format!("{prefix}/{name}")
        } else {
            format!("{prefix}/{nonce}-{name}")
        };
        let id = upload_id(store, &key, &s3);
        let put_url = s3.presign_put(&key, begin.size, now_ms, URL_SECONDS);
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
    /// URI under the prefix this cell writes (`uploads/<scope>/`) or of a
    /// host fixture (`fixtures/`).
    fn read(&self, scope_key: &str, body: &str, now_ms: u64) -> Response {
        let not_found = || refused(404, "upload.not_found", "no such upload");
        let request: Value = serde_json::from_str(body).unwrap_or_default();
        let found = if let Some(id) = request.get("uploadId").and_then(Value::as_str) {
            self.resolve(id, scope_key)
                .and_then(|u| Some((self.store(u.store)?, u.key)))
        } else {
            let uri = request
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            [Store::Uploads, Store::Documents]
                .into_iter()
                .filter_map(|store| self.store(store))
                .find_map(|s3| {
                    let key = uri.strip_prefix(&format!("s3://{}/", s3.bucket))?;
                    readable(key, scope_key).then(|| (s3, key.to_owned()))
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

/// `POST /v1/observations:load` on a host with no source connections (the
/// dev host): decided by the cell's policy as `Operation::SyncSource` first,
/// exactly as [`Connect::route`] decides it (CONNECT-AUTH-1), then loaded as
/// on a hosted cell. `None` for any other path.
pub fn observations<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    request: &Request<'_>,
) -> Option<Response> {
    if !matches!(route_of(request.path), Some(Route::Observations)) {
        return None;
    }
    let _scope = RequestScope::new(principals);
    let actor = principals.resolve(request.authorization, request.now);
    let allowed = request.method == "POST"
        && authorize(
            host,
            &scope_of(request.query),
            actor.as_deref(),
            Operation::SyncSource("observations".to_owned()),
        )
        .is_ok();
    Some(if allowed {
        load_observations(host, request.body, &DEV_LOAD)
    } else {
        server::outside::<E>(request)
    })
}

/// What one load may carry (INGEST-BATCH-1): its rows, and, on a host that
/// runs a load inside one bounded call, a work budget charged for every row
/// captured and every commit the cell actually attempts. A load that reaches
/// the budget stops at a page boundary and answers `next`, the row the
/// caller resumes from.
struct LoadBound {
    /// The most rows one load carries.
    rows: usize,
    /// The work one load may take; `None` when the host releases the cell
    /// between commits and serves reads in between (INGEST-YIELD-1), so a
    /// load's length never holds a read.
    budget: Option<norn_api::host::WorkBudget>,
}

/// A hosted cell runs a load inside one cell call: a commit costs about
/// 0.8 s and a row about 0.1 s there, so a load stops at the work of 100
/// rows and 8 commits (about 16 s), however the cell splits its pages.
const HOSTED_LOAD: LoadBound = LoadBound {
    rows: 100,
    budget: Some(norn_api::host::WorkBudget {
        row_ms: 100,
        commit_ms: 800,
        total_ms: 100 * 100 + 8 * 800,
    }),
};

/// The dev host commits a page at a time and serves waiting reads between
/// commits, so one load carries up to one OBS-PAGE-1 page bound of rows.
const DEV_LOAD: LoadBound = LoadBound {
    rows: norn_cell::mapping_page::MAX_PAGE_RECORDS,
    budget: None,
};

/// `POST /v1/observations:load`: rows of a recorded export, captured and
/// mapped through the cell's admitted mappings exactly as a source sync's
/// rows are (OBS-PAGE-1). No caller supplies facts: a row the mapping
/// refuses stays in the inbox as quarantined evidence, with its reasons,
/// and the answer explains each one (MAP-EXPLAIN-1): the record, its
/// dataset, the code, the mapping rule and, for a nonconforming field, the
/// field, its value and the declared type.
fn load_observations<E: SqlEngine>(host: &DevHost<E>, body: &str, bound: &LoadBound) -> Response {
    let answer = |refusal: &Refusal| crate::incident::respond(refusal.clone());
    let recorded = match serde_json::from_str::<Value>(body)
        .map_err(|error| error.to_string())
        .and_then(|document| crate::recorded::Recorded::parse(&document))
    {
        Ok(recorded) => recorded,
        Err(error) => return answer(&Refusal::invalid(error)),
    };
    if recorded.rows.is_empty() {
        return answer(&Refusal::invalid("a load carries at least one row"));
    }
    // UPLOAD-RECORD-2 (S5): the cell's own reports (connections, sync runs,
    // upload records) land only from the cell itself, never from a caller's
    // export: a forged `source.Upload` would name an object to sign.
    if recorded.source == norn_api::sources::REPORT_SOURCE
        || recorded.mapping == norn_api::sources::REPORT_MAPPING
        || recorded.instance == norn_api::sources::CELL_INSTANCE
    {
        return refused(
            403,
            norn_api::refusal::codes::AUTHZ_DENIED,
            "the cell's own records are written by the cell, never loaded",
        );
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
    if rows.len() > bound.rows {
        return refused(
            413,
            "observations.too_large",
            &format!(
                "a load carries at most {} rows (this one: {}); send it in smaller slices",
                bound.rows,
                rows.len()
            ),
        );
    }
    let report = match host.ingest_within(&rows, bound.budget) {
        Ok(report) => report,
        Err(refusal) => return answer(&refusal),
    };
    let ingested = &report.outcomes;
    let (mut landed, mut already, mut quarantined) = (0_usize, 0_usize, 0_usize);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut explained = Vec::new();
    for (row, (raw, input)) in ingested.iter().zip(&rows) {
        match &row.outcome {
            norn_cell::MappingOutcome::Landed => landed = landed.saturating_add(1),
            norn_cell::MappingOutcome::AlreadyLanded => already = already.saturating_add(1),
            norn_cell::MappingOutcome::Quarantined(why) => {
                quarantined = quarantined.saturating_add(1);
                for code in row.outcome.codes() {
                    let n = reasons.entry(code).or_insert(0);
                    *n = n.saturating_add(1);
                }
                explained.extend(why.iter().map(|reason| {
                    json!({
                        "record": raw.provenance.source_id,
                        "dataset": input.dataset,
                        "code": reason.code,
                        "map": reason.map,
                        "field": reason.field,
                        "value": reason.value,
                        "expected": reason.expected,
                        "reason": reason.reason,
                    })
                }));
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
            "quarantine": explained,
            "pages": report.pages,
            "commits": report.commits,
            // Resume with `snapshot[next..]` when the work budget stopped
            // the load; null when every row is reported.
            "next": report.next,
        }),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "route parsing scenarios")]
mod tests {
    use super::*;
    use crate::seed::demo_scope;

    #[test]
    fn only_the_declared_routes_are_this_modules() {
        assert!(matches!(
            route_of("/v1/sources/erpnext.rest:sync"),
            Some(Route::Sync("erpnext.rest"))
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
            "NORN_S3_ACCESS_KEY_ID": "key-id",
            "NORN_S3_SECRET_ACCESS_KEY": secret,
            "NORN_S3_DOCUMENTS_ACCESS_KEY_ID": "documents-key-id",
            "NORN_S3_DOCUMENTS_SECRET_ACCESS_KEY": secret,
        });
        Connect::new(&demo_scope(), "[]", &vars.to_string())
            .unwrap_or_else(|error| unreachable!("the configuration is well formed: {error}"))
    }

    /// OLAP-01: no secretRef a connector can write reaches a deployment
    /// secret: storage keys and the analytics signing key are outside the
    /// map connector auth resolves from, under any name, and a deployment
    /// that still keeps one under a connection secret's name is refused.
    #[test]
    fn deployment_secrets_never_reach_connector_auth() {
        const DEPLOYMENT: [&str; 5] = [
            "s3-uploads-secret",
            "s3-documents-secret",
            "s3-uploads-id",
            "s3-documents-id",
            "k1:olap-signing-seed",
        ];
        let vars: BTreeMap<String, String> = [
            ("NORN_S3_ENDPOINT", "https://storage.example.test"),
            ("NORN_S3_BUCKET", "uploads-bucket"),
            ("NORN_S3_SECRET_ACCESS_KEY", DEPLOYMENT[0]),
            ("NORN_S3_DOCUMENTS_SECRET_ACCESS_KEY", DEPLOYMENT[1]),
            ("NORN_S3_ACCESS_KEY_ID", DEPLOYMENT[2]),
            ("NORN_S3_DOCUMENTS_ACCESS_KEY_ID", DEPLOYMENT[3]),
            ("NORN_OLAP_SIGNING_KEY", DEPLOYMENT[4]),
            ("NORN_SECRET_ERPNEXT_REST_TOKEN", "connection-token"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
        let secrets = connection_secrets(&vars).expect("configured");
        assert_eq!(
            secrets.values().collect::<Vec<_>>(),
            ["connection-token"],
            "only the connection's own secret"
        );
        // Every name a secretRef resolves to, whatever the connector writes,
        // misses every deployment secret.
        for reference in [
            "s3_secret_access_key",
            "S3_SECRET_ACCESS_KEY",
            "../NORN_S3_SECRET_ACCESS_KEY",
            "olap_signing_key",
            "OLAP-SIGNING-KEY",
            "",
        ] {
            let resolved = secrets.get(&norn_api::sources::secret_env(reference));
            assert!(
                resolved.is_none_or(|value| !DEPLOYMENT.contains(&value.as_str())),
                "{reference}"
            );
        }
        // The storage still gets its keys from their own names.
        assert!(S3Config::from_vars(&vars, Store::Uploads).is_some());
        // A deployment that kept an old name is refused, never served.
        for old in [
            "NORN_SECRET_S3_SECRET_ACCESS_KEY",
            "NORN_SECRET_S3_DOCUMENTS_ACCESS_KEY_ID",
            "NORN_SECRET_OLAP_SIGNING_KEY",
            "NORN_SECRET_DUCKDB_TOKEN",
        ] {
            let mut stale = vars.clone();
            stale.insert(old.to_owned(), "x".to_owned());
            assert!(
                connection_secrets(&stale).is_err_and(|e| e.contains(old)),
                "{old}"
            );
            let refused = serde_json::to_string(&stale)
                .is_ok_and(|vars| Connect::new(&demo_scope(), "[]", &vars).is_err());
            assert!(refused, "{old}");
        }
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

    /// The answer's JSON document.
    fn document(response: &Response) -> Value {
        serde_json::from_str(&response.body).expect("a JSON answer")
    }

    /// UPLOAD-SCOPE-1 (S4): every key a tenant begins is under its own
    /// `uploads/<scope>/`, a `synthetic/` prefix included, so no tenant
    /// signs a `PUT` outside its scope or over a shared fixture, and no
    /// tenant gets a signed read of another's object by id or by URI.
    #[test]
    fn a_tenant_never_signs_another_scopes_key_or_writes_a_fixture() {
        let mut cell = connect("secret-one");
        let body = |prefix: Option<&str>| {
            json!({"fileName": "rows.csv", "contentType": "text/csv", "size": 3, "prefix": prefix})
                .to_string()
        };
        let begun = |cell: &mut Connect, scope: &str, prefix: Option<&str>| {
            let response = cell.begin(scope, &body(prefix), 1_000);
            assert_eq!(response.status, 200, "{}", response.body);
            let document = document(&response);
            (
                document["uploadId"].as_str().expect("an id").to_owned(),
                document["uri"].as_str().expect("a uri").to_owned(),
                document["putUrl"].as_str().expect("a put url").to_owned(),
            )
        };
        for prefix in [None, Some("synthetic/scans"), Some("synthetic/")] {
            let (_, uri, put) = begun(&mut cell, "org_a/prod", prefix);
            assert!(
                uri.starts_with("s3://uploads-bucket/uploads/org_a/prod/"),
                "{prefix:?}: {uri}"
            );
            assert!(put.contains("/uploads/org_a/prod/"), "{prefix:?}: {put}");
            assert!(!uri.starts_with("s3://uploads-bucket/synthetic/"), "{uri}");
        }
        // A prefix outside `synthetic/…` (another scope's) is refused outright.
        for prefix in [
            "uploads/org_b/prod",
            "../uploads/org_b/prod",
            "synthetic//x",
        ] {
            let response = cell.begin("org_a/prod", &body(Some(prefix)), 1_000);
            assert_eq!(response.status, 400, "{prefix}");
        }
        // B's object, by id or by URI, is not A's to read; B reads it.
        let (b_id, b_uri, _) = begun(&mut cell, "org_b/prod", Some("synthetic/scans"));
        for request in [json!({"uploadId": b_id}), json!({"uri": b_uri})] {
            let body = request.to_string();
            assert_eq!(cell.read("org_a/prod", &body, 1_000).status, 404, "{body}");
            assert_eq!(cell.read("org_b/prod", &body, 1_000).status, 200, "{body}");
        }
        assert!(cell.resolve(&b_id, "org_a/prod").is_none());
        // What B wrote under its `synthetic/scans` is B's alone: a shared
        // fixture is a different, read-only object in the operator's namespace.
        assert_eq!(
            cell.read(
                "org_a/prod",
                &json!({"uri": "s3://uploads-bucket/fixtures/scans/rows.csv"}).to_string(),
                1_000,
            )
            .status,
            200,
            "a fixture stays readable"
        );
        assert_ne!(b_uri, "s3://uploads-bucket/synthetic/scans/rows.csv");
        assert!(
            !b_uri.starts_with("s3://uploads-bucket/fixtures/"),
            "{b_uri}"
        );
        // No prefix names the operator's namespace.
        for prefix in ["fixtures/scans", "synthetic/../fixtures/scans"] {
            assert_eq!(
                cell.begin("org_a/prod", &body(Some(prefix)), 1_000).status,
                400,
                "{prefix}"
            );
        }
    }

    /// S5: a loader signs a kept record only when its upload id is one this
    /// host sealed, readable from the scope, for exactly the record's URI.
    #[test]
    fn only_an_issued_upload_for_its_scope_and_uri_is_issued() {
        let mut cell = connect("secret-one");
        let response = cell.begin(
            "org_a/prod",
            &json!({"fileName": "rows.csv", "contentType": "text/csv", "size": 3}).to_string(),
            1_000,
        );
        let begun = document(&response);
        let id = begun["uploadId"].as_str().expect("an id");
        let uri = begun["uri"].as_str().expect("a uri");
        assert!(cell.issued_upload(id, "org_a/prod", uri));
        // After a restart the seal alone still answers.
        assert!(connect("secret-one").issued_upload(id, "org_a/prod", uri));
        assert!(!cell.issued_upload(id, "org_b/prod", uri));
        assert!(!cell.issued_upload(
            id,
            "org_a/prod",
            "s3://uploads-bucket/uploads/org_b/prod/x.csv"
        ));
        assert!(!connect("secret-two").issued_upload(id, "org_a/prod", uri));
        let s3 = cell.store(Store::Uploads).expect("uploads store").clone();
        let other = upload_id(Store::Uploads, "uploads/org_b/prod/x.csv", &s3);
        assert!(!cell.issued_upload(
            &other,
            "org_a/prod",
            "s3://uploads-bucket/uploads/org_b/prod/x.csv"
        ));
        assert!(!cell.issued_upload("u-forged", "org_a/prod", uri));
    }

    #[test]
    fn operator_fixtures_resolve_in_every_scope() {
        let cell = connect("secret-one");
        let s3 = cell
            .store(Store::Documents)
            .expect("documents store")
            .clone();
        let id = upload_id(Store::Documents, "fixtures/scans/receipt-01.pdf", &s3);
        for scope in ["org_z/dev", "org_a/prod"] {
            let upload = cell.resolve(&id, scope).expect("resolves");
            assert_eq!(upload.file_name, "receipt-01.pdf");
            assert_eq!(
                upload.uri,
                "s3://documents-bucket/fixtures/scans/receipt-01.pdf"
            );
        }
    }

    /// S9: the scan fixtures the repository ships name their documents in
    /// the operator-only `fixtures/scans/` namespace, which every scope
    /// resolves, and never a legacy bare `synthetic/` key, which none does.
    #[test]
    fn shipped_scan_fixtures_name_operator_fixture_keys() {
        let cell = connect("secret-one");
        let s3 = cell
            .store(Store::Documents)
            .expect("documents store")
            .clone();
        let mut keys = std::collections::BTreeSet::new();
        for text in [
            include_str!("../../../fixtures/documents/scans.snapshot.json"),
            include_str!("../../../fixtures/connectors/scans/extractions.page.json"),
        ] {
            for uri in text.split('"').filter(|part| part.starts_with("s3://")) {
                let key = uri
                    .strip_prefix("s3://documents-bucket/")
                    .unwrap_or_else(|| panic!("{uri} is in the documents store"));
                keys.insert(key.to_owned());
            }
        }
        assert!(keys.len() >= 10, "{keys:?}");
        for key in &keys {
            assert!(key.starts_with("fixtures/scans/") && fixture(key), "{key}");
            let id = upload_id(Store::Documents, key, &s3);
            for scope in ["org_a/prod", "org_z/dev"] {
                assert!(cell.resolve(&id, scope).is_some(), "{scope}: {key}");
            }
        }
    }

    /// S4: a bare `synthetic/` key is what the old rules let any tenant
    /// sign a `PUT` for, so it is no shared fixture: no scope (the writer's
    /// included) reads it, by sealed id or by URI, in either store.
    #[test]
    fn a_legacy_bare_synthetic_key_is_read_by_no_scope() {
        let cell = connect("secret-one");
        for (store, bucket) in [
            (Store::Uploads, "uploads-bucket"),
            (Store::Documents, "documents-bucket"),
        ] {
            let s3 = cell.store(store).expect("store").clone();
            // The id the old `:begin` sealed for org_b's `synthetic/scans` prefix.
            let key = "synthetic/scans/rows.csv";
            let id = upload_id(store, key, &s3);
            let uri = format!("s3://{bucket}/{key}");
            for scope in ["org_a/prod", "org_b/prod", "org_z/dev"] {
                assert!(cell.resolve(&id, scope).is_none(), "{scope}: {key}");
                assert!(!cell.issued_upload(&id, scope, &uri), "{scope}: {key}");
                for request in [json!({"uploadId": id}), json!({"uri": uri})] {
                    let body = request.to_string();
                    let read = cell.read(scope, &body, 1_000);
                    assert_eq!(read.status, 404, "{scope}: {body}");
                    assert!(!read.body.contains("X-Amz-Signature"), "{scope}: {body}");
                }
            }
        }
    }
}
