//! Structural law admission and conservative whole-set impact closure. No proof claims.
use crate::entity_artifacts::{Decoded, Layer, decode_v2};
use crate::{
    Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, EffectiveCatalog, Span,
};
use norn_expr::env::FieldSource;
use norn_expr::{EntityDefinitions, EnvOptions, Expression, Type, ZenTypeEnv, check_expression};
use norn_graph::{Budget, Direction, Graph, reachability};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, LawIR, LawModality, LawResponse, LawRoot,
    LawTiming, PropertyTypeName, ResourceLimitProfile,
};
use std::collections::{BTreeMap, BTreeSet};

const RETAINED_SCOPE_BUDGET: Budget = Budget {
    max_nodes: 16_384,
    max_edges: 131_072,
    max_steps: 1_048_576,
};

/// Stable law diagnostic codes.
pub mod codes {
    /// A retained pack/app root or its complete membership is unavailable.
    pub const UNRESOLVED_SCOPE: &str = "core.laws.unresolved_scope";
    /// Malformed closed `LawIR` document.
    pub const INVALID_DOCUMENT: &str = "law.validation.invalid_document";
    /// Unsupported temporal combination.
    pub const UNSUPPORTED_TEMPORAL: &str = "law.validation.unsupported_temporal";
    /// Non-Boolean, unresolved, or inadmissible ZEN predicate.
    pub const PREDICATE_TYPE: &str = "law.model.predicate_type";
    /// An ontology slot or its exact pinned dependency is absent.
    pub const UNKNOWN_BINDING: &str = "law.model.unknown_binding";
    /// A child declaration replaces rather than tightens an inherited law.
    pub const WEAKENS_INHERITED: &str = "law.model.weakens_inherited";
    /// Closure cannot be computed completely within the supplied budget.
    pub const CLOSURE_REFUSED: &str = "law.model.closure_refused";
}

fn finding(
    code: &str,
    artifact: &ArtifactEnvelope,
    pointer: &str,
    message: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

/// Decode and locally check a law; used by the ordinary artifact checker/WASM ABI.
#[must_use]
pub fn check_law_local(artifact: &ArtifactEnvelope) -> Vec<Diagnostic> {
    match decode(artifact) {
        Ok(law) => local(&law, artifact),
        Err(error) => vec![finding(
            codes::INVALID_DOCUMENT,
            artifact,
            "/payload",
            error,
        )],
    }
}

fn decode(artifact: &ArtifactEnvelope) -> Result<LawIR, String> {
    serde_json::to_value(artifact.payload.to_json_value())
        .and_then(serde_json::from_value)
        .map_err(|error| error.to_string())
}

fn local(law: &LawIR, artifact: &ArtifactEnvelope) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let root = match &law.scope.root {
        LawRoot::Company(id)
        | LawRoot::App(id)
        | LawRoot::Pack(id)
        | LawRoot::Cell(id)
        | LawRoot::Entity(id) => id,
    };
    if [&law.id, &law.owner, &law.rationale, &law.subject, root]
        .iter()
        .any(|v| v.trim().is_empty())
        || law.scope.object_types.is_empty()
        || law
            .scope
            .object_types
            .iter()
            .chain(&law.scope.commitments)
            .any(|v| v.trim().is_empty())
    {
        out.push(finding(codes::INVALID_DOCUMENT, artifact, "/payload", "law identity, owner, rationale, subject, root and object types must be explicit and nonblank"));
    }
    if !matches!(law.modality, LawModality::Eventually { .. }) && law.timing.is_some() {
        out.push(finding(
            codes::UNSUPPORTED_TEMPORAL,
            artifact,
            "/payload/timing",
            "timing bounds are supported only on eventually; safety and strong-until are unbounded",
        ));
    }
    if let Some(LawTiming::Within { millis }) = &law.timing
        && crate::deontic::canonical_positive_millis(millis).is_none()
    {
        out.push(finding(
            codes::INVALID_DOCUMENT,
            artifact,
            "/payload/timing/millis",
            "within requires positive canonical u64 milliseconds as decimal text",
        ));
    }
    if matches!(law.response, LawResponse::Invariant { .. })
        && (!matches!(law.modality, LawModality::Always { .. })
            || law.condition.is_some()
            || law.timing.is_some()
            || !law.scope.lifecycle_states.is_empty())
    {
        out.push(finding(codes::UNSUPPORTED_TEMPORAL, artifact, "/payload/response", "an invariant identity is an unconditional always law; filtering or negating it would change its meaning"));
    }
    if let LawRoot::Entity(name) = &law.scope.root
        && name != &law.subject
    {
        out.push(finding(
            codes::UNKNOWN_BINDING,
            artifact,
            "/payload/scope/root",
            "entity root must name the subject",
        ));
    }
    check_exception_tree(law, artifact, &mut out);
    out
}

/// Exception-tree admission: shape, bounds and a response that an exception
/// can lift. Sibling overlap admits: an exception only lifts its parent.
fn check_exception_tree(law: &LawIR, artifact: &ArtifactEnvelope, out: &mut Vec<Diagnostic>) {
    use crate::law_exceptions::{ExceptionTreeRefusal, check_tree, codes as exception_codes};
    if law.exceptions.is_empty() {
        return;
    }
    if matches!(law.response, LawResponse::Invariant { .. }) {
        out.push(finding(
            exception_codes::UNSUPPORTED_RESPONSE,
            artifact,
            "/payload/exceptions",
            "an invariant identity is an unconditional always law; an exception would change its meaning",
        ));
    }
    if let Err(refusal) = check_tree(&law.id, &law.exceptions) {
        let (pointer, message) = match refusal {
            ExceptionTreeRefusal::Limit => (
                "/payload/exceptions".to_owned(),
                format!(
                    "a law declares at most {} exceptions",
                    crate::law_exceptions::MAX_LAW_EXCEPTIONS
                ),
            ),
            ExceptionTreeRefusal::InvalidDeclaration(index) => (
                format!("/payload/exceptions/{index}"),
                format!(
                    "exception {index} needs a nonblank id that is unique and differs from law {}, a nonblank rationale and a valid source",
                    law.id
                ),
            ),
            ExceptionTreeRefusal::DanglingOverride(index) => (
                format!("/payload/exceptions/{index}/overrides"),
                format!(
                    "exception {index} overrides a target that is neither law {} nor an exception of its tree",
                    law.id
                ),
            ),
            ExceptionTreeRefusal::Cycle(index) => (
                format!("/payload/exceptions/{index}/overrides"),
                format!(
                    "exception {index} is on an overrides cycle that never reaches law {}",
                    law.id
                ),
            ),
        };
        out.push(finding(refusal.code(), artifact, &pointer, message));
    }
}

