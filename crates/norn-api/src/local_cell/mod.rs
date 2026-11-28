//! Embedded native local cell runtime shared by `norn test` and `norn dev`.
//!
//! Wraps [`DevHost<NativeEngine>`](crate::host::DevHost) with a deterministic
//! [`ManualClock`], fail-closed scope and role-based authorization, and real
//! `norn-api` route dispatch (`get_defs`, `plan_definitions`, `apply_definitions`,
//! `call_action`, `get_object_query`, `get_queried_object`).

mod policy;

pub use policy::LocalActor;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, RwLock};

use http::StatusCode;
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, InstalledDefinitionInput, StoreOptions};
use norn_ir::ArtifactEnvelope;
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::action_admission::Serving;
use crate::actions::{ActionAnswer, ActionQuery, call_action, decode_action_request};
use crate::cell_definitions::{
    DefinitionsApplied, DefinitionsPlan, apply_definitions, plan_definitions,
};
use crate::clock::{Clock as _, ManualClock};
use crate::definition_routes::{DefinitionKinds, checker_restatement};
use crate::host::{DevHost, check_reopens, reopen_model};
use crate::kernel_set::{self, AdmittedSet, KernelRefusal};
use crate::object_query::ObjectQueryParams;
use crate::object_routes::{get_object_query, get_queried_object};
use crate::privacy::{DisclosedObjectQueryPage, DisclosedQueriedObject};
use crate::refusal::{Refusal, RefusalDiagnostic};
use crate::routes::{DefinitionsQuery, DefsResponse, get_defs};
use crate::scope::{CellScope, ScopeQuery};

use self::policy::{
    LocalDisclosure, LocalObjectAccess, LocalPolicy, PolicySnapshot, STANDARD_ROLES, with_actor,
};

/// Deterministic default starting timestamp (`2026-01-01T00:00:00Z`).
pub const DEFAULT_LOCAL_CLOCK_MS: u64 = 1_767_225_600_000;

const LOCAL_OWNER_ACTOR: &str = "local:owner";

#[derive(Deserialize)]
struct ArtifactIdProbe {
    #[serde(rename = "artifactId")]
    artifact_id: Option<String>,
}

fn partition_initial_artifacts(
    raw_artifacts: &[Box<RawValue>],
) -> Result<(Vec<ArtifactEnvelope>, Vec<Box<RawValue>>), Refusal> {
    let mut base = Vec::new();
    let mut tenant = Vec::new();
    let mut base_ids = BTreeSet::new();
    for raw in raw_artifacts {
        let probe: ArtifactIdProbe = serde_json::from_str(raw.get()).map_err(|error| {
            Refusal::invalid(format!("artifact envelope is not valid JSON: {error}"))
        })?;
        let id = probe
            .artifact_id
            .ok_or_else(|| Refusal::invalid("an artifact envelope must declare artifactId"))?;
        if kernel_set::is_reserved(&id) {
            if !base_ids.insert(id) {
                continue;
            }
            let rv = norn_ir::RuntimeValue::from_json_text(raw.get()).map_err(|error| {
                Refusal::invalid(format!("reserved artifact JSON invalid: {error}"))
            })?;
            let envelope: ArtifactEnvelope = norn_ir::runtime::decode(&rv).map_err(|error| {
                Refusal::invalid(format!("reserved artifact decode failed: {error}"))
            })?;
            base.push(envelope);
        } else {
            tenant.push(raw.clone());
        }
    }
    Ok((base, tenant))
}

fn kernel_refusal_to_refusal(refusal: KernelRefusal) -> Refusal {
    match refusal {
        KernelRefusal::Refused(findings) => {
            let mut out = Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                crate::cell_definitions::codes::REFUSED,
                "the initial definitions are not admissible",
                "each diagnostic names the rule and where in the submission it applies",
            );
            out.diagnostics = RefusalDiagnostic::from_checker(&findings, "");
            out
        }
        KernelRefusal::Invalid(detail) => {
            let mut out = Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                crate::cell_definitions::codes::REFUSED,
                "the initial definitions could not be projected",
                detail.clone(),
            );
            out.diagnostics = vec![RefusalDiagnostic {
                code: crate::cell_definitions::codes::REFUSED.to_owned(),
                severity: "error".to_owned(),
                message: detail,
                law: None,
                span: None,
                witness: None,
            }];
            out
        }
    }
}

fn provision_fresh_store(
    engine: NativeEngine,
    scope: &CellScope,
    base: &[ArtifactEnvelope],
    clock_ms: u64,
) -> Result<CellStore<NativeEngine>, Refusal> {
    let admitted: AdmittedSet =
        kernel_set::admit_kernel_set(base).map_err(kernel_refusal_to_refusal)?;
    let model = reopen_model(&admitted.canonical_set, Serving::ACTIONS)?;
    check_reopens(&model, &admitted.canonical_set, Serving::ACTIONS)?;
    let input = InstalledDefinitionInput {
        canonical_set: admitted.canonical_set,
        registry: Vec::new(),
        receipt_sha256: admitted.receipt_sha256,
        admission_receipt: Some(admitted.receipt),
        readmission: None,
        diff: serde_json::json!({}),
        installed_at: clock_ms,
        layout: None,
    };
    CellStore::provision_definitions(engine, model, &input, &scope.key()).map_err(Refusal::host)
}

