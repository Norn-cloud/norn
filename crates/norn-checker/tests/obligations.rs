#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "protected challenge regression tests"
)]
use norn_checker::obligations::{ApprovedContract, Challenge, ChallengeError, DefinitionSource};
use norn_checker::verification::LawWriter;
use serde_json::{Value, json};
use sha2::Digest;
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
        ("/obligations/0/statement/rule", json!("trivial")),
        ("/obligations/0/assessment", json!("proven")),
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
    let writers = challenge.body()["writerStatements"]
        .as_array()
        .expect("writer statements");
    assert!(
        goals
            .iter()
            .any(|g| g["goal"]["writer"].as_u64().is_some_and(|index| writers
                [usize::try_from(index).expect("writer index")]["identity"]["kind"]
                == "artifact"))
    );
    assert!(
        goals
            .iter()
            .any(|g| g["goal"]["writer"].as_u64().is_some_and(|index| writers
                [usize::try_from(index).expect("writer index")]["identity"]["kind"]
                == "transition"))
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

fn frame_pool() -> Vec<Value> {
    let mut pool = pool();
    pool[3]["payload"]["modality"] = json!({"kind":"always"});
    pool[3]["payload"]["response"]["expression"] = json!("self.paid == true");
    pool
}

fn sales_transition() -> LawWriter {
    LawWriter::Transition {
        object_type: "erpnext.SalesOrder".into(),
        property: "status".into(),
        transition: "shipped".into(),
    }
}

fn goal<'a>(
    challenge: &'a Challenge,
    law: &str,
    kind: &str,
    writer: Option<&LawWriter>,
) -> &'a Value {
    let law_index = challenge.body()["lawStatements"]
        .as_array()
        .expect("laws")
        .iter()
        .position(|record| record["artifactId"] == law)
        .expect("law index");
    let writer_index = writer.map(|writer| {
        challenge.body()["writerStatements"]
            .as_array()
            .expect("writers")
            .iter()
            .position(|record| record["identity"] == writer.to_json())
            .expect("writer index")
    });
    challenge.body()["obligations"]
        .as_array()
        .expect("goals")
        .iter()
        .find(|goal| {
            goal["law"] == json!(law_index)
                && goal["goal"]["kind"] == kind
                && writer_index.is_none_or(|index| goal["goal"]["writer"] == json!(index))
        })
        .expect("required goal")
}

fn challenge(pool: &[Value]) -> Challenge {
    let source = source(pool);
    approve(&source)
        .challenge(&source, &BTreeSet::new())
        .expect("challenge")
}

#[test]
fn property_condition_and_scope_reads_block_transition_frame() {
    let law = "erpnext.no_unpaid_ship.law";
    let baseline = challenge(&pool());
    assert_eq!(
        goal(&baseline, law, "preservation", Some(&sales_transition()))["assessment"],
        "unknown"
    );
    let mut condition = frame_pool();
    condition[3]["payload"]["condition"] = json!("self.status == 'open'");
    let mut scope = frame_pool();
    scope[3]["payload"]["scope"]["lifecycleStates"] =
        json!([{"objectType":"erpnext.SalesOrder","property":"status","states":["open"]}]);
    let mut computed = frame_pool();
    computed[0]["payload"]["objectType"]["properties"].as_array_mut().expect("properties")
        .push(json!({"id":"derived_paid","type":{"kind":"boolean"},"computed":"self.status == 'open'"}));
    computed[3]["payload"]["response"]["expression"] = json!("self.derived_paid");
    let mut actor = frame_pool();
    actor[3]["payload"]["response"]["expression"] = json!("actor.subjectId == 'operator'");
    for pool in [condition, scope, actor, computed] {
        assert_eq!(
            goal(
                &challenge(&pool),
                law,
                "preservation",
                Some(&sales_transition())
            )["assessment"],
            "unknown"
        );
    }
}