/// Whole-set structural checks, against the same effective entities as invariants/guards.
#[must_use]
pub fn check_law_set(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Vec<Diagnostic> {
    let pool: Vec<_> = base.iter().chain(artifacts).cloned().collect();
    if !pool.iter().any(|a| a.kind() == ArtifactKind::Law) {
        return Vec::new();
    }
    let limits = ResourceLimitProfile::norn_limits_v1();
    let catalog = EffectiveCatalog::from_artifacts_with_registry(&pool, registry);
    let mut layer = Layer::default();
    for (index, a) in pool.iter().enumerate() {
        if let Decoded::Root(root) = decode_v2(a) {
            layer.push(index, *root);
        }
    }
    let property_types: Vec<_> = layer.catalog.iter().map(|(_, def)| def.clone()).collect();
    let env = ZenTypeEnv::from_artifact_set_with_registry(
        &EntityDefinitions {
            property_types: &property_types,
            interfaces: &layer.interfaces,
            object_types: &layer.object_types,
            extensions: &layer.extensions,
        },
        registry,
        EnvOptions::from_limit_profile(&limits),
    );
    let context = LawContext {
        base,
        pool: &pool,
        catalog: &catalog,
        layer: &layer,
        env: env.as_ref().ok(),
    };
    // The whole-set inventory is identical for every law; compute it once.
    let closure_refused = pool
        .iter()
        .find(|a| a.kind() == ArtifactKind::Law)
        .is_some_and(|law| {
            conservative_closure(
                law,
                &pool,
                registry,
                Budget {
                    max_nodes: 16_384,
                    max_edges: 131_072,
                    max_steps: 1_048_576,
                },
            )
            .is_err()
        });
    let mut out = Vec::new();
    let mut laws: BTreeMap<String, Vec<(&ArtifactEnvelope, LawIR)>> = BTreeMap::new();
    for artifact in &pool {
        if !matches!(artifact.payload, ArtifactPayload::Law(_)) {
            continue;
        }
        let Ok(law) = decode(artifact) else {
            out.extend(check_law_local(artifact));
            continue;
        };
        out.extend(local(&law, artifact));
        context.check_bound_law(&law, artifact, &mut out);
        for (parent, inherited) in laws.get(&law.id).into_iter().flatten() {
            if inherited != &law || crate::scope_conflict(&artifact.scope, &parent.scope).is_some()
            {
                // A law is closed: exceptions live only in its owner's
                // artifact, so a redeclaration that changes them is refused.
                let message = if inherited.exceptions != law.exceptions {
                    "an installed law is closed: another artifact cannot add or change its exceptions; narrow your own law instead"
                } else {
                    "an existing law identity cannot be replaced; retain it and add a new conjunctive law identity"
                };
                out.push(finding(
                    codes::WEAKENS_INHERITED,
                    artifact,
                    "/payload/id",
                    message,
                ));
            }
        }
        if closure_refused {
            out.push(finding(
                codes::CLOSURE_REFUSED,
                artifact,
                "/dependencies",
                "whole-set closure contains unresolved dependencies or exceeds its graph budget",
            ));
        }
        laws.entry(law.id.clone())
            .or_default()
            .push((artifact, law));
    }
    out
}

struct LawContext<'a> {
    base: &'a [ArtifactEnvelope],
    pool: &'a [ArtifactEnvelope],
    catalog: &'a EffectiveCatalog,
    layer: &'a Layer,
    env: Option<&'a ZenTypeEnv>,
}

/// Exact retained artifact members and every root-to-instance containment path.
pub type RetainedLawMembership =
    BTreeMap<norn_ir::definition_catalog::ArtifactKey, Vec<Vec<norn_ir::PackInstanceRef>>>;

type ScopePacks<'a> = BTreeMap<
    norn_ir::definition_catalog::ArtifactKey,
    (&'a ArtifactEnvelope, norn_ir::CompositionPackRoot, String),
>;

/// LAW-CONTAINED-1: resolve only explicit retained instance imports/containment.
/// This structural query grants no proof, runtime permission or installation authority.
/// # Errors
/// Refuses unresolved roots, stale pins, incomplete members, cycles or budget exhaustion.
pub fn retained_law_membership(
    law: &ArtifactEnvelope,
    base: &[ArtifactEnvelope],
    budget: Budget,
) -> Result<RetainedLawMembership, Vec<Diagnostic>> {
    let refused = || {
        vec![finding(
            codes::UNRESOLVED_SCOPE,
            law,
            "/payload/scope/root",
            "retained instance membership is missing, ambiguous, stale or exceeds its budget",
        )]
    };
    let declaration = decode(law).map_err(|_error| refused())?;
    let (LawRoot::App(root) | LawRoot::Pack(root)) = &declaration.scope.root else {
        return Err(refused());
    };
    if base.len() > budget.max_nodes {
        return Err(refused());
    }
    let packs = scope_packs(base).ok_or_else(refused)?;
    let mut instances = BTreeMap::new();
    for (owner, pack, _) in packs.values() {
        for instance in pack.instances.iter().flatten() {
            if instance.scope == law.scope
                && (crate::scope_conflict(&instance.scope, &owner.scope).is_some()
                    || instance.instance_id.trim().is_empty()
                    || instances
                        .insert(instance.instance_id.clone(), instance.clone())
                        .is_some()
                    || instances.len() > budget.max_nodes)
            {
                return Err(refused());
            }
        }
    }
    let first = instances.get(root).ok_or_else(refused)?;
    let initial = norn_ir::PackInstanceRef {
        instance_id: root.clone(),
        pack: first.pack.clone(),
    };
    scope_members(base, &packs, &instances, initial, budget).ok_or_else(refused)
}

fn scope_packs(base: &[ArtifactEnvelope]) -> Option<ScopePacks<'_>> {
    use norn_ir::definition_catalog::ArtifactKey;
    let mut packs = BTreeMap::new();
    let mut bytes = 0usize;
    for artifact in base
        .iter()
        .filter(|a| a.kind() == ArtifactKind::BusinessPack)
    {
        let value = serde_json::to_value(artifact.payload.to_json_value()).ok()?;
        let size = norn_canonical::serialize_json_verbatim(&value).ok()?.len();
        bytes = bytes.saturating_add(size);
        if bytes > 8_388_608 {
            return None;
        }
        if let norn_ir::BusinessPackRoot::Composition(pack) = serde_json::from_value(value).ok()? {
            let key = ArtifactKey {
                artifact_id: artifact.artifact_id.clone(),
                version: artifact.version.clone(),
            };
            let digest = crate::ArtifactRef::of(artifact).ok()?.sha256;
            if packs.insert(key, (artifact, pack, digest)).is_some() {
                return None;
            }
        }
    }
    Some(packs)
}

