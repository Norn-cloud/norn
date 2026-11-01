#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "protected challenge regression tests"
)]
use norn_checker::obligations::{ApprovedContract, Challenge, ChallengeError, DefinitionSource};
use norn_checker::verification::LawWriter;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn pool() -> Vec<Value> {
    let mut entities: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json"))
            .expect("entities");
    entities.extend(
        serde_json::from_str::<Vec<Value>>(include_str!("../../../fixtures/law-ir/laws.json"))
            .expect("laws"),
    );
    entities
}
fn source(pool: &[Value]) -> DefinitionSource {
    let artifacts: Vec<norn_ir::ArtifactEnvelope> = pool
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("artifact"))
        .collect();
    let findings = norn_checker::check_artifact_set(
        &artifacts,
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(
        findings.is_empty(),
        "fixture must be admitted: {:?}",
        findings
            .iter()
            .map(|d| (&d.code, &d.message))
            .collect::<Vec<_>>()
    );
    DefinitionSource::parse(&serde_json::to_string(pool).expect("source"), "[]")
        .expect("exact source")
}
fn approve(source: &DefinitionSource) -> ApprovedContract {
    ApprovedContract::approve(source,&BTreeSet::new(),json!({"expression":"norn-zen/6","numeric":"exact-decimal","time":"logical-millis","missing":"distinct-null","refusal":"atomic"}),json!({"axioms":[],"host":"single-cell"}),json!({"required":"checked-proof","checker":"not-yet-qualified"})).expect("approved admitted set")
}
#[test]
fn required_inventory_is_reconstructed_and_every_mutated_echo_is_rejected() {
    let source = source(&pool());
    let authority = approve(&source);
    let challenge = authority
        .challenge(&source, &BTreeSet::new())
        .expect("challenge");
    assert!(challenge.writers().contains(&LawWriter::Transition {
        object_type: "erpnext.SalesOrder".into(),
        property: "status".into(),
        transition: "shipped".into()
    }));
    assert!(challenge.writers().contains(&LawWriter::Transition {
        object_type: "erpnext.PurchaseOrder".into(),
        property: "status".into(),
        transition: "approved".into()
    }));
    challenge
        .bind_submission(challenge.body())
        .expect("exact echo binds");
    let goals = challenge.body()["obligations"].as_array().expect("goals");
    for i in 0..goals.len() {
        let mut omitted = challenge.body().clone();
        omitted["obligations"]
            .as_array_mut()
            .expect("goals")
            .remove(i);
        assert_eq!(
            challenge.bind_submission(&omitted),
            Err(ChallengeError::InvalidSubmission)
        );
    }
    for (path, value) in [
        ("/assumptions/axioms", json!(["false"])),
        ("/semantics/numeric", json!("float")),
        ("/assurance/required", json!("sampled")),
        ("/obligations/0/goal", json!({"kind":"trivial"})),
    ] {
        let mut altered = challenge.body().clone();
        *altered.pointer_mut(path).expect("path") = value;
        assert_eq!(
            challenge.bind_submission(&altered),
            Err(ChallengeError::InvalidSubmission)
        );
    }
    let mut duplicate = challenge.body().clone();
    duplicate["obligations"]
        .as_array_mut()
        .expect("goals")
        .push(goals[0].clone());
    assert_eq!(
        challenge.bind_submission(&duplicate),
        Err(ChallengeError::InvalidSubmission)
    );
}

