//! Artifact-set obligations of claims and rulings (norn#62, Astra verdict
//! 2026-10-30), evaluated over the definition catalog
//! ([`crate::definition_catalog`]).
//!
//! - **References bind.** Every member a claim or ruling names exists in the
//!   catalog at the exact version it pins, and every claim a ruling resolves
//!   is in the set.
//! - **A derived state has no declared transition into it** (norn#59). A
//!   `derived_state` claim makes every declared transition targeting that
//!   state an error on the entity that declares it. No ruling is consulted:
//!   the definition must change.
//! - **Every known divergence cites a ratified ruling**: an `approved` or
//!   `active` ruling in the set resolves the claim's exact version, is not
//!   superseded by another ratified ruling, and governs at least one of the
//!   definitions the witness depends on (a mapping it observes through, or
//!   the target).
//! - **A ruling never waives a contradiction.** The witness of every
//!   divergence is rechecked: each observation's record runs through the
//!   *admitted* version of its mapping ([`crate::mapping::compile_mapping_artifact`],
//!   the same compilation and application the runtime uses), and if both
//!   mappings can apply to one object (their scopes are not disjoint) and
//!   still produce different values of the target, the contradiction
//!   persists whatever rulings cite it. A witness that cannot be evaluated
//!   fails closed.

use std::collections::BTreeSet;

use norn_ir::definition_catalog::{ArtifactKey, DefinitionCatalog, Member, MemberRef};
use norn_ir::mapping::MappingIr;
use norn_ir::provenance::UtcTimestamp;
use norn_ir::ruling::{ClaimIr, ClaimStatement, DivergenceWitness, Observation, RulingIr};
use norn_ir::{ArtifactEnvelope, ArtifactLifecycle, ArtifactPayload, RuntimeValue};

use crate::artifact_set::ResolvedArtifactSet;
use crate::mapping::{SourceDocument, compile_mapping_artifact};
use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

/// Stable codes of the claim and ruling set obligations.
pub mod codes {
    /// A claim names a member the catalog does not hold at that version.
    pub const CLAIM_UNKNOWN_MEMBER: &str = "claim.reference.unknown_member";
    /// A ruling names a member the catalog does not hold at that version.
    pub const RULING_UNKNOWN_MEMBER: &str = "ruling.reference.unknown_member";
    /// A ruling resolves a claim version the set does not contain.
    pub const RULING_UNKNOWN_CLAIM: &str = "ruling.reference.unknown_claim";
    /// A declared transition targets a state a claim says is derived.
    pub const DERIVED_STATE_TRANSITION: &str = "catalog.derived_state.declared_transition";
    /// A divergence no ratified ruling resolves.
    pub const UNRULED_DIVERGENCE: &str = "ruling.obligation.unruled_divergence";
    /// A divergence's witness still contradicts under the admitted mappings.
    pub const CONTRADICTION_PERSISTS: &str = "ruling.obligation.contradiction_persists";
    /// A divergence's witness cannot be evaluated.
    pub const WITNESS_UNEVALUABLE: &str = "ruling.obligation.witness_unevaluable";
    /// The catalog of the set could not be projected.
    pub const CATALOG_UNAVAILABLE: &str = "ruling.obligation.catalog_unavailable";
}

/// The recording time of witness evaluation; nothing reads it back.
const WITNESS_TIME: &str = "1970-01-01T00:00:00Z";

struct Claim<'a> {
    artifact: &'a ArtifactEnvelope,
    claim: ClaimIr,
}

struct Ruling<'a> {
    artifact: &'a ArtifactEnvelope,
    ruling: RulingIr,
}

fn key(artifact: &ArtifactEnvelope) -> ArtifactKey {
    ArtifactKey {
        artifact_id: artifact.artifact_id.clone(),
        version: artifact.version.clone(),
    }
}

fn finding(code: &str, artifact: &ArtifactEnvelope, pointer: &str, message: String) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