fn scope_members(
    base: &[ArtifactEnvelope],
    packs: &ScopePacks<'_>,
    instances: &BTreeMap<String, norn_ir::PackInstance>,
    initial: norn_ir::PackInstanceRef,
    budget: Budget,
) -> Option<RetainedLawMembership> {
    use norn_ir::definition_catalog::ArtifactKey;
    let mut pending = vec![vec![initial]];
    let mut out: RetainedLawMembership = BTreeMap::new();
    let mut steps = 0usize;
    let mut copies = 0usize;
    let mut bytes = 0usize;
    while let Some(path) = pending.pop() {
        steps = steps.saturating_add(1);
        if steps > budget.max_steps || path.len() > budget.max_nodes {
            return None;
        }
        let last = path.last()?;
        let instance = instances.get(&last.instance_id)?;
        if instance.pack != last.pack {
            return None;
        }
        let key = ArtifactKey {
            artifact_id: last.pack.artifact_id.clone(),
            version: last.pack.version.clone(),
        };
        let (artifact, pack, digest) = packs.get(&key)?;
        if digest != &last.pack.sha256
            || crate::scope_conflict(&instance.scope, &artifact.scope).is_some()
        {
            return None;
        }
        for member in &pack.artifacts {
            let mut matches = base
                .iter()
                .filter(|a| a.artifact_id == member.artifact_id && a.version == member.version);
            let target = matches.next()?;
            if matches.next().is_some()
                || target.kind() != crate::artifact_set::pack_kind(member.kind)
                || crate::scope_conflict(&instance.scope, &target.scope).is_some()
            {
                return None;
            }
            if target.kind() == ArtifactKind::BusinessPack {
                if !instance
                    .imports
                    .values()
                    .chain(&instance.instances)
                    .any(|r| {
                        r.pack.artifact_id == member.artifact_id && r.pack.version == member.version
                    })
                {
                    return None;
                }
                continue;
            }
            copies = copies.saturating_add(path.len());
            let size = norn_canonical::serialize_json_verbatim(&path).ok()?.len();
            bytes = bytes.saturating_add(size);
            if copies > budget.max_edges || bytes > 8_388_608 {
                return None;
            }
            out.entry(ArtifactKey {
                artifact_id: member.artifact_id.clone(),
                version: member.version.clone(),
            })
            .or_default()
            .push(path.clone());
        }
        for next in instance.imports.values().chain(&instance.instances) {
            if path
                .iter()
                .any(|previous| previous.instance_id == next.instance_id)
            {
                return None;
            }
            copies = copies.saturating_add(path.len().saturating_add(1));
            bytes = bytes.saturating_add(
                norn_canonical::serialize_json_verbatim(&path)
                    .ok()?
                    .len()
                    .saturating_add(norn_canonical::serialize_json_verbatim(next).ok()?.len()),
            );
            if copies > budget.max_edges || bytes > 8_388_608 {
                return None;
            }
            let mut child = path.clone();
            child.push(next.clone());
            pending.push(child);
        }
    }
    for paths in out.values_mut() {
        paths.sort_by_cached_key(|path| {
            path.iter()
                .map(|p| p.instance_id.clone())
                .collect::<Vec<_>>()
        });
        paths.dedup();
    }
    Some(out)
}

