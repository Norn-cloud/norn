//! One scope's cell on this host: vacant until it is provisioned, then
//! served. Portable Rust; the Durable Object binding is thin glue over it and
//! the native tests drive the same code.
//!
//! - A **vacant** scope holds no store and writes nothing. A verified member
//!   of its organization is told `cell.not_provisioned` (404); anyone else
//!   gets exactly what a provisioned cell answers a caller nobody verified
//!   ([`server::outside`]), so an outsider cannot tell the two apart.
//! - `POST /v1/cell:provision` (`norn_api::provision`, HOST-PROVISION-1)
//!   creates the cell in one engine transaction when the host grant table's
//!   provisioner role asks ([`crate::policy::Provisioners`]). On a cell that
//!   already exists the same submission replays and any other refuses.
//! - A restarted host reopens the cell at its active definitions
//!   (HOST-REOPEN-1): the model is rebuilt from the retained set and nothing
//!   is written.
//! - Governed actions (HOST-ACTION-2, #1252) are decided by the scope's own
//!   policy ([`ActionPolicy`]), compiled from the active definitions and
//!   recompiled before the first request after they change. A tenant cell's
//!   receipt handles are keyed by the stage's lineage key; without one it
//!   serves no action.

use std::sync::Arc;

use http::StatusCode;
use norn_api::authz::Operation;
use norn_api::definition_routes::{self, DefinitionKinds};
use norn_api::host::DevHost;
use norn_api::provision::{self, PROVISION_PATH, TrustedBase};
use norn_api::question_routes::AdHocQuestions;
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::CellStore;
use norn_cell::engine::SqlEngine;

use norn_api::authz::ReadCapability;
use norn_api::question_routes::{QuestionRuntime, QuestionSnapshot};
use norn_cell::ObjectRecord;
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::MetricDefinition;

use crate::actions::ActionPolicy;
use crate::analytics::DemoQuestions;
use crate::policy::{ActionSlot, CallerSlot, GrantTable, Provisioners, ReadPolicy};
use crate::principal::{PrincipalResolver, RequestScope};
use crate::questions::DerivedQuestions;
use crate::seed::SeedError;
use crate::server::{self, Request, Response};

/// Opens this scope's storage engine: once when the host starts, and again
/// for a provisioning attempt (a refused attempt consumes its engine).
pub type OpenEngine<E> = Box<dyn Fn() -> Result<E, String>>;

/// The retained-definition kinds every cell of this host admits.
#[must_use]
pub fn host_kinds(table: &'static GrantTable) -> DefinitionKinds {
    // Dashboards are built in; agents wait for lane CANVAS's checker.
    DefinitionKinds::default()
        // Sign-offs verify against the checker's words, as Studio shows
        // them; D59: a flow is installed by signing the server's words for it.
        .with_restater(definition_routes::checker_restatement)
        // A binding runs as a role of the cell's declared grant table.
        .with_roles(table.entries.iter().map(|(role, _)| *role))
        // A flow's performs may name a registered built-in.
        .with_builtins(norn_cell::builtin_registered)
}

/// The receipt-disclosure authority of a cell that publishes no questions:
/// the handle key that keys its receipt coordinates (D53), and nothing else.
struct ReceiptHandles {
    key: Vec<u8>,
    catalog: EffectiveCatalog,
}

impl<E: SqlEngine> QuestionRuntime<E> for ReceiptHandles {
    fn plan(&self, _question: &str) -> Result<QuestionPlan, Refusal> {
        Err(norn_api::authz::read_denied())
    }
    fn snapshot(
        &self,
        _store: &CellStore<E>,
        _plan: &QuestionPlan,
        _actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        Err(norn_api::authz::read_denied())
    }
    fn access(
        &self,
        _scope: &CellScope,
        _actor: &str,
        _question: &str,
        _capability: ReadCapability,
        _row: Option<&ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        Err(norn_api::authz::read_denied())
    }
    fn min_disclosed_bucket(&self) -> usize {
        norn_api::question_routes::DEFAULT_MIN_DISCLOSED_BUCKET
    }
    fn handle_key(&self) -> &[u8] {
        &self.key
    }
}

impl<E: SqlEngine> AdHocQuestions<E> for ReceiptHandles {
    fn definitions(&self) -> &[MetricDefinition] {
        &[]
    }
    fn catalog(&self) -> &EffectiveCatalog {
        &self.catalog
    }
    fn max_rows(&self) -> u32 {
        0
    }
    fn max_buckets(&self) -> usize {
        0
    }
}

/// The smallest lineage key a cell's receipt handles are keyed with, in bytes.
pub const MIN_HANDLE_KEY_BYTES: usize = 32;