#[test]
fn never_can_frame_but_temporal_and_invariant_responses_stay_unknown() {
    let mut never = frame_pool();
    never[3]["payload"]["modality"] = json!({"kind":"never"});
    assert_eq!(
        goal(
            &challenge(&never),
            "erpnext.no_unpaid_ship.law",
            "preservation",
            Some(&sales_transition())
        )["assessment"],
        "transition_frame"
    );
    for modality in [
        json!({"kind":"eventually"}),
        json!({"kind":"until","predicate":"self.paid == true"}),
    ] {
        let mut temporal = frame_pool();
        temporal[3]["payload"]["modality"] = modality;
        assert_eq!(
            goal(
                &challenge(&temporal),
                "erpnext.no_unpaid_ship.law",
                "preservation",
                Some(&sales_transition())
            )["assessment"],
            "unknown"
        );
    }
    let challenge = challenge(&pool());
    assert!(
        challenge.body()["obligations"]
            .as_array()
            .expect("goals")
            .iter()
            .all(|goal| goal["assessment"] == "unknown")
    );
}

#[test]
fn guard_and_transition_relation_digests_bind_effective_semantics() {
    let initial = source(&frame_pool());
    let authority = approve(&initial);
    let baseline = authority
        .challenge(&initial, &BTreeSet::new())
        .expect("initial challenge");
    let mut guarded = frame_pool();
    guarded[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][0]["guards"] =
        json!(["self.paid == true"]);
    let changed = challenge(&guarded);
    assert_eq!(
        authority.challenge(&source(&guarded), &BTreeSet::new()),
        Err(ChallengeError::LawChanged)
    );
    let writer = |challenge: &Challenge| {
        challenge.body()["writerStatements"]
            .as_array()
            .expect("writers")
            .iter()
            .find(|record| record["identity"] == sales_transition().to_json())
            .expect("transition")
            .clone()
    };
    assert_ne!(
        writer(&baseline)["relation"]["transitionDigest"],
        writer(&changed)["relation"]["transitionDigest"]
    );
    assert_eq!(writer(&changed)["relation"]["rule"], "transition_only");
    assert_eq!(
        goal(
            &changed,
            "erpnext.no_unpaid_ship.law",
            "preservation",
            Some(&sales_transition())
        )["assessment"],
        "transition_frame"
    );
    assert_eq!(
        changed.bind_submission(baseline.body()),
        Err(ChallengeError::InvalidSubmission)
    );
    for field in [
        "lawStatements",
        "writerStatements",
        "inductionRules",
        "closure",
    ] {
        let mut altered = changed.body().clone();
        altered[field] = json!({"replacement":"unbound"});
        assert_eq!(
            changed.bind_submission(&altered),
            Err(ChallengeError::InvalidSubmission)
        );
    }
    let frame = goal(
        &changed,
        "erpnext.no_unpaid_ship.law",
        "preservation",
        Some(&sales_transition()),
    );
    let index = changed.body()["obligations"]
        .as_array()
        .expect("goals")
        .iter()
        .position(|goal| goal == frame)
        .expect("frame index");
    for field in ["law", "writer"] {
        let mut altered = changed.body().clone();
        altered["obligations"][index]["statement"][field] = json!(9999);
        assert_eq!(
            changed.bind_submission(&altered),
            Err(ChallengeError::InvalidSubmission)
        );
    }
}

#[test]
fn inherited_guards_are_bound_on_each_concrete_transition_writer() {
    let mut pool = frame_pool();
    pool[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][0]["guards"] =
        json!(["self.paid == true"]);
    let mut child = pool[0].clone();
    child["artifactId"] = json!("erpnext.SpecialOrder.entity");
    child["dependencies"] = json!([{"artifactId":"erpnext.SalesOrder.entity","kind":"entity","version":"0.1.0","required":true}]);
    child["payload"]["objectType"] =
        json!({"name":"erpnext.SpecialOrder","extends":"erpnext.SalesOrder"});
    pool.push(child);
    let challenge = challenge(&pool);
    let writers = challenge.body()["writerStatements"]
        .as_array()
        .expect("writers");
    let child = LawWriter::Transition {
        object_type: "erpnext.SpecialOrder".into(),
        property: "status".into(),
        transition: "shipped".into(),
    };
    let relation = |writer: &LawWriter| {
        writers
            .iter()
            .find(|record| record["identity"] == writer.to_json())
            .expect("writer")["relation"]
            .clone()
    };
    assert_eq!(relation(&child), relation(&sales_transition()));
    assert_eq!(
        goal(
            &challenge,
            "erpnext.no_unpaid_ship.law",
            "preservation",
            Some(&child)
        )["assessment"],
        "unknown"
    );
    assert_eq!(challenge.writers().len(), writers.len());
}

