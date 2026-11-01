//! Admission-only jewelry fallback. Run through scripts/demo/run.sh.
use std::{
    collections::BTreeSet,
    error::Error,
    io::{Read as _, Write as _},
};

use norn_checker::obligations::{ApprovedContract, ChallengeError, DefinitionSource};
use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::deontic::{RequirementText, RequirementWording};
use norn_ir::{ArtifactEnvelope, ArtifactKind, ArtifactPayload, LawIR, LawResponse, ZenExpression};
use serde_json::json;

type DemoResult<T> = Result<T, Box<dyn Error>>;

fn conservation() -> DemoResult<LawIR> {
    let artifact: serde_json::Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.law.conversion_conservation.law.json"
    ))?;
    Ok(serde_json::from_value(
        artifact
            .get("payload")
            .ok_or("missing law payload")?
            .clone(),
    )?)
}

fn wording() -> RequirementWording {
    let text = |en: &str, ar: &str| RequirementText {
        en: en.into(),
        ar: ar.into(),
    };
    RequirementWording {
        scope: text("Each core conversion", "كل عملية تحويل"),
        activation: text("When its state is posted", "عندما تكون حالتها مرحلة"),
        component: text(
            "Its fine-metal measurements",
            "قياسات المعدن الخالص الخاصة بها",
        ),
        response: text(
            "Input equals output plus recorded loss",
            "المدخل يساوي المخرج مع الفاقد المسجل",
        ),
    }
}

fn with_law(types: &[ArtifactEnvelope], law: &LawIR) -> DemoResult<Vec<ArtifactEnvelope>> {
    let mut envelope: ArtifactEnvelope = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.law.conversion_conservation.law.json"
    ))?;
    envelope.payload = ArtifactPayload::from_kind_json(
        &ArtifactKind::Law,
        serde_json::from_str(&serde_json::to_string(law)?)?,
    )?;
    let mut artifacts = types.to_vec();
    artifacts.push(envelope);
    Ok(artifacts)
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
    let mut types: Vec<ArtifactEnvelope> = serde_json::from_str(&input)?;
    for document in [
        include_str!("../../../registry/core/artifacts/core.Conversion.interface.json"),
        include_str!("../../../registry/core/artifacts/std.value_types.entity.json"),
    ] {
        types.push(serde_json::from_str(document)?);
    }
    let law = conservation()?;
    let artifacts = with_law(&types, &law)?;
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
        "CURRENT: admitted Melt inherits the unchanged core conversion law (measurement scope)"
    )?;
    writeln!(stdout, "{}", serde_json::to_string_pretty(&artifacts)?)?;
    writeln!(
        stdout,
        "EN/AR reviewed wording slots for {} (display data, not the pending renderer):",
        law.id
    )?;
    writeln!(stdout, "{}", serde_json::to_string_pretty(&wording())?)?;
    let exact = source(&artifacts)?;
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
    let mut weakened = law.clone();
    weakened.response = LawResponse::Predicate {
        expression: ZenExpression("true".into()),
    };
    let edited = source(&with_law(&types, &weakened)?)?;
    if authority.challenge(&edited, &registry).err() != Some(ChallengeError::LawChanged) {
        return Err("protected-law edit did not refuse".into());
    }
    writeln!(
        stdout,
        "CURRENT: protected-law edit refused: LawChanged (challenge boundary only)"
    )?;
    let mut invalid = law;
    invalid.response = LawResponse::Predicate {
        expression: ZenExpression("self.missing == 1".into()),
    };
    let refused = check_artifact_set(&with_law(&types, &invalid)?, &context);
    if refused.is_empty() {
        return Err("invalid property binding was admitted".into());
    }
    writeln!(stdout, "CURRENT: invalid law diagnostics: {refused:?}")?;
    Ok(())
}
