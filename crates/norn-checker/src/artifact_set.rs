//! The artifact-set resolver (T1-CHK-5, #2387): the first T3 step. It turns a
//! candidate set of artifacts, checked against its installed base, into a
//! **resolved set** — every reference pinned to exactly one artifact — or
//! reports why it cannot.
//!
//! # Identity and closure
//!
//! An artifact's identity is its `(artifactId, version)` pair. A resolved set
//! holds at most one version of each artifact id, and no candidate redefines
//! an artifact id of the installed base: base artifacts (registry packages,
//! already-installed layers) are read-only (2026-09-25 ruling §27).
//!
//! References are the envelope's `dependencies` and, for a composition
//! `business_pack`, its `payload.artifacts` members. Versions are **exact**:
//! the 2026-08-27 T3 tier asks for "exact dependency/version closure", so a
//! reference resolves only to the artifact with the same id *and* version, in
//! the candidate set or the base. Registry packages are semver-versioned, but
//! choosing a version from a range is the registry's install step, and semver
//! monotonicity is a T4 (BAC plan) rule; neither is resolution, so no semver
//! ordering is needed here.
//!
//! A required reference must resolve. An optional (`required: false`)
//! dependency may be absent, but when an artifact with its id is present it
//! must match the pinned version and kind. Malformed references (blank id,
//! missing or blank version) are T2 findings of the per-artifact check
//! (`ir.validation.invalid_dependency`) and are skipped here, as is an
//! artifact's same-version *dependency* on itself, which the per-artifact
//! check also rejects. A composition pack that lists itself (its own id and
//! version) among its `payload.artifacts` has no per-artifact check, so the
//! resolver reports it as a one-artifact [`codes::DEPENDENCY_CYCLE`] at that
//! member: a pack that contains itself has no installation order, exactly
//! like a longer cycle.
//!
//! # Kind compatibility
//!
//! A reference names the kind of its target, which must be the resolved
//! artifact's kind ([`codes::KIND_MISMATCH`]), and the referring kind must be
//! allowed to reference that kind at all ([`may_reference`],
//! [`codes::KIND_INCOMPATIBLE`]). Each row follows from what the kind
//! *is* in the 2026-09-25 ruling (§3 decisions, §4 emergent flows, §5 the four
//! verbs):
//!
//! | from | may reference | why |
//! | --- | --- | --- |
//! | `entity` | `entity` | The data model: supertypes, interfaces, link targets, and property type catalogs are all `entity` artifacts. It references nothing behavioural. |
//! | `relation` | `entity`, `relation` | A relation (v1) joins entity types; it may refine another relation. |
//! | `derived` | `entity`, `relation`, `derived` | A pure view over source facts and other views; no IO, so no actions, connectors, or rules. |
//! | `setting` | `entity`, `relation`, `setting` | A typed configuration value, possibly per entity type or layered on another setting; it does not behave. |
//! | `rule` | `entity`, `relation`, `derived`, `setting`, `rule`, `action`, `connector`, `mapping` | A pure decision. Declared guarded source commands (#3115) bind a connector and mapping; graph command records only activate these capabilities. Other external commands continue through actions. |
//! | `action` | `entity`, `relation`, `derived`, `setting`, `rule`, `policy`, `connector`, `action` | A command handler: it writes facts, is guarded by rules and policies, reads views and settings, performs through a connector binding (§5 `perform`), and may invoke other actions. |
//! | `connector` | `entity`, `relation`, `setting`, `rule`, `action`, `connector`, `mapping` | Lowers to ordinary actions (AGENTS.md): it observes facts into entities and relations (§5 `observe`), maps with rules or mappings, binds actions it performs, and may build on another connector. |
//! | `policy` | `entity`, `relation`, `derived`, `setting`, `rule`, `action`, `policy` | Authorizes actions on entities and relations, with context from views, settings, and rules; it may compose other policies. |
//! | `widget` | `entity`, `relation`, `derived`, `setting`, `rule`, `action`, `policy`, `widget` | A read surface over the model that invokes actions (never raw mutations), shows policy outcomes, and may embed widgets. No connectors: a widget never reaches outside the kernel. |
//! | `customization` | every kind but `business_pack` | A constrained overlay on any definition; packs are composed, not customized. |
//! | `business_pack` | every kind | A pack composes any artifact, including other packs. |
//! | `source_schema` | nothing | Discovery evidence (T8-SRC-1): what a source declares and what Norn measured. |
//! | `mapping` | `entity`, `source_schema` | Maps one source schema onto interfaces and object types (T8-MAP-1a); it binds its source by hash and names its targets by artifact id. |
//!
//! # Cycles
//!
//! Candidates must form a dependency DAG, so the resolved set has an
//! installation order (dependencies first). Each strongly connected group of
//! candidates is reported once as [`codes::DEPENDENCY_CYCLE`]. Inheritance and
//! inverse-link cycles between entity definitions are name-level rules of the
//! entity model and are reported by the per-kind set check below.
//!
//! # Scope compatibility
//!
//! Scope fails closed (2026-08-27 ruling: org and tenant scope are explicit at
//! every boundary). An artifact sees another when every scope dimension the
//! referenced artifact sets (`orgId`, `tenantId`, `workspaceId`,
//! `environmentId`, `businessUnitId`) is set to the same value on the
//! referring artifact ([`scope_conflict`]): a narrower artifact may reference
//! a broader one, never the reverse or a sibling. So a tenant layer sees the
//! registry layer it builds on, but a registry artifact never references a
//! tenant's, one tenant never references another's, and no reference crosses
//! an org or environment. After resolution every resolved reference is
//! checked ([`codes::INCOMPATIBLE_SCOPE`]); the action set check applies the
//! same rule to the object types an action writes
//! ([`crate::action_set`]).
//!
//! # Per-kind set checks
//!
//! [`check_artifact_set`] is the recommended entry point: it resolves the set
//! and, when resolution succeeds, runs the set checks over it —
//! [`crate::check_entity_artifacts`] (per-artifact checks plus the `EntityIR`
//! v2 whole-model check against the same base), the scope compatibility of
//! every resolved reference, the `RuleIR` v2 sub-decision closure check
//! ([`crate::jdm_profile::check_sub_decision_closures`]), the `ActionIR`
//! effect and write-set check ([`crate::action_set::check_action_set`]), and
//! the lifecycle writes of decide graphs
//! ([`crate::lifecycle_writes::check_lifecycle_write_set`]), and the source
//! schema and mapping check ([`crate::mapping::check_mapping_artifacts`]). When
//! resolution fails it reports the resolution findings with the per-artifact
//! checks only, because set semantics over an unresolved set are not
//! meaningful.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, DiagnosticRemediation, DiagnosticSeverity,
    PackArtifactKind, PackArtifactRef, ScopeRef,
};