#[test]
fn registry_closure_does_not_bypass_the_existing_receipt_environment_refusal() {
    let mut pool = pool();
    pool[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({"id":"currency", "type":{"kind":"ref", "name":"iso4217.currency_code"}}));
    let source = DefinitionSource::parse(&serde_json::to_string(&pool).expect("source"), "[]")
        .expect("exact source");
    let registry = BTreeSet::from([norn_ir::PropertyTypeName("iso4217.currency_code".into())]);
    // The closure accepts the real registry (covered in law_ir), while the
    // existing receipt profile still refuses registry-only environment types.
    // This repair must not expand that separate admission authority.
    assert_eq!(
        ApprovedContract::approve(
            &source,
            &registry,
            json!({"expression":"norn-zen/6"}),
            json!({"axioms":[]}),
            json!({"required":"checked-proof"}),
        )
        .err(),
        Some(ChallengeError::NotAdmitted)
    );
}
#[test]
fn laws_and_resolved_reference_contracts_cannot_be_weakened() {
    let pool = pool();
    let authority = approve(&source(&pool));
    for (index, path, value) in [
        (3, "/payload/response/expression", json!("false")),
        (3, "/payload/condition", json!("false")),
        (
            1,
            "/payload/objectType/invariants/0/expression",
            json!("true"),
        ),
    ] {
        let mut candidate = pool.clone();
        if path.ends_with("condition") {
            candidate[index]["payload"]["condition"] = value;
        } else {
            *candidate[index].pointer_mut(path).expect("path") = value;
        }
        assert_eq!(
            authority.challenge(&source(&candidate), &BTreeSet::new()),
            Err(ChallengeError::LawChanged)
        );
    }
    let mut removed = pool;
    removed.remove(3);
    assert_eq!(
        authority.challenge(&source(&removed), &BTreeSet::new()),
        Err(ChallengeError::LawChanged)
    );
}
#[test]
fn exact_raw_literals_survive_or_are_refused_before_projection() {
    let pool = pool();
    let make = |literal: &str| {
        let mut candidate = pool.clone();
        candidate[1]["payload"]["objectType"]["properties"][1]["defaultValue"] =
            json!("RAW_NUMBER");
        serde_json::to_string(&candidate)
            .expect("source")
            .replace("\"RAW_NUMBER\"", literal)
    };
    // A payload number that typed JsonValue reduces to f64 may never share
    // identity silently. Exact representable integers pass ingress; lossy ones refuse.
    assert!(DefinitionSource::parse(&make("9007199254740992"), "[]").is_ok());
    assert_eq!(
        DefinitionSource::parse(&make("1e-999999"), "[]").err(),
        Some(ChallengeError::LossyInput)
    );
    assert_eq!(
        DefinitionSource::parse(&make("9007199254740993"), "[]").err(),
        Some(ChallengeError::LossyInput)
    );
    assert_eq!(
        DefinitionSource::parse(&make("1.0000000000000000000000000001"), "[]").err(),
        Some(ChallengeError::LossyInput)
    );
    assert_eq!(
        DefinitionSource::parse("[]", &make("9007199254740993")).err(),
        Some(ChallengeError::LossyInput)
    );
}
#[test]
fn stale_source_or_base_and_aggregate_exhaustion_are_not_success() {
    let mut pool = pool();
    let initial = source(&pool);
    let authority = approve(&initial);
    let old = authority
        .challenge(&initial, &BTreeSet::new())
        .expect("old");
    pool.reverse();
    let reordered = authority
        .challenge(&source(&pool), &BTreeSet::new())
        .expect("new source");
    assert_ne!(old.sha256(), reordered.sha256());
    assert_eq!(
        reordered.bind_submission(old.body()),
        Err(ChallengeError::InvalidSubmission)
    );
    let split = DefinitionSource::parse(
        &serde_json::to_string(&pool[..pool.len() - 3]).expect("candidate"),
        &serde_json::to_string(&pool[pool.len() - 3..]).expect("base"),
    )
    .expect("split");
    let based = authority
        .challenge(&split, &BTreeSet::new())
        .expect("new base");
    assert_ne!(based.sha256(), reordered.sha256());
    assert_eq!(
        DefinitionSource::parse(&" ".repeat(8_388_609), "[]").err(),
        Some(ChallengeError::Limit)
    );
}
#[test]
fn aggregate_goal_budget_refuses_before_materializing_per_law_inventory() {
    let original = pool();
    let mut large = original[..3].to_vec();
    for index in 0..1_000 {
        let mut law = original[3].clone();
        law["artifactId"] = json!(format!("erpnext.law_{index}"));
        law["payload"]["id"] = json!(format!("erpnext.law_{index}"));
        large.push(law);
    }
    let source = source(&large);
    let authority = approve(&source);
    assert_eq!(
        authority.challenge(&source, &BTreeSet::new()),
        Err(ChallengeError::Limit)
    );
}
fn rule(artifact_id: &str) -> Value {
    let mut rule = pool()[3].clone();
    rule["artifactId"] = json!(artifact_id);
    rule["kind"] = json!("rule");
    rule["dependencies"] = json!([]);
    let schema = json!({"type":"object","properties":{"approved":{"type":"boolean"}},"required":["approved"],"additionalProperties":false});
    rule["payload"] = json!({"ruleId":"erpnext.identity","ruleVersion":"2.0","contract":"sub_decision","scopeRequired":["orgId","environmentId"],"inputSchema":schema,"outputSchema":schema,"graph":{"nodes":[{"id":"input","name":"input","type":"inputNode"},{"id":"output","name":"output","type":"outputNode"}],"edges":[{"id":"edge","sourceId":"input","targetId":"output"}]}});
    rule
}
#[test]
fn an_artifact_and_a_transition_with_the_same_display_name_remain_distinct_writers() {
    let mut pool = pool();
    let authority = approve(&source(&pool));
    let id = "erpnext.SalesOrder:status:shipped";
    pool.push(rule(id));
    let challenge = authority
        .challenge(&source(&pool), &BTreeSet::new())
        .expect("admitted new writer");
    assert!(challenge.writers().contains(&LawWriter::Artifact {
        artifact_id: id.into()
    }));
    assert!(challenge.writers().contains(&LawWriter::Transition {
        object_type: "erpnext.SalesOrder".into(),
        property: "status".into(),
        transition: "shipped".into()
    }));
    let goals = challenge.body()["obligations"].as_array().expect("goals");
    assert!(
        goals
            .iter()
            .any(|g| g["goal"]["writer"]["kind"] == "artifact")
    );
    assert!(
        goals
            .iter()
            .any(|g| g["goal"]["writer"]["kind"] == "transition")
    );
}
#[test]
fn huge_writer_ids_cannot_multiply_into_unbounded_goal_allocations() {
    let original = pool();
    let mut pool = original[..3].to_vec();
    for index in 0..500 {
        let mut law = original[3].clone();
        law["artifactId"] = json!(format!("erpnext.law_{index}"));
        law["payload"]["id"] = json!(format!("erpnext.law_{index}"));
        pool.push(law);
    }
    pool.push(rule(&"w".repeat(4_194_304)));
    assert_eq!(
        DefinitionSource::parse(&serde_json::to_string(&pool).expect("source"), "[]").err(),
        Some(ChallengeError::Limit)
    );
}
#[test]
fn dependency_fanout_does_not_copy_a_large_source_id_per_edge() {
    let mut pool = pool();
    let mut large = rule(&"w".repeat(4_194_304));
    let mut dependencies = Vec::new();
    for index in 0..500 {
        let id = format!("erpnext.writer_{index}");
        dependencies.push(json!({"artifactId":id,"kind":"rule","version":"0.1.0","required":true}));
        pool.push(rule(&id));
    }
    large["dependencies"] = json!(dependencies);
    pool.push(large);
    let typed: Vec<norn_ir::ArtifactEnvelope> = pool
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("artifact"))
        .collect();
    let closure = norn_checker::verification::conservative_closure(
        &typed[3],
        &typed,
        &BTreeSet::new(),
        norn_graph::Budget {
            max_nodes: 1024,
            max_edges: 4096,
            max_steps: 65_536,
        },
    )
    .expect("borrowed dependency graph");
    assert_eq!(closure.artifacts.len(), typed.len());
    assert_eq!(
        DefinitionSource::parse(&serde_json::to_string(&pool).expect("source"), "[]").err(),
        Some(ChallengeError::Limit)
    );
}