/// One opened cell and what its requests are served with.
struct Serving<E: SqlEngine> {
    host: DevHost<E>,
    /// The cell's admitted questions, when it publishes any.
    questions: Option<DemoQuestions>,
    /// The cell's collections, for `GET /v1/access`.
    object_types: Vec<String>,
    /// Where the cell's action policy is installed.
    actions: ActionSlot,
    /// The definitions version the installed action policy was compiled from.
    compiled: Option<u64>,
}

impl<E: SqlEngine> Serving<E> {
    /// Serve `store` under the read policy compiled against its own model.
    fn from_store(
        store: CellStore<E>,
        scope: &CellScope,
        table: &'static GrantTable,
        caller: &Arc<CallerSlot>,
        questions: Option<DemoQuestions>,
    ) -> Result<Self, SeedError> {
        // Data reads are decided by the kernel PDP over this cell's own model.
        let policy = ReadPolicy::compile(store.model().object_types(), scope, table)?
            .with_caller(Arc::clone(caller));
        let actions = policy.action_slot();
        let object_types = store
            .model()
            .object_types()
            .map(|object_type| object_type.name.as_str().to_owned())
            .collect();
        let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
            .map_err(|refusal| SeedError(refusal.code))?
            .with_authorizer(policy);
        Ok(Self {
            host,
            questions,
            object_types,
            actions,
            compiled: None,
        })
    }

    /// Install the action policy of the active definitions when they changed
    /// since it was compiled. A policy that does not compile, or definitions
    /// that cannot be read, install none: the cell then serves no action.
    fn refresh_actions(&mut self) {
        let active = self.host.active_definitions().ok().flatten();
        let version = active.as_ref().map(|set| set.version);
        if version == self.compiled && version.is_some() {
            return;
        }
        let policy = active.and_then(|set| {
            let artifacts = norn_api::kernel_set::retained_artifacts(&set.canonical_set).ok()?;
            let model = norn_api::kernel_set::kernel_model(&artifacts).ok()?;
            ActionPolicy::compile(&model, &artifacts, self.host.scope())
                .ok()
                .flatten()
        });
        if let Ok(mut slot) = self.actions.write() {
            *slot = policy.map(Arc::new);
            self.compiled = version;
        }
    }
}

enum State<E: SqlEngine> {
    Vacant,
    Serving(Box<Serving<E>>),
}

/// The cell of one scope: vacant, or provisioned and served.
pub struct Tenant<E: SqlEngine> {
    scope: CellScope,
    base: TrustedBase,
    table: &'static GrantTable,
    caller: Arc<CallerSlot>,
    kinds: DefinitionKinds,
    open: OpenEngine<E>,
    state: State<E>,
    /// The receipt-disclosure authority of a cell with no questions.
    handles: Option<ReceiptHandles>,
    /// Ad-hoc questions over the metrics the cell's active definitions
    /// derive, keyed by the stage's lineage key; none without one.
    derived: Option<DerivedQuestions>,
}

impl<E: SqlEngine> Tenant<E> {
    /// The cell of `scope` over the engine `open` yields: served when the
    /// engine holds a provisioned cell, else vacant. Nothing is written.
    ///
    /// # Errors
    /// The engine does not open, holds a cell bound to another scope, or
    /// holds definitions this host cannot serve.
    pub fn open(
        scope: CellScope,
        table: &'static GrantTable,
        open: OpenEngine<E>,
        handle_key: Option<&[u8]>,
    ) -> Result<Self, SeedError> {
        let caller = Arc::<CallerSlot>::default();
        let engine = open().map_err(SeedError)?;
        let state = reopen(engine, &scope, table, &caller)?;
        let derived = handle_key
            .and_then(|key| DerivedQuestions::keyed(key, scope.clone()))
            .map(|questions| questions.with_label("tenant"));
        Ok(Self {
            base: crate::base::trusted_base()?,
            kinds: host_kinds(table),
            scope,
            table,
            caller,
            open,
            state,
            handles: handles(handle_key)?,
            derived,
        })
    }

    /// A cell this build seeds itself (the demo cells), already opened as
    /// `store`.
    ///
    /// # Errors
    /// The read policy does not compile against the store's model.
    pub fn seeded(
        scope: CellScope,
        table: &'static GrantTable,
        store: CellStore<E>,
        questions: Option<DemoQuestions>,
    ) -> Result<Self, SeedError> {
        let caller = Arc::<CallerSlot>::default();
        let serving = Serving::from_store(store, &scope, table, &caller, questions)?;
        Ok(Self {
            base: crate::base::trusted_base()?,
            kinds: host_kinds(table),
            scope,
            table,
            caller,
            open: Box::new(|| Err("a seeded cell is never provisioned again".to_owned())),
            state: State::Serving(Box::new(serving)),
            handles: None,
            derived: None,
        })
    }

