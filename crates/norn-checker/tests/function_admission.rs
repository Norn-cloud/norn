#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    missing_docs,
    reason = "integration-test crate"
)]

//! Function contract admission (T1-FN-1, #2776): the three records under all
//! three profiles admit, and every negative fixture under
//! `fixtures/function/invalid` (a mutable image tag or bare URI as code or
//! input identity, a secret input, a missing dependency closure, a
//! determinism claim without its evidence, an unbounded output) is refused
//! with exactly its code, plus the remaining rules exercised in code.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use norn_checker::Diagnostic;
use norn_checker::function::{
    AdmittedFunction, admit_function_manifest, check_function_invocation, check_function_receipt,
    codes, decode_function_invocation, decode_function_manifest, decode_function_receipt,
};
use norn_ir::RuntimeValue;
use norn_ir::function::{
    CacheOrigin, CachePolicy, Capability, DeterminismProfile, Digest, ExhaustedResource,
    FreshnessMode, FunctionInvocation, FunctionManifest, FunctionOutcome, FunctionReceipt,
    GoldenRun, ImplementationFormat, output_manifest_digest,
};

const PROFILES: [&str; 3] = ["wasm", "container", "effectful"];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/function")
}

fn read(name: &str) -> String {
    let path = fixture_dir().join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn manifest(profile: &str) -> FunctionManifest {
    decode_function_manifest(&read(&format!("manifest.{profile}.json")))
        .unwrap_or_else(|findings| panic!("manifest.{profile}: {findings:?}"))
}

fn invocation(profile: &str) -> FunctionInvocation {
    decode_function_invocation(&read(&format!("invocation.{profile}.json")))
        .unwrap_or_else(|findings| panic!("invocation.{profile}: {findings:?}"))
}

fn receipt(profile: &str) -> FunctionReceipt {
    decode_function_receipt(&read(&format!("receipt.{profile}.json")))
        .unwrap_or_else(|findings| panic!("receipt.{profile}: {findings:?}"))
}

fn admitted(profile: &str) -> AdmittedFunction {
    admit(&manifest(profile))
}

fn admit(manifest: &FunctionManifest) -> AdmittedFunction {
    admit_function_manifest(manifest).unwrap_or_else(|findings| panic!("{findings:?}"))
}

fn manifest_findings(manifest: &FunctionManifest) -> Vec<Diagnostic> {
    admit_function_manifest(manifest).err().unwrap_or_default()
}

fn found(findings: &[Diagnostic]) -> BTreeSet<(String, String)> {
    findings
        .iter()
        .map(|finding| {
            (
                finding.code.as_str().to_owned(),
                finding
                    .span
                    .as_ref()
                    .map_or_else(String::new, |span| span.pointer.clone()),
            )
        })
        .collect()
}

fn found_codes(findings: &[Diagnostic]) -> BTreeSet<String> {
    findings
        .iter()
        .map(|finding| finding.code.as_str().to_owned())
        .collect()
}

fn only(code: &str) -> BTreeSet<String> {
    BTreeSet::from([code.to_owned()])
}

#[test]
fn function_admission_admits_all_three_profiles() {
    for profile in PROFILES {
        let admitted = admitted(profile);
        assert_eq!(
            admitted.digest,
            manifest(profile).content_digest().expect("digest")
        );
        let invocation = invocation(profile);
        assert_eq!(
            check_function_invocation(&invocation, &admitted),
            Vec::new(),
            "invocation.{profile}"
        );
        assert_eq!(
            check_function_receipt(&receipt(profile), &invocation, &admitted),
            Vec::new(),
            "receipt.{profile}"
        );
    }
}

/// Every file under `fixtures/function/invalid` is refused with exactly the
/// expected code at the expected pointer, and every file is listed here.
#[test]
fn function_admission_refuses_every_negative_fixture() {
    let expected: [(&str, &str, &[&str]); 10] = [
        (
            "manifest.credential-locator.json",
            codes::EMBEDDED_SECRET,
            &["/implementation/locator"],
        ),
        (
            "invocation.bearer-authorization.json",
            codes::SECRET_INPUT,
            &["/inputs/authorization"],
        ),
        (
            "manifest.mutable-image-tag.json",
            codes::MUTABLE_IDENTITY,
            &["/implementation/locator"],
        ),
        (
            "manifest.bare-uri-code.json",
            codes::MUTABLE_IDENTITY,
            &["/implementation/digest"],
        ),
        (
            "manifest.missing-dependency-closure.json",
            codes::MISSING_DEPENDENCY_CLOSURE,
            &["/dependencyClosure"],
        ),
        (
            "manifest.determinism-without-evidence.json",
            codes::MISSING_DETERMINISM_EVIDENCE,
            &[
                "/determinism/qualified_wasm/importClosure",
                "/determinism/qualified_wasm/fuel",
            ],
        ),
        (
            "manifest.unbounded-output.json",
            codes::UNBOUNDED_RESOURCE,
            &["/bounds/maxOutputBytes"],
        ),
        (
            "invocation.secret-input.json",
            codes::SECRET_INPUT,
            &["/inputs/apiKey"],
        ),
        (
            "invocation.mutable-url-input.json",
            codes::MUTABLE_INPUT,
            &["/inputs/attachment"],
        ),
        (
            "invocation.object-key-blob.json",
            codes::MUTABLE_INPUT,
            &["/blobs/image/digest"],
        ),
    ];
    let listed: BTreeSet<String> = expected
        .iter()
        .map(|(file, _, _)| (*file).to_owned())
        .collect();
    let on_disk: BTreeSet<String> = fs::read_dir(fixture_dir().join("invalid"))
        .expect("invalid fixtures are listable")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("UTF-8 name")
        })
        .collect();
    assert_eq!(listed, on_disk, "every negative fixture has an expectation");

    for (file, code, pointers) in expected {
        let text = read(&format!("invalid/{file}"));
        let findings = if file.starts_with("manifest.") {
            let manifest = decode_function_manifest(&text)
                .unwrap_or_else(|findings| panic!("{file} decodes: {findings:?}"));
            manifest_findings(&manifest)
        } else {
            let invocation = decode_function_invocation(&text)
                .unwrap_or_else(|findings| panic!("{file} decodes: {findings:?}"));
            let profile = if file.contains("blob") {
                "container"
            } else {
                "wasm"
            };
            check_function_invocation(&invocation, &admitted(profile))
        };
        assert_eq!(found_codes(&findings), only(code), "{file}: {findings:?}");
        let expected: BTreeSet<(String, String)> = pointers
            .iter()
            .map(|pointer| (code.to_owned(), (*pointer).to_owned()))
            .collect();
        assert_eq!(found(&findings), expected, "{file}");
    }
}

