//! PROOF-BIND-1: inventory admitted pack laws; generate goals, never proof badges.
use std::{
    collections::BTreeSet,
    error::Error,
    io::{Read as _, Write as _},
};

use norn_checker::obligations::{ApprovedContract, DefinitionSource};
use norn_checker::{ArtifactRef, EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, ArtifactKind};
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    std::io::stdin()
        .take(8_388_609)
        .read_to_string(&mut input)?;
    if input.len() > 8_388_608 {
        return Err("pack inventory exceeds 8 MiB".into());
    }
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(&input)?;
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let diagnostics = check_artifact_set(&artifacts, &context);
    if diagnostics.iter().any(norn_checker::Diagnostic::is_error) {
        return Err(format!("pack admission refused: {diagnostics:?}").into());
    }
    let source = DefinitionSource::parse(&input, "[]")?;
    let contract = ApprovedContract::approve(
        &source,
        &registry,
        json!({"expression":"norn-zen/6","execution":"unqualified"}),
        json!({"scope":"read-only pack law inventory"}),
        json!({"required":"unknown; no checked solver results"}),
    )?;
    let challenge = contract.challenge(&source, &registry)?;
    let laws = artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Law)
        .map(|artifact| {
            let pin = ArtifactRef::of(artifact)?;
            Ok(
                json!({"pin": {"artifactId":pin.artifact_id, "version":pin.version,
                "sha256":pin.sha256}, "law": artifact.payload.to_json_value()}),
            )
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string(&json!({
            "authority":"projection_only", "laws":laws,
            "challenge":challenge.body(), "sourceDigest":source.exact_digest()
        }))?
    )?;
    Ok(())
}
