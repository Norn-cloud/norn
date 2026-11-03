//! SR1/LAW-NORMATIVE-1: project a structured Law artifact through the B0 renderer.
use std::{
    error::Error,
    io::{Read as _, Write as _},
};

use norn_checker::{ArtifactRef, diagnostics_to_json};
use norn_ir::requirement_display::RequirementView;
use norn_ir::{ArtifactEnvelope, ArtifactKind, deontic::StructuredLaw};
use serde_json::{Value, json};

fn project(artifact: &ArtifactEnvelope) -> Result<Value, Box<dyn Error>> {
    if artifact.kind() != ArtifactKind::Law {
        return Err("expected a structured Law artifact".into());
    }
    let law: StructuredLaw =
        serde_json::from_value(serde_json::to_value(artifact.payload.to_json_value())?)?;
    let pin = ArtifactRef::of(artifact)?;
    let mut views = Vec::new();
    for locale in law.requirement.wording.0.keys() {
        let view = RequirementView {
            requirement: law.requirement.clone(),
            locale: locale.clone(),
        };
        let slots: Vec<_> = view
            .slots()?
            .into_iter()
            .map(|s| json!({"id":s.id,"label":s.label,"text":s.text}))
            .collect();
        views.push(json!({"locale":locale,"direction":view.direction()?,"slots":slots}));
    }
    Ok(
        json!({"authority":"projection_only", "admission":"unverified",
        "pin":{"artifactId":pin.artifact_id,"version":pin.version,"sha256":pin.sha256},
        "original":artifact,"views":views,
        "diagnostics":diagnostics_to_json(&norn_checker::verification::check_law_local(artifact))}),
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    std::io::stdin()
        .take(1_048_577)
        .read_to_string(&mut input)?;
    if input.len() > 1_048_576 {
        return Err("structured law exceeds 1 MiB".into());
    }
    let artifact: ArtifactEnvelope = serde_json::from_str(&input)?;
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string_pretty(&project(&artifact)?)?
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(grade: &str, root: &str, source: &str) -> Result<ArtifactEnvelope, Box<dyn Error>> {
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json"))?;
        let mut artifact = fixtures.into_iter().next().ok_or("missing law fixture")?;
        let mut law = artifact.get("payload").cloned().ok_or("missing payload")?;
        law.as_object_mut()
            .ok_or("law payload must be an object")?
            .remove("timing");
        *law.get_mut("modality").ok_or("modality")? =
            json!({"kind":if grade == "prohibition" { "never" } else { "always" }});
        *law.pointer_mut("/scope/root/kind").ok_or("root kind")? = json!(root);
        let mut wording: Value =
            serde_json::from_str(include_str!("../../../fixtures/wording/locales.json"))?;
        for (tag, pack) in [("en", "pack"), ("ar", "حزمة")] {
            *wording
                .pointer_mut(&format!("/{tag}/terms"))
                .and_then(Value::as_object_mut)
                .ok_or("locale terms")?
                .entry("pack")
                .or_insert(Value::Null) = json!(pack);
        }
        artifact.as_object_mut().ok_or("envelope")?.insert(
            "payload".into(),
            json!({"requirement":{
                "norm":{"law":law,"modality":grade,"authorityId":source,"authorityVersion":"1",
                    "source":{"artifactId":source,"pointer":"/rules/0"}},
                "timing":"same_commit", "wording":wording
            }}),
        );
        Ok(serde_json::from_value(artifact)?)
    }

    #[test]
    fn structured_law_views_keep_all_grades_pack_roots_and_semantic_slot_ids()
    -> Result<(), Box<dyn Error>> {
        for grade in ["obligation", "permission", "prohibition"] {
            let artifact = artifact(grade, "pack", "fixture.source")?;
            let projected = project(&artifact)?;
            let views = projected
                .get("views")
                .and_then(Value::as_array)
                .ok_or("views")?;
            let ids = |view: &Value| -> Option<Vec<Value>> {
                view.get("slots")?
                    .as_array()?
                    .iter()
                    .map(|s| s.get("id").cloned())
                    .collect()
            };
            assert_eq!(views.len(), 2);
            assert_eq!(views.first().and_then(ids), views.get(1).and_then(ids));
            for view in views {
                let preferences = view["slots"]
                    .as_array()
                    .ok_or("slots")?
                    .iter()
                    .find(|slot| {
                        slot["id"]
                            .as_str()
                            .is_some_and(|id| id.ends_with("/norm/defeatedBy"))
                    })
                    .ok_or("preferences")?;
                assert_eq!(preferences["text"], "\u{2205}");
                let expected = if view["locale"] == "ar" { "rtl" } else { "ltr" };
                assert_eq!(view["direction"], expected);
            }
            assert_eq!(projected.get("admission"), Some(&json!("unverified")));
            assert_eq!(
                projected.get("original"),
                Some(&serde_json::to_value(&artifact)?)
            );
        }
        Ok(())
    }

    #[test]
    fn unsupported_structured_timing_has_no_rendered_success() -> Result<(), Box<dyn Error>> {
        let artifact = artifact("permission", "entity", "fixture.source")?;
        let mut wire = serde_json::to_value(artifact)?;
        *wire
            .pointer_mut("/payload/requirement/norm/law/modality")
            .ok_or("modality")? = json!({"kind":"eventually"});
        assert!(project(&serde_json::from_value(wire)?).is_err());
        Ok(())
    }

    proptest::proptest! {
        #[test]
        fn projection_cannot_replace_the_source_or_create_an_admission(source in "[a-zA-Z0-9_.]{1,32}") {
            let artifact = artifact("permission", "entity", &source).ok();
            proptest::prop_assert!(artifact.is_some());
            let view = artifact.as_ref().and_then(|a| project(a).ok());
            proptest::prop_assert!(view.is_some());
            let original = artifact.and_then(|a| serde_json::to_value(a).ok());
            proptest::prop_assert_eq!(view.as_ref().and_then(|v| v.get("original")).cloned(), original);
            proptest::prop_assert_eq!(view.as_ref().and_then(|v| v.get("admission")).and_then(Value::as_str), Some("unverified"));
        }
    }
}