#[test]
fn function_admission_undecodable_records_are_invalid_documents() {
    let findings = decode_function_manifest("{\"abi\":\"norn.function-abi/1\"}")
        .expect_err("a partial manifest does not decode");
    assert_eq!(found_codes(&findings), only(codes::INVALID_DOCUMENT));
    let findings =
        decode_function_receipt(&read("receipt.wasm.json").replacen('{', "{\"timestamp\":1,", 1))
            .expect_err("closed members");
    assert_eq!(found_codes(&findings), only(codes::INVALID_DOCUMENT));
}

#[test]
fn function_admission_manifest_rules() {
    let mut abi = manifest("wasm");
    abi.abi = "norn.function-abi/0".to_owned();
    assert_eq!(
        found_codes(&manifest_findings(&abi)),
        only(codes::UNSUPPORTED_ABI)
    );

    let mut capable = manifest("wasm");
    capable.capabilities.push(Capability {
        name: "net.egress".to_owned(),
        scope: "api.example.com".to_owned(),
    });
    assert_eq!(
        found_codes(&manifest_findings(&capable)),
        only(codes::PROFILE_MISMATCH),
        "deterministic profiles are capability-free"
    );

    let mut packaged = manifest("wasm");
    packaged.implementation.format = ImplementationFormat::OciImage;
    assert_eq!(
        found_codes(&manifest_findings(&packaged)),
        only(codes::PROFILE_MISMATCH)
    );

    let mut threaded = manifest("wasm");
    if let DeterminismProfile::QualifiedWasm(evidence) = &mut threaded.determinism {
        evidence.threads = Some(true);
        evidence.import_closure = Some(vec!["wasi_snapshot_preview1.fd_write".to_owned()]);
    }
    assert_eq!(
        found_codes(&manifest_findings(&threaded)),
        only(codes::INADMISSIBLE_EVIDENCE),
        "threads and any unadmitted import are refused"
    );

    let mut cached = manifest("container");
    cached.cache = CachePolicy::ActionCache;
    assert_eq!(
        found_codes(&manifest_findings(&cached)),
        only(codes::UNCACHEABLE_PROFILE),
        "an unqualified container is not cacheable"
    );

    let mut conflicting = manifest("container");
    if let DeterminismProfile::HermeticContainer(evidence) = &mut conflicting.determinism {
        evidence.golden_runs.push(GoldenRun {
            placement: "pool-ap-1/node-d".to_owned(),
            output_digest: Digest(format!("sha256:{}", "c".repeat(64))),
        });
    }
    assert_eq!(
        found_codes(&manifest_findings(&conflicting)),
        only(codes::CONFLICTING_GOLDEN_RUNS)
    );

    let mut one_placement = manifest("container");
    if let DeterminismProfile::HermeticContainer(evidence) = &mut one_placement.determinism {
        evidence.qualified = true;
        evidence.golden_runs.truncate(1);
    }
    assert_eq!(
        found_codes(&manifest_findings(&one_placement)),
        only(codes::MISSING_DETERMINISM_EVIDENCE),
        "qualification needs repeated cross-placement golden runs"
    );

    let mut qualified = manifest("container");
    if let DeterminismProfile::HermeticContainer(evidence) = &mut qualified.determinism {
        evidence.qualified = true;
    }
    qualified.cache = CachePolicy::ActionCache;
    assert!(manifest_findings(&qualified).is_empty());

    let mut networked = manifest("container");
    if let DeterminismProfile::HermeticContainer(evidence) = &mut networked.determinism {
        evidence.network = Some(true);
    }
    assert_eq!(
        found_codes(&manifest_findings(&networked)),
        only(codes::INADMISSIBLE_EVIDENCE)
    );

    let mut tampered = manifest("container");
    if let Some(closure) = &mut tampered.dependency_closure {
        closure.entries.pop();
    }
    assert_eq!(
        found_codes(&manifest_findings(&tampered)),
        only(codes::DEPENDENCY_CLOSURE_MISMATCH)
    );

    let mut empty_image = manifest("container");
    if let Some(closure) = &mut empty_image.dependency_closure {
        closure.entries.clear();
        closure.digest = closure.computed_digest().expect("digest");
    }
    assert_eq!(
        found_codes(&manifest_findings(&empty_image)),
        only(codes::MISSING_DEPENDENCY_CLOSURE)
    );

    let mut zero = manifest("effectful");
    zero.bounds.max_cpu_millis = Some(0);
    assert_eq!(
        found_codes(&manifest_findings(&zero)),
        only(codes::UNBOUNDED_RESOURCE)
    );

    let mut steps = manifest("container");
    let first = steps.steps[0].clone();
    steps.steps.push(first);
    assert_eq!(
        found_codes(&manifest_findings(&steps)),
        only(codes::DUPLICATE_STEP)
    );
}