fn payload_json(artifact: &ArtifactEnvelope) -> serde_json::Value {
    serde_json::to_value(artifact.payload.to_json_value()).unwrap_or_default()
}

/// The obligations of the claims and rulings among `artifacts` (the
/// candidates) and `base`. See the module docs. Malformed claims and rulings
/// are T2 findings and are skipped here.
#[must_use]
pub fn check_claim_set(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let pool: Vec<&ArtifactEnvelope> = artifacts.iter().chain(base).collect();
    let claims: Vec<Claim<'_>> = pool
        .iter()
        .filter(|artifact| matches!(artifact.payload, ArtifactPayload::Claim(_)))
        .filter_map(|artifact| {
            let claim = serde_json::from_value(payload_json(artifact)).ok()?;
            Some(Claim { artifact, claim })
        })
        .collect();
    let rulings: Vec<Ruling<'_>> = pool
        .iter()
        .filter(|artifact| matches!(artifact.payload, ArtifactPayload::Ruling(_)))
        .filter_map(|artifact| {
            let ruling = serde_json::from_value(payload_json(artifact)).ok()?;
            Some(Ruling { artifact, ruling })
        })
        .collect();
    if claims.is_empty() && rulings.is_empty() {
        return Vec::new();
    }
    // Every base artifact is visible to the rules, not only the ones the
    // candidates depend on: a claim may be about any installed definition.
    let set = ResolvedArtifactSet {
        artifacts: artifacts.iter().collect(),
        base_dependencies: base.iter().collect(),
    };
    let catalog = match crate::definition_catalog::project(&set, base) {
        Ok(catalog) => catalog,
        Err(error) => {
            let owner = claims
                .first()
                .map(|claim| claim.artifact)
                .or_else(|| rulings.first().map(|ruling| ruling.artifact));
            return owner
                .map(|owner| {
                    finding(
                        codes::CATALOG_UNAVAILABLE,
                        owner,
                        "",
                        format!("claims cannot be checked without the catalog: {error}"),
                    )
                })
                .into_iter()
                .collect();
        }
    };
    let mut out = Vec::new();
    for claim in &claims {
        out.extend(claim_references(claim, &catalog));
        match &claim.claim.statement {
            ClaimStatement::DerivedState { state, .. } => {
                out.extend(derived_state(claim, state, &catalog, &pool));
            }
            ClaimStatement::Divergence { witness } => {
                out.extend(divergence(claim, witness, &rulings, artifacts, base));
            }
        }
    }
    for ruling in &rulings {
        out.extend(ruling_references(ruling, &catalog, &claims));
    }
    out
}

fn claim_references(claim: &Claim<'_>, catalog: &DefinitionCatalog) -> Vec<Diagnostic> {
    let mut members: Vec<(String, &MemberRef)> = Vec::new();
    match &claim.claim.statement {
        ClaimStatement::DerivedState { state, from } => {
            members.push(("/payload/statement/state".to_owned(), state));
            members.extend(
                from.iter()
                    .enumerate()
                    .map(|(index, member)| (format!("/payload/statement/from/{index}"), member)),
            );
        }
        ClaimStatement::Divergence { witness } => {
            members.push((
                "/payload/statement/witness/target".to_owned(),
                &witness.target,
            ));
        }
    }
    members
        .into_iter()
        .filter(|(_, member)| !catalog.contains(member))
        .map(|(pointer, member)| {
            finding(
                codes::CLAIM_UNKNOWN_MEMBER,
                claim.artifact,
                &pointer,
                format!(
                    "{} of {}@{} is not a member of the admitted set",
                    member.member.id(),
                    member.artifact_id,
                    member.version
                ),
            )
        })
        .collect()
}

