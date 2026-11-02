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
        activation: text("At every accepted measurement", "عند كل قياس مقبول"),
        component: text("Its measurements in one unit", "قياساتها بوحدة واحدة"),
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

/// LAW-CONVERSION-OVERLAY: the Core 06 validator preserves retained core authority.
fn overlay_cannot_weaken(artifacts: &[ArtifactEnvelope]) -> DemoResult<String> {
    use norn_checker::obligations::{RetainedLayer, RetainedLayers};
    use norn_ir::{CustomizationOverlayOp, CustomizationRoot};
    let registry = BTreeSet::new();
    let exact = source(artifacts)?;
    let native: Vec<_> = artifacts
        .iter()
        .map(norn_ir::runtime::encode)
        .collect::<Result<_, _>>()?;
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&native, &[])?,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )?;
    if !checked.receipt.admitted {
        return Err("retained demo source refused".into());
    }
    let receipt = String::from_utf8(checked.receipt.canonical_json()?)?;
    let receipt_sha256 = checked.receipt.content_sha256()?;
    let retained = RetainedLayers::verify(
        &[RetainedLayer {
            source: &exact,
            source_sha256: exact.exact_digest(),
            receipt: &receipt,
            receipt_sha256: &receipt_sha256,
        }],
        &registry,
    )?;
    let target = retained
        .artifacts()
        .iter()
        .find(|artifact| artifact.artifact_id == "demo.Melt.entity")
        .ok_or("missing retained Melt")?;
    let pin = norn_checker::ArtifactRef::of(target)?;
    let law = retained
        .artifacts()
        .iter()
        .find(|artifact| artifact.artifact_id == "core.law.conversion_conservation")
        .ok_or("missing retained core law")?;
    let law_pin = norn_checker::ArtifactRef::of(law)?;
    let mut envelope: ArtifactEnvelope = serde_json::from_str(include_str!(
        "../../../fixtures/ir/retail-sku-reorder-point.customization.json"
    ))?;
    envelope.artifact_id = "demo.Melt.loss_bound.customization".into();
    envelope.namespace = "demo".into();
    envelope.name = "Melt loss bound".into();
    envelope.dependencies.clear();
    envelope.scope = target.scope.clone();
    let mut overlay: CustomizationRoot =
        serde_json::from_value(serde_json::to_value(envelope.payload.to_json_value())?)?;
    overlay.customization_id = "demo.Melt.loss_bound".into();
    overlay.target_artifact.artifact_id = target.artifact_id.clone();
    overlay.target_artifact.version = target.version.clone();
    overlay.target_artifact.additional = std::collections::BTreeMap::from([(
        "sha256".into(),
        norn_ir::JsonValue::String(pin.sha256),
    )]);
    let addition = std::collections::BTreeMap::from([
        (
            "id".into(),
            norn_ir::JsonValue::String("demo_loss_bound".into()),
        ),
        (
            "expression".into(),
            norn_ir::JsonValue::String("self.loss_quantity <= 1".into()),
        ),
    ]);
    let base = serde_json::to_string(retained.artifacts())?;
    let input = |overlay: &CustomizationRoot| -> DemoResult<DefinitionSource> {
        let mut candidate = envelope.clone();
        candidate.payload = ArtifactPayload::from_kind_json(
            &ArtifactKind::Customization,
            serde_json::from_str(&serde_json::to_string(overlay)?)?,
        )?;
        Ok(DefinitionSource::parse(
            &serde_json::to_string(&[candidate])?,
            &base,
        )?)
    };
    overlay.overlay.operations = vec![CustomizationOverlayOp::AddValidation {
        additional: addition,
    }];
    let added = input(&overlay)?;
    let plan = norn_checker::overlay::overlay_obligations(&added, &retained, &target.scope)
        .map_err(|error| format!("additive overlay: {} / {}", error.code(), error.reason()))?;
    if plan.retained_sha256() != retained.exact_digest()
        || plan.source_sha256() != added.exact_digest()
        || !plan.goals().any(|(_, guards)| guards.len() > 1)
        || !plan.laws().iter().any(|instance| {
            instance.law_digest == law_pin.sha256
                && instance.subject.artifact.artifact_id == target.artifact_id
        })
    {
        return Err("additive overlay did not retain core guard goals".into());
    }
    overlay.overlay.operations = vec![CustomizationOverlayOp::UpdateValidation {
        additional: std::collections::BTreeMap::from([
            (
                "id".into(),
                norn_ir::JsonValue::String("conversion_conservation".into()),
            ),
            (
                "expression".into(),
                norn_ir::JsonValue::String("true".into()),
            ),
        ]),
    }];
    let refusal = match norn_checker::overlay::overlay_obligations(
        &input(&overlay)?,
        &retained,
        &target.scope,
    ) {
        Ok(_) => return Err("overlay replaced the retained conversion invariant".into()),
        Err(refusal) => refusal,
    };
    if refusal.code() != "core.composition.overlay_conflict"
        || refusal.reason() != "weakens_inherited"
    {
        return Err("unexpected weakening refusal".into());
    }
    Ok(format!(
        "{} / {}; additive guards remain pending solver/progress evidence",
        refusal.code(),
        refusal.reason()
    ))
}

fn main() -> DemoResult<()> {
    let mut arguments = std::env::args().skip(1);
    let challenge_only = match (arguments.next().as_deref(), arguments.next()) {
        (None, None) => false,
        (Some("--challenge-json"), None) => true,
        _ => return Err("usage: jewelry_demo [--challenge-json]".into()),
    };
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
    let exact = source(&artifacts)?;
    let authority = ApprovedContract::approve(
        &exact,
        &registry,
        json!({"expression":"norn-zen/6","execution":"unqualified"}),
        json!({"scope":"local admission example, not deployment authority"}),
        json!({"required":"unknown; no solver result supplied"}),
    )?;
    let challenge = authority.challenge(&exact, &registry)?;
    if challenge_only {
        writeln!(stdout, "{}", serde_json::to_string(challenge.body())?)?;
        return Ok(());
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
    writeln!(
        stdout,
        "CURRENT: weakening overlay refused: {}",
        overlay_cannot_weaken(&artifacts)?
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