use crate::action_set::check_action_set;
use crate::entity_artifacts::{EntityArtifactSetContext, check_entity_artifacts, dedupe_findings};
use crate::graph::{cycle_groups, topological_order};
use crate::jdm_profile::check_sub_decision_closures;
use crate::lifecycle_writes::check_lifecycle_write_set;
use crate::{Diagnostic, DiagnosticCode, Span};

/// Diagnostic codes emitted by [`resolve_artifact_set`].
pub mod codes {
    /// Two candidates share an artifact id and version.
    pub const DUPLICATE_ARTIFACT: &str = "artifact_set.resolution.duplicate_artifact";
    /// Two candidates share an artifact id with different versions.
    pub const CONFLICTING_VERSION: &str = "artifact_set.resolution.conflicting_version";
    /// A candidate redefines an artifact id of the read-only installed base.
    pub const SHADOWS_BASE: &str = "artifact_set.resolution.shadows_base";
    /// A required reference names an artifact id neither the set nor the
    /// base contains.
    pub const MISSING_DEPENDENCY: &str = "artifact_set.resolution.missing_dependency";
    /// A reference's artifact id is present, but not at the pinned version.
    pub const VERSION_MISMATCH: &str = "artifact_set.resolution.version_mismatch";
    /// A reference declares a kind other than the resolved artifact's kind.
    pub const KIND_MISMATCH: &str = "artifact_set.resolution.kind_mismatch";
    /// The referring artifact's kind may not reference the declared kind
    /// (see [`super::may_reference`]).
    pub const KIND_INCOMPATIBLE: &str = "artifact_set.resolution.kind_incompatible";
    /// Candidates depend on each other in a cycle.
    pub const DEPENDENCY_CYCLE: &str = "artifact_set.resolution.dependency_cycle";
    /// A reference resolves to an artifact whose scope the referring
    /// artifact cannot see (see [`super::scope_conflict`]).
    pub const INCOMPATIBLE_SCOPE: &str = "artifact_set.scope.incompatible_reference";
}