#[test]
fn function_admission_invocation_rules() {
    let wasm = admitted("wasm");
    let check = |invocation: &FunctionInvocation| {
        found_codes(&check_function_invocation(invocation, &wasm))
    };

    let mut fork = invocation("wasm");
    fork.world = "fork-what-if-1".to_owned();
    assert_eq!(check(&fork), only(codes::FORK_PERFORM));

    let mut other = invocation("wasm");
    other.manifest = Digest(format!("sha256:{}", "a".repeat(64)));
    assert_eq!(check(&other), only(codes::MANIFEST_MISMATCH));

    let mut structured = invocation("wasm");
    structured.inputs.insert(
        "order".to_owned(),
        RuntimeValue::from_json_text("{\"lines\":[1,2]}").expect("value"),
    );
    assert_eq!(check(&structured), only(codes::NON_SCALAR_INPUT));

    let mut bearer = invocation("wasm");
    bearer.inputs.insert(
        "header".to_owned(),
        RuntimeValue::String("Bearer eyJhbGciOi".to_owned()),
    );
    assert_eq!(check(&bearer), only(codes::SECRET_INPUT));

    let mut mentions = invocation("wasm");
    mentions.inputs.insert(
        "messageText".to_owned(),
        RuntimeValue::String("see https://example.com/menu for prices".to_owned()),
    );
    assert!(
        check(&mentions).is_empty(),
        "text that mentions a URL is data, not a mutable reference"
    );

    let mut stale_token = invocation("wasm");
    stale_token.dependency = norn_ir::function::RelevantStateToken::default();
    assert_eq!(check(&stale_token), only(codes::FRESHNESS_SHAPE));

    let mut no_destination = invocation("wasm");
    no_destination.freshness = FreshnessMode::DestinationCas;
    no_destination.destination = None;
    assert_eq!(check(&no_destination), only(codes::FRESHNESS_SHAPE));

    let mut no_deadline = invocation("wasm");
    no_deadline.deadline_millis = 0;
    no_deadline.retry.max_cost_units = 0;
    assert_eq!(check(&no_deadline), only(codes::UNBOUNDED_COMMAND));

    let mut blank = invocation("wasm");
    blank.tenant = " ".to_owned();
    assert_eq!(check(&blank), only(codes::INVOCATION_BLANK_VALUE));

    let container = admitted("container");
    let mut huge = invocation("container");
    if let Some(blob) = huge.blobs.get_mut("image") {
        blob.size_bytes = 64 * 1024 * 1024;
    }
    assert_eq!(
        found_codes(&check_function_invocation(&huge, &container)),
        only(codes::INPUT_BOUND)
    );

    let effectful = admitted("effectful");
    let mut retries = invocation("effectful");
    retries.retry.max_attempts = 9;
    assert_eq!(
        found_codes(&check_function_invocation(&retries, &effectful)),
        only(codes::UNBOUNDED_COMMAND),
        "the command cannot exceed the producer's attempt ceiling"
    );
}

