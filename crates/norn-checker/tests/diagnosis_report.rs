//! Refusal pins never constitute proof or admission evidence.
use norn_checker::consistency::{
    RuleOrigin,
    report::{Attempt, diagnose_attempt},
};
use norn_expr::schema::Schema;
use std::collections::BTreeSet;

#[test]
fn exact_pins_source_locations_protected_authority_and_unknowns() {
    let people = Schema::String(Some(
        ["person-a", "person-b", "person-c"]
            .map(String::from)
            .into(),
    ));
    let schema = Schema::Object {
        properties: [("first".into(), people.clone()), ("second".into(), people)].into(),
        required: ["first".into(), "second".into()].into(),
    };
    let member = |pointer: &str, predicate: &str| {
        (
            RuleOrigin {
                artifact_id: Some("staffing".into()),
                pointer: pointer.into(),
            },
            predicate.into(),
        )
    };
    let members = vec![
        member("/laws/two-assayers", "first != second"),
        member(
            "/laws/seller-exclusion",
            "first != 'person-a' and second != 'person-a'",
        ),
        member(
            "/parameters/roster",
            "first != 'person-c' and second != 'person-c'",
        ),
    ];
    let editable = BTreeSet::from([2]);
    let protected = BTreeSet::from([0, 1]);
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
    assert_eq!(report.diagnostics.len(), 3);
    let diagnostic = report.diagnostics.first().unwrap();
    assert_eq!(diagnostic.code.as_str(), "consistency.diagnosis.conflict");
    assert_eq!(
        diagnostic.span.as_ref().unwrap().pointer,
        "/laws/two-assayers"
    );
    assert!(diagnostic.message.contains("roster exactly"));
    assert!(diagnostic.message.contains("256"));
    assert!(
        diagnostic
            .message
            .contains("correction set: staffing/parameters/roster")
    );
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

proptest::proptest! {
    #[test]
    fn attempted_bytes_reproduce_and_a_changed_byte_changes_pins(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
        let schema = Schema::Boolean;
        let none = BTreeSet::new();
        let mut attempt = Attempt { source: &bytes, base: b"base", profile: b"profile", assumptions: "finite Boolean universe", schema: &schema, members: &[], editable: &none, protected: &none, max_calls: 100 };
        let first = diagnose_attempt(&attempt).digests;
        proptest::prop_assert_eq!(&first, &diagnose_attempt(&attempt).digests);
        let mut changed = bytes.clone(); changed.push(0);
        attempt.source = &changed;
        proptest::prop_assert_ne!(first, diagnose_attempt(&attempt).digests);
    }
}

#[test]
fn invalid_member_identity_and_protected_indices_are_unknown() {
    let schema = Schema::Boolean;
    let none = BTreeSet::new();
    for (ids, protected) in [
        (vec![(Some("a"), "/p"), (Some("a"), "/p")], none.clone()),
        (vec![(None, "/p")], none.clone()),
        (vec![(Some(""), "/p")], none.clone()),
        (vec![(Some("a"), "p")], none.clone()),
        (vec![(Some("a"), "/p")], BTreeSet::from([1])),
    ] {
        let members: Vec<_> = ids
            .into_iter()
            .map(|(id, p)| {
                (
                    RuleOrigin {
                        artifact_id: id.map(str::to_owned),
                        pointer: p.into(),
                    },
                    "true".into(),
                )
            })
            .collect();
        let report = diagnose_attempt(&Attempt {
            source: b"source",
            base: b"base",
            profile: b"profile",
            assumptions: "Boolean universe",
            schema: &schema,
            members: &members,
            editable: &none,
            protected: &protected,
            max_calls: 100,
        });
        assert!(report.diagnosis.is_err());
        assert_eq!(
            report.diagnostics.first().unwrap().code.as_str(),
            "consistency.diagnosis.unknown"
        );
    }
}

#[test]
fn an_unencodable_profile_stays_unknown_without_a_profile_digest() {
    let schema=(0..130).fold(Schema::Boolean,|schema,_|Schema::Array(Box::new(schema)));
    let none=BTreeSet::new();
    let report=diagnose_attempt(&Attempt{source:b"source",base:b"base",profile:b"profile",assumptions:"bounded",schema:&schema,members:&[],editable:&none,protected:&none,max_calls:100});
    assert!(report.diagnosis.is_err());
    assert!(report.digests.last().is_some_and(String::is_empty));
    assert!(report.diagnostics.iter().any(|d|d.message.contains("profile cannot be canonicalized")));
}