/// A candidate artifact set whose references all resolve.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedArtifactSet<'a> {
    /// Every candidate artifact in installation order: each artifact after
    /// the candidates it references, ties broken by artifact id.
    pub artifacts: Vec<&'a ArtifactEnvelope>,
    /// The installed base artifacts the candidates reference directly, in
    /// `(artifactId, version)` order, each once.
    pub base_dependencies: Vec<&'a ArtifactEnvelope>,
}

/// Whether an artifact of kind `from` may reference (depend on or, for a
/// business pack, contain) an artifact of kind `to`. See the module docs.
#[must_use]
pub const fn may_reference(from: &ArtifactKind, to: &ArtifactKind) -> bool {
    use ArtifactKind::{
        Action, BusinessPack, Claim, Connector, Customization, Derived, Entity, Law, Mapping,
        Policy, Relation, Rule, Ruling, Setting, SourceSchema, Widget,
    };
    match from {
        Entity => matches!(to, Entity),
        Relation => matches!(to, Entity | Relation),
        Derived => matches!(to, Entity | Relation | Derived),
        Setting => matches!(to, Entity | Relation | Setting),
        Rule => matches!(
            to,
            Entity | Relation | Derived | Setting | Rule | Action | Connector | Mapping
        ),
        Action => matches!(
            to,
            Entity | Relation | Derived | Setting | Rule | Policy | Connector | Action
        ),
        Connector => matches!(
            to,
            Entity | Relation | Setting | Rule | Action | Connector | Mapping
        ),
        Policy => matches!(
            to,
            Entity | Relation | Derived | Setting | Rule | Action | Policy
        ),
        Widget => matches!(
            to,
            Entity | Relation | Derived | Setting | Rule | Action | Policy | Widget
        ),
        Customization => !matches!(to, BusinessPack),
        BusinessPack => true,
        SourceSchema => false,
        Mapping => matches!(to, Entity | SourceSchema),
        Law => matches!(
            to,
            Entity
                | Relation
                | Derived
                | Setting
                | Rule
                | Action
                | Policy
                | Connector
                | Mapping
                | Law
        ),
        Claim => matches!(
            to,
            Entity
                | Relation
                | Derived
                | Setting
                | Rule
                | Action
                | Policy
                | Connector
                | SourceSchema
                | Mapping
                | Law
                | Claim
        ),
        Ruling => !matches!(to, Widget | Customization | BusinessPack),
    }
}

