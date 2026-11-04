//! Regression checks for conservative normative lowering.
use norn_checker::deontic::{LawLoweringRefusal, check_requirement, lower_deontic_law};
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
    use norn_ir::deontic::{RequirementTiming, RequirementWording};
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
        wording: serde_json::from_str::<RequirementWording>(include_str!(
            "../../../fixtures/wording/locales.json"
        ))
        .expect("complete locale data"),
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
fn structured_safety_preserves_grades_timing_sources_and_precedence() {
    use norn_checker::deontic::check_requirement;
    use norn_checker::norm_precedence::{ConstitutionLayer, GroundedNorm, check_norm_precedence};
    use norn_ir::deontic::{RequirementTiming, StructuredRequirement};
    use std::collections::BTreeSet;
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
            let grounded = GroundedNorm {
                id: r.norm.law.id,
                modality: r.norm.modality,
                authority: (r.norm.authority_id, r.norm.authority_version),
                authority_level: Some(1),
                enacted: 0,
                applicable: BTreeSet::from([0, 1]),
                excepted: BTreeSet::from([1]),
            };
            let origin = norn_checker::consistency::RuleOrigin {
                artifact_id: Some(r.norm.source.artifact_id),
                pointer: r.norm.source.pointer,
            };
            let contexts = check_norm_precedence(
                &[grounded],
                &[origin],
                &[ConstitutionLayer::Company],
                &[],
                &BTreeSet::new(),
                2,
                100,
            )
            .expect("declared precedence");
            let obligations = &contexts.first().expect("applicable").obligations;
            assert_eq!(obligations.is_empty(), grade != DeonticModality::Obligation);
            assert!(contexts.get(1).expect("excepted").survivors.is_empty());
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
        "law.validation.unsupported_temporal"
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
    r.wording.0.get_mut("ar").expect("Arabic").response.clear();
    assert_eq!(
        check_requirement(&r, &env),
        Err(R::Wording("law.wording.incomplete_locale"))
    );
    r = requirement();
    r.norm.law.response = norn_ir::LawResponse::Action {
        artifact_id: "future.action".into(),
    };
    assert_eq!(check_requirement(&r, &env), Err(R::UnsupportedResponse));
}

#[test]
fn structured_nested_bindings_must_identify_real_slots() {
    use norn_checker::deontic::{RequirementRefusal as R, check_requirement};
    let mut r = requirement();
    r.norm.exceptions.push(norn_ir::deontic::RuleException {
        id: "exception".into(),
        condition: norn_ir::ZenExpression("input.active".into()),
        source: r.norm.source.clone(),
    });
    r.norm.law.response = norn_ir::LawResponse::Invariant {
        object_type: "example.Type".into(),
        invariant_id: "nonnegative".into(),
    };
    let scope = &mut r.norm.law.scope;
    scope.commitments.push("example.Commitment".into());
    r.norm.defeated_by.push("preferred.norm".into());
    scope.lifecycle_states.push(norn_ir::LawStateScope {
        object_type: "example.Type".into(),
        property: "status".into(),
        states: vec!["posted".into()],
    });
    let wire = serde_json::to_value(r).expect("typed fixture");
    let exception = wire.pointer("/norm/exceptions/0").expect("exception");
    for (pointer, value) in [
        ("/norm/exceptions", json!([exception, exception])),
        ("/norm/law/owner", json!("")),
        ("/norm/law/scope/root/id", json!("")),
        ("/norm/law/scope/objectTypes/0", json!(" ")),
        ("/norm/exceptions/0/id", json!("")),
        ("/norm/exceptions/0/source/pointer", json!("/~2")),
        ("/norm/exceptions/0/source/artifactId", json!("")),
        ("/norm/law/response/objectType", json!("")),
        ("/norm/law/response/invariantId", json!(" ")),
        ("/norm/law/scope/commitments/0", json!("")),
        ("/norm/defeatedBy/0", json!("")),
        ("/norm/law/scope/lifecycleStates/0/property", json!("")),
        ("/norm/law/scope/lifecycleStates/0/states", json!([])),
    ] {
        let mut invalid = wire.clone();
        *invalid.pointer_mut(pointer).expect("declared slot") = value;
        let invalid = serde_json::from_value(invalid).expect("shape");
        assert_eq!(
            check_requirement(&invalid, &requirement_env()),
            Err(R::InvalidBinding)
        );
        if pointer == "/norm/exceptions" {
            let view = norn_ir::requirement_display::RequirementView {
                requirement: invalid,
                locale: "ar".into(),
            };
            assert!(view.slots().is_err());
        }
    }
}

