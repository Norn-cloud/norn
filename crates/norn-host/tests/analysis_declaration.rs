//! Entirely fictional preregistration metadata, without source evidence claims.
#![allow(clippy::expect_used, reason = "typed declaration regressions")]
use norn_host::analysis_jobs::{Declaration, Method, Remedy};
use norn_host::analysis_native::{ExportPlan, Sample};

fn declaration() -> Declaration {
    Declaration {
        version: 1,
        question: "synthetic.demand".into(),
        population: ExportPlan {
            object_type: "synthetic.Demand".into(),
            features: vec!["stock".into()],
            outcome: "units".into(),
            event_time: "at".into(),
            max_age_micros: 0,
            as_of: 200,
            samples: vec![Sample {
                key: "fictional-row".into(),
                at: 100,
                feature_cursor: 1,
                outcome_cursor: 2,
            }],
        },
        method: Method::Ols,
        alpha_basis_points: 500,
        independence: "Entirely synthetic independent inputs.".into(),
        confounding: "Associational demonstration only.".into(),
        remedy: Remedy::Block,
    }
}

#[test]
fn fixed_registration_metadata_round_trips_and_refuses_invalid_bounds() {
    let valid = declaration();
    valid.validate().expect("shape admitted");
    let bytes = serde_json::to_vec(&valid).expect("typed serialization");
    let decoded: Declaration = serde_json::from_slice(&bytes).expect("typed decode");
    decoded.validate().expect("valid metadata");
    assert_eq!(decoded.question, valid.question);
    for alpha in [0, 10_000, u32::MAX] {
        let mut invalid = declaration();
        invalid.alpha_basis_points = alpha;
        assert_eq!(
            invalid.validate().expect_err("alpha").code,
            "request.invalid"
        );
    }
    let mut invalid = declaration();
    invalid.version = 2;
    assert!(invalid.validate().is_err());
    for question in ["", "has a space", "has/slash", "newline\n"] {
        let mut invalid = declaration();
        invalid.question = question.into();
        assert!(invalid.validate().is_err());
    }
    let mut invalid = declaration();
    invalid.independence = " ".into();
    assert!(invalid.validate().is_err());
    let mut invalid = declaration();
    invalid.confounding = "x".repeat(4097);
    assert!(invalid.validate().is_err());
    let mut invalid = declaration();
    invalid.remedy = Remedy::DescriptiveOnly { reason: " ".into() };
    assert!(invalid.validate().is_err());
}

#[test]
fn a_large_population_cannot_bypass_the_encoded_registration_bound() {
    let mut invalid = declaration();
    invalid.population.samples = vec![
        Sample {
            key: "fictional-row".into(),
            at: 100,
            feature_cursor: 1,
            outcome_cursor: 2
        };
        2000
    ];
    assert!(invalid.validate().is_err());
}
