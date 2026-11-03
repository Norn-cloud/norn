//! Refusal pins never constitute proof or admission evidence.
use norn_checker::consistency::{
    RuleOrigin,
    report::{Attempt, diagnose_attempt},
};
use norn_expr::schema::Schema;
use std::collections::BTreeSet;

#[test]
fn exact_pins_source_locations_protected_authority_and_unknowns() {
    let schema = Schema::Boolean;
    let members = vec![
        (
            RuleOrigin {
                artifact_id: Some("staffing".into()),
                pointer: "/laws/two-assayers".into(),
            },
            "true".into(),
        ),
        (
            RuleOrigin {
                artifact_id: Some("staffing".into()),
                pointer: "/parameters/roster".into(),
            },
            "false".into(),
        ),
    ];
    let editable = BTreeSet::from([1]);
    let protected = BTreeSet::from([0]);
    let mut attempt = Attempt {
        source: b"attempted source",
        base: b"retained base",
        profile: b"authority-v1",
        assumptions: "roster exactly {person-a, person-b} at snapshot 7; seller person-a; two distinct accountable assayers required",
        schema: &schema,
        members: &members,
        editable: &editable,
        protected: &protected,
        max_calls: 100,
    };
    let report = diagnose_attempt(&attempt);
    assert!(report.diagnosis.is_ok());
    assert_eq!(report.digests, diagnose_attempt(&attempt).digests);
    assert_eq!(report.diagnostics.len(), 1);
    let diagnostic = report.diagnostics.first().unwrap();
    assert_eq!(diagnostic.code.as_str(), "consistency.diagnosis.conflict");
    assert_eq!(
        diagnostic.span.as_ref().unwrap().pointer,
        "/parameters/roster"
    );
    assert!(diagnostic.message.contains("roster exactly"));
    assert!(diagnostic.message.contains("256"));
    attempt.source = b"attempted source\n";
    assert_ne!(report.digests, diagnose_attempt(&attempt).digests);
    attempt.source = b"attempted source";
    attempt.base = b"different base";
    assert_ne!(report.digests, diagnose_attempt(&attempt).digests);
    attempt.base = b"retained base";
    attempt.profile = b"authority-v2";
    assert_ne!(report.digests, diagnose_attempt(&attempt).digests);
    attempt.profile = b"authority-v1";
    attempt.max_calls = 0;
    assert_eq!(
        diagnose_attempt(&attempt)
            .diagnostics
            .first()
            .unwrap()
            .code
            .as_str(),
        "consistency.diagnosis.unknown"
    );
    attempt.max_calls = 100;
    attempt.editable = &protected;
    assert!(diagnose_attempt(&attempt).diagnosis.is_err());
    let none = BTreeSet::new();
    attempt.editable = &none;
    assert_eq!(
        diagnose_attempt(&attempt)
            .diagnostics
            .first()
            .unwrap()
            .code
            .as_str(),
        "consistency.diagnosis.no_editable_repair"
    );
}
