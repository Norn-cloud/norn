//! Regression checks for conservative normative lowering.
use norn_checker::deontic::{LawLoweringRefusal, lower_deontic_law};
use norn_ir::LawModality;
use norn_ir::deontic::{DeonticModality, DeonticRule};
use serde_json::json;

fn obligation() -> Result<DeonticRule, serde_json::Error> {
    serde_json::from_value(json!({
        "law": {
            "lawVersion":"0.1", "id":"sales.approve", "owner":"policy-board",
            "severity":"error", "rationale":"Manager approval within 48 hours over 10000",
            "provenance":{"source":"authoring_sdk"},
            "scope":{"root":{"kind":"app","id":"sales"},"objectTypes":["commerce.SalesOrder"]},
            "subject":"commerce.SalesOrder", "condition":"self.total > 10000",
            "modality":{"kind":"eventually"},
            "timing":{"kind":"within","millis":"172800000"},
            "response":{"kind":"predicate","expression":"self.managerApproved == true"}
        },
        "modality":"obligation", "authorityId":"sales-board", "authorityVersion":"1",
        "source":{"artifactId":"sales.rules","pointer":"/rules/0","bytes":{"start":0,"end":48}}
    }))
}

#[test]
fn lowering_preserves_every_law_slot_and_source() {
    let rule = obligation().expect("typed fixture");
    let lowered = lower_deontic_law(&rule).expect("supported obligation");
    assert_eq!(lowered.law, rule.law);
    assert_eq!(lowered.source, rule.source);
    assert_eq!(lowered.authority_id, "sales-board");
    assert_eq!(lowered.authority_version, "1");
}

#[test]
fn permission_is_never_misrepresented_as_a_required_action() {
    let mut rule = obligation().expect("typed fixture");
    rule.modality = DeonticModality::Permission;
    assert_eq!(
        lower_deontic_law(&rule),
        Err(LawLoweringRefusal::Permission)
    );
}

#[test]
fn preference_and_exception_are_not_erased() {
    let mut rule = obligation().expect("typed fixture");
    rule.defeated_by.push("sales.emergency".into());
    assert_eq!(
        lower_deontic_law(&rule),
        Err(LawLoweringRefusal::Defeasible)
    );
    rule.defeated_by.clear();
    rule.exceptions.push(
        serde_json::from_value(json!({
            "id":"emergency", "condition":"self.emergency == true",
            "source":{"artifactId":"sales.rules","pointer":"/rules/0/exceptions/0"}
        }))
        .expect("exception"),
    );
    assert_eq!(
        lower_deontic_law(&rule),
        Err(LawLoweringRefusal::Defeasible)
    );
}

#[test]
fn prohibition_requires_never_and_no_deadline() {
    let mut rule = obligation().expect("typed fixture");
    rule.modality = DeonticModality::Prohibition;
    assert_eq!(
        lower_deontic_law(&rule),
        Err(LawLoweringRefusal::ModalityMismatch)
    );
    rule.law.modality = LawModality::Never {};
    assert_eq!(
        lower_deontic_law(&rule),
        Err(LawLoweringRefusal::ProhibitionTiming)
    );
    rule.law.timing = None;
    assert!(lower_deontic_law(&rule).is_ok());
}

#[test]
fn malformed_source_and_unknown_fields_are_refused() {
    let mut rule = obligation().expect("typed fixture");
    rule.source.bytes.as_mut().expect("range").start = 49;
    assert_eq!(
        lower_deontic_law(&rule),
        Err(LawLoweringRefusal::InvalidSource)
    );
    let mut wire = serde_json::to_value(obligation().expect("typed fixture")).expect("wire");
    wire["implicitOverride"] = json!(true);
    assert!(serde_json::from_value::<DeonticRule>(wire).is_err());
}

#[test]
fn l1_fragment_never_strengthens_eventually_or_erases_timing() {
    use norn_checker::deontic::lower_theorem_candidate;
    let mut rule = obligation().expect("typed fixture");
    assert_eq!(
        lower_theorem_candidate(&rule),
        Err(LawLoweringRefusal::UnsupportedProofFragment)
    );
    rule.law.timing = None;
    assert_eq!(
        lower_theorem_candidate(&rule),
        Err(LawLoweringRefusal::UnsupportedProofFragment)
    );
    rule.law.modality = LawModality::Always {};
    assert_eq!(
        lower_theorem_candidate(&rule).expect("candidate only").law,
        rule.law
    );
}

#[test]
fn source_pointers_obey_rfc6901_escaping() {
    let mut rule = obligation().expect("typed fixture");
    for pointer in ["/rules/~2", "/rules/~", "rules/0"] {
        rule.source.pointer = pointer.into();
        assert_eq!(
            lower_deontic_law(&rule),
            Err(LawLoweringRefusal::InvalidSource)
        );
    }
    for pointer in ["", "/rules/~0~1", "/rules/0"] {
        rule.source.pointer = pointer.into();
        assert!(lower_deontic_law(&rule).is_ok());
    }
}