#[allow(clippy::expect_used, reason = "typed physics corpus fixture")]
fn physics_requirement(row: &str) -> (norn_ir::deontic::StructuredRequirement, [&str; 4]) {
    use norn_ir::deontic::RequirementTiming;
    let columns: [&str; 12] = row
        .split('\t')
        .collect::<Vec<_>>()
        .try_into()
        .expect("twelve typed slots");
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
        condition,
    ] = columns;
    let prohibited = prohibited.parse::<bool>().expect("grade flag");
    let mut r = requirement();
    r.norm.law.id = format!("core.{id}");
    r.norm.law.condition = Some(norn_ir::ZenExpression(condition.into()));
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
    if prohibited {
        r.norm.modality = DeonticModality::Prohibition;
        r.norm.law.modality = LawModality::Never {};
    }
    if prohibited || id == "commitment" {
        r.timing = RequirementTiming::EveryCommittedState;
    }
    for (tag, scope, component, response, activation) in [
        ("en", scope_en, component_en, en, when_en),
        ("ar", scope_ar, component_ar, ar, when_ar),
    ] {
        let w = r.wording.0.get_mut(tag).expect("declared locale");
        w.scope = scope.into();
        w.component = component.into();
        w.response = response.into();
        w.activation = activation.into();
    }
    (r, [expression, en, ar, condition])
}

#[test]
fn core_physics_bilingual_views_round_trip_one_semantic_target() {
    use norn_expr::{EntityDefinitions, EnvOptions, Type, ZenTypeEnv};
    use norn_ir::requirement_display::RequirementView;
    use std::collections::BTreeMap;
    // Bounded, partitioned authoring inputs; not aggregate-law admission or preservation.
    let corpus = include_str!("../../../docs/platform/structured-physics.tsv");
    assert_eq!(corpus.lines().count(), 8);
    let mut fields = BTreeMap::new();
    for name in "external loss available held funded".split_whitespace() {
        fields.insert(name.into(), Type::Number);
    }
    for name in "committed ledgerCommitted proposed wasTerminal terminal custodyChanged ownershipTransfer correctionCommitted consumed produced recorded open custodyRecorded edited deleted linked inverse"
        .split_whitespace()
    {
        fields.insert(name.into(), Type::Bool);
    }
    for name in ["ownerBefore", "ownerAfter"] {
        fields.insert(name.into(), Type::text());
    }
    for name in ["inputs", "outputs"] {
        fields.insert(name.into(), Type::List(Box::new(Type::Number)));
    }
    let env = ZenTypeEnv::from_artifact_set(&EntityDefinitions::default(), EnvOptions::default())
        .expect("env")
        .bind_input(fields)
        .expect("typed corpus inputs");
    for row in corpus.lines() {
        let (mut r, [expression, en, ar, condition]) = physics_requirement(row);
        assert!(r.norm.law.id != "core.history" || condition == "true");
        norn_checker::deontic::check_requirement(&r, &env).expect("closed corpus");
        let mut ids = Vec::new();
        for locale in ["en", "ar"] {
            let view = RequirementView {
                requirement: r.clone(),
                locale: locale.into(),
            };
            let slots = view.slots().expect("safety display");
            assert!(slots.iter().any(|s| s.text.contains(expression)));
            let activation = slots.get(1).expect("activation");
            assert!(activation.id.ends_with("/condition") && activation.text.contains(condition));
            let response = slots
                .iter()
                .find(|s| s.id.ends_with("/response"))
                .expect("response slot");
            let expected = if locale == "ar" { ar } else { en };
            assert!(response.text.contains(expected));
            assert!(slots.iter().all(|s| s.id.starts_with(&r.norm.law.id)));
            assert_eq!(
                view.direction().expect("declared direction"),
                if locale == "ar" { "rtl" } else { "ltr" }
            );
            ids.push(slots.iter().map(|s| s.id.clone()).collect::<Vec<_>>());
            let encoded = serde_json::to_string(&view).expect("view wire");
            let decoded: RequirementView = serde_json::from_str(&encoded).expect("view decode");
            assert_eq!(decoded, view);
        }
        assert_eq!(ids.first(), ids.get(1));
        r.norm.law.condition = Some(norn_ir::ZenExpression(
            "input.ownerBefore == 'x\u{2069}'".into(),
        ));
        norn_checker::deontic::check_requirement(&r, &env).expect("typed bidi literal");
        let mut view = RequirementView {
            requirement: r,
            locale: "ar".into(),
        };
        let slots = view.slots().expect("escaped controls");
        let activation = slots
            .iter()
            .find(|s| s.id.ends_with("/condition"))
            .expect("activation");
        assert!(activation.text.contains("\\u{2069}"));
        assert_eq!(activation.text.matches('\u{2069}').count(), 1);
        view.requirement.norm.law.modality = LawModality::Eventually {};
        assert_eq!(view.slots(), Err("law.validation.unsupported_temporal"));
    }
}

