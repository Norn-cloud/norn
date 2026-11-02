//! Admission-only jewelry fallback. Run through scripts/demo/run.sh.
use std::{
    collections::BTreeSet,
    error::Error,
    io::{Read as _, Write as _},
};

use norn_checker::obligations::{ApprovedContract, ChallengeError, DefinitionSource};
use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::deontic::{RequirementText, RequirementWording};
use norn_ir::{ArtifactEnvelope, ArtifactKind, ArtifactPayload, LawIR, LawResponse};
use serde_json::json;

type DemoResult<T> = Result<T, Box<dyn Error>>;

// TARGET: the profile lacks structured bilingual authoring; https://github.com/Norn-cloud/norn/issues/58
fn target_wording() -> RequirementWording {
    let text = |en: &str, ar: &str| RequirementText {
        en: en.into(),
        ar: ar.into(),
    };
    RequirementWording {
        scope: text("Each melt in this app", "كل عملية صهر في هذا التطبيق"),
        activation: text("When the melt is posted", "عندما تكون عملية الصهر مُرحَّلة"),
        component: text(
            "Its fine-metal mass measurements",
            "قياسات كتلة المعدن الخالص للعملية",
        ),
        response: text(
            "Input fine mass equals output fine mass plus recorded fine-metal loss",
            "كتلة المعدن الخالص الداخلة تساوي مجموع كتلته الخارجة وكتلة الفاقد المُسجَّل منه",
        ),
    }
}

fn authored_law(artifacts: &[ArtifactEnvelope]) -> DemoResult<LawIR> {
    let envelope = artifacts
        .iter()
        .find(|a| {
            a.artifact_id == "demo.fine_mass_conservation.law" && a.kind() == ArtifactKind::Law
        })
        .ok_or("missing TS-authored conservation law")?;
    Ok(serde_json::from_value(serde_json::to_value(
        envelope.payload.to_json_value(),
    )?)?)
}

fn refusal_probe(
    artifacts: &[ArtifactEnvelope],
    predicate: &str,
) -> DemoResult<Vec<ArtifactEnvelope>> {
    let mut edited = artifacts.to_vec();
    let mut law = authored_law(artifacts)?;
    let LawResponse::Predicate { expression } = &mut law.response else {
        return Err("refusal probe requires the authored predicate law".into());
    };
    expression.0 = predicate.into();
    let envelope = edited
        .iter_mut()
        .find(|a| a.artifact_id == "demo.fine_mass_conservation.law")
        .ok_or("missing authored law for refusal probe")?;
    envelope.payload = ArtifactPayload::from_kind_json(
        &ArtifactKind::Law,
        serde_json::from_str(&serde_json::to_string(&law)?)?,
    )?;
    Ok(edited)
}

fn source(artifacts: &[ArtifactEnvelope]) -> DemoResult<DefinitionSource> {
    Ok(DefinitionSource::parse(
        &serde_json::to_string(artifacts)?,
        "[]",
    )?)
}

fn main() -> DemoResult<()> {
    let mut stdout = std::io::stdout().lock();
    let mut input = String::new();
    std::io::stdin()
        .take(1_048_577)
        .read_to_string(&mut input)?;
    if input.len() > 1_048_576 {
        return Err("demo input exceeds 1 MiB".into());
    }
    let exact = DefinitionSource::parse(&input, "[]")?;
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(&input)?;
    let law = authored_law(&artifacts)?;
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let diagnostics = check_artifact_set(&artifacts, &context);
    if !diagnostics.is_empty() {
        return Err(format!("admission: {diagnostics:?}").into());
    }
    writeln!(
        stdout,
        "CURRENT: admitted measurement schema, lifecycle and pointwise law (no runtime claim)"
    )?;
    writeln!(stdout, "{}", serde_json::to_string_pretty(&artifacts)?)?;
    writeln!(
        stdout,
        "TARGET (#58): reviewed EN/AR wording for {} (not the pending #322 renderer):",
        law.id
    )?;
    writeln!(
        stdout,
        "{}",
        serde_json::to_string_pretty(&target_wording())?
    )?;
    let authority = ApprovedContract::approve(
        &exact,
        &registry,
        json!({"expression":"norn-zen/6","execution":"unqualified"}),
        json!({"scope":"local admission example, not deployment authority"}),
        json!({"required":"unknown; no solver result supplied"}),
    )?;
    let challenge = authority.challenge(&exact, &registry)?;
    writeln!(
        stdout,
        "Checker-generated obligations {}; no handwritten theorem or proof:",
        challenge.sha256()
    )?;
    writeln!(
        stdout,
        "{}",
        serde_json::to_string_pretty(challenge.body())?
    )?;
    let edited = source(&refusal_probe(&artifacts, "true")?)?;
    if authority.challenge(&edited, &registry).err() != Some(ChallengeError::LawChanged) {
        return Err("protected-law edit did not refuse".into());
    }
    writeln!(
        stdout,
        "CURRENT: protected-law edit refused: LawChanged (challenge boundary only)"
    )?;
    let refused = check_artifact_set(&refusal_probe(&artifacts, "self.missing == 1")?, &context);
    if refused.is_empty() {
        return Err("invalid property binding was admitted".into());
    }
    writeln!(stdout, "CURRENT: invalid law diagnostics: {refused:?}")?;
    Ok(())
}