fn ruling_references(
    ruling: &Ruling<'_>,
    catalog: &DefinitionCatalog,
    claims: &[Claim<'_>],
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (index, member) in ruling.ruling.affects.iter().enumerate() {
        if !catalog.contains(member) {
            out.push(finding(
                codes::RULING_UNKNOWN_MEMBER,
                ruling.artifact,
                &format!("/payload/affects/{index}"),
                format!(
                    "{} of {}@{} is not a member of the admitted set",
                    member.member.id(),
                    member.artifact_id,
                    member.version
                ),
            ));
        }
    }
    for (index, resolved) in ruling.ruling.resolves.iter().enumerate() {
        if !claims.iter().any(|claim| &key(claim.artifact) == resolved) {
            out.push(finding(
                codes::RULING_UNKNOWN_CLAIM,
                ruling.artifact,
                &format!("/payload/resolves/{index}"),
                format!(
                    "claim {}@{} is not in the set",
                    resolved.artifact_id, resolved.version
                ),
            ));
        }
    }
    out
}

/// norn#59: no declared transition may target a derived state.
fn derived_state(
    claim: &Claim<'_>,
    state: &MemberRef,
    catalog: &DefinitionCatalog,
    pool: &[&ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let Member::State {
        type_name,
        lifecycle,
        state: target,
    } = &state.member
    else {
        return Vec::new();
    };
    catalog
        .transitions
        .iter()
        .filter(|row| {
            row.at.artifact_id == state.artifact_id
                && row.at.version == state.version
                && &row.to == target
                && matches!(&row.at.member, Member::Transition { type_name: t, lifecycle: l, .. }
                    if t == type_name && l == lifecycle)
        })
        .filter_map(|row| {
            let owner = pool.iter().find(|artifact| {
                artifact.artifact_id == row.at.artifact_id && artifact.version == row.at.version
            })?;
            let writers: Vec<&str> = catalog
                .writes
                .iter()
                .filter(|write| write.target == row.at)
                .map(|write| write.writer.artifact_id.as_str())
                .collect();
            Some(finding(
                codes::DERIVED_STATE_TRANSITION,
                owner,
                "/payload/objectType/lifecycles",
                format!(
                    "{} declares a transition into {target:?}, which claim {} says is derived; derive the state from its facts instead of authoring it (written by {writers:?})",
                    row.at.member.id(),
                    claim.artifact.artifact_id
                ),
            ))
        })
        .collect()
}

const fn ratified(lifecycle: &ArtifactLifecycle) -> bool {
    matches!(
        lifecycle,
        ArtifactLifecycle::Approved | ArtifactLifecycle::Active
    )
}

fn divergence(
    claim: &Claim<'_>,
    witness: &DivergenceWitness,
    rulings: &[Ruling<'_>],
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let pool: Vec<&ArtifactEnvelope> = artifacts.iter().chain(base).collect();
    let admitted = |observation: &Observation| {
        pool.iter()
            .copied()
            .find(|artifact| artifact.artifact_id == observation.mapping.artifact_id)
    };
    let mut out = Vec::new();
    // What a resolving ruling must govern: the admitted observed mappings,
    // or the target's declaring artifact.
    let mut governed: BTreeSet<ArtifactKey> = [&witness.left, &witness.right]
        .into_iter()
        .filter_map(|observation| admitted(observation).map(key))
        .collect();
    governed.insert(ArtifactKey {
        artifact_id: witness.target.artifact_id.clone(),
        version: witness.target.version.clone(),
    });
    if !ruled(&key(claim.artifact), &governed, rulings) {
        out.push(finding(
            codes::UNRULED_DIVERGENCE,
            claim.artifact,
            "/payload/statement",
            "no ratified ruling resolves this divergence and governs a definition its witness depends on".to_owned(),
        ));
    }
    let Member::Property {
        type_name,
        property,
    } = &witness.target.member
    else {
        return out;
    };
    let mut values = Vec::new();
    for (side, observation) in [("left", &witness.left), ("right", &witness.right)] {
        let pointer = format!("/payload/statement/witness/{side}");
        let Some(mapping) = admitted(observation) else {
            out.push(finding(
                codes::WITNESS_UNEVALUABLE,
                claim.artifact,
                &pointer,
                format!(
                    "mapping {} is not in the set",
                    observation.mapping.artifact_id
                ),
            ));
            return out;
        };
        match observe(mapping, observation, (type_name, property), artifacts, base) {
            Ok(value) => values.push((mapping, value)),
            Err(reason) => {
                out.push(finding(
                    codes::WITNESS_UNEVALUABLE,
                    claim.artifact,
                    &pointer,
                    reason,
                ));
                return out;
            }
        }
    }
    if let [(left, left_value), (right, right_value)] = values.as_slice()
        && left_value != right_value
        && !crate::ruling::disjoint(&left.scope, &right.scope)
    {
        out.push(finding(
            codes::CONTRADICTION_PERSISTS,
            claim.artifact,
            "/payload/statement/witness",
            format!(
                "{}@{} maps {property:?} to {left_value:?} and {}@{} to {right_value:?} for the same object; a ruling never waives a contradiction, so change a mapping, scope, parameter, or the theory",
                left.artifact_id, left.version, right.artifact_id, right.version
            ),
        ));
    }
    out
}

/// Whether a ratified, unsuperseded ruling resolves `claim` and governs one
/// of the `governed` artifacts.
fn ruled(claim: &ArtifactKey, governed: &BTreeSet<ArtifactKey>, rulings: &[Ruling<'_>]) -> bool {
    let superseded: BTreeSet<ArtifactKey> = rulings
        .iter()
        .filter(|ruling| ratified(&ruling.artifact.lifecycle))
        .filter_map(|ruling| ruling.ruling.supersedes.clone())
        .collect();
    rulings.iter().any(|ruling| {
        ratified(&ruling.artifact.lifecycle)
            && !superseded.contains(&key(ruling.artifact))
            && ruling.ruling.resolves.contains(claim)
            && ruling.ruling.affects.iter().any(|member| {
                governed.contains(&ArtifactKey {
                    artifact_id: member.artifact_id.clone(),
                    version: member.version.clone(),
                })
            })
    })
}

/// The value `mapping`'s object map gives the target `(type, property)` for
/// the observed record. A map that produces another type, or no value for the
/// property, cannot witness anything.
fn observe(
    mapping: &ArtifactEnvelope,
    observation: &Observation,
    (type_name, property): (&str, &str),
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Result<RuntimeValue, String> {
    let document: MappingIr = serde_json::from_value(payload_json(mapping))
        .map_err(|error| format!("{} is not a mapping: {error}", mapping.artifact_id))?;
    let map = document.map(&observation.map).ok_or_else(|| {
        format!(
            "{}@{} has no object map {:?}",
            mapping.artifact_id, mapping.version, observation.map
        )
    })?;
    let compiled = compile_mapping_artifact(mapping, artifacts, base).map_err(|findings| {
        format!(
            "{}@{} does not compile ({} findings)",
            mapping.artifact_id,
            mapping.version,
            findings.len()
        )
    })?;
    let record = serde_json::to_string(&observation.record).map_err(|error| error.to_string())?;
    let source = SourceDocument::from_json(map.source.dataset.clone(), &record, None)
        .map_err(|error| format!("the witness record is not canonical JSON: {error}"))?;
    let records = compiled
        .apply(&[source], &UtcTimestamp(WITNESS_TIME.to_owned()))
        .map_err(|findings| {
            format!(
                "the witness record does not map ({} findings)",
                findings.len()
            )
        })?;
    let mapped = records
        .iter()
        .find(|record| record.map == observation.map)
        .ok_or_else(|| {
            format!(
                "map {:?} does not apply to the witness record",
                observation.map
            )
        })?;
    if mapped.object_type.as_str() != type_name {
        return Err(format!(
            "map {:?} produces {}, not the witness target {type_name}",
            observation.map,
            mapped.object_type.as_str()
        ));
    }
    mapped
        .invocations
        .iter()
        .find_map(|invocation| invocation.input.get(property).cloned())
        .ok_or_else(|| {
            format!(
                "map {:?} gives no value for {property:?} of the witness record",
                observation.map
            )
        })
}