#[test]
fn initialization_and_reachable_witness_are_not_discharged_by_constant_true() {
    let mut pool = frame_pool();
    pool[3]["payload"]["response"]["expression"] = json!("true");
    let challenge = challenge(&pool);
    assert_eq!(
        goal(
            &challenge,
            "erpnext.no_unpaid_ship.law",
            "initialization",
            None
        )["assessment"],
        "unknown"
    );
    assert_eq!(
        goal(
            &challenge,
            "erpnext.no_unpaid_ship.law",
            "non_vacuity",
            None
        )["assessment"],
        "unknown"
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
    use ChallengeError::{Limit, NotAdmitted};
    let costly = format!("[{}]", vec!["1e400"; 22_000].join(","));
    for (text, error) in [("{}", NotAdmitted), (costly.as_str(), Limit)] {
        for (candidate, base) in [(text, "[]"), ("[]", text)] {
            let refusal = DefinitionSource::parse(candidate, base).err();
            assert_eq!(refusal, Some(error.clone()));
        }
    }
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
        "norn.application-obligations/7",
    ] {
        let mut body = challenge.body().clone();
        body["generator"] = json!(stale);
        assert_eq!(
            challenge.bind_submission(&body),
            Err(ChallengeError::InvalidSubmission)
        );
    }
}

#[test]
fn public_induction_goals_bind_exact_law_and_transition_commitments() {
    let make = |expression: &str| {
        let mut documents = frame_pool();
        documents[3]["payload"]["response"]["expression"] = json!(expression);
        documents[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][0]["guards"] =
            json!([expression]);
        challenge(&documents)
    };
    let left = make("len('é') == 2");
    // Keep the accepted law-edit case and add NFC-equivalent source whose
    // literal spelling changes evaluation and the actual transition commitment.
    for expression in ["len('é') == 3", "len('e\u{301}') == 2"] {
        let right = make(expression);
        let statement = |challenge: &Challenge, table: &str, key: &str, value: &Value| {
            challenge.body()[table]
                .as_array()
                .expect("table")
                .iter()
                .find(|record| &record[key] == value)
                .expect("statement")
                .clone()
        };
        let law_id = json!("erpnext.no_unpaid_ship.law");
        let left_law = statement(&left, "lawStatements", "artifactId", &law_id);
        let right_law = statement(&right, "lawStatements", "artifactId", &law_id);
        assert_eq!(
            left_law["sha256"],
            left.body()["closure"]["erpnext.no_unpaid_ship.law"]
        );
        assert_ne!(left_law["sha256"], right_law["sha256"]);
        let writer_id = sales_transition().to_json();
        assert_ne!(
            statement(&left, "writerStatements", "identity", &writer_id)["relation"]["transitionDigest"],
            statement(&right, "writerStatements", "identity", &writer_id)["relation"]["transitionDigest"]
        );
        assert_ne!(left.sha256(), right.sha256());
        assert_eq!(
            right.bind_submission(left.body()),
            Err(ChallengeError::InvalidSubmission)
        );
        assert_eq!(left.body()["generator"], "norn.application-obligations/9");
        assert_eq!(
            left.body()["inductionRules"]["profile"],
            "norn.induction-statements/2"
        );
    }
}

#[test]
fn transition_frames_only_assess_conditional_preservation() {
    let mut documents = pool();
    documents[3]["payload"]["modality"] = json!({"kind":"always"});
    documents[3]["payload"]["response"]["expression"] = json!("self.paid == true");
    let definitions = source(&documents);
    let challenge = approve(&definitions)
        .challenge(&definitions, &BTreeSet::new())
        .expect("challenge");
    let body = challenge.body();
    let law = body["lawStatements"]
        .as_array()
        .expect("laws")
        .iter()
        .position(|entry| entry["artifactId"] == documents[3]["artifactId"])
        .expect("law");
    let transition = LawWriter::Transition {
        object_type: "erpnext.SalesOrder".into(),
        property: "status".into(),
        transition: "shipped".into(),
    };
    let writer = body["writerStatements"]
        .as_array()
        .expect("writers")
        .iter()
        .position(|entry| entry["identity"] == transition.to_json())
        .expect("writer");
    let mut preservation = false;
    for goal in body["obligations"]
        .as_array()
        .expect("goals")
        .iter()
        .filter(|entry| entry["law"] == json!(law))
    {
        if goal["goal"]["kind"] == "preservation" && goal["goal"]["writer"] == json!(writer) {
            assert_eq!(goal["assessment"], "transition_frame");
            preservation = true;
        } else {
            assert_eq!(goal["assessment"], "unknown");
        }
    }
    assert!(preservation);
}

