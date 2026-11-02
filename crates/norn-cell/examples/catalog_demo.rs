//! Bounded catalog reads of compiled Norn definitions through the kernel PDP.
use std::{
    collections::BTreeMap, collections::BTreeSet, error::Error, io::Read as _, io::Write as _,
};

use norn_cell::catalog::{CatalogSnapshot, resource_types};
use norn_cell::reactive::{ReactiveLimits, SliceQuery};
use norn_cell::{
    CellError, CellStore, CommitBatch, FactWriteRow, InstalledDefinitionInput, MAIN_WORLD, Model,
};
use norn_checker::{
    EntityArtifactSetContext,
    receipt::{ReceiptSources, check_artifact_set_with_receipt},
};
use norn_ir::access::{AccessIr, AccessRule, AccessScope, ScopeDimension};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, EntityRoot, IrVersion01, ResourceLimitProfile, RuntimeValue,
};
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp, PrincipalInput,
    ResourceInput, ScopeKey,
};

type DemoResult<T> = Result<T, Box<dyn Error>>;

fn install(artifacts: &[ArtifactEnvelope]) -> DemoResult<CellStore> {
    let registry = BTreeSet::new();
    let sources = artifacts
        .iter()
        .map(|artifact| Ok(RuntimeValue::from_json(serde_json::to_value(artifact)?)?))
        .collect::<DemoResult<Vec<_>>>()?;
    let sources = ReceiptSources::new(&sources, &[])?;
    let checked = check_artifact_set_with_receipt(
        &sources,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )?;
    if !checked.receipt.admitted {
        return Err(format!("definition admission refused: {:?}", checked.diagnostics).into());
    }
    let mut objects = Vec::new();
    let mut interfaces = Vec::new();
    let mut properties = Vec::new();
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot =
            serde_json::from_value(serde_json::to_value(artifact.payload.to_json_value())?)?;
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            EntityRoot::V2Markings(_) => {}
            EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                return Err("catalog demo needs v2 objects/interfaces without extensions".into());
            }
        }
    }
    let model = Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)?
        .with_governing_laws(artifacts)?;
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(
            RuntimeValue::from_json(serde_json::to_value(artifacts)?)?.canonical_json()?,
        )?,
        registry: vec![],
        receipt_sha256: checked.receipt.content_sha256()?,
        admission_receipt: Some(String::from_utf8(checked.receipt.canonical_json()?)?),
        readmission: None,
        diff: serde_json::json!({}),
        installed_at: 1,
        layout: None,
    };
    let mut store = CellStore::open_in_memory(model.clone())?;
    store.install_definitions(model, input, Some(0), "org_demo/dev")?;
    Ok(store)
}

fn access(scope: &ScopeKey, capabilities: Vec<String>) -> DemoResult<(Pdp, AccessRequest)> {
    // Virtual descriptors belong only to the Cedar projection, never the stored model.
    let types = resource_types()?;
    let projection = norn_projections::project_cedar_schema(&types, &[], &[]);
    let host = norn_projections::project_host_access_schema(&types);
    let schema = AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])?;
    let policy = AccessIr {
        access_version: IrVersion01::V0_1,
        policy_id: "demo.catalog.read".into(),
        rules: types
            .iter()
            .map(|t| AccessRule {
                rule_id: format!("read_{}", t.name.as_str()),
                actions: vec!["norn.read".into()],
                resource_type: t.name.clone(),
                scope: AccessScope {
                    dimensions: vec![ScopeDimension::Org, ScopeDimension::Environment],
                },
                all_of: vec![],
            })
            .collect(),
        generic_rules: vec![],
        extensions: None,
    };
    let pdp = Pdp::new(
        authz::compile(&schema, &policy)?,
        &[CapabilityGrant {
            subject_id: "demo.catalog.reader".into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(scope.clone()),
            capabilities,
            binding: None,
        }],
    )?;
    let request = AccessRequest {
        principal: PrincipalInput {
            subject_id: "demo.catalog.reader".into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles: vec![],
            attributes: BTreeMap::new(),
        },
        scope: Some(scope.clone()),
        capability: "norn.read".into(),
        action_id: "norn.read".into(),
        resource: ResourceInput {
            object_type: "norn.catalog.Artifact".into(),
            id: String::new(),
            scope: scope.clone(),
            attributes: BTreeMap::new(),
        },
        context: BTreeMap::new(),
        binding: None,
        delegation: Delegation::Direct,
    };
    Ok((pdp, request))
}