#[test]
fn goal_byte_preflight_is_independent_of_goal_count_and_ingress_limits() {
    let original = pool();
    let mut pool = original[..3].to_vec();
    for index in 0..50 {
        let mut law = original[3].clone();
        law["artifactId"] = json!(format!("erpnext.law_{index}"));
        law["payload"]["id"] = json!(format!("erpnext.law_{index}"));
        pool.push(law);
    }
    let authority = approve(&source(&pool));
    for index in 0..100 {
        pool.push(rule(&format!("writer_{index}_{}", "w".repeat(256))));
    }
    assert_eq!(
        authority.challenge(&source(&pool), &BTreeSet::new()),
        Err(ChallengeError::Limit)
    );
}

fn source_in_layer(text: &str, in_base: bool) -> DefinitionSource {
    let (candidate, base) = if in_base { ("[]", text) } else { (text, "[]") };
    DefinitionSource::parse(candidate, base).expect("verbatim source")
}

fn assert_distinct_challenges(left: &Challenge, right: &Challenge) {
    assert_ne!(left.sha256(), right.sha256());
    assert_eq!(
        left.bind_submission(right.body()),
        Err(ChallengeError::InvalidSubmission)
    );
    assert_eq!(
        right.bind_submission(left.body()),
        Err(ChallengeError::InvalidSubmission)
    );
}