#[test]
#[allow(clippy::expect_used, reason = "locale conformance fixtures")]
fn wording_third_locale_is_data_only_and_unknown_locale_refuses() {
    use norn_ir::{deontic::RequirementWording, requirement_display::RequirementView};
    let mut r = requirement();
    let french: RequirementWording =
        serde_json::from_str(include_str!("../../../fixtures/wording/french.json"))
            .expect("complete French vocabulary");
    r.wording.0.extend(french.0);
    check_requirement(&r, &requirement_env())
        .expect("French admitted without language-specific code");
    let mut view = RequirementView {
        requirement: r.clone(),
        locale: "fr".into(),
    };
    let slots = view.slots().expect("French rendered");
    assert_eq!(slots.get(3).expect("grade").text, "doit");
    assert_eq!(slots.first().expect("scope").label, "Portée");
    assert_eq!(view.direction(), Ok("ltr"));
    assert_eq!(view.requirement.norm, r.norm);
    view.locale = "FR".into();
    assert_eq!(
        view.slots().expect("BCP 47 case-insensitive identity"),
        slots
    );
    view.locale = "fr-CA".into();
    assert_eq!(view.slots(), Err("law.wording.unknown_locale"));
    assert_eq!(view.direction(), Err("law.wording.unknown_locale"));
    // Direction is declared data, independently of the language tag.
    view.locale = "fr".into();
    view.requirement
        .wording
        .0
        .get_mut("fr")
        .expect("French")
        .direction = norn_ir::deontic::TextDirection::Rtl;
    assert_eq!(view.direction(), Ok("rtl"));
}

#[test]
#[allow(clippy::expect_used, reason = "locale refusal fixtures")]
fn wording_incomplete_locale_refuses_even_when_rendering_another_locale() {
    use norn_ir::{deontic::RequirementWording, requirement_display::RequirementView};
    let mut r = requirement();
    r.wording.0.get_mut("ar").expect("Arabic").response.clear();
    assert_eq!(
        check_requirement(&r, &requirement_env())
            .expect_err("incomplete Arabic")
            .code(),
        "law.wording.incomplete_locale"
    );
    let view = RequirementView {
        requirement: r,
        locale: "en".into(),
    };
    assert_eq!(view.slots(), Err("law.wording.incomplete_locale"));
    let wire: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/wording/locales.json"))
            .expect("fixture");
    for pointer in ["/ar", "/ar/labels", "/ar/terms"] {
        let keys: Vec<_> = wire
            .pointer(pointer)
            .expect("record")
            .as_object()
            .expect("record")
            .keys()
            .cloned()
            .collect();
        for key in keys {
            let mut missing = wire.clone();
            missing
                .pointer_mut(pointer)
                .expect("record")
                .as_object_mut()
                .expect("record")
                .remove(&key);
            let error = serde_json::from_value::<RequirementWording>(missing)
                .expect_err("missing required field");
            assert!(
                error.to_string().contains("law.wording.incomplete_locale"),
                "{pointer}/{key}: {error}"
            );
        }
    }
}

#[test]
#[allow(clippy::expect_used, reason = "locale boundary fixtures")]
fn wording_tags_duplicates_and_nonblank_text_are_strict() {
    use norn_ir::deontic::RequirementWording;
    let wire: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/wording/french.json"))
            .expect("fixture");
    let entry = wire.get("fr").expect("French");
    for tag in [
        "en_US",
        "",
        "en--US",
        "en-123456789",
        "fr-CA-CA",
        "de-CH-1901-1901",
        "en-u-ca-gregory-u-nu-latn",
        "en-x",
    ] {
        let text = serde_json::json!({tag: entry});
        assert!(
            serde_json::from_value::<RequirementWording>(text).is_err(),
            "{tag}"
        );
    }
    for tag in [
        "fr",
        "fr-CA",
        "zh-Hant-TW",
        "en-US-u-ca-gregory",
        "x-demo",
        "i-klingon",
    ] {
        assert!(
            serde_json::from_value::<RequirementWording>(serde_json::json!({tag: entry})).is_ok(),
            "{tag}"
        );
    }
    for (a, b) in [("fr", "fr"), ("fr", "FR")] {
        let text = format!("{{\"{a}\":{entry},\"{b}\":{entry}}}");
        assert!(
            serde_json::from_str::<RequirementWording>(&text)
                .expect_err("duplicate locale")
                .to_string()
                .contains("law.wording.duplicate_locale")
        );
    }
    for pointer in [
        "/fr/scope",
        "/fr/response",
        "/fr/labels/source",
        "/fr/terms/permission",
    ] {
        let mut blank = wire.clone();
        *blank.pointer_mut(pointer).expect("text") = serde_json::json!(" \t\n");
        assert!(
            serde_json::from_value::<RequirementWording>(blank)
                .expect_err("blank text")
                .to_string()
                .contains("law.wording.incomplete_locale")
        );
    }
}