/// LAW-COMPLETE-1 / LAW-CONTAINED-INSTANCE-1: project unchanged laws with every
/// origin/retained instance path. Empty instances are not non-vacuity evidence.
/// This structural projection grants no runtime or proof authority.
///
/// # Errors
/// Refuses inadmissible inputs, unavailable subjects, unresolved app/pack instances
/// and incomplete provenance under the supplied graph/path budget.
pub fn effective_law_instances(
    artifacts: &[ArtifactEnvelope],
    context: &crate::EntityArtifactSetContext<'_>,
    budget: Budget,
) -> Result<Vec<norn_ir::verification::EffectiveLawInstance>, Vec<Diagnostic>> {
    let findings = crate::check_artifact_set(artifacts, context);
    if findings.iter().any(Diagnostic::is_error) {
        return Err(findings);
    }
    let pool: Vec<_> = context.base.iter().chain(artifacts).cloned().collect();
    let Some(first) = pool.iter().find(|a| a.kind() == ArtifactKind::Law) else {
        return Ok(Vec::new());
    };
    let refused = || {
        vec![finding(
            codes::CLOSURE_REFUSED,
            first,
            "/payload",
            "effective law paths exceed their budget or are incomplete",
        )]
    };
    if pool.len() > budget.max_nodes {
        return Err(refused());
    }
    let catalog = EffectiveCatalog::from_artifacts_with_registry(&pool, context.registry);
    let (owners, edges) = law_type_graph(&pool, &catalog);
    let digests = pool
        .iter()
        .map(|a| {
            crate::ArtifactRef::of(a)
                .map(|r| ((a.artifact_id.as_str(), a.version.as_str()), r.sha256))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(|_error| refused())?;
    let mut path_work = LawPathWork::default();
    if edges
        .values()
        .try_fold(0usize, |count, next| count.checked_add(next.len()))
        .is_none_or(|count| count > budget.max_edges)
    {
        return Err(refused());
    }
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let projection = LawProjection {
        catalog: &catalog,
        owners: &owners,
        edges: &edges,
        digests: &digests,
        base: context.base,
        budget,
    };
    for artifact in pool.iter().filter(|a| a.kind() == ArtifactKind::Law) {
        projection.append(artifact, &mut out, &mut bytes, &mut path_work)?;
    }
    out.sort_by_cached_key(|a| {
        (
            a.law.clone(),
            a.subject.clone(),
            a.bindings.clone(),
            a.instance_paths
                .first()
                .and_then(|p| p.last())
                .map(|p| p.instance_id.clone()),
        )
    });
    Ok(out)
}

fn contained_paths(
    membership: Option<&RetainedLawMembership>,
    subject: &norn_ir::definition_catalog::MemberRef,
) -> Vec<Vec<Vec<norn_ir::PackInstanceRef>>> {
    let Some(membership) = membership else {
        return vec![Vec::new()];
    };
    let key = norn_ir::definition_catalog::ArtifactKey {
        artifact_id: subject.artifact_id.clone(),
        version: subject.version.clone(),
    };
    let mut grouped: BTreeMap<String, Vec<Vec<norn_ir::PackInstanceRef>>> = BTreeMap::new();
    for path in membership.get(&key).into_iter().flatten() {
        if let Some(last) = path.last() {
            grouped
                .entry(last.instance_id.clone())
                .or_default()
                .push(path.clone());
        }
    }
    grouped.into_values().collect()
}

struct LawProjection<'a> {
    catalog: &'a EffectiveCatalog,
    owners: &'a LawOwners,
    edges: &'a LawEdges,
    digests: &'a LawDigests<'a>,
    base: &'a [ArtifactEnvelope],
    budget: Budget,
}

impl LawProjection<'_> {
    fn append(
        &self,
        artifact: &ArtifactEnvelope,
        out: &mut Vec<norn_ir::verification::EffectiveLawInstance>,
        bytes: &mut usize,
        path_work: &mut LawPathWork,
    ) -> Result<(), Vec<Diagnostic>> {
        let refused = || {
            vec![finding(
                codes::CLOSURE_REFUSED,
                artifact,
                "/payload",
                "effective law paths exceed their budget or are incomplete",
            )]
        };
        let law = decode(artifact).map_err(|detail| {
            vec![finding(
                codes::INVALID_DOCUMENT,
                artifact,
                "/payload",
                detail,
            )]
        })?;
        let membership = matches!(law.scope.root, LawRoot::App(_) | LawRoot::Pack(_))
            .then(|| retained_law_membership(artifact, self.base, self.budget))
            .transpose()?;
        let origin = self.owners.get(&law.subject).ok_or_else(refused)?;
        let targets = self
            .catalog
            .target_types(&law.subject)
            .ok_or_else(refused)?;
        if let Some(membership) = &membership {
            let scoped = |at: &norn_ir::definition_catalog::MemberRef| {
                membership.contains_key(&norn_ir::definition_catalog::ArtifactKey {
                    artifact_id: at.artifact_id.clone(),
                    version: at.version.clone(),
                })
            };
            if !scoped(origin)
                && !targets
                    .iter()
                    .filter_map(|name| self.owners.get(name))
                    .any(scoped)
            {
                return Err(refused());
            }
        }
        for target in targets {
            let subject = self.owners.get(&target).ok_or_else(refused)?;
            let paths = law_paths(
                &law.subject,
                &target,
                self.edges,
                self.owners,
                self.budget,
                path_work,
            )
            .ok_or_else(refused)?;
            let mut instance = law_instance(artifact, &law, subject, origin, paths, self.digests)
                .ok_or_else(refused)?;
            let groups = contained_paths(membership.as_ref(), subject);
            for instance_paths in groups {
                instance.instance_paths = instance_paths;
                *bytes = bytes.saturating_add(
                    norn_canonical::serialize_json_verbatim(&instance)
                        .map_err(|_error| refused())?
                        .len(),
                );
                if *bytes > 8_388_608 || out.len() >= self.budget.max_nodes {
                    return Err(refused());
                }
                out.push(instance.clone());
            }
        }
        Ok(())
    }
}

type LawDigests<'a> = BTreeMap<(&'a str, &'a str), String>;

fn law_instance(
    artifact: &ArtifactEnvelope,
    law: &LawIR,
    subject: &norn_ir::definition_catalog::MemberRef,
    origin: &norn_ir::definition_catalog::MemberRef,
    paths: Vec<Vec<norn_ir::definition_catalog::MemberRef>>,
    digests: &LawDigests<'_>,
) -> Option<norn_ir::verification::EffectiveLawInstance> {
    use norn_ir::definition_catalog::{Member, MemberRef};
    Some(norn_ir::verification::EffectiveLawInstance {
        law: MemberRef {
            artifact_id: artifact.artifact_id.clone(),
            version: artifact.version.clone(),
            member: Member::Law {
                law: law.id.clone(),
            },
        },
        law_digest: digests
            .get(&(artifact.artifact_id.as_str(), artifact.version.as_str()))?
            .clone(),
        subject: subject.clone(),
        subject_digest: digests
            .get(&(subject.artifact_id.as_str(), subject.version.as_str()))?
            .clone(),
        attachment: law.scope.root.clone(),
        origin: origin.clone(),
        paths,
        instance_paths: Vec::new(),
        bindings: BTreeMap::from([("self".to_owned(), subject.clone())]),
    })
}

type LawOwners = BTreeMap<String, norn_ir::definition_catalog::MemberRef>;
type LawEdges = BTreeMap<String, BTreeSet<String>>;

