//! The dev host's server policy: one local developer, named by the project
//! manifest, holding every grant on a loopback-only cell (DEV-LOOPBACK-1).
//!
//! The principal comes only from the session bearer token in
//! `.norn/dev.token`; any other caller is denied by default. This is server
//! policy, stated on every `ready` event, never client logic.
use crate::principal::PrincipalResolver;
use norn_api::authz::{
    AccessRequest, Authorizer, DenyAll, ReadCapability, ReadRequest, RefusalDetailRequest,
};
use norn_api::question_routes::{AdHocQuestions, QuestionRuntime, QuestionSnapshot};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, ObjectRecord};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::MetricDefinition;
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
};

/// Every grant to exactly one actor; default deny for everyone else.
pub(crate) struct LocalDeveloper {
    pub(crate) actor: String,
}

impl LocalDeveloper {
    fn is(&self, actor: &str) -> bool {
        actor == self.actor
    }
}

impl Authorizer for LocalDeveloper {
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        if self.is(&request.access.principal.subject_id) {
            Ok(())
        } else {
            Err(norn_api::authz::read_denied())
        }
    }
    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.is(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Forbidden {
                reason: norn_api::refusal::codes::AUTHZ_DENIED.into(),
            })
        }
    }
    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.is(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        if self.is(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
        self.is(request.actor)
    }
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        if request.actor.is_some_and(|actor| self.is(actor)) {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }
}

/// The session token is the only credential; it names the manifest's actor.
pub(crate) struct DevPrincipals {
    pub(crate) bearer: String,
    pub(crate) actor: String,
}

impl PrincipalResolver for DevPrincipals {
    fn resolve(&self, authorization: Option<&str>, _now: u64) -> Option<String> {
        let presented = authorization?.strip_prefix("Bearer ")?;
        // Equal length first, then every byte: no early exit on a prefix.
        (presented.len() == self.bearer.len()
            && presented
                .bytes()
                .zip(self.bearer.bytes())
                .fold(0_u8, |seen, (a, b)| seen | (a ^ b))
                == 0)
            .then(|| self.actor.clone())
    }
    fn end_request(&self) {}
}

/// The dev cell has no declared questions; it carries only the disclosure
/// authority governed actions need (D53): a per-session handle key.
pub(crate) struct DevQuestions {
    pub(crate) handle_key: [u8; 32],
    pub(crate) catalog: EffectiveCatalog,
}

impl QuestionRuntime<NativeEngine> for DevQuestions {
    fn plan(&self, _question: &str) -> Result<QuestionPlan, Refusal> {
        Err(norn_api::authz::read_denied())
    }
    fn snapshot(
        &self,
        _store: &CellStore<NativeEngine>,
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
        &self.handle_key
    }
}

impl AdHocQuestions<NativeEngine> for DevQuestions {
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