proptest::proptest! {
    #[test]
    fn wording_locale_round_trip_preserves_semantics_and_rendering(text in "[^\\p{C}]{1,80}") {
        use norn_ir::requirement_display::RequirementView;
        let mut r = requirement();
        let original = r.norm.clone();
        if let Some(w) = r.wording.0.get_mut("en") { w.response = format!("response {text}"); }
        let view = RequirementView { requirement: r, locale: "en".into() };
        let encoded = serde_json::to_string(&view);
        proptest::prop_assert!(encoded.is_ok());
        if let Ok(encoded) = encoded {
            let decoded = serde_json::from_str::<RequirementView>(&encoded);
            proptest::prop_assert!(decoded.is_ok());
            if let Ok(decoded) = decoded {
                proptest::prop_assert_eq!(&decoded.requirement.norm, &original);
                proptest::prop_assert_eq!(view.slots(), decoded.slots());
            }
        }
    }
}

#[test]
fn native_order_preserves_explicit_defeat_and_own_exception() {
    use norn_checker::deontic::lower_declared_order;
    use norn_checker::norm_precedence::{ConstitutionLayer, check_norm_precedence};
    use std::collections::BTreeSet;
    let mut low = obligation().expect("fixture");
    low.law.modality = LawModality::Always {};
    low.law.timing = None;
    low.law.condition = Some(norn_ir::ZenExpression("true".into()));
    low.law.response = norn_ir::LawResponse::Predicate {
        expression: norn_ir::ZenExpression("true".into()),
    };
    let mut high = low.clone();
    high.law.id = "sales.block".into();
    high.source.pointer = "/rules/1".into();
    high.modality = DeonticModality::Prohibition;
    high.law.modality = LawModality::Never {};
    low.defeated_by.push(high.law.id.clone());
    let rules = [low.clone(), high.clone()];
    let grounded = lower_declared_order(&rules, &norn_expr::schema::Schema::Boolean)
        .expect("native declarations lower");
    assert_eq!(grounded.edges, vec![(1, 0)]);
    let layers = [ConstitutionLayer::Company; 2];
    assert!(check_norm_precedence(&grounded.norms, &grounded.origins, &layers,
        &grounded.edges, &BTreeSet::new(), grounded.contexts, 1000).is_ok());
    low.defeated_by.clear();
    let tied = lower_declared_order(&[low.clone(), high.clone()], &norn_expr::schema::Schema::Boolean)
        .expect("ties lower without resolution");
    assert!(check_norm_precedence(&tied.norms, &tied.origins, &layers,
        &tied.edges, &BTreeSet::new(), tied.contexts, 1000).is_err());
    low.exceptions.push(norn_ir::deontic::RuleException {
        id: "explicit".into(), condition: norn_ir::ZenExpression("true".into()),
        source: low.source.clone(),
    });
    let excepted = lower_declared_order(&[low.clone(), high], &norn_expr::schema::Schema::Boolean)
        .expect("explicit exceptions lower");
    assert_eq!(excepted.norms[0].excepted, excepted.norms[0].applicable);
    assert!(excepted.norms[1].excepted.is_empty());
    low.defeated_by.push("missing".into());
    assert!(lower_declared_order(&[low], &norn_expr::schema::Schema::Boolean).is_err());
}

#[test]
fn native_order_refuses_unbounded_and_temporal_grounding() {
    use norn_checker::deontic::lower_declared_order;
    let mut rule = obligation().expect("fixture");
    assert!(lower_declared_order(&[rule.clone()], &norn_expr::schema::Schema::Boolean).is_err());
    rule.law.modality = LawModality::Always {};
    rule.law.timing = None;
    assert!(lower_declared_order(&[rule], &norn_expr::schema::Schema::Integer).is_err());
}