struct LayerFixture {
    source: DefinitionSource,
    source_sha256: String,
    receipt: String,
    receipt_sha256: String,
}

fn retained_fixture() -> Vec<LayerFixture> {
    let mut base = Vec::new();
    let original = pool();
    let mut candidates = vec![vec![original[0].clone()]];
    for namespace in ["acme", "other"] {
        let mut extension = original[0].clone();
        extension["artifactId"] = json!(format!("{namespace}.SalesOrder.extension"));
        extension["namespace"] = json!(namespace);
        extension["payload"] = json!({"entityVersion":"2.0", "objectTypeExtension":{
            "namespace":namespace, "target":"erpnext.SalesOrder",
            "properties":[{"id":format!("{namespace}__flag"), "type":{"kind":"boolean"}}]
        }});
        candidates.push(vec![extension]);
    }
    candidates
        .into_iter()
        .map(|candidate| {
            let artifacts: Vec<norn_ir::ArtifactEnvelope> =
                serde_json::from_value(json!(candidate)).expect("typed");
            let source =
                DefinitionSource::parse(&json!(candidate).to_string(), &json!(base).to_string())
                    .expect("exact");
            let checked = norn_checker::check_artifact_set_with_receipt(
                &norn_checker::ReceiptSources::new(
                    &artifacts
                        .iter()
                        .map(|a| norn_ir::runtime::encode(a).expect("native source"))
                        .collect::<Vec<_>>(),
                    &base
                        .iter()
                        .map(|a| norn_ir::runtime::encode(a).expect("native base"))
                        .collect::<Vec<_>>(),
                )
                .expect("receipt sources"),
                &norn_checker::EntityArtifactSetContext {
                    registry: &BTreeSet::new(),
                    base: &base,
                    tenant: None,
                },
                &norn_ir::ResourceLimitProfile::norn_limits_v1(),
            )
            .expect("receipt");
            assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
            base.extend(artifacts);
            LayerFixture {
                source_sha256: source.exact_digest().to_owned(),
                source,
                receipt: String::from_utf8(checked.receipt.canonical_json().expect("canonical"))
                    .expect("UTF-8"),
                receipt_sha256: checked.receipt.content_sha256().expect("hash"),
            }
        })
        .collect()
}

fn verify_layers(
    layers: &[LayerFixture],
) -> Result<norn_checker::obligations::RetainedLayers, ChallengeError> {
    let retained: Vec<_> = layers
        .iter()
        .map(|layer| norn_checker::obligations::RetainedLayer {
            source: &layer.source,
            source_sha256: &layer.source_sha256,
            receipt: &layer.receipt,
            receipt_sha256: &layer.receipt_sha256,
        })
        .collect();
    norn_checker::obligations::RetainedLayers::verify(&retained, &BTreeSet::new())
}

