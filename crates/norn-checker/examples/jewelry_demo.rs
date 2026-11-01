//! Admission-only jewelry fallback. Run through scripts/demo/run.sh.
use std::{
    collections::BTreeSet,
    error::Error,
    io::{Read as _, Write as _},
};

use norn_checker::obligations::{ApprovedContract, ChallengeError, DefinitionSource};
use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::deontic::{RequirementText, RequirementWording};
use norn_ir::{
    ArtifactDependency, ArtifactEnvelope, ArtifactKind, ArtifactPayload, IrVersion01, LawIR,
    LawModality, LawProvenance, LawResponse, LawRoot, LawScope, LawSeverity, ProvenanceSource,
    ZenExpression,
};
use serde_json::json;

type DemoResult<T> = Result<T, Box<dyn Error>>;

fn conservation() -> LawIR {
    LawIR {
        law_version: IrVersion01::V0_1,
        id: "demo.fine_mass_conservation".into(),
        owner: "demo.jewelry".into(),
        severity: LawSeverity::Error,
        rationale: "Input fine mass equals output plus explicitly recorded loss.".into(),
        provenance: LawProvenance {
            source: ProvenanceSource::AuthoringSdk,
            source_path: Some("crates/norn-checker/examples/jewelry_demo.rs".into()),
            actor_id: None,
            created_at: None,
        },
        scope: LawScope {
            root: LawRoot::App("demo.jewelry".into()),
            object_types: vec!["demo.Melt".into()],
            lifecycle_states: vec![],
            commitments: vec![],
        },
        subject: "demo.Melt".into(),
        condition: Some(ZenExpression("self.status == 'posted'".into())),
        modality: LawModality::Always {},
        timing: None,
        response: LawResponse::Predicate {
            expression: ZenExpression(
                "self.input_fine_g == self.output_fine_g + self.loss_fine_g".into(),
            ),
        },
    }
}

fn wording() -> RequirementWording {
    let text = |en: &str, ar: &str| RequirementText {
        en: en.into(),
        ar: ar.into(),
    };
    RequirementWording {
        scope: text("Each melt in this app", "كل عملية صهر في هذا التطبيق"),
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
    let subject = types
        .iter()
        .find(|a| a.artifact_id == "demo.Melt.entity")
        .ok_or("missing authored demo.Melt.entity")?;
    let mut envelope = subject.clone();
    envelope.artifact_id = format!("{}.law", law.id);
    envelope.name.clone_from(&law.id);
    envelope.dependencies = vec![ArtifactDependency {
        artifact_id: subject.artifact_id.clone(),
        kind: ArtifactKind::Entity,
        version: Some(subject.version.clone()),
        required: true,
    }];
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
    let types: Vec<ArtifactEnvelope> = serde_json::from_str(&input)?;
    let law = conservation();
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
        "CURRENT: admitted measurement schema, lifecycle and pointwise law (no runtime claim)"
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