#[test]
fn function_admission_receipt_rules() {
    let wasm = admitted("wasm");
    let dispatched = invocation("wasm");
    let check =
        |receipt: &FunctionReceipt| found(&check_function_receipt(receipt, &dispatched, &wasm));
    let at = |code: &str, pointer: &str| (code.to_owned(), pointer.to_owned());

    let mut late = receipt("wasm");
    late.run.generation = 2;
    assert_eq!(
        check(&late),
        BTreeSet::from([at(codes::DISPATCH_MISMATCH, "/run")]),
        "a receipt of another generation is not this dispatch's"
    );

    let mut forged = receipt("wasm");
    forged.dispatch = Digest(format!("sha256:{}", "d".repeat(64)));
    forged.input_identity = Digest(format!("sha256:{}", "e".repeat(64)));
    assert_eq!(
        check(&forged),
        BTreeSet::from([
            at(codes::DISPATCH_MISMATCH, "/dispatch"),
            at(codes::DISPATCH_MISMATCH, "/inputIdentity"),
        ])
    );

    let mut runtime = receipt("wasm");
    runtime.provenance.runtime = Digest(format!("sha256:{}", "f".repeat(64)));
    assert_eq!(
        check(&runtime),
        BTreeSet::from([at(codes::DISPATCH_MISMATCH, "/provenance/runtime")])
    );

    let mut attempt = receipt("wasm");
    attempt.attempt = 3;
    assert_eq!(
        check(&attempt),
        BTreeSet::from([at(codes::ATTEMPT_OUT_OF_RANGE, "/attempt")])
    );

    let mut tampered = receipt("wasm");
    if let FunctionOutcome::Success(success) = &mut tampered.outcome {
        success.summary.insert(
            "intent".to_owned(),
            RuntimeValue::String("refund".to_owned()),
        );
    }
    assert_eq!(
        check(&tampered),
        BTreeSet::from([at(
            codes::OUTPUT_MANIFEST_MISMATCH,
            "/outcome/success/outputManifest"
        )])
    );

    let mut oversized = receipt("wasm");
    if let FunctionOutcome::Success(success) = &mut oversized.outcome {
        success.summary.insert(
            "transcript".to_owned(),
            RuntimeValue::String("x".repeat(40 * 1024)),
        );
        success.output_manifest =
            output_manifest_digest(&success.outputs, &success.summary).expect("digest");
    }
    assert_eq!(
        check(&oversized),
        BTreeSet::from([at(codes::SUMMARY_BOUND, "/outcome/success/summary")])
    );

    let mut overran = receipt("wasm");
    overran.resources.cpu_millis = 101;
    assert_eq!(
        check(&overran),
        BTreeSet::from([at(codes::BOUND_EXCEEDED, "/resources/cpuMillis")])
    );

    let mut exhausted = receipt("wasm");
    exhausted.outcome = FunctionOutcome::ResourceExhausted {
        resource: ExhaustedResource::Cpu,
    };
    exhausted.resources.cpu_millis = 101;
    assert!(
        check(&exhausted).is_empty(),
        "an exhausted attempt reports the use that stopped it"
    );

    let key = dispatched.action_key(&wasm.manifest).expect("key");
    let mut hit = receipt("wasm");
    hit.cache = CacheOrigin::Hit {
        action_key: key.clone(),
    };
    assert!(
        check(&hit).is_empty(),
        "a qualified WASM success may be served"
    );

    let mut wrong_key = receipt("wasm");
    wrong_key.cache = CacheOrigin::Hit {
        action_key: Digest(format!("sha256:{}", "0".repeat(64))),
    };
    assert_eq!(
        check(&wrong_key),
        BTreeSet::from([at(codes::CACHE_ORIGIN, "/cache/hit/actionKey")])
    );

    let mut cached_failure = exhausted;
    cached_failure.resources.cpu_millis = 1;
    cached_failure.cache = CacheOrigin::Hit { action_key: key };
    assert_eq!(
        check(&cached_failure),
        BTreeSet::from([at(codes::CACHE_ORIGIN, "/cache")]),
        "timeouts and exhaustion never become cache entries"
    );

    let container = admitted("container");
    let thumbnail = invocation("container");
    let mut uncacheable = receipt("container");
    uncacheable.cache = CacheOrigin::Hit {
        action_key: thumbnail.action_key(&container.manifest).expect("key"),
    };
    assert_eq!(
        found(&check_function_receipt(
            &uncacheable,
            &thumbnail,
            &container
        )),
        BTreeSet::from([at(codes::CACHE_ORIGIN, "/cache")]),
        "an unqualified container result is never served from the cache"
    );
}