#[test]
fn non_nfc_expressions_are_admitted_and_exact_protected_identities_stay_distinct() {
    for (composed, decomposed, escaped) in [
        ("é", "e\u{301}", r"e\u0301"),
        ("أ", "ا\u{654}", r"\u0627\u0654"),
    ] {
        for (index, path) in [
            (3, "/payload/response/expression"),
            (3, "/payload/condition"),
            (1, "/payload/objectType/invariants/0/expression"),
        ] {
            let mut documents = pool();
            if path == "/payload/condition" {
                documents[index]["payload"]["condition"] = json!(format!("len('{composed}') == 2"));
            } else {
                *documents[index].pointer_mut(path).expect("expression") =
                    json!(format!("len('{composed}') == 2"));
            }
            let raw = serde_json::to_string(&documents).expect("source");
            let changed_raw = raw.replace(composed, decomposed);
            let changed_documents: Vec<Value> =
                serde_json::from_str(&changed_raw).expect("changed documents");
            let original_artifact: norn_ir::ArtifactEnvelope =
                serde_json::from_value(documents[index].clone()).expect("typed");
            let changed_artifact: norn_ir::ArtifactEnvelope =
                serde_json::from_value(changed_documents[index].clone()).expect("typed");
            // Ordinary Norn1 identities still normalize; protected identities must
            // distinguish literal bytes even when the ordinary hashes are equal.
            assert_eq!(
                norn_checker::ArtifactRef::of(&original_artifact).expect("ordinary"),
                norn_checker::ArtifactRef::of(&changed_artifact).expect("ordinary")
            );
            for in_base in [false, true] {
                let initial = source_in_layer(&raw, in_base);
                let authority = approve(&initial);
                let original = authority
                    .challenge(&initial, &BTreeSet::new())
                    .expect("ordinarily admitted challenge");
                let mut previous: Option<Challenge> = None;
                // Literal and JSON-escaped non-NFC text both pass independent
                // base admission / layered candidate admission, not just parsing.
                for text in [
                    changed_raw.clone(),
                    changed_raw.replace(decomposed, escaped),
                ] {
                    let changed_source = source_in_layer(&text, in_base);
                    assert_eq!(
                        authority.challenge(&changed_source, &BTreeSet::new()),
                        Err(ChallengeError::LawChanged)
                    );
                    let changed = approve(&changed_source)
                        .challenge(&changed_source, &BTreeSet::new())
                        .expect("ordinarily admitted non-NFC challenge");
                    assert_ne!(
                        original.body()["exactSourceDigest"],
                        changed.body()["exactSourceDigest"]
                    );
                    assert_ne!(
                        original.body()["closure"][&original_artifact.artifact_id],
                        changed.body()["closure"][&original_artifact.artifact_id]
                    );
                    assert_distinct_challenges(&original, &changed);
                    if let Some(previous) = &previous {
                        // Escaping changes document bytes, not the decoded artifact.
                        assert_eq!(previous.body()["closure"], changed.body()["closure"]);
                        assert_ne!(
                            previous.body()["exactSourceDigest"],
                            changed.body()["exactSourceDigest"]
                        );
                        assert_distinct_challenges(previous, &changed);
                    }
                    previous = Some(changed);
                }
            }
        }
    }
}