/// Resolve `artifacts` against the installed, read-only `base`.
///
/// # Errors
///
/// Returns every resolution finding (duplicate or conflicting identities,
/// base shadowing, missing dependencies, version and kind mismatches, kind
/// incompatibility, and dependency cycles), each located at its artifact id
/// and a JSON pointer into that artifact's envelope. The per-artifact (T2)
/// checks are not run; see [`check_artifact_set`].
pub fn resolve_artifact_set<'a>(
    artifacts: &'a [ArtifactEnvelope],
    base: &'a [ArtifactEnvelope],
) -> Result<ResolvedArtifactSet<'a>, Vec<Diagnostic>> {
    let pools = Pools {
        artifacts,
        base,
        artifacts_by_id: index_by_id(artifacts),
        base_by_id: index_by_id(base),
    };
    let mut findings = identity_findings(artifacts, &pools.base_by_id);

    let mut edges: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut base_dependencies = BTreeSet::new();
    for (owner_index, owner) in artifacts.iter().enumerate() {
        let targets = edges.entry(owner_index).or_default();
        for reference in references(owner) {
            if !may_reference(&owner.kind(), &reference.kind) {
                findings.push(finding(
                    codes::KIND_INCOMPATIBLE,
                    format!(
                        "a {} artifact may not reference a {} artifact ({:?})",
                        kind_name(&owner.kind()),
                        kind_name(&reference.kind),
                        reference.artifact_id
                    ),
                    owner,
                    &format!("{}/kind", reference.pointer),
                ));
            }
            match pools.resolve(owner, &reference) {
                Ok(Some(Target::Candidate(index))) => {
                    if index != owner_index {
                        targets.insert(index);
                    } else if reference.pack_member {
                        findings.push(finding(
                            codes::DEPENDENCY_CYCLE,
                            format!(
                                "pack {:?} version {:?} lists itself as a member; a pack cannot contain itself",
                                owner.artifact_id, owner.version
                            ),
                            owner,
                            &reference.pointer,
                        ));
                    }
                }
                Ok(Some(Target::Base(index))) => {
                    base_dependencies.insert(index);
                }
                Ok(None) => {}
                Err(finding) => findings.push(*finding),
            }
        }
    }

    let (order, blocked) = installation_order(artifacts, &edges);
    findings.extend(cycle_findings(artifacts, &edges, &blocked));

    if !findings.is_empty() {
        return Err(findings);
    }
    let mut base_dependencies: Vec<&ArtifactEnvelope> = base_dependencies
        .into_iter()
        .filter_map(|index| base.get(index))
        .collect();
    base_dependencies.sort_by(|left, right| {
        (&left.artifact_id, &left.version).cmp(&(&right.artifact_id, &right.version))
    });
    base_dependencies.dedup_by(|left, right| {
        left.artifact_id == right.artifact_id && left.version == right.version
    });
    Ok(ResolvedArtifactSet {
        artifacts: order
            .into_iter()
            .filter_map(|index| artifacts.get(index))
            .collect(),
        base_dependencies,
    })
}