/// The admitted manifest is what the identities are computed over: the same
/// manifest re-admitted has the same digest, and `admit` is pure.
#[test]
fn function_admission_is_deterministic() {
    let first = admit(&manifest("wasm"));
    let second = admit(&manifest("wasm"));
    assert_eq!(first, second);
    assert_ne!(first.digest, first.execution_profile);
}

/// Hashing NFC-normalizes text, so a decomposed string would share an
/// identity with its composed form: admission refuses it in every record.
#[test]
fn function_admission_refuses_non_nfc_text() {
    let mut decomposed = manifest("wasm");
    decomposed.implementation.entry_point = "classify\u{e9}".to_owned();
    assert!(
        manifest_findings(&decomposed).is_empty(),
        "composed text is canonical"
    );
    decomposed.implementation.entry_point = "classifye\u{301}".to_owned();
    assert_eq!(
        found_codes(&manifest_findings(&decomposed)),
        only(codes::MANIFEST_NON_CANONICAL_TEXT)
    );

    let wasm = admitted("wasm");
    let mut input = invocation("wasm");
    input.inputs.insert(
        "messageText".to_owned(),
        RuntimeValue::String("cafe\u{301} au lait".to_owned()),
    );
    assert_eq!(
        found_codes(&check_function_invocation(&input, &wasm)),
        only(codes::INVOCATION_NON_CANONICAL_TEXT)
    );
    let mut key = invocation("wasm");
    key.inputs.insert(
        "note\u{301}".to_owned(),
        RuntimeValue::String("x".to_owned()),
    );
    assert_eq!(
        found_codes(&check_function_invocation(&key, &wasm)),
        only(codes::INVOCATION_NON_CANONICAL_TEXT)
    );

    let dispatched = invocation("wasm");
    let mut summary = receipt("wasm");
    if let FunctionOutcome::Success(success) = &mut summary.outcome {
        success.summary.insert(
            "intent".to_owned(),
            RuntimeValue::String("re\u{301}sume\u{301}".to_owned()),
        );
        success.output_manifest =
            output_manifest_digest(&success.outputs, &success.summary).expect("digest");
    }
    assert_eq!(
        found_codes(&check_function_receipt(&summary, &dispatched, &wasm)),
        only(codes::RECEIPT_NON_CANONICAL_TEXT)
    );
}

