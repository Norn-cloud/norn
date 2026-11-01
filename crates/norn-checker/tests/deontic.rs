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