    /// The same cell, its receipt coordinates keyed with `handle_key` (D53)
    /// when it publishes no questions of its own: the disclosure authority a
    /// decision and an action call answer with. Without it such a cell
    /// decides nothing and serves no action.
    ///
    /// # Errors
    /// A key shorter than [`MIN_HANDLE_KEY_BYTES`]: the cell refuses to serve
    /// weakly keyed handles.
    pub fn with_receipt_handles(mut self, handle_key: Option<&[u8]>) -> Result<Self, SeedError> {
        self.handles = handles(handle_key)?;
        Ok(self)
    }

    /// The slot the cell's policy reads the request's verified principal
    /// from; the cell's [`PrincipalResolver`] records into it.
    #[must_use]
    pub fn caller(&self) -> Arc<CallerSlot> {
        Arc::clone(&self.caller)
    }

    /// The scope this cell serves.
    #[must_use]
    pub const fn scope(&self) -> &CellScope {
        &self.scope
    }

    /// The served cell's host when the scope is provisioned, for the host's
    /// own background work (a source sync); requests go through [`Self::handle`].
    #[must_use]
    pub fn serving_host(&self) -> Option<&DevHost<E>> {
        match &self.state {
            State::Serving(serving) => Some(&serving.host),
            State::Vacant => None,
        }
    }

    /// Whether the scope has a cell.
    #[must_use]
    pub const fn is_provisioned(&self) -> bool {
        matches!(self.state, State::Serving(_))
    }

    /// The digest a submission must name: this host's trusted base.
    #[must_use]
    pub fn base_sha256(&self) -> &str {
        self.base.sha256()
    }

    /// Serve one request; every outcome is a status and a JSON document.
    pub fn handle(
        &mut self,
        principals: &dyn PrincipalResolver,
        request: &Request<'_>,
    ) -> Response {
        if request.path == PROVISION_PATH {
            return server::respond(self.provision(principals, request));
        }
        if let State::Serving(serving) = &mut self.state {
            serving.refresh_actions();
        }
        match &self.state {
            State::Serving(serving) if request.path == "/v1/access" => server::access(
                &serving.host,
                principals,
                &self.caller,
                &serving.object_types,
                request,
            ),
            State::Serving(serving) => {
                // A cell that publishes no questions answers ad-hoc questions
                // over its derived metrics; a revision whose metrics do not
                // derive serves receipt handles only.
                let derived = match (&serving.questions, &self.derived) {
                    (None, Some(derived)) => derived.questions(&serving.host).ok(),
                    _ => None,
                };
                let questions = serving
                    .questions
                    .as_ref()
                    .map(|questions| -> &dyn AdHocQuestions<E> { questions })
                    .or_else(|| {
                        derived
                            .as_ref()
                            .map(|runtime| -> &dyn AdHocQuestions<E> { runtime })
                    })
                    .or_else(|| {
                        self.handles
                            .as_ref()
                            .map(|handles| -> &dyn AdHocQuestions<E> { handles })
                    });
                server::handle_all(&serving.host, principals, questions, &self.kinds, request)
            }
            State::Vacant => self.vacant(principals, request),
        }
    }

    /// A request to a scope with no cell.
    fn vacant(&self, principals: &dyn PrincipalResolver, request: &Request<'_>) -> Response {
        let member = {
            // The verified principal lives exactly as long as this decision.
            let _request = RequestScope::new(principals);
            principals
                .resolve(request.authorization, request.now)
                .is_some_and(|subject| {
                    self.caller
                        .current()
                        .filter(|caller| caller.subject_id == subject)
                        .is_some_and(|caller| !self.table.capabilities(&caller.roles).is_empty())
                })
        };
        if member && self.names_this_scope(request.query) {
            server::respond(Err(provision::not_provisioned()))
        } else if request.path == "/v1/access" {
            server::access_outside::<E>(principals, request)
        } else {
            // Served with what a provisioned cell of this host is served
            // with (its receipt handles, its kinds), so no route branches
            // on whether the scope has a cell (#1297 r1).
            let questions = self
                .handles
                .as_ref()
                .map(|handles| -> &dyn AdHocQuestions<E> { handles });
            server::outside_with::<E>(questions, &self.kinds, request)
        }
    }

