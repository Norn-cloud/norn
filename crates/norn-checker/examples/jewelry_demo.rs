//! Admission-only jewelry fallback. Run through scripts/demo/run.sh.
use std::{
    collections::BTreeSet,
    error::Error,
    io::{Read as _, Write as _},
};

use norn_checker::obligations::{ApprovedContract, ChallengeError, DefinitionSource};
use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::deontic::{
    DeonticModality, DeonticRule, RequirementText, RequirementTiming, RequirementWording,
    RuleSourceSpan, StructuredRequirement,
};
use norn_ir::requirement_display::{RequirementLocale, RequirementView};
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

// TARGET #58: structured bilingual authoring still needs a Rust adapter to the compiled TS law.
fn target_requirement(law: LawIR) -> StructuredRequirement {
    StructuredRequirement {
        norm: DeonticRule {
            source: RuleSourceSpan {
                artifact_id: format!("{}.law", law.id),
                pointer: "/payload".into(),
                bytes: None,
            },
            authority_id: law.owner.clone(),
            authority_version: "0.1.0".into(),
            law,
            modality: DeonticModality::Obligation,
            exceptions: vec![],
            defeated_by: vec![],
        },
        timing: RequirementTiming::EveryCommittedState,
        wording: target_wording(),
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
    let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(&input)?;
    artifacts.push(serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.units.json"
    ))?);
    let exact = source(&artifacts)?;
    let law = authored_law(&artifacts)?;
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let diagnostics = check_artifact_set(&artifacts, &context);
    if diagnostics.iter().any(norn_checker::Diagnostic::is_error) {
        return Err(format!("admission: {diagnostics:?}").into());
    }
    writeln!(
        stdout,
        "CURRENT: admitted measurement schema, lifecycle and pointwise law (no runtime claim)"
    )?;
    writeln!(stdout, "{}", serde_json::to_string_pretty(&artifacts)?)?;
    writeln!(
        stdout,
        "TARGET (#58): Rust bilingual authoring adapter for {}; CURRENT: real EN/AR views (display only):",
        law.id
    )?;
    let requirement = target_requirement(law);
    for locale in [RequirementLocale::En, RequirementLocale::Ar] {
        let view = RequirementView {
            requirement: requirement.clone(),
            locale,
        };
        writeln!(stdout, "{locale:?} direction={}", view.direction())?;
        for slot in view.slots()? {
            writeln!(stdout, "{} | {}: {}", slot.id, slot.label, slot.text)?;
        }
    }
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
    if !refused.iter().any(norn_checker::Diagnostic::is_error) {
        return Err("invalid property binding was admitted".into());
    }
    writeln!(stdout, "CURRENT: invalid law diagnostics: {refused:?}")?;
    Ok(())
}