fn law_type_graph(pool: &[ArtifactEnvelope], catalog: &EffectiveCatalog) -> (LawOwners, LawEdges) {
    use norn_ir::definition_catalog::{Member, MemberRef};
    let mut owners = BTreeMap::new();
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for artifact in pool {
        let mut declare = |name: &str, parents: Vec<String>| {
            owners.insert(
                name.to_owned(),
                MemberRef {
                    artifact_id: artifact.artifact_id.clone(),
                    version: artifact.version.clone(),
                    member: Member::Type {
                        type_name: name.to_owned(),
                    },
                },
            );
            for parent in parents {
                edges.entry(parent).or_default().insert(name.to_owned());
            }
        };
        if let Some(Ok(object)) = catalog.object_type(&artifact.artifact_id) {
            declare(object.name.as_str(), Vec::new());
        }
        if let Decoded::Root(root) = decode_v2(artifact) {
            match *root {
                norn_ir::EntityRoot::V2(root) => declare(
                    root.object_type.name.as_str(),
                    root.object_type
                        .extends
                        .iter()
                        .map(|p| p.as_str().to_owned())
                        .chain(
                            root.object_type
                                .implements
                                .iter()
                                .map(|i| i.as_str().to_owned()),
                        )
                        .collect(),
                ),
                norn_ir::EntityRoot::V2Interface(root) => declare(
                    root.interface.name.as_str(),
                    root.interface
                        .extends
                        .iter()
                        .map(|p| p.as_str().to_owned())
                        .collect(),
                ),
                norn_ir::EntityRoot::V1(_)
                | norn_ir::EntityRoot::V2Extension(_)
                | norn_ir::EntityRoot::V2PropertyTypes(_)
                | norn_ir::EntityRoot::V2Markings(_) => {}
            }
        }
    }
    (owners, edges)
}

#[derive(Default)]
struct LawPathWork {
    steps: usize,
    copies: usize,
    bytes: usize,
}

fn law_paths(
    origin: &str,
    target: &str,
    edges: &BTreeMap<String, BTreeSet<String>>,
    owners: &BTreeMap<String, norn_ir::definition_catalog::MemberRef>,
    budget: Budget,
    work: &mut LawPathWork,
) -> Option<Vec<Vec<norn_ir::definition_catalog::MemberRef>>> {
    let mut pending = vec![vec![origin]];
    let mut paths = Vec::new();
    while let Some(path) = pending.pop() {
        work.steps = work.steps.saturating_add(1);
        if work.steps > budget.max_steps || path.len() > budget.max_nodes {
            return None;
        }
        let last = *path.last()?;
        if last == target {
            let mut resolved = Vec::new();
            for name in path {
                let reference = owners.get(name)?;
                work.bytes = work.bytes.saturating_add(
                    norn_canonical::serialize_json_verbatim(reference)
                        .ok()?
                        .len(),
                );
                if work.bytes > 8_388_608 {
                    return None;
                }
                resolved.push(reference.clone());
            }
            paths.push(resolved);
        } else {
            for next in edges.get(last).into_iter().flatten().rev() {
                if path.contains(&next.as_str()) {
                    return None;
                }
                work.copies = work.copies.saturating_add(path.len().saturating_add(1));
                if work.copies > budget.max_edges {
                    return None;
                }
                let mut child = path.clone();
                child.push(next.as_str());
                pending.push(child);
            }
        }
    }
    (!paths.is_empty()).then_some(paths)
}