/// Credentials are refused by name and by shape, case-insensitively, while
/// ordinary data that merely resembles a credential name is admitted.
#[test]
fn function_admission_detects_credentials_by_name_and_shape() {
    let wasm = admitted("wasm");
    let check = |name: &str, value: &str| {
        let mut invocation = invocation("wasm");
        invocation
            .inputs
            .insert(name.to_owned(), RuntimeValue::String(value.to_owned()));
        found_codes(&check_function_invocation(&invocation, &wasm))
    };
    for (name, value) in [
        ("authorization", "opaque"),
        ("Authorization", "opaque"),
        ("accessToken", "opaque"),
        ("token", "opaque"),
        ("apiKey", "opaque"),
        ("X-Api-Key", "opaque"),
        ("privateKey", "opaque"),
        ("clientSecret", "opaque"),
        ("dbPassword", "opaque"),
        ("note", "bearer eyJhbGciOi"),
        ("note", "BASIC dXNlcjpwYXNz"),
        (
            "note",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzdmMifQ.c2lnbmF0dXJl",
        ),
        ("note", "AKIAIOSFODNN7EXAMPLE"),
        ("note", "github_pat_11ABCDEFG0123456789"),
        ("accesstoken", "opaque"),
        ("privatekey", "opaque"),
        ("api_key_value", "opaque"),
        ("csrfToken", "opaque"),
        ("refresh_token_v2", "opaque"),
    ] {
        assert_eq!(
            check(name, value),
            only(codes::SECRET_INPUT),
            "{name}: {value}"
        );
    }
    for (name, value) in [
        ("maxTokens", "256"),
        ("tokenCount", "12"),
        ("sortKey", "created"),
        ("idempotencyKey", "msg-42"),
        ("note", "please send the basic plan"),
    ] {
        assert!(check(name, value).is_empty(), "{name}: {value}");
    }

    for scope in [
        "Bearer sk-live-0000",
        "https://api.example.com?access_token=abc123",
        "https://api.example.com/#token=abc123",
        "https://svc:hunter2@api.example.com",
        "provider: tenant configured",
    ] {
        let mut scoped = manifest("effectful");
        scoped.capabilities[0].scope = scope.to_owned();
        assert_eq!(
            found_codes(&manifest_findings(&scoped)),
            only(codes::EMBEDDED_SECRET),
            "{scope}"
        );
    }
    let mut host = manifest("effectful");
    host.capabilities[0].scope = "https://api.example.com/v1/*".to_owned();
    assert!(manifest_findings(&host).is_empty());
}

#[test]
fn function_admission_requires_named_placements_and_bounded_fuel() {
    let mut blank_placement = manifest("container");
    if let DeterminismProfile::HermeticContainer(evidence) = &mut blank_placement.determinism {
        evidence.qualified = true;
        evidence.golden_runs[0].placement = " ".to_owned();
    }
    assert_eq!(
        found_codes(&manifest_findings(&blank_placement)),
        BTreeSet::from([
            codes::MANIFEST_BLANK_VALUE.to_owned(),
            codes::MISSING_DETERMINISM_EVIDENCE.to_owned(),
        ]),
        "a blank placement is neither a placement nor a second one"
    );

    let wasm = admitted("wasm");
    let dispatched = invocation("wasm");
    for fuel in [None, Some(10_000_001)] {
        let mut metered = receipt("wasm");
        metered.resources.fuel = fuel;
        assert_eq!(
            found_codes(&check_function_receipt(&metered, &dispatched, &wasm)),
            only(codes::BOUND_EXCEEDED),
            "{fuel:?}"
        );
    }
}