#[allow(clippy::expect_used, reason = "test-only typed requirement fixture")]
fn requirement() -> norn_ir::deontic::StructuredRequirement {
    use norn_ir::deontic::{RequirementText, RequirementTiming, RequirementWording};
    let text = || RequirementText {
        en: "declared predicate".into(),
        ar: "المحمول المعلن".into(),
    };
    let mut norm = obligation().expect("fixture");
    norm.law.modality = LawModality::Always {};
    norm.law.timing = None;
    norm.law.condition = Some(norn_ir::ZenExpression("input.active".into()));
    norm.law.response = norn_ir::LawResponse::Predicate {
        expression: norn_ir::ZenExpression("input.quantity >= 0.00".into()),
    };
    norn_ir::deontic::StructuredRequirement {
        norm,
        timing: RequirementTiming::SameCommit,
        wording: RequirementWording {
            scope: text(),
            activation: text(),
            component: text(),
            response: text(),
        },
    }
}

#[allow(clippy::expect_used, reason = "test-only typed environment fixture")]
fn requirement_env() -> norn_expr::ZenTypeEnv {
    use norn_expr::{EntityDefinitions, EnvOptions, Type, ZenTypeEnv};
    ZenTypeEnv::from_artifact_set(&EntityDefinitions::default(), EnvOptions::default())
        .expect("environment")
        .bind_input(std::collections::BTreeMap::from([
            ("active".into(), Type::Bool),
            ("quantity".into(), Type::Number),
        ]))
        .expect("input")
}

#[test]
fn structured_safety_fragment_preserves_grades_timing_and_source() {
    use norn_checker::deontic::check_requirement;
    use norn_ir::deontic::{RequirementTiming, StructuredRequirement};
    for grade in [
        DeonticModality::Obligation,
        DeonticModality::Permission,
        DeonticModality::Prohibition,
    ] {
        for timing in [
            RequirementTiming::SameCommit,
            RequirementTiming::EveryCommittedState,
        ] {
            let mut r = requirement();
            r.norm.modality = grade;
            r.norm.law.modality = if grade == DeonticModality::Prohibition {
                LawModality::Never {}
            } else {
                LawModality::Always {}
            };
            r.timing = timing;
            check_requirement(&r, &requirement_env()).expect("structural only");
            let ir = serde_json::to_string(&r).expect("wire");
            assert_eq!(
                serde_json::from_str::<StructuredRequirement>(&ir).expect("typed"),
                r
            );
            if grade == DeonticModality::Permission {
                assert_eq!(
                    lower_deontic_law(&r.norm),
                    Err(LawLoweringRefusal::Permission)
                );
            }
        }
    }
}

#[test]
fn structured_fragment_refuses_progress_mismatches_and_untyped_predicates() {
    use norn_checker::deontic::{RequirementRefusal as R, check_requirement};
    let env = requirement_env();
    let mut r = requirement();
    for modality in [
        LawModality::Eventually {},
        LawModality::Until {
            predicate: norn_ir::ZenExpression("true".into()),
        },
    ] {
        r.norm.law.modality = modality;
        assert_eq!(check_requirement(&r, &env), Err(R::UnsupportedTiming));
    }
    r = requirement();
    r.norm.law.timing = Some(norn_ir::LawTiming::Within { millis: "1".into() });
    assert_eq!(
        check_requirement(&r, &env).expect_err("deadline").code(),
        "requirement.timing.unsupported"
    );
    r = requirement();
    r.norm.modality = DeonticModality::Prohibition;
    assert_eq!(check_requirement(&r, &env), Err(R::ModalityMismatch));
    for expression in [
        "input.quantity",
        "input.missing == 1",
        "input.quantity == 'kg'",
    ] {
        r = requirement();
        r.norm.law.condition = Some(norn_ir::ZenExpression(expression.into()));
        assert_eq!(check_requirement(&r, &env), Err(R::PredicateType));
    }
    r = requirement();
    r.wording.response.ar.clear();
    assert_eq!(check_requirement(&r, &env), Err(R::MissingTemplate));
    r = requirement();
    r.norm.law.response = norn_ir::LawResponse::Action {
        artifact_id: "future.action".into(),
    };
    assert_eq!(check_requirement(&r, &env), Err(R::UnsupportedResponse));
}

#[test]
fn structured_sources_feed_existing_precedence_without_a_new_hierarchy() {
    use norn_checker::norm_precedence::{GroundedNorm, check_norm_precedence};
    use std::collections::BTreeSet;
    let r = requirement();
    let grounded = GroundedNorm {
        id: r.norm.law.id,
        modality: r.norm.modality,
        authority: (r.norm.authority_id, r.norm.authority_version),
        authority_level: Some(1),
        enacted: 0,
        applicable: BTreeSet::from([0, 1]),
        excepted: BTreeSet::from([1]),
    };
    let contexts = check_norm_precedence(&[grounded], 2).expect("existing precedence");
    assert_eq!(contexts.first().expect("applicable").obligations, vec![0]);
    assert!(contexts.get(1).expect("excepted").survivors.is_empty());
}