fn open_or_provision_store(
    engine: NativeEngine,
    scope: &CellScope,
    base: &[ArtifactEnvelope],
    clock_ms: u64,
) -> Result<CellStore<NativeEngine>, Refusal> {
    if let Some(active) = CellStore::read_active_definitions(&engine).map_err(Refusal::host)? {
        let retained =
            kernel_set::retained_artifacts(&active.canonical_set).map_err(Refusal::host)?;
        let model = kernel_set::serving_model_installed(&retained, &active.model_hash)
            .map_err(Refusal::host)?;
        return CellStore::from_engine(engine, model).map_err(Refusal::host);
    }
    provision_fresh_store(engine, scope, base, clock_ms)
}

fn encode_apply_body(artifacts: &[Box<RawValue>], expected_version: u64) -> String {
    let items: Vec<&str> = artifacts.iter().map(|raw| raw.get()).collect();
    format!(
        r#"{{"artifacts":[{}],"expectedVersion":{expected_version}}}"#,
        items.join(",")
    )
}

/// An embedded native cell backed by [`DevHost`] and [`ManualClock`].
pub struct LocalCell {
    host: DevHost<NativeEngine>,
    clock: ManualClock,
    kinds: DefinitionKinds,
    disclosure: LocalDisclosure,
    policies: Arc<RwLock<PolicySnapshot>>,
}

impl LocalCell {
    /// Boot an in-memory local cell for `scope` at deterministic `clock_ms` and
    /// install `initial_artifacts` through the real admission and apply gates.
    ///
    /// # Errors
    /// Returns a [`Refusal`] when `scope` is blank, the store fails, or initial
    /// definition admission/install refuses.
    pub fn open_in_memory(
        scope: CellScope,
        clock_ms: u64,
        initial_artifacts: &[Box<RawValue>],
    ) -> Result<Self, Refusal> {
        let engine = NativeEngine::open_in_memory().map_err(Refusal::host)?;
        Self::open_engine(engine, scope, clock_ms, initial_artifacts)
    }

    /// Boot or reopen a file-backed `SQLite` local cell at `db_path` for `scope`.
    ///
    /// # Errors
    /// Returns a [`Refusal`] when `scope` is blank, the `SQLite` database cannot be
    /// opened, or initial definition admission/install refuses.
    pub fn open_file(
        db_path: &Path,
        scope: CellScope,
        clock_ms: u64,
        initial_artifacts: &[Box<RawValue>],
    ) -> Result<Self, Refusal> {
        let engine = NativeEngine::open(db_path, StoreOptions::default()).map_err(Refusal::host)?;
        Self::open_engine(engine, scope, clock_ms, initial_artifacts)
    }

