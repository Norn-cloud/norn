//! HOST-SCOPE, HOST-WIRE and HOST-REFUSAL correspondence.
use http::StatusCode;
use norn_api::{
    refusal::{Refusal, RefusalCategory},
    scope::{CellScope, RequestScope, ScopeQuery},
};
#[test]
fn host_scope_requires_both_exact_nonblank_dimensions() {
    let cell = CellScope {
        org_id: "org".into(),
        environment_id: "dev".into(),
    };
    for (org, env) in [
        (None, Some("dev")),
        (Some("org"), None),
        (Some(""), Some("dev")),
        (Some("org"), Some(" \t")),
    ] {
        let refusal = cell.admit(org, env).unwrap_err();
        assert_eq!(
            (refusal.code.as_str(), refusal.status, refusal.category),
            ("scope.missing", 400, RefusalCategory::Invalid)
        );
    }
    for (org, env) in [("other", "dev"), ("org", "prod"), (" org", "dev")] {
        let refusal = cell.admit(Some(org), Some(env)).unwrap_err();
        assert_eq!(
            (refusal.code.as_str(), refusal.status, refusal.category),
            ("scope.denied", 403, RefusalCategory::Permission)
        );
    }
    assert!(cell.admit(Some("org"), Some("dev")).is_ok());
    let scope: norn_ir::ScopeRef = cell.as_scope_ref();
    assert_eq!(scope.org_id.as_deref(), Some("org"));
    assert_eq!(scope.environment_id.as_deref(), Some("dev"));
}

#[test]
fn host_scope_keys_escape_separators_and_percent_once() {
    let key = |org: &str, env: &str| {
        CellScope {
            org_id: org.into(),
            environment_id: env.into(),
        }
        .key()
    };
    assert_eq!(key("a%/b", "c/d%"), "a%25%2Fb/c%2Fd%25");
    for ((a, b), (c, d)) in [
        (("org/x", "dev"), ("org", "x/dev")),
        (("a%2Fb", "c"), ("a/b", "c")),
        (("a%", "2Fb"), ("a", "%2Fb")),
    ] {
        assert_ne!(key(a, b), key(c, d));
    }
}

#[test]
fn host_scope_wire_keeps_camel_case_and_absence() -> Result<(), serde_json::Error> {
    let scope: RequestScope = serde_json::from_str(r#"{"orgId":"org"}"#)?;
    assert_eq!(
        serde_json::to_string(&scope)?,
        r#"{"orgId":"org","environmentId":null}"#
    );
    assert!(serde_json::from_str::<RequestScope>(r#"{"orgId":"org","extra":true}"#).is_err());
    let query: ScopeQuery = serde_json::from_str("{}")?;
    assert_eq!(query, ScopeQuery::default());
    Ok(())
}

#[test]
fn host_refusal_preserves_status_category_and_conceals_opaque_detail()
-> Result<(), Box<dyn std::error::Error>> {
    for (status, category) in [
        (400, RefusalCategory::Invalid),
        (401, RefusalCategory::Permission),
        (403, RefusalCategory::Permission),
        (404, RefusalCategory::Visibility),
        (409, RefusalCategory::Conflict),
        (413, RefusalCategory::Limit),
        (422, RefusalCategory::Invalid),
        (429, RefusalCategory::Limit),
        (500, RefusalCategory::Internal),
    ] {
        let refusal = Refusal::new(
            StatusCode::from_u16(status)?,
            "stable.code",
            "summary",
            "opaque",
        )
        .project_detail(false);
        assert_eq!((refusal.status, refusal.category), (status, category));
        let wire = serde_json::to_value(&refusal)?;
        assert_eq!(wire["format"], "norn/refusal@2");
        assert_eq!(wire["reason"], serde_json::Value::Null);
        assert_eq!(wire["retryable"], false);
        assert_eq!(wire["explanation"], Refusal::UNPROVEN_DETAIL);
    }
    let malformed = Refusal::invalid("syntax");
    assert_eq!(
        (malformed.code.as_str(), malformed.status),
        ("request.invalid", 400)
    );
    let precondition = malformed.with_category(RefusalCategory::Precondition);
    assert_eq!(precondition.category, RefusalCategory::Precondition);
    assert_eq!(
        Refusal::host("stored-secret").explanation,
        Refusal::UNPROVEN_DETAIL
    );
    Ok(())
}

#[test]
fn invariant_refusal_retains_pin_and_typed_reason_through_api()
-> Result<(), Box<dyn std::error::Error>> {
    use norn_kernel::{CommitError, FactKey, Refusable};
    let error = CommitError::<std::convert::Infallible>::InvariantViolation {
        key: Box::new(FactKey {
            object_type: norn_ir::ObjectTypeName("demo.Item".into()),
            key: "one".into(),
        }),
        owner: norn_ir::InvariantOwner::Object(norn_ir::ObjectTypeName("demo.Item".into())),
        version: "sha256:owner-definition".into(),
        id: norn_ir::InvariantId("positive".into()),
        message: "must be positive".into(),
        outcome: Box::new(norn_ir::DataQualityOutcome::NotSatisfied),
    };
    let kernel = error.refusal();
    let projected = Refusal::from_kernel(kernel.clone(), true);
    assert_eq!(projected.reason.as_deref(), kernel.reason.as_ref());
    assert_eq!(projected.code, "commit.invariant_violation");
    assert!(!projected.retryable);
    let reason = projected.reason.ok_or("missing invariant reason")?;
    assert_eq!(
        reason.values.get("outcome"),
        Some(&norn_ir::RuntimeValue::from("not_satisfied"))
    );
    assert!(
        matches!(reason.pin, Some(norn_ir::DataQualityRuleRef::Invariant { version, .. }) if version == "sha256:owner-definition")
    );
    let hidden = Refusal::from_kernel(kernel, false);
    assert!(hidden.reason.is_none());
    assert_eq!(hidden.explanation, Refusal::UNPROVEN_DETAIL);
    Ok(())
}

proptest::proptest! {
    #[test]
    fn unauthorized_reason_projection_is_independent_of_secret(secret in "(?s:.{0,256})") {
        use norn_kernel::RefusalCategory as Category;
        for category in [Category::Visibility, Category::Permission, Category::Precondition, Category::Conflict, Category::Invalid, Category::Limit, Category::Internal] {
            let make = |value: &str| {
                let mut source = norn_kernel::Refusal::new("stable.code", category, value);
                source.reason = Some(norn_ir::RefusalReason {
                    law: value.into(), subject: value.into(),
                    values: [("secret".into(), norn_ir::RuntimeValue::from(value))].into(),
                    pin: Some(norn_ir::DataQualityRuleRef::Builtin { id: value.into(), version: value.into() }),
                });
                if matches!(category, Category::Visibility | Category::Internal) {
                    let privileged = Refusal::from_kernel(source.clone(), true);
                    assert!(privileged.reason.is_none());
                    assert_eq!(privileged.explanation, Refusal::UNPROVEN_DETAIL);
                }
                Refusal::from_kernel(source, false)
            };
            proptest::prop_assert_eq!(make(&secret), make("different-secret"));
        }
    }
}
