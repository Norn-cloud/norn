//! Identity and wire-boundary checks; these do not exercise runtime authorization.
use norn_ir::{
    decision_question::{
        DecisionFamily, DecisionQuestion, DecisionQuestionState, DecisionSource,
        QuestionRequirement,
    },
    function::Digest,
};

fn question() -> Result<DecisionQuestion, serde_json::Error> {
    Ok(DecisionQuestion {
        question_version: serde_json::from_str("\"0.1\"")?,
        family: DecisionFamily::RequiredPathBinding,
        source: DecisionSource {
            obligation: Digest::from_hex(&"a".repeat(64)),
            candidate: Digest::from_hex(&"b".repeat(64)),
            base: Digest::from_hex(&"c".repeat(64)),
            profile: Digest::from_hex(&"d".repeat(64)),
            artifact_id: "mapping.test".into(),
            pointer: "/payload/pathMappings".into(),
        },
        requirement: QuestionRequirement::Required,
        options: Vec::new(),
    })
}

#[test]
fn question_identity_is_stable_and_obligation_bound() -> Result<(), serde_json::Error> {
    let original = question()?;
    let encoded = serde_json::to_vec(&original)?;
    let decoded: DecisionQuestion = serde_json::from_slice(&encoded)?;
    assert_eq!(original.id(), decoded.id());
    assert!(original.id().is_some_and(|id| id.is_well_formed()));
    let mut changed = original.clone();
    changed.source.obligation = Digest::from_hex(&"e".repeat(64));
    assert_ne!(original.id(), changed.id());
    changed.source.obligation = Digest("a mutable label".into());
    assert_eq!(changed.id(), None);
    Ok(())
}

#[test]
fn lifecycle_wire_requires_proposal_approval_and_closure_pins() {
    for value in [
        r#"{"state":"proposed"}"#,
        r#"{"state":"approved","actor":"expert"}"#,
        r#"{"state":"closed","receipt":"pretend"}"#,
        r#"{"state":"open","approval":{"actor":"expert"}}"#,
        r#"{"state":"future"}"#,
    ] {
        assert!(serde_json::from_str::<DecisionQuestionState>(value).is_err());
    }
}

#[test]
fn wire_rejects_future_versions_and_authority_fields() -> Result<(), serde_json::Error> {
    let mut value = serde_json::to_value(question()?)?;
    value["questionVersion"] = "9.9".into();
    assert!(serde_json::from_value::<DecisionQuestion>(value.clone()).is_err());
    value["questionVersion"] = "0.1".into();
    value["authorized"] = true.into();
    assert!(serde_json::from_value::<DecisionQuestion>(value).is_err());
    Ok(())
}