#[test]
fn nested_unicode_keys_and_values_are_ordinarily_admitted_in_both_layers() {
    let mut documents = pool();
    // Use the existing importer metadata surface; an object default for the
    // fixture's Boolean property would not be an ordinarily admitted example.
    documents[0]["payload"]["extensions"] =
        json!({"test.unicode/0.1.0":{"é":{"nested":[{"أ":"أ"}]}}});
    let raw = serde_json::to_string(&documents).expect("source");
    for in_base in [false, true] {
        let initial = source_in_layer(&raw, in_base);
        let authority = approve(&initial);
        let original = authority
            .challenge(&initial, &BTreeSet::new())
            .expect("ordinarily admitted metadata");
        let decomposed = raw.replace('é', "e\u{301}").replace('أ', "ا\u{654}");
        let mut previous: Option<Challenge> = None;
        for text in [
            decomposed.clone(),
            decomposed
                .replace("e\u{301}", r"e\u0301")
                .replace("ا\u{654}", r"\u0627\u0654"),
        ] {
            let changed_source = source_in_layer(&text, in_base);
            assert_eq!(
                authority.challenge(&changed_source, &BTreeSet::new()),
                Err(ChallengeError::LawChanged)
            );
            let changed = approve(&changed_source)
                .challenge(&changed_source, &BTreeSet::new())
                .expect("ordinarily admitted non-NFC keys and values");
            assert_ne!(
                original.body()["closure"]["erpnext.SalesOrder.entity"],
                changed.body()["closure"]["erpnext.SalesOrder.entity"]
            );
            assert_ne!(
                original.body()["exactSourceDigest"],
                changed.body()["exactSourceDigest"]
            );
            assert_distinct_challenges(&original, &changed);
            if let Some(previous) = &previous {
                assert_eq!(previous.body()["closure"], changed.body()["closure"]);
                assert_distinct_challenges(previous, &changed);
            }
            previous = Some(changed);
        }
    }
}

#[test]
fn all_authority_profiles_retain_non_nfc_keys_and_text_in_actual_challenges() {
    let source = source(&pool());
    for index in 0..3 {
        let mut profiles = [
            json!({"expression":"norn-zen/6"}),
            json!({"axioms":[]}),
            json!({"proof":"checked"}),
        ];
        profiles[index] = json!({"é":{"nested":[{"أ":"أ"}]}});
        let make = |profiles: &[Value; 3]| {
            ApprovedContract::approve(
                &source,
                &BTreeSet::new(),
                profiles[0].clone(),
                profiles[1].clone(),
                profiles[2].clone(),
            )
            .expect("profile approval")
            .challenge(&source, &BTreeSet::new())
            .expect("profile challenge")
        };
        let original = make(&profiles);
        let mut seen = vec![original.clone()];
        for profile in [
            json!({"e\u{301}":{"nested":[{"أ":"أ"}]}}),
            json!({"é":{"nested":[{"ا\u{654}":"ا\u{654}"}]}}),
            // Exact profiles can even distinguish NFC-equivalent keys within
            // one map: they never pass through Norn1 serialization.
            json!({"é":"أ","e\u{301}":"ا\u{654}"}),
        ] {
            profiles[index] = profile.clone();
            let changed = make(&profiles);
            assert_eq!(
                changed.body()[["semantics", "assumptions", "assurance"][index]],
                profile
            );
            assert_eq!(
                original.body()["exactSourceDigest"],
                changed.body()["exactSourceDigest"]
            );
            for previous in &seen {
                assert_distinct_challenges(previous, &changed);
            }
            seen.push(changed);
        }
    }
}