/// Resolve `artifacts` against `context.base` and run the per-kind set
/// checks over the resolved set: the recommended T3 entry point.
///
/// When resolution succeeds this is [`check_entity_artifacts`] over the set
/// (the per-artifact check of every artifact plus the `EntityIR` v2
/// whole-model check), the scope compatibility of every resolved reference,
/// the `RuleIR` v2 sub-decision closure check
/// ([`check_sub_decision_closures`]), the `ActionIR` effect and write-set
/// check ([`check_action_set`]), and the source schema and mapping check
/// ([`crate::mapping::check_mapping_artifacts`]); the resolved set holds exactly the
/// candidates, so they are checked in their given order. When resolution fails it returns the
/// per-artifact findings followed by the resolution findings, without set
/// checks. Findings identical in `(code, artifactId, pointer)` are reported
/// once; UI-law findings also distinguish their messages so each action effect
/// at a render site remains visible. An empty result means the set resolves and
/// is valid.
#[must_use]
pub fn check_artifact_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let mut layout_findings = Vec::new();
    let normalized_artifacts = normalize_artifacts(artifacts, &mut layout_findings);
    let normalized_base = normalize_artifacts(context.base, &mut layout_findings);
    let normalized_context = EntityArtifactSetContext {
        base: &normalized_base,
        ..*context
    };

    let findings = match resolve_artifact_set(&normalized_artifacts, &normalized_base) {
        Ok(_resolved) => check_entity_artifacts(&normalized_artifacts, &normalized_context)
            .into_iter()
            .chain(scope_findings(&normalized_artifacts, &normalized_base))
            .chain(check_sub_decision_closures(
                &normalized_artifacts,
                &normalized_base,
            ))
            .chain(check_action_set(&normalized_artifacts, &normalized_context))
            .chain(check_lifecycle_write_set(
                &normalized_artifacts,
                &normalized_context,
            ))
            .chain(crate::rule_bindings::check_rule_binding_set(
                &normalized_artifacts,
                &normalized_context,
            ))
            .chain(crate::source_commands::check_set(
                &normalized_artifacts,
                &normalized_context,
            ))
            .chain(crate::mapping::check_mapping_artifacts(
                &normalized_artifacts,
                &normalized_base,
            ))
            // Typed Widget UX references and expression contracts resolve
            // only after the candidate and installed-base artifact pool
            // is available.
            .chain(crate::widget_ux::check_widget_ux_set(
                &normalized_artifacts,
                &normalized_context,
            ))
            .chain(crate::markings::check_references(
                &normalized_artifacts,
                &normalized_base,
            ))
            .chain(crate::verification::check_law_set(
                &normalized_artifacts,
                &normalized_base,
                normalized_context.registry,
            ))
            .chain(crate::claims::check_claim_set(
                &normalized_artifacts,
                &normalized_base,
            ))
            .collect::<Vec<_>>(),
        Err(resolution) => normalized_artifacts
            .iter()
            .flat_map(crate::check_artifact)
            .chain(crate::markings::check_references(
                &normalized_artifacts,
                &normalized_base,
            ))
            .chain(resolution)
            .collect(),
    };
    dedupe_findings(findings.into_iter().chain(layout_findings))
}

fn normalize_artifacts(
    artifacts: &[ArtifactEnvelope],
    findings: &mut Vec<Diagnostic>,
) -> Vec<ArtifactEnvelope> {
    artifacts
        .iter()
        .map(
            |artifact| match crate::layout::normalize_artifact(artifact) {
                Ok((normalized, _layout)) => normalized,
                Err(layout_findings) => {
                    findings.extend(layout_findings);
                    artifact.clone()
                }
            },
        )
        .collect()
}

/// The first scope dimension, as `(wire name, referenced value, referring
/// value)`, that makes an artifact scoped `to` invisible from one scoped
/// `from`: a dimension `to` sets that `from` leaves unset or sets to another
/// value. `None` when `from` sees `to`. See the module docs.
#[must_use]
pub fn scope_conflict<'s>(
    from: &'s ScopeRef,
    to: &'s ScopeRef,
) -> Option<(&'static str, &'s str, Option<&'s str>)> {
    [
        ("orgId", &from.org_id, &to.org_id),
        ("tenantId", &from.tenant_id, &to.tenant_id),
        ("workspaceId", &from.workspace_id, &to.workspace_id),
        ("environmentId", &from.environment_id, &to.environment_id),
        (
            "businessUnitId",
            &from.business_unit_id,
            &to.business_unit_id,
        ),
    ]
    .into_iter()
    .find_map(|(name, from, to)| {
        let to = to.as_deref()?;
        (from.as_deref() != Some(to)).then_some((name, to, from.as_deref()))
    })
}

/// The finding for a reference from `owner` (at `pointer`) to `target` that
/// `owner`'s scope cannot see, if any.
pub(crate) fn scope_finding(
    owner: &ArtifactEnvelope,
    target: &ArtifactEnvelope,
    pointer: &str,
) -> Option<Diagnostic> {
    let (dimension, required, actual) = scope_conflict(&owner.scope, &target.scope)?;
    let actual = actual.map_or_else(|| "unset".to_owned(), |value| format!("{value:?}"));
    Some(Diagnostic {
        code: DiagnosticCode::new(codes::INCOMPATIBLE_SCOPE),
        severity: DiagnosticSeverity::Error,
        message: format!(
            "{:?} is scoped to {dimension} {required:?}, but the referring artifact's {dimension} is {actual}; an artifact may reference only artifacts of its own or a broader scope",
            target.artifact_id
        ),
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(owner.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::CheckScope),
    })
}