fn normative_fixture(
    padding: &str,
) -> (
    norn_checker::obligations::RetainedLayers,
    norn_ir::PackInstanceRef,
) {
    let entity = pool()[0].clone();
    let mut leaf = entity.clone();
    leaf["artifactId"] = json!("erpnext.authority.pack");
    leaf["kind"] = json!("business_pack");
    leaf["payload"] = json!({"packId":"erpnext.authority", "packVersion":"0.1.0",
        "artifacts":[{"artifactId":entity["artifactId"],"version":"0.1.0","kind":"entity"}]});
    let typed = serde_json::from_value(leaf.clone()).expect("leaf");
    let pin = norn_checker::ArtifactRef::of(&typed).expect("pin");
    let reference: norn_ir::PackInstanceRef =
        serde_json::from_value(json!({"instanceId":"app-authority",
        "pack":{"artifactId":pin.artifact_id,"version":pin.version,"sha256":pin.sha256}}))
        .expect("reference");
    let mut holder = leaf.clone();
    holder["artifactId"] = json!("erpnext.instances.pack");
    holder["payload"]["packId"] = json!("erpnext.instances");
    holder["payload"]["instances"] = json!([
        {"instanceId":reference.instance_id,"scope":entity["scope"],"pack":reference.pack,"imports":{},"instances":[]},
        {"instanceId":"other-authority","scope":entity["scope"],"pack":reference.pack,"imports":{},"instances":[]}
    ]);
    let documents = vec![entity, leaf, holder];
    let artifacts: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_value(json!(documents)).expect("artifacts");
    let source =
        DefinitionSource::parse(&(padding.to_owned() + &json!(documents).to_string()), "[]")
            .expect("source");
    let sources = artifacts
        .iter()
        .map(|a| norn_ir::runtime::encode(a).expect("source"))
        .collect::<Vec<_>>();
    let registry = BTreeSet::new();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&sources, &[]).expect("sources"),
        &norn_checker::EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("receipt");
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let receipt =
        String::from_utf8(checked.receipt.canonical_json().expect("canonical")).expect("text");
    let digest = checked.receipt.content_sha256().expect("digest");
    let retained = norn_checker::obligations::RetainedLayers::verify(
        &[norn_checker::obligations::RetainedLayer {
            source: &source,
            source_sha256: source.exact_digest(),
            receipt: &receipt,
            receipt_sha256: &digest,
        }],
        &registry,
    )
    .expect("verified retained layer");
    (retained, reference)
}

#[test]
fn law_normative_context_binds_every_host_selection_and_refuses_stale_instances() {
    use norn_checker::obligations::normative::RetainedNormativeContext as Context;
    let (retained, enacting) = normative_fixture("");
    let original =
        Context::select(&retained, &enacting, "board", "1", 7, u64::MAX).expect("selection");
    assert_eq!(
        original.receipt()["authority"]["enactmentOrdinal"],
        json!(u64::MAX.to_string())
    );
    let (other, _) = normative_fixture(" ");
    for changed in [
        Context::select(&other, &enacting, "board", "1", 7, u64::MAX),
        Context::select(&retained, &enacting, "other", "1", 7, u64::MAX),
        Context::select(&retained, &enacting, "board", "2", 7, u64::MAX),
        Context::select(&retained, &enacting, "board", "1", 8, u64::MAX),
        Context::select(&retained, &enacting, "board", "1", 7, 0),
        Context::select(
            &retained,
            &norn_ir::PackInstanceRef {
                instance_id: "other-authority".into(),
                ..enacting.clone()
            },
            "board",
            "1",
            7,
            u64::MAX,
        ),
    ] {
        assert_ne!(
            changed.expect("new host selection").exact_digest(),
            original.exact_digest()
        );
    }
    let mut stale = enacting.clone();
    stale.pack.sha256 = "0".repeat(64);
    assert!(Context::select(&retained, &stale, "board", "1", 7, 0).is_err());
    stale = enacting;
    stale.instance_id = "missing".into();
    assert!(Context::select(&retained, &stale, "board", "1", 7, 0).is_err());
    let source = retained_candidate(&[], &retained);
    assert_eq!(
        ApprovedContract::approve_normative(
            &source,
            &other,
            &original,
            json!({"s":1}),
            json!({"a":1}),
            json!({"r":1})
        )
        .err(),
        Some(ChallengeError::InvalidSubmission)
    );
    let mut candidate = pool()[3]["payload"].clone();
    candidate["normativeContext"] = original.receipt().clone();
    assert!(serde_json::from_value::<norn_ir::deontic::LawDocument>(candidate).is_err());
}

proptest::proptest! {
    #[test]
    fn law_normative_receipt_and_challenge_change_with_any_enactment(rank in proptest::num::u32::ANY, ordinal in proptest::num::u64::ANY) {
        use norn_checker::obligations::normative::RetainedNormativeContext as Context;
        let (retained, enacting) = normative_fixture("");
        let left = Context::select(&retained,&enacting,"board","1",rank,ordinal).expect("selection");
        let right = Context::select(&retained,&enacting,"board","1",rank,ordinal^1).expect("selection");
        proptest::prop_assert_ne!(left.exact_digest(),right.exact_digest());
        let source = retained_candidate(&[], &retained);
        let challenge = |context| ApprovedContract::approve_normative(&source,&retained,context,json!({"s":1}),json!({"a":1}),json!({"r":1})).expect("approval").challenge_retained(&source,&retained).expect("challenge");
        let a = challenge(&left); let b = challenge(&right);
        proptest::prop_assert_ne!(a.sha256(),b.sha256());
        proptest::prop_assert!(a.bind_submission(b.body()).is_err());
        proptest::prop_assert_eq!(&a.body()["normativeContext"]["receipt"],left.receipt());
    }
}