    /// Whether the query names exactly this cell's scope, each part once.
    fn names_this_scope(&self, query: &str) -> bool {
        let (mut org, mut environment) = (Vec::new(), Vec::new());
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            match &*key {
                "orgId" => org.push(value),
                "environmentId" => environment.push(value),
                _ => {}
            }
        }
        let ([org], [environment]) = (org.as_slice(), environment.as_slice()) else {
            return false;
        };
        self.scope.admit(Some(&**org), Some(&**environment)).is_ok()
    }

    /// `POST /v1/cell:provision`: authorized, with its explicit scope, before
    /// its body is read or storage is touched.
    fn provision(
        &mut self,
        principals: &dyn PrincipalResolver,
        request: &Request<'_>,
    ) -> Result<String, Refusal> {
        // The verified principal lives exactly as long as this request.
        let _request = RequestScope::new(principals);
        let actor = principals.resolve(request.authorization, request.now);
        let actor = actor.as_deref();
        if request.method != "POST" {
            return Err(Refusal::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "route.method_not_allowed",
                "the route does not accept this method",
                "a cell is provisioned with POST",
            ));
        }
        let mut fields = server::parse(request.query, &["orgId", "environmentId"])?;
        let scope = ScopeQuery {
            org_id: fields.remove("orgId"),
            environment_id: fields.remove("environmentId"),
        };
        match &self.state {
            State::Serving(serving) => {
                definition_routes::authorize(
                    &serving.host,
                    &scope,
                    actor,
                    Operation::ProvisionCell,
                )?;
            }
            State::Vacant => provision::authorize(
                &Provisioners::new(self.table, Arc::clone(&self.caller)),
                &self.scope,
                &scope,
                actor,
            )?,
        }
        let admitted = provision::admit_submission(&self.base, &self.scope, request.body)?;
        if matches!(self.state, State::Vacant) {
            // A model this host could not serve is refused before anything
            // is written, never provisioned and then left unopenable.
            ReadPolicy::compile(admitted.model.object_types(), &self.scope, self.table)
                .map_err(|error| provision::unservable(error.to_string()))?;
            let engine = (self.open)().map_err(Refusal::host)?;
            if CellStore::read_active_definitions(&engine)
                .map_err(Refusal::host)?
                .is_some()
            {
                // Provisioned since this host last looked (a reply it lost):
                // serve it and answer from its first revision.
                self.state =
                    reopen(engine, &self.scope, self.table, &self.caller).map_err(Refusal::host)?;
            } else {
                let (store, receipt) = provision::provision_cell(
                    engine,
                    &self.scope,
                    &self.base,
                    &admitted,
                    request.now.saturating_mul(1000),
                )?;
                let serving =
                    Serving::from_store(store, &self.scope, self.table, &self.caller, None)
                        .map_err(Refusal::host)?;
                self.state = State::Serving(Box::new(serving));
                return server::document_json(&receipt);
            }
        }
        let State::Serving(serving) = &self.state else {
            return Err(Refusal::host("the provisioned cell did not open"));
        };
        server::document_json(&provision::replay(&serving.host, &self.base, &admitted)?)
    }
}

/// The receipt handles keyed by `key`; none without a key.
///
/// # Errors
/// A key shorter than [`MIN_HANDLE_KEY_BYTES`]: the cell refuses to open
/// rather than serve unkeyed or weakly keyed handles.
fn handles(key: Option<&[u8]>) -> Result<Option<ReceiptHandles>, SeedError> {
    match key {
        None => Ok(None),
        Some(key) if key.len() < MIN_HANDLE_KEY_BYTES => Err(SeedError(format!(
            "the lineage key is {} bytes; at least {MIN_HANDLE_KEY_BYTES} are required",
            key.len()
        ))),
        Some(key) => Ok(Some(ReceiptHandles {
            key: key.to_vec(),
            catalog: EffectiveCatalog::default(),
        })),
    }
}

/// HOST-REOPEN-1: the cell `engine` holds, at its active definitions with
/// the model rebuilt from the retained set; vacant when it holds none.
fn reopen<E: SqlEngine>(
    engine: E,
    scope: &CellScope,
    table: &'static GrantTable,
    caller: &Arc<CallerSlot>,
) -> Result<State<E>, SeedError> {
    let fail =
        |context: &str, error: &dyn std::fmt::Display| SeedError(format!("{context}: {error}"));
    let Some(active) =
        CellStore::read_active_definitions(&engine).map_err(|e| fail("definitions", &e))?
    else {
        return Ok(State::Vacant);
    };
    let artifacts = norn_api::kernel_set::retained_artifacts(&active.canonical_set)
        .map_err(|e| fail("active definitions", &e))?;
    let model = norn_api::kernel_set::kernel_model(&artifacts)
        .map_err(|refusal| SeedError(format!("active definitions: {refusal:?}")))?;
    // As provisioned: the kernel's action types are part of the model.
    let model = norn_api::action_admission::prepare_action_model(&model)
        .map_err(|refusal| SeedError(format!("action model: {}", refusal.code)))?;
    let mut store = CellStore::from_engine(engine, model).map_err(|e| fail("open", &e))?;
    // A store bound to another scope is never served under this one.
    store
        .bind("scope", &scope.key())
        .map_err(|e| fail("scope", &e))?;
    Ok(State::Serving(Box::new(Serving::from_store(
        store, scope, table, caller, None,
    )?)))
}