/// Scope compatibility of every resolved reference of every candidate.
fn scope_findings(artifacts: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    let pools = Pools {
        artifacts,
        base,
        artifacts_by_id: index_by_id(artifacts),
        base_by_id: index_by_id(base),
    };
    let mut findings = Vec::new();
    for owner in artifacts {
        for reference in references(owner) {
            let target = match pools.resolve(owner, &reference) {
                Ok(Some(Target::Candidate(index))) => artifacts.get(index),
                Ok(Some(Target::Base(index))) => base.get(index),
                Ok(None) | Err(_) => None,
            };
            if let Some(finding) =
                target.and_then(|target| scope_finding(owner, target, &reference.pointer))
            {
                findings.push(finding);
            }
        }
    }
    findings
}

/// One reference of an artifact, with the JSON pointer of the reference
/// object in its envelope.
struct Reference {
    artifact_id: String,
    kind: ArtifactKind,
    version: String,
    required: bool,
    /// A composition pack member rather than an envelope dependency.
    pack_member: bool,
    pointer: String,
}

/// Where a reference resolved.
enum Target {
    /// The candidate at this index.
    Candidate(usize),
    /// The installed base artifact at this index.
    Base(usize),
}

/// The candidate set and the installed base, indexed by artifact id.
struct Pools<'a> {
    artifacts: &'a [ArtifactEnvelope],
    base: &'a [ArtifactEnvelope],
    artifacts_by_id: BTreeMap<&'a str, Vec<usize>>,
    base_by_id: BTreeMap<&'a str, Vec<usize>>,
}

impl Pools<'_> {
    /// Resolve one reference of `owner`: `Ok(None)` for an absent optional
    /// dependency. Candidates resolve first (a candidate that redefines a
    /// base id is reported separately).
    fn resolve(
        &self,
        owner: &ArtifactEnvelope,
        reference: &Reference,
    ) -> Result<Option<Target>, Box<Diagnostic>> {
        let id = reference.artifact_id.as_str();
        let (in_set, pool, indices) = if let Some(indices) = self.artifacts_by_id.get(id) {
            (true, self.artifacts, indices)
        } else if let Some(indices) = self.base_by_id.get(id) {
            (false, self.base, indices)
        } else if reference.required {
            return Err(Box::new(finding(
                codes::MISSING_DEPENDENCY,
                format!(
                    "required {} {id:?} version {:?} is neither in the artifact set nor installed",
                    kind_name(&reference.kind),
                    reference.version
                ),
                owner,
                &reference.pointer,
            )));
        } else {
            return Ok(None);
        };
        let Some((index, target)) = indices
            .iter()
            .filter_map(|&index| pool.get(index).map(|target| (index, target)))
            .find(|(_, target)| target.version == reference.version)
        else {
            let available: Vec<&str> = indices
                .iter()
                .filter_map(|&index| pool.get(index))
                .map(|target| target.version.as_str())
                .collect();
            return Err(Box::new(finding(
                codes::VERSION_MISMATCH,
                format!(
                    "{id:?} is pinned at version {:?}, but resolves only at {available:?}",
                    reference.version
                ),
                owner,
                &format!("{}/version", reference.pointer),
            )));
        };
        if target.kind() != reference.kind {
            return Err(Box::new(finding(
                codes::KIND_MISMATCH,
                format!(
                    "{id:?} is referenced as a {} artifact but is a {} artifact",
                    kind_name(&reference.kind),
                    kind_name(&target.kind())
                ),
                owner,
                &format!("{}/kind", reference.pointer),
            )));
        }
        Ok(Some(if in_set {
            Target::Candidate(index)
        } else {
            Target::Base(index)
        }))
    }
}

