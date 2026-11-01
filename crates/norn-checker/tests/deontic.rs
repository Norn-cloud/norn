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
fn structured_safety_preserves_grades_timing_sources_and_precedence() {
    use norn_checker::deontic::check_requirement;
    use norn_checker::norm_precedence::{GroundedNorm, check_norm_precedence};
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
            let contexts = check_norm_precedence(&[grounded], 2).expect("existing precedence");
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
    for (pointer, value) in [
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
    }
}