#[test]
fn verbatim_profiles_still_enforce_retained_byte_and_json_safety_limits() {
    let source = source(&pool());
    let approve_profile = |profile| {
        ApprovedContract::approve(
            &source,
            &BTreeSet::new(),
            profile,
            json!({"axioms":[]}),
            json!({"proof":"checked"}),
        )
    };
    // {"text":""} occupies 11 bytes. Measure the retained serialization,
    // including decomposed UTF-8, rather than the shorter NFC equivalent.
    assert!(approve_profile(json!({"text":"a".repeat(65_525)})).is_ok());
    for text in ["a".repeat(65_526), "e\u{301}".repeat(22_000)] {
        assert_eq!(
            approve_profile(json!({"text":text})).err(),
            Some(ChallengeError::Limit)
        );
    }
    let mut nested = json!("e\u{301}");
    for _ in 0..=norn_canonical::MAX_JSON_NESTING_DEPTH {
        nested = json!([nested]);
    }
    assert_eq!(
        approve_profile(json!({"nested":nested})).err(),
        Some(ChallengeError::LossyInput)
    );
}

#[test]
fn verbatim_source_preserves_existing_json_and_ordinary_receipt_refusals() {
    let mut documents = pool();
    documents[0]["payload"]["extensions"] = json!({"test.unicode/0.1.0":{"é":"value"}});
    let raw = serde_json::to_string(&documents).expect("source");
    for invalid in [
        raw.replace(r#""é":"value""#, r#""é":"value","\u00e9":"duplicate""#),
        format!("{raw} trailing"),
        format!("{}0{}", "[".repeat(257), "]".repeat(257)),
    ] {
        for (candidate, base) in [(invalid.as_str(), "[]"), ("[]", invalid.as_str())] {
            assert_eq!(
                DefinitionSource::parse(candidate, base).err(),
                Some(ChallengeError::LossyInput)
            );
        }
    }
    // This is an existing Norn1 receipt collision, not a blanket NFC policy.
    documents[0]["payload"]["extensions"] =
        json!({"test.unicode/0.1.0":{"é":"one","e\u{301}":"two"}});
    let raw = serde_json::to_string(&documents).expect("source");
    for in_base in [false, true] {
        let source = source_in_layer(&raw, in_base);
        assert_eq!(
            ApprovedContract::approve(
                &source,
                &BTreeSet::new(),
                json!({"expression":"norn-zen/6"}),
                json!({"axioms":[]}),
                json!({"proof":"checked"}),
            )
            .err(),
            Some(ChallengeError::NotAdmitted)
        );
    }
}

#[test]
fn protected_base_profile_distinguishes_retained_layers_from_invalid_definitions() {
    let original = pool();
    let base = vec![original[0].clone()];
    let mut extension = original[0].clone();
    extension["artifactId"] = json!("acme.SalesOrder.extension");
    extension["namespace"] = json!("acme");
    extension["payload"] = json!({"entityVersion":"2.0", "objectTypeExtension":{
        "namespace":"acme", "target":"erpnext.SalesOrder",
        "properties":[{"id":"acme__flag", "type":{"kind":"boolean"}}]
    }});
    let parse = |candidate: &[Value], base: &[Value]| {
        DefinitionSource::parse(
            &serde_json::to_string(candidate).expect("candidate"),
            &serde_json::to_string(base).expect("base"),
        )
        .expect("source")
    };
    let source = parse(&[extension.clone()], &base);
    let authority = approve(&source);
    authority
        .challenge(&source, &BTreeSet::new())
        .expect("extension challenge");
    let refuses = |candidate: &[Value], base: &[Value], expected: ChallengeError| {
        let source = parse(candidate, base);
        assert_eq!(
            ApprovedContract::approve(
                &source,
                &BTreeSet::new(),
                json!({"zen":"6"}),
                json!({"axioms":[]}),
                json!({"proof":"checked"})
            )
            .err(),
            Some(expected.clone())
        );
        assert_eq!(
            authority.challenge(&source, &BTreeSet::new()).err(),
            Some(expected)
        );
    };
    let mut local = base.clone();
    local.push(extension.clone());
    // The same diagnostic is invalid in the candidate, but unsupported retained
    // layer authority in the base. Neither placement is admitted.
    refuses(&local, &[], ChallengeError::NotAdmitted);
    for candidate in [vec![], vec![original[1].clone()]] {
        refuses(
            &candidate,
            &local,
            ChallengeError::MultiLayerBaseRequiresRetainedInput,
        );
    }
    assert_eq!(
        ChallengeError::MultiLayerBaseRequiresRetainedInput.to_string(),
        "multi-layer base requires a verified retained-layer input (#3604)"
    );
    let mut malformed = base.clone();
    malformed[0]["payload"]["objectType"]["properties"][0]["type"] = json!({"kind":"unknown"});
    refuses(&[extension], &malformed, ChallengeError::NotAdmitted);
    // Unrelated malformed content takes precedence over the layer refusal.
    let mut mixed = local;
    mixed[1]["payload"]["objectTypeExtension"]["properties"] = json!([
        {"id":"acme__flag", "type":{"kind":"boolean"}},
        {"id":"acme__flag", "type":{"kind":"boolean"}}
    ]);
    refuses(&[], &mixed, ChallengeError::NotAdmitted);
    mixed.reverse();
    refuses(&[], &mixed, ChallengeError::NotAdmitted);
    // Base law depends on a candidate entity: candidate data cannot repair it.
    refuses(&base, &[original[3].clone()], ChallengeError::NotAdmitted);
}

#[test]
fn public_challenges_bind_original_source_bytes_and_accepted_law_edits() {
    let mut documents = pool();
    documents[3]["payload"]["response"]["expression"] = json!("len('é') == 2");
    let raw = serde_json::to_string(&documents).expect("source");
    let initial = DefinitionSource::parse(&raw, "[]").expect("source");
    let authority = approve(&initial);
    let challenge = authority
        .challenge(&initial, &BTreeSet::new())
        .expect("challenge");
    for equivalent in [raw.replace('é', r"\u00e9"), format!(" {raw}\n")] {
        let source = DefinitionSource::parse(&equivalent, "[]").expect("accepted source");
        let changed = authority
            .challenge(&source, &BTreeSet::new())
            .expect("challenge");
        assert_eq!(
            challenge.body()["admissionReceipt"],
            changed.body()["admissionReceipt"]
        );
        assert_eq!(challenge.body()["closure"], changed.body()["closure"]);
        assert_ne!(
            challenge.body()["exactSourceDigest"],
            changed.body()["exactSourceDigest"]
        );
        assert_ne!(challenge.sha256(), changed.sha256());
        assert_eq!(
            changed.bind_submission(challenge.body()),
            Err(ChallengeError::InvalidSubmission)
        );
    }
    documents[3]["payload"]["response"]["expression"] = json!("len('é') == 3");
    let edited = source(&documents);
    assert_eq!(
        authority.challenge(&edited, &BTreeSet::new()),
        Err(ChallengeError::LawChanged)
    );
    let changed = approve(&edited)
        .challenge(&edited, &BTreeSet::new())
        .expect("new approval");
    assert_ne!(
        challenge.body()["closure"]["erpnext.no_unpaid_ship.law"],
        changed.body()["closure"]["erpnext.no_unpaid_ship.law"]
    );
    assert_ne!(challenge.sha256(), changed.sha256());
    for stale in [
        "norn.application-obligations/1",
        "norn.application-obligations/2",
        "norn.application-obligations/3",
        "norn.application-obligations/4",
        "norn.application-obligations/5",
        "norn.application-obligations/6",
    ] {
        let mut body = challenge.body().clone();
        body["generator"] = json!(stale);
        assert_eq!(
            challenge.bind_submission(&body),
            Err(ChallengeError::InvalidSubmission)
        );
    }
}