/// The well-formed references of `artifact`: its dependencies and, for a
/// composition pack, its members. Malformed ones (blank id, missing or blank
/// version, an undecodable member list) and a same-version self-dependency
/// are T2 findings of the per-artifact check and are skipped.
fn references(artifact: &ArtifactEnvelope) -> Vec<Reference> {
    let dependencies =
        artifact
            .dependencies
            .iter()
            .enumerate()
            .filter_map(|(index, dependency)| {
                let version = dependency.version.clone()?;
                Some(Reference {
                    artifact_id: dependency.artifact_id.clone(),
                    kind: dependency.kind.clone(),
                    version,
                    required: dependency.required,
                    pack_member: false,
                    pointer: format!("/dependencies/{index}"),
                })
            });
    let members = pack_members(artifact)
        .into_iter()
        .enumerate()
        .map(|(index, member)| Reference {
            artifact_id: member.artifact_id,
            kind: pack_kind(member.kind),
            version: member.version,
            required: true,
            pack_member: true,
            pointer: format!("/payload/artifacts/{index}"),
        });
    dependencies
        .chain(members)
        .filter(|reference| {
            // A same-version self-dependency is a T2 finding; a pack listing
            // itself is kept and reported as a cycle by the resolver.
            let self_dependency = !reference.pack_member
                && reference.artifact_id == artifact.artifact_id
                && reference.version == artifact.version;
            !self_dependency
                && !reference.artifact_id.trim().is_empty()
                && !reference.version.trim().is_empty()
        })
        .collect()
}

/// The `payload.artifacts` members of a composition `business_pack`.
fn pack_members(artifact: &ArtifactEnvelope) -> Vec<PackArtifactRef> {
    let ArtifactPayload::BusinessPack(payload) = &artifact.payload else {
        return Vec::new();
    };
    payload
        .artifacts
        .as_value()
        .and_then(|members| {
            serde_json::to_value(members)
                .and_then(serde_json::from_value)
                .ok()
        })
        .unwrap_or_default()
}

const fn pack_kind(kind: PackArtifactKind) -> ArtifactKind {
    match kind {
        PackArtifactKind::Entity => ArtifactKind::Entity,
        PackArtifactKind::Relation => ArtifactKind::Relation,
        PackArtifactKind::Action => ArtifactKind::Action,
        PackArtifactKind::Rule => ArtifactKind::Rule,
        PackArtifactKind::Connector => ArtifactKind::Connector,
        PackArtifactKind::Widget => ArtifactKind::Widget,
        PackArtifactKind::Customization => ArtifactKind::Customization,
        PackArtifactKind::Setting => ArtifactKind::Setting,
        PackArtifactKind::BusinessPack => ArtifactKind::BusinessPack,
        PackArtifactKind::Policy => ArtifactKind::Policy,
        PackArtifactKind::SourceSchema => ArtifactKind::SourceSchema,
        PackArtifactKind::Mapping => ArtifactKind::Mapping,
        PackArtifactKind::Law => ArtifactKind::Law,
        PackArtifactKind::Claim => ArtifactKind::Claim,
        PackArtifactKind::Ruling => ArtifactKind::Ruling,
    }
}

/// The wire name of a kind (`business_pack`).
const fn kind_name(kind: &ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Entity => "entity",
        ArtifactKind::Relation => "relation",
        ArtifactKind::Action => "action",
        ArtifactKind::Rule => "rule",
        ArtifactKind::Connector => "connector",
        ArtifactKind::Widget => "widget",
        ArtifactKind::Customization => "customization",
        ArtifactKind::Setting => "setting",
        ArtifactKind::BusinessPack => "business_pack",
        ArtifactKind::Policy => "policy",
        ArtifactKind::Derived => "derived",
        ArtifactKind::SourceSchema => "source_schema",
        ArtifactKind::Mapping => "mapping",
        ArtifactKind::Law => "law",
        ArtifactKind::Claim => "claim",
        ArtifactKind::Ruling => "ruling",
    }
}