impl LawContext<'_> {
    /// LAW-COMPLETE-1: validate the declaration even with zero implementors,
    /// then check every concrete subject selected by the shared catalog.
    fn check_bound_law(&self, law: &LawIR, artifact: &ArtifactEnvelope, out: &mut Vec<Diagnostic>) {
        let Some(targets) = self.catalog.target_types(&law.subject) else {
            out.push(finding(
                codes::UNKNOWN_BINDING,
                artifact,
                "/payload/subject",
                "law subject has no complete effective contract",
            ));
            return;
        };
        if matches!(law.scope.root, LawRoot::Pack(_))
            && let Err(findings) =
                retained_law_membership(artifact, self.base, RETAINED_SCOPE_BUDGET)
        {
            out.extend(findings);
            return;
        }
        if let Some(interface) = self
            .layer
            .interfaces
            .iter()
            .find(|i| i.name.as_str() == law.subject)
        {
            self.check_interface(law, artifact, interface, out);
        }
        for name in targets {
            let Some(Ok(subject)) = self.catalog.object_type_named(&name) else {
                out.push(finding(
                    codes::UNKNOWN_BINDING,
                    artifact,
                    "/payload/subject",
                    "an effective subject is unavailable",
                ));
                continue;
            };
            self.check_scope(law, artifact, subject, out);
            self.check_predicates(law, artifact, subject, out);
        }
    }

    fn check_scope(
        &self,
        law: &LawIR,
        artifact: &ArtifactEnvelope,
        subject: &crate::EffectiveObjectType,
        out: &mut Vec<Diagnostic>,
    ) {
        if !law.scope.object_types.iter().any(|name| {
            self.catalog
                .target_types(name)
                .is_some_and(|types| types.contains(subject.name.as_str()))
        }) {
            out.push(finding(
                codes::UNKNOWN_BINDING,
                artifact,
                "/payload/scope/objectTypes",
                "scope must include the subject or one of its supertypes",
            ));
        }
        self.check_scope_bindings(law, artifact, out);
        for state in &law.scope.lifecycle_states {
            let selected = if self
                .catalog
                .target_types(&state.object_type)
                .is_some_and(|types| types.contains(subject.name.as_str()))
            {
                Some(subject)
            } else {
                self.catalog
                    .object_type_named(&state.object_type)
                    .and_then(Result::ok)
            };
            let machine = selected.and_then(|t| t.lifecycle(&state.property));
            if !law.scope.object_types.contains(&state.object_type)
                || state.states.is_empty()
                || !machine.is_some_and(|m| state.states.iter().all(|s| m.states.contains(s)))
            {
                out.push(finding(
                    codes::UNKNOWN_BINDING,
                    artifact,
                    "/payload/scope/lifecycleStates",
                    "state selectors must bind declared lifecycle states of a scoped type",
                ));
            }
        }
    }

    fn check_scope_bindings(
        &self,
        law: &LawIR,
        artifact: &ArtifactEnvelope,
        out: &mut Vec<Diagnostic>,
    ) {
        for name in law
            .scope
            .object_types
            .iter()
            .chain(&law.scope.commitments)
            .chain(std::iter::once(&law.subject))
        {
            let bound = self.type_owner(name);
            if !bound.is_some_and(|target| pinned(artifact, target)) {
                out.push(finding(
                    codes::UNKNOWN_BINDING,
                    artifact,
                    "/payload/scope",
                    format!(
                        "type {name:?} needs a required exact-version, visible entity dependency"
                    ),
                ));
            }
            if law.scope.commitments.contains(name)
                && self
                    .catalog
                    .object_type_named(name)
                    .and_then(Result::ok)
                    .is_none_or(|t| t.lifecycles.is_empty())
            {
                out.push(finding(
                    codes::UNKNOWN_BINDING,
                    artifact,
                    "/payload/scope/commitments",
                    "Commitment scope needs a lifecycle-bearing registry type",
                ));
            }
        }
    }

    fn type_owner(&self, name: &str) -> Option<&ArtifactEnvelope> {
        let matches: Vec<_> = self
            .pool
            .iter()
            .filter(|artifact| {
                self.catalog
                    .object_type(&artifact.artifact_id)
                    .and_then(Result::ok)
                    .is_some_and(|t| t.name.as_str() == name)
                    || self
                        .layer
                        .interfaces
                        .iter()
                        .zip(&self.layer.interface_owners)
                        .any(|(definition, owner)| {
                            definition.name.as_str() == name
                                && self.pool.get(*owner).is_some_and(|a| {
                                    a.artifact_id == artifact.artifact_id
                                        && a.version == artifact.version
                                })
                        })
            })
            .collect();
        match matches.as_slice() {
            [artifact] => Some(*artifact),
            _ => None,
        }
    }

    fn check_interface(
        &self,
        law: &LawIR,
        artifact: &ArtifactEnvelope,
        interface: &norn_ir::InterfaceDef,
        out: &mut Vec<Diagnostic>,
    ) {
        if matches!(law.scope.root, LawRoot::App(_))
            && let Err(findings) =
                retained_law_membership(artifact, self.base, RETAINED_SCOPE_BUDGET)
        {
            out.extend(findings);
        }
        self.check_scope_bindings(law, artifact, out);
        let owners = interface
            .contract_owners(|name| self.layer.interfaces.iter().find(|i| &i.name == name));
        if !law
            .scope
            .object_types
            .iter()
            .any(|name| owners.iter().any(|i| i.name.as_str() == name))
            || !law.scope.lifecycle_states.is_empty()
            || law.timing.is_some()
        {
            out.push(finding(codes::UNKNOWN_BINDING, artifact, "/payload/scope", "interface law scope must bind its contract; lifecycle selectors and deadlines require a concrete contract"));
        }
        let Some(env) = self
            .env
            .and_then(|env| env.clone().bind_interface_self(&interface.name).ok())
        else {
            out.push(finding(
                codes::PREDICATE_TYPE,
                artifact,
                "/payload",
                "interface law environment cannot be derived",
            ));
            return;
        };
        if let Some(condition) = &law.condition {
            predicate(
                condition.as_str(),
                &env,
                artifact,
                "/payload/condition",
                out,
            );
        }
        for (index, exception) in law.exceptions.iter().enumerate() {
            predicate(
                exception.condition.as_str(),
                &env,
                artifact,
                &format!("/payload/exceptions/{index}/condition"),
                out,
            );
        }
        if let LawModality::Until { predicate: stop } = &law.modality {
            predicate(
                stop.as_str(),
                &env,
                artifact,
                "/payload/modality/predicate",
                out,
            );
        }
        self.check_interface_response(law, artifact, &owners, &env, out);
    }

    fn check_interface_response(
        &self,
        law: &LawIR,
        artifact: &ArtifactEnvelope,
        owners: &[&norn_ir::InterfaceDef],
        env: &ZenTypeEnv,
        out: &mut Vec<Diagnostic>,
    ) {
        match &law.response {
            LawResponse::Predicate { expression } => predicate(
                expression.as_str(),
                env,
                artifact,
                "/payload/response/expression",
                out,
            ),
            LawResponse::Invariant {
                object_type,
                invariant_id,
            } => {
                let invariant = owners
                    .iter()
                    .flat_map(|i| &i.invariants)
                    .find(|i| i.id.as_str() == invariant_id);
                if object_type != &law.subject || invariant.is_none() {
                    out.push(finding(
                        codes::UNKNOWN_BINDING,
                        artifact,
                        "/payload/response",
                        "invariant is not declared by the interface contract",
                    ));
                } else if let Some(invariant) = invariant {
                    predicate(
                        invariant.expression.as_str(),
                        env,
                        artifact,
                        "/payload/response",
                        out,
                    );
                }
            }
            LawResponse::Action { artifact_id } => {
                if !self.pool.iter().any(|a| {
                    a.artifact_id == *artifact_id
                        && a.kind() == ArtifactKind::Action
                        && pinned(artifact, a)
                }) {
                    out.push(finding(
                        codes::UNKNOWN_BINDING,
                        artifact,
                        "/payload/response",
                        "action response needs an exact pinned ActionIR dependency",
                    ));
                }
            }
            LawResponse::Transition { .. } => out.push(finding(
                codes::UNKNOWN_BINDING,
                artifact,
                "/payload/response",
                "interfaces do not declare lifecycle transition contracts",
            )),
        }
    }

    fn check_predicates(
        &self,
        law: &LawIR,
        artifact: &ArtifactEnvelope,
        subject: &crate::EffectiveObjectType,
        out: &mut Vec<Diagnostic>,
    ) {
        let bound_env = self
            .env
            .and_then(|env| env.clone().bind_self(&subject.name).ok());
        if let Some(env) = &bound_env {
            if let Some(condition) = &law.condition {
                predicate(condition.as_str(), env, artifact, "/payload/condition", out);
            }
            for (index, exception) in law.exceptions.iter().enumerate() {
                predicate(
                    exception.condition.as_str(),
                    env,
                    artifact,
                    &format!("/payload/exceptions/{index}/condition"),
                    out,
                );
            }
            if let LawModality::Until { predicate: stop } = &law.modality {
                predicate(
                    stop.as_str(),
                    env,
                    artifact,
                    "/payload/modality/predicate",
                    out,
                );
            }
            self.check_response(law, artifact, subject, env, out);
            if let Some(LawTiming::BeforeDeadline { property }) = &law.timing
                && !env
                    .record(subject.name.as_str())
                    .and_then(|r| r.fields.get(property))
                    .is_some_and(|f| {
                        matches!(f.ty, Type::Timestamp { .. })
                            && matches!(f.source, FieldSource::Property { read_only: false })
                    })
            {
                out.push(finding(
                    codes::UNKNOWN_BINDING,
                    artifact,
                    "/payload/timing/property",
                    "deadline must bind a required stored timestamp property of the subject; computed fields are unsupported",
                ));
            }
        } else {
            out.push(finding(
                codes::PREDICATE_TYPE,
                artifact,
                "/payload",
                "law's effective ZEN environment cannot be derived",
            ));
        }
    }

    fn check_response(
        &self,
        law: &LawIR,
        artifact: &ArtifactEnvelope,
        subject: &crate::EffectiveObjectType,
        env: &ZenTypeEnv,
        out: &mut Vec<Diagnostic>,
    ) {
        match &law.response {
            LawResponse::Predicate { expression } => predicate(
                expression.as_str(),
                env,
                artifact,
                "/payload/response/expression",
                out,
            ),
            LawResponse::Invariant {
                object_type,
                invariant_id,
            } => {
                let invariant = self
                    .layer
                    .object_types
                    .iter()
                    .filter(|t| subject.lineage.contains(&t.name))
                    .flat_map(|t| &t.invariants)
                    .chain(
                        self.layer
                            .interfaces
                            .iter()
                            .filter(|i| subject.interfaces.contains(&i.name))
                            .flat_map(|i| &i.invariants),
                    )
                    .find(|invariant| invariant.id.as_str() == invariant_id);
                if object_type != &law.subject || invariant.is_none() {
                    out.push(finding(
                        codes::UNKNOWN_BINDING,
                        artifact,
                        "/payload/response",
                        "invariant identity is absent from the subject's effective contract",
                    ));
                } else if let Some(invariant) = invariant {
                    predicate(
                        invariant.expression.as_str(),
                        env,
                        artifact,
                        "/payload/response",
                        out,
                    );
                }
            }
            LawResponse::Action { artifact_id } => {
                if !self.pool.iter().any(|a| {
                    &a.artifact_id == artifact_id
                        && a.kind() == ArtifactKind::Action
                        && pinned(artifact, a)
                }) {
                    out.push(finding(
                        codes::UNKNOWN_BINDING,
                        artifact,
                        "/payload/response",
                        "action response needs an exact pinned ActionIR dependency",
                    ));
                }
            }
            LawResponse::Transition {
                object_type,
                property,
                transition_id,
            } => {
                if object_type != &law.subject
                    || !subject.lifecycle(property).is_some_and(|m| {
                        m.lifecycle
                            .transitions
                            .iter()
                            .any(|t| t.id.as_str() == transition_id)
                    })
                {
                    out.push(finding(
                        codes::UNKNOWN_BINDING,
                        artifact,
                        "/payload/response",
                        "transition response must bind a subject lifecycle transition",
                    ));
                }
            }
        }
    }
}