fn main() -> DemoResult<()> {
    let mut input = String::new();
    std::io::stdin()
        .take(1_048_577)
        .read_to_string(&mut input)?;
    if input.len() > 1_048_576 {
        return Err("catalog input exceeds 1 MiB".into());
    }
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(&input)?;
    let mut store = install(&artifacts)?;
    let snapshot = CatalogSnapshot::read_active(&store, &BTreeSet::new())?;
    let head = store.verified_head()?;
    // This local fixture's physical scope and read grant are host-owned inputs.
    let scope = ScopeKey {
        org: "org_demo".into(),
        tenant: None,
        workspace: None,
        environment: Some("dev".into()),
        business_unit: None,
    };
    let (pdp, request) = access(&scope, vec!["norn.read".into()])?;
    let limits = ReactiveLimits::from_profile(&ResourceLimitProfile::norn_limits_v1());
    let mut stdout = std::io::stdout().lock();
    let mut first = None;
    for descriptor in resource_types()? {
        let query = SliceQuery {
            object_type: descriptor.name.as_str().into(),
            filter: BTreeMap::new(),
            limit: limits.max_records.min(100),
            binding: None,
        };
        let result = snapshot.read(&query, &limits, &pdp, &request, &scope)?;
        if !result.complete || result.items.len() >= usize::try_from(query.limit)? {
            return Err(format!("incomplete catalog slice: {}", query.object_type).into());
        }
        if first.is_none() {
            first = result.items.first().cloned();
        }
        writeln!(
            stdout,
            "{} {}",
            query.object_type,
            serde_json::to_string(&result)?
        )?;
    }
    let row = first.ok_or("no admitted catalog rows")?;
    let query = SliceQuery {
        object_type: row.object_type.clone(),
        filter: BTreeMap::new(),
        limit: 1,
        binding: None,
    };
    let (denied, _) = access(&scope, vec!["norn.admin".into()])?;
    if !snapshot
        .read(&query, &limits, &denied, &request, &scope)?
        .items
        .is_empty()
    {
        return Err("catalog read without norn.read capability succeeded".into());
    }
    writeln!(
        stdout,
        "CURRENT: kernel PDP denies the reader without norn.read"
    )?;
    let mut wrong = request;
    wrong.scope = None;
    let error = snapshot
        .read(&query, &limits, &pdp, &wrong, &scope)
        .err()
        .ok_or("missing scope admitted")?;
    writeln!(stdout, "CURRENT: missing trusted scope refused: {error}")?;
    let virtual_type = row.object_type.clone();
    let result = store.commit(&CommitBatch {
        world: MAIN_WORLD.into(),
        scope: "org_demo/dev".into(),
        logical_time: 2,
        writes: vec![FactWriteRow::Put {
            object_type: row.object_type,
            key: row.key,
            values: row.values,
        }],
        ..CommitBatch::default()
    });
    let error = match result {
        Err(CellError::UnknownObjectType(object_type)) if object_type == virtual_type => {
            CellError::UnknownObjectType(object_type)
        }
        Err(error) => return Err(format!("unexpected catalog write error: {error}").into()),
        Ok(_) => return Err("catalog write admitted".into()),
    };
    writeln!(stdout, "CURRENT: catalog write refused: {error}")?;
    if store.verified_head()? != head {
        return Err("catalog reads/refusals changed the cell".into());
    }
    writeln!(
        stdout,
        "CURRENT: cell head unchanged by catalog reads and refusals"
    )?;
    Ok(())
}