    fn open_engine(
        engine: NativeEngine,
        scope: CellScope,
        clock_ms: u64,
        initial_artifacts: &[Box<RawValue>],
    ) -> Result<Self, Refusal> {
        scope.admit(Some(&scope.org_id), Some(&scope.environment_id))?;
        let (base, tenant) = partition_initial_artifacts(initial_artifacts)?;
        let store = open_or_provision_store(engine, &scope, &base, clock_ms)?;
        let policies = Arc::new(RwLock::new(PolicySnapshot::default()));
        let policy = LocalPolicy {
            scope: scope.clone(),
            policies: Arc::clone(&policies),
        };
        let disclosure = LocalDisclosure::for_scope(&scope);
        let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())?
            .with_authorizer(policy)
            .with_pseudonym_secret(Some(&disclosure.handle_key));
        let kinds = DefinitionKinds::default()
            .with_roles(STANDARD_ROLES)
            .with_restater(checker_restatement);
        let cell = Self {
            host,
            clock: ManualClock::at(clock_ms),
            kinds,
            disclosure,
            policies,
        };
        cell.refresh_policies()?;
        if !tenant.is_empty() {
            let expected = cell.active_definitions_version()?.unwrap_or(1);
            let body = encode_apply_body(&tenant, expected);
            let scope_query = ScopeQuery {
                org_id: Some(scope.org_id),
                environment_id: Some(scope.environment_id),
            };
            let owner = LocalActor::owner(LOCAL_OWNER_ACTOR);
            let _applied = cell.apply_definitions(&scope_query, Some(&owner), &body)?;
        }
        Ok(cell)
    }

    fn refresh_policies(&self) -> Result<(), Refusal> {
        let snapshot = match self.host.active_definitions()? {
            Some(active) => PolicySnapshot::from_canonical_set(&active.canonical_set)?,
            None => PolicySnapshot::default(),
        };
        match self.policies.write() {
            Ok(mut slot) => *slot = snapshot,
            Err(poisoned) => *poisoned.into_inner() = snapshot,
        }
        Ok(())
    }

    /// Physical scope bound to this cell.
    #[must_use]
    pub const fn scope(&self) -> &CellScope {
        self.host.scope()
    }

    /// Current logical clock timestamp in Unix milliseconds.
    #[must_use]
    pub fn clock_ms(&self) -> u64 {
        self.clock.now().as_millis()
    }

    /// Advance the deterministic [`ManualClock`] by `delta_ms` milliseconds.
    #[must_use]
    pub fn advance_clock(&self, delta_ms: u64) -> u64 {
        let next = self.clock_ms().saturating_add(delta_ms);
        self.clock.set(next);
        next
    }

    /// Active definitions version, if installed.
    ///
    /// # Errors
    /// Store or synchronization failure.
    pub fn active_definitions_version(&self) -> Result<Option<u64>, Refusal> {
        self.host.active_definitions_version()
    }

    /// Read admitted definitions (`GET /v1/definitions`).
    ///
    /// # Errors
    /// Scope, authorization, version-not-found, or host refusal.
    pub fn get_defs(
        &self,
        query: &DefinitionsQuery,
        actor: Option<&LocalActor>,
    ) -> Result<DefsResponse, Refusal> {
        let actor_id = actor.map(|caller| caller.user_id.as_str());
        with_actor(actor, || get_defs(&self.host, query, actor_id))
    }

    /// Plan a definitions change (`POST /v1/cell/definitions:plan`).
    ///
    /// # Errors
    /// Scope, authorization, malformed-body, checker, or live-law refusal.
    pub fn plan_definitions(
        &self,
        scope: &ScopeQuery,
        actor: Option<&LocalActor>,
        body: &str,
    ) -> Result<DefinitionsPlan, Refusal> {
        let actor_id = actor.map(|caller| caller.user_id.as_str());
        with_actor(actor, || {
            plan_definitions(&self.host, scope, actor_id, &self.kinds, body)
        })
    }

    /// Apply a planned definitions change (`POST /v1/cell/definitions:apply`).
    ///
    /// # Errors
    /// Scope, authorization, stale-version, checker, or live-law refusal.
    pub fn apply_definitions(
        &self,
        scope: &ScopeQuery,
        actor: Option<&LocalActor>,
        body: &str,
    ) -> Result<DefinitionsApplied, Refusal> {
        let actor_id = actor.map(|caller| caller.user_id.as_str());
        let now_ms = self.clock_ms();
        let applied = with_actor(actor, || {
            apply_definitions(&self.host, scope, actor_id, &self.kinds, body, now_ms)
        })?;
        self.refresh_policies()?;
        Ok(applied)
    }

    /// Invoke a governed action (`POST /v1/actions/{actionId}`).
    ///
    /// # Errors
    /// Scope, authorization, input validation, precondition, law, or host refusal.
    pub fn call_action(
        &self,
        query: &ActionQuery,
        actor: Option<&LocalActor>,
        action_id: &str,
        body: &str,
    ) -> Result<ActionAnswer, Refusal> {
        if actor.is_none_or(|caller| !caller.is_valid()) {
            return Err(crate::actions::action_denied());
        }
        if self
            .host
            .scope()
            .admit(query.org_id.as_deref(), query.environment_id.as_deref())
            .is_err()
        {
            return Err(crate::actions::action_denied());
        }
        let actor_id = actor.map(|caller| caller.user_id.as_str());
        let request = decode_action_request(body)?;
        let now = self.clock.now();
        with_actor(actor, || {
            call_action(
                &self.host,
                query,
                actor_id,
                action_id,
                &request,
                &self.disclosure,
                now,
            )
        })
    }

    /// Query visible objects of `object_type` (`GET /v1/objects/{type}`).
    ///
    /// # Errors
    /// Scope, authorization, invalid query, or unknown-type refusal.
    pub fn query_objects(
        &self,
        scope: &ScopeQuery,
        actor: Option<&LocalActor>,
        object_type: &str,
        params: &ObjectQueryParams,
    ) -> Result<DisclosedObjectQueryPage, Refusal> {
        let actor_id = actor.map(|caller| caller.user_id.as_str());
        with_actor(actor, || {
            get_object_query(
                &self.host,
                scope,
                actor_id,
                object_type,
                params,
                &LocalObjectAccess,
            )
        })
    }

    /// Read one visible object (`GET /v1/objects/{type}/{id}`).
    ///
    /// # Errors
    /// Scope, authorization, or not-found refusal.
    pub fn get_object(
        &self,
        scope: &ScopeQuery,
        actor: Option<&LocalActor>,
        object_type: &str,
        id: &str,
    ) -> Result<DisclosedQueriedObject, Refusal> {
        let actor_id = actor.map(|caller| caller.user_id.as_str());
        with_actor(actor, || {
            get_queried_object(
                &self.host,
                scope,
                actor_id,
                object_type,
                id,
                &LocalObjectAccess,
            )
        })
    }
}