/// Artifact indices by artifact id, in input order.
fn index_by_id(artifacts: &[ArtifactEnvelope]) -> BTreeMap<&str, Vec<usize>> {
    let mut out: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        out.entry(artifact.artifact_id.as_str())
            .or_default()
            .push(index);
    }
    out
}

/// Duplicate and conflicting candidate identities (each repeat after the
/// first occurrence of its id), and candidates that redefine a base id.
fn identity_findings(
    artifacts: &[ArtifactEnvelope],
    base_by_id: &BTreeMap<&str, Vec<usize>>,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    let mut first: BTreeMap<&str, &ArtifactEnvelope> = BTreeMap::new();
    for artifact in artifacts {
        let id = artifact.artifact_id.as_str();
        match first.get(id) {
            Some(earlier) if earlier.version == artifact.version => findings.push(finding(
                codes::DUPLICATE_ARTIFACT,
                format!(
                    "{id:?} version {:?} appears more than once in the artifact set",
                    artifact.version
                ),
                artifact,
                "/artifactId",
            )),
            Some(earlier) => findings.push(finding(
                codes::CONFLICTING_VERSION,
                format!(
                    "{id:?} is in the artifact set at version {:?} and at version {:?}; a resolved set holds one version of each artifact",
                    earlier.version, artifact.version
                ),
                artifact,
                "/version",
            )),
            None => {
                first.insert(id, artifact);
                if base_by_id.contains_key(id) {
                    findings.push(finding(
                        codes::SHADOWS_BASE,
                        format!(
                            "{id:?} is an installed base artifact, which is read-only; give the definition its own artifact id"
                        ),
                        artifact,
                        "/artifactId",
                    ));
                }
            }
        }
    }
    findings
}

/// Kahn's algorithm over the candidate dependency graph (`edges` maps each
/// candidate to the candidates it references). Returns the installation
/// order, ties broken by artifact id, and the candidates left over, which are
/// on or behind a cycle.
fn installation_order(
    artifacts: &[ArtifactEnvelope],
    edges: &BTreeMap<usize, BTreeSet<usize>>,
) -> (Vec<usize>, BTreeSet<usize>) {
    topological_order(edges, |index| {
        artifacts
            .get(index)
            .map_or("", |artifact| artifact.artifact_id.as_str())
    })
}

/// One finding per dependency cycle among the `blocked` candidates (those
/// Kahn's algorithm could not order). A cycle is a strongly connected group
/// ([`cycle_groups`]), reported at its member with the smallest artifact id,
/// on that member's first reference into the group.
fn cycle_findings(
    artifacts: &[ArtifactEnvelope],
    edges: &BTreeMap<usize, BTreeSet<usize>>,
    blocked: &BTreeSet<usize>,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for group in cycle_groups(edges, blocked) {
        let mut members: Vec<&ArtifactEnvelope> = group
            .iter()
            .filter_map(|&index| artifacts.get(index))
            .collect();
        members.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
        let Some(&anchor) = members.first() else {
            continue;
        };
        let pointer = references(anchor)
            .into_iter()
            .find(|reference| {
                let own = reference.artifact_id == anchor.artifact_id
                    && reference.version == anchor.version;
                !own && members.iter().any(|member| {
                    member.artifact_id == reference.artifact_id
                        && member.version == reference.version
                })
            })
            .map_or_else(|| "/dependencies".to_owned(), |reference| reference.pointer);
        let names: Vec<&str> = members
            .iter()
            .map(|member| member.artifact_id.as_str())
            .collect();
        findings.push(finding(
            codes::DEPENDENCY_CYCLE,
            format!(
                "artifacts {names:?} depend on each other in a cycle; the set has no installation order"
            ),
            anchor,
            &pointer,
        ));
    }
    findings
}

fn finding(code: &str, message: String, artifact: &ArtifactEnvelope, pointer: &str) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::ResolveDependency),
    }
}