fn pinned(owner: &ArtifactEnvelope, target: &ArtifactEnvelope) -> bool {
    crate::scope_conflict(&owner.scope, &target.scope).is_none()
        && owner.dependencies.iter().any(|d| {
            d.required
                && d.artifact_id == target.artifact_id
                && d.kind == target.kind()
                && d.version.as_ref() == Some(&target.version)
        })
}

fn predicate(
    source: &str,
    env: &ZenTypeEnv,
    artifact: &ArtifactEnvelope,
    pointer: &str,
    out: &mut Vec<Diagnostic>,
) {
    let result = Expression::compile(source)
        .map_err(|e| e.to_string())
        .and_then(|expr| {
            check_expression(&expr, env, &Type::Bool)
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        });
    if let Err(error) = result {
        out.push(finding(codes::PREDICATE_TYPE, artifact, pointer, error));
    }
}

/// Disjoint writer identities; artifact ids cannot collide with lifecycle coordinates.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LawWriter {
    /// A complete admitted behavior artifact (including cascaded/opaque effects).
    Artifact {
        /// The artifact identity, without delimiter interpretation.
        artifact_id: String,
    },
    /// One transition of an effective, possibly inherited lifecycle.
    Transition {
        /// Effective concrete type.
        object_type: String,
        /// Lifecycle property.
        property: String,
        /// Declared transition id.
        transition: String,
    },
}
impl LawWriter {
    /// Tagged wire identity, with independently encoded components.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Artifact { artifact_id } => {
                serde_json::json!({"kind":"artifact","artifactId":artifact_id})
            }
            Self::Transition {
                object_type,
                property,
                transition,
            } => {
                serde_json::json!({"kind":"transition","objectType":object_type,"property":property,"transition":transition})
            }
        }
    }
    /// Conservative UTF-8/JSON-escaping allocation bound, before copying any id.
    #[must_use]
    pub fn encoded_byte_bound(&self) -> usize {
        let lengths = match self {
            Self::Artifact { artifact_id } => artifact_id.len(),
            Self::Transition {
                object_type,
                property,
                transition,
            } => object_type
                .len()
                .saturating_add(property.len())
                .saturating_add(transition.len()),
        };
        lengths.saturating_mul(6).saturating_add(128)
    }
}

/// Conservative invalidation inventory. Every artifact is retained: hidden/opaque writers
/// cannot disappear merely because their write set is not syntactically visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LawClosure {
    /// Exact artifact identities, including read-only base and laws.
    pub artifacts: BTreeSet<(String, String)>,
    /// Every action/decision/connector/mapping and effective transition.
    pub writers: BTreeSet<LawWriter>,
    /// Exact-byte full-envelope digests; same-version content edits invalidate evidence too.
    pub content_digests: BTreeMap<String, String>,
    /// Runtime entrances requiring later verification contracts; never certified here.
    pub open_boundaries: BTreeSet<String>,
}

