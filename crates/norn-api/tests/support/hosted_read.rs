use std::collections::BTreeMap;

use norn_api::authz::ReadCapability;
use norn_api::object_routes::ObjectAccess;
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::ObjectRecord;
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, Binding, ResourceInput, ScopeKey, UnboundProfile,
    VerifiedPrincipal,
};

pub(crate) struct Mapping;

impl ObjectAccess for Mapping {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<PdpRequest, Refusal> {
        let key = ScopeKey {
            org: scope.org_id.clone(),
            environment: Some(scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        Ok(PdpRequest::new(
            &VerifiedPrincipal::new(actor, ActorKind::User, None, vec![], BTreeMap::new()),
            Some(key.clone()),
            format!("fixture.{capability:?}"),
            "norn.read",
            ResourceInput {
                object_type: row
                    .map_or(object_type, |row| row.object_type.as_str())
                    .into(),
                id: row.map_or(object_type, |row| row.id.as_str()).into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            BTreeMap::new(),
            Binding::Unbound(UnboundProfile::HostedRead),
        ))
    }
}