#[test]
fn core_physics_bilingual_views_round_trip_one_semantic_target() {
    use norn_expr::{EntityDefinitions, EnvOptions, Type, ZenTypeEnv};
    use norn_ir::deontic::{RequirementText, RequirementTiming};
    use norn_ir::requirement_display::{RequirementLocale as L, RequirementView};
    use std::collections::BTreeMap;
    // Corpus inputs are already partitioned by the scope's dimension/currency.
    // This tests authoring/display, not aggregate-law admission or preservation.
    let corpus = include_str!("../../../docs/platform/structured-physics.tsv");
    let mut fields = BTreeMap::new();
    for name in "external loss available held funded ownerBefore ownerAfter".split_whitespace() {
        fields.insert(name.into(), Type::Number);
    }
    for name in "active recorded terminal open custodyRecorded edited deleted linked inverse"
        .split_whitespace()
    {
        fields.insert(name.into(), Type::Bool);
    }
    for name in ["inputs", "outputs"] {
        fields.insert(name.into(), Type::List(Box::new(Type::Number)));
    }
    let env = ZenTypeEnv::from_artifact_set(&EntityDefinitions::default(), EnvOptions::default())
        .expect("env")
        .bind_input(fields)
        .expect("typed corpus inputs");
    for row in corpus.lines() {
        let columns: [&str; 11] = row
            .split('\t')
            .collect::<Vec<_>>()
            .try_into()
            .expect("eleven typed slots");
        let [
            id,
            scope_en,
            scope_ar,
            component_en,
            component_ar,
            expression,
            en,
            ar,
            prohibited,
            when_en,
            when_ar,
        ] = columns;
        let prohibited = prohibited.parse::<bool>().expect("grade flag");
        let mut r = requirement();
        r.norm.law.id = format!("core.{id}");
        r.norm.law.rationale = en.into();
        r.norm.authority_id = "core.physics".into();
        r.norm.source.artifact_id = "core.physics.conformance".into();
        r.norm.source.pointer = format!("/requirements/{id}");
        r.norm.source.bytes = None;
        r.norm.law.subject = "conformance.RequirementContext".into();
        r.norm.law.scope.object_types = vec![r.norm.law.subject.clone()];
        r.norm.law.scope.root = norn_ir::LawRoot::App("physics-conformance".into());
        r.norm.law.response = norn_ir::LawResponse::Predicate {
            expression: norn_ir::ZenExpression(expression.into()),
        };
        r.norm.modality = if prohibited {
            DeonticModality::Prohibition
        } else {
            DeonticModality::Obligation
        };
        r.norm.law.modality = if prohibited {
            LawModality::Never {}
        } else {
            LawModality::Always {}
        };
        r.timing = if prohibited || id == "commitment" {
            RequirementTiming::EveryCommittedState
        } else {
            RequirementTiming::SameCommit
        };
        r.wording.scope = RequirementText {
            en: scope_en.into(),
            ar: scope_ar.into(),
        };
        r.wording.component = RequirementText {
            en: component_en.into(),
            ar: component_ar.into(),
        };
        r.wording.response = RequirementText {
            en: en.into(),
            ar: ar.into(),
        };
        r.wording.activation = RequirementText {
            en: when_en.into(),
            ar: when_ar.into(),
        };
        norn_checker::deontic::check_requirement(&r, &env).expect("closed corpus");
        let mut ids = Vec::new();
        for locale in [L::En, L::Ar] {
            let view = RequirementView {
                requirement: r.clone(),
                locale,
            };
            let slots = view.slots().expect("safety display");
            assert_eq!(view.slots().expect("deterministic"), slots);
            assert!(slots.iter().any(|s| s.text.contains(expression)));
            let response = slots
                .iter()
                .find(|s| s.id.ends_with("/response"))
                .expect("response slot");
            assert!(
                response
                    .text
                    .contains(if locale == L::Ar { ar } else { en })
            );
            assert!(
                slots
                    .iter()
                    .all(|s| s.id.starts_with(&format!("core.{id}#")))
            );
            assert_eq!(
                view.direction(),
                if locale == L::Ar { "rtl" } else { "ltr" }
            );
            ids.push(slots.iter().map(|s| s.id.clone()).collect::<Vec<_>>());
            let encoded = serde_json::to_string(&view).expect("view wire");
            let decoded: RequirementView = serde_json::from_str(&encoded).expect("view decode");
            assert_eq!(decoded.requirement, r);
            assert_eq!(decoded.slots().expect("display roundtrip"), slots);
        }
        assert_eq!(ids.first(), ids.get(1));
        r.norm.law.modality = LawModality::Eventually {};
        assert_eq!(
            RequirementView {
                requirement: r,
                locale: L::Ar
            }
            .slots(),
            Err("requirement.timing.unsupported")
        );
    }
}