/// Compute bounded whole-set invalidation via the shared graph reachability library.
/// The pool must be the complete effective set, not merely declared law dependencies.
/// `registry` must be the admission registry, so registry-typed entities resolve.
/// # Errors
/// Refuses duplicate/missing nodes, absent required dependencies, exhausted budgets
/// or an entity type that does not resolve, whose writers would otherwise be omitted.
pub fn conservative_closure(
    law: &ArtifactEnvelope,
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    budget: Budget,
) -> Result<LawClosure, norn_graph::Error<String>> {
    if pool.len() > budget.max_nodes {
        return Err(norn_graph::Error::BudgetExceeded(
            norn_graph::BudgetExceeded {
                resource: norn_graph::Limit::Nodes,
                limit: budget.max_nodes,
            },
        ));
    }
    let nodes: Vec<_> = pool.iter().map(|a| a.artifact_id.as_str()).collect();
    let by_id: BTreeMap<_, _> = pool.iter().map(|a| (a.artifact_id.as_str(), a)).collect();
    let mut edges = Vec::new();
    for a in pool {
        // Whole-set invalidation is deliberately broader than field-level influence.
        if edges.len() >= budget.max_edges {
            return Err(norn_graph::Error::BudgetExceeded(
                norn_graph::BudgetExceeded {
                    resource: norn_graph::Limit::Edges,
                    limit: budget.max_edges,
                },
            ));
        }
        edges.push((law.artifact_id.as_str(), a.artifact_id.as_str()));
        for d in &a.dependencies {
            if let Some(target) = by_id.get(d.artifact_id.as_str()) {
                if d.version.as_ref() != Some(&target.version) || d.kind != target.kind() {
                    return Err(norn_graph::Error::UnknownNode(d.artifact_id.clone()));
                }
                if edges.len() >= budget.max_edges {
                    return Err(norn_graph::Error::BudgetExceeded(
                        norn_graph::BudgetExceeded {
                            resource: norn_graph::Limit::Edges,
                            limit: budget.max_edges,
                        },
                    ));
                }
                edges.push((a.artifact_id.as_str(), target.artifact_id.as_str()));
            } else if d.required {
                return Err(norn_graph::Error::UnknownNode(d.artifact_id.clone()));
            }
        }
    }
    let reached = reachability(
        &Graph { nodes, edges },
        &[law.artifact_id.as_str()],
        Direction::Downstream,
        budget,
    )
    .map_err(owned_graph_error)?;
    let content_digests = pool
        .iter()
        .map(|a| {
            crate::obligations::exact_artifact_digest(a)
                .map(|digest| (a.artifact_id.clone(), digest))
                .map_err(|_hash_error| norn_graph::Error::Invariant)
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let artifacts = pool
        .iter()
        .filter(|a| reached.contains(a.artifact_id.as_str()))
        .map(|a| (a.artifact_id.clone(), a.version.clone()))
        .collect();
    let writers = writer_inventory(pool, registry)?;
    Ok(LawClosure {
        artifacts,
        writers,
        content_digests,
        open_boundaries: [
            "observe",
            "reconciliation",
            "migration",
            "bootstrap",
            "restore",
            "opaque_effects",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect(),
    })
}

fn writer_inventory(
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<BTreeSet<LawWriter>, norn_graph::Error<String>> {
    let mut writers: BTreeSet<_> = pool
        .iter()
        .filter(|a| {
            matches!(
                a.kind(),
                ArtifactKind::Action
                    | ArtifactKind::Rule
                    | ArtifactKind::Connector
                    | ArtifactKind::Mapping
            )
        })
        .map(|a| LawWriter::Artifact {
            artifact_id: a.artifact_id.clone(),
        })
        .collect();
    let catalog = EffectiveCatalog::from_artifacts_with_registry(pool, registry);
    for artifact in pool {
        // An unresolved entity fails closed: dropping it would omit its transitions.
        let object_type = match catalog.object_type(&artifact.artifact_id) {
            None => {
                if let ArtifactPayload::Entity(payload) = &artifact.payload {
                    // Catalog-only entity artifacts define no object. Malformed or
                    // unavailable object definitions must not disappear here.
                    if !matches!(crate::entity_object_type(payload), Ok(None)) {
                        return Err(norn_graph::Error::UnknownNode(artifact.artifact_id.clone()));
                    }
                }
                continue;
            }
            Some(Ok(object_type)) => object_type,
            Some(Err(_unresolved)) => {
                return Err(norn_graph::Error::UnknownNode(artifact.artifact_id.clone()));
            }
        };
        for lifecycle in &object_type.lifecycles {
            for transition in &lifecycle.lifecycle.transitions {
                writers.insert(LawWriter::Transition {
                    object_type: object_type.name.as_str().to_owned(),
                    property: lifecycle.lifecycle.property.as_str().to_owned(),
                    transition: transition.id.as_str().to_owned(),
                });
            }
        }
    }
    Ok(writers)
}

// Graphs borrow names: a large artifact id is never copied once per edge.
// Only an actual refusal is projected back to the public owned diagnostic type.
fn owned_graph_error(error: norn_graph::Error<&str>) -> norn_graph::Error<String> {
    use norn_graph::Error;
    match error {
        Error::BudgetExceeded(limit) => Error::BudgetExceeded(limit),
        Error::DuplicateNode(node) => Error::DuplicateNode(node.to_owned()),
        Error::UnknownNode(node) => Error::UnknownNode(node.to_owned()),
        Error::MissingValue(node) => Error::MissingValue(node.to_owned()),
        Error::NegativeValue(node) => Error::NegativeValue(node.to_owned()),
        Error::Cycle(nodes) => Error::Cycle(nodes.into_iter().map(str::to_owned).collect()),
        Error::NotTree => Error::NotTree,
        Error::Arithmetic(error) => Error::Arithmetic(error),
        Error::Invariant => Error::Invariant,
    }
}