#[test]
fn layers_chain_retains_exact_order_without_enabling_flat_challenges() {
    let layers = retained_fixture();
    let checked = verify_layers(&layers).expect("three retained layers");
    assert_eq!(checked.artifacts().len(), 3);
    assert!(checked.registry().is_empty());
    assert_eq!(checked.exact_digest().len(), 64);
    assert_eq!(
        checked.exact_digest(),
        verify_layers(&layers).expect("repeat").exact_digest()
    );
    let source =
        DefinitionSource::parse("[]", &json!(checked.artifacts()).to_string()).expect("source");
    assert_eq!(
        ApprovedContract::approve(
            &source,
            &BTreeSet::new(),
            json!({"semantics":1}),
            json!({"assumptions":1}),
            json!({"assurance":1})
        )
        .err(),
        Some(ChallengeError::MultiLayerBaseRequiresRetainedInput)
    );
}

#[test]
fn layers_exact_source_bytes_are_retained_separately_from_receipt_semantics() {
    let mut layers = retained_fixture();
    let original = verify_layers(&layers)
        .expect("original")
        .exact_digest()
        .to_owned();
    let candidate = format!(" {}", json!([pool()[0].clone()]));
    layers[0].source = DefinitionSource::parse(&candidate, "[]").expect("whitespace source");
    assert!(
        verify_layers(&layers).is_err(),
        "old exact commitment refuses byte edit"
    );
    layers[0].source_sha256 = layers[0].source.exact_digest().to_owned();
    assert_ne!(
        verify_layers(&layers)
            .expect("new authority retains source edit")
            .exact_digest(),
        original
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn layers_forged_reordered_missing_receipts_never_construct_authority(index in 0usize..3, mutation in 0u8..5) {
        let mut layers = retained_fixture();
        match mutation {
            0 => layers[index].receipt.clear(),
            1 => { layers.swap(0, if index == 0 { 1 } else { 2 }); }
            2 => { layers[index].receipt_sha256 = "0".repeat(64); }
            3 => {
                let mut receipt: Value = serde_json::from_str(&layers[index].receipt).expect("receipt");
                receipt["admitted"] = json!(false);
                let bytes = norn_canonical::canonical_json_of(&receipt).expect("canonical");
                layers[index].receipt_sha256 = format!("{:x}", sha2::Sha256::digest(&bytes));
                layers[index].receipt = String::from_utf8(bytes).expect("UTF-8");
            }
            4 => { layers.remove(0); }
            _ => unreachable!(),
        }
        proptest::prop_assert!(verify_layers(&layers).is_err());
    }
}

fn retained_candidate(
    candidate: &[Value],
    retained: &norn_checker::obligations::RetainedLayers,
) -> DefinitionSource {
    DefinitionSource::parse(
        &json!(candidate).to_string(),
        &json!(retained.artifacts()).to_string(),
    )
    .expect("candidate")
}

fn retained_law() -> Value {
    let mut law = pool()[3].clone();
    law["payload"]["scope"]["root"] = json!({"kind":"company", "id":"org_demo"});
    law
}

fn approve_layers(
    source: &DefinitionSource,
    retained: &norn_checker::obligations::RetainedLayers,
) -> ApprovedContract {
    ApprovedContract::approve_retained(
        source,
        retained,
        json!({"semantics":1}),
        json!({"assumptions":1}),
        json!({"assurance":"unknown"}),
    )
    .expect("retained approval")
}

#[test]
fn layers_challenge_binds_checked_authority_and_refuses_substituted_base() {
    let layers = retained_fixture();
    let retained = verify_layers(&layers).expect("authority");
    let source = retained_candidate(&[retained_law()], &retained);
    let approved = approve_layers(&source, &retained);
    let challenge = approved
        .challenge_retained(&source, &retained)
        .expect("retained challenge");
    assert_eq!(
        challenge.body()["retainedLayers"],
        json!(retained.exact_digest())
    );
    assert_eq!(
        challenge.body()["generator"],
        json!(norn_checker::obligations::GENERATOR_PROFILE)
    );
    assert!(approved.challenge(&source, &BTreeSet::new()).is_err());
    let other = verify_layers(&layers[..2]).expect("checked prefix");
    assert_eq!(
        approved.challenge_retained(&source, &other).err(),
        Some(ChallengeError::InvalidSubmission)
    );
    let substituted =
        DefinitionSource::parse(&json!([retained_law()]).to_string(), "[]").expect("source");
    assert!(
        ApprovedContract::approve_retained(
            &substituted,
            &retained,
            json!({"s":1}),
            json!({"a":1}),
            json!({"p":1})
        )
        .is_err()
    );
    let unsupported = retained_candidate(&[pool()[3].clone()], &retained);
    assert_eq!(
        ApprovedContract::approve_retained(
            &unsupported,
            &retained,
            json!({"s":1}),
            json!({"a":1}),
            json!({"p":1})
        )
        .err(),
        Some(ChallengeError::NotAdmitted),
        "unresolved app refuses"
    );
    let mut invalid = retained_law();
    invalid["artifactId"] = json!("");
    let invalid = retained_candidate(&[invalid], &retained);
    assert_eq!(
        ApprovedContract::approve_retained(&invalid, &retained, json!({}), json!({}), json!({}))
            .err(),
        Some(ChallengeError::NotAdmitted)
    );
}

#[test]
fn writer_new_regenerates_retained_goals_and_omissions_refuse() {
    let retained = verify_layers(&retained_fixture()).expect("authority");
    let source = retained_candidate(&[retained_law()], &retained);
    let approved = approve_layers(&source, &retained);
    let original = approved
        .challenge_retained(&source, &retained)
        .expect("challenge");
    let source = retained_candidate(&[retained_law(), rule("erpnext.new_writer")], &retained);
    let changed = approved
        .challenge_retained(&source, &retained)
        .expect("new writer");
    assert_ne!(original.sha256(), changed.sha256());
    assert!(changed.writers().contains(&LawWriter::Artifact {
        artifact_id: "erpnext.new_writer".into()
    }));
    assert!(
        changed.body()["obligations"]
            .as_array()
            .expect("goals")
            .len()
            > original.body()["obligations"]
                .as_array()
                .expect("goals")
                .len()
    );
    assert_eq!(
        changed.bind_submission(original.body()),
        Err(ChallengeError::InvalidSubmission)
    );
    let count = changed.body()["obligations"]
        .as_array()
        .expect("goals")
        .len();
    for index in 0..count {
        let mut omitted = changed.body().clone();
        omitted["obligations"]
            .as_array_mut()
            .expect("goals")
            .remove(index);
        assert_eq!(
            changed.bind_submission(&omitted),
            Err(ChallengeError::InvalidSubmission)
        );
    }
    let mut forged = changed.body().clone();
    forged["obligations"][0]["assessment"] = json!("proved");
    assert_eq!(
        changed.bind_submission(&forged),
        Err(ChallengeError::InvalidSubmission)
    );
    let mut weakened = retained_law();
    weakened["payload"]["response"]["expression"] = json!("false");
    let changed_law = retained_candidate(&[weakened], &retained);
    assert_eq!(
        approved.challenge_retained(&changed_law, &retained).err(),
        Some(ChallengeError::LawChanged)
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(8))]
    #[test]
    fn retained_challenge_source_spellings_change_commitment_without_changing_goals(padding in "[ \n\t]{1,8}") {
        let retained = verify_layers(&retained_fixture()).expect("authority");
        let source = retained_candidate(&[retained_law()], &retained);
        let approved = approve_layers(&source, &retained);
        let original = approved.challenge_retained(&source, &retained).expect("original");
        let edited = DefinitionSource::parse(&(padding + &json!([retained_law()]).to_string()), &json!(retained.artifacts()).to_string()).expect("source bytes");
        let changed = approved.challenge_retained(&edited, &retained).expect("same typed definitions");
        proptest::prop_assert_ne!(original.sha256(), changed.sha256());
        proptest::prop_assert_eq!(&original.body()["obligations"], &changed.body()["obligations"]);
        proptest::prop_assert_eq!(changed.bind_submission(original.body()), Err(ChallengeError::InvalidSubmission));
    }
}
