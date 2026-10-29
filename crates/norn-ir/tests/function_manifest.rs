#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! The Function contract records (T1-FN-1, #2776): published JSON Schemas,
//! canonical fixtures for all three records under all three profiles
//! (`fixtures/function`), pinned identities (`fixtures/function/golden.json`,
//! shared with the `norn-checker-wasm` parity run, which recomputes them on
//! `wasm32`), and the separation of command, input, action-cache, and result
//! identities. Admission rules are `norn-checker`'s (`function_admission`).

use std::{
    fs,
    path::{Path, PathBuf},
};

use norn_ir::RuntimeValue;
use norn_ir::canonical::canonical_json_of;
use norn_ir::function::{
    CacheOrigin, FunctionInvocation, FunctionManifest, FunctionOutcome, FunctionReceipt, MemberPin,
    function_invocation_schema, function_manifest_schema, function_receipt_schema,
};
use serde_json::{Value, json};

const UPDATE_ENV: &str = "NORN_IR_UPDATE_PAYLOAD_SCHEMAS";
const PROFILES: [&str; 3] = ["wasm", "container", "effectful"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> String {
    let path = repo_root().join("fixtures/function").join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn manifest(profile: &str) -> FunctionManifest {
    serde_json::from_str(&fixture(&format!("manifest.{profile}.json")))
        .unwrap_or_else(|error| panic!("manifest.{profile}: {error}"))
}

fn invocation(profile: &str) -> FunctionInvocation {
    serde_json::from_str(&fixture(&format!("invocation.{profile}.json")))
        .unwrap_or_else(|error| panic!("invocation.{profile}: {error}"))
}

fn receipt(profile: &str) -> FunctionReceipt {
    serde_json::from_str(&fixture(&format!("receipt.{profile}.json")))
        .unwrap_or_else(|error| panic!("receipt.{profile}: {error}"))
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("canonical JSON is UTF-8")
}

#[test]
fn function_manifest_schemas_are_checked_in_and_current() {
    let mut stale = Vec::new();
    for (file, schema) in [
        ("function_manifest.schema.json", function_manifest_schema()),
        (
            "function_invocation.schema.json",
            function_invocation_schema(),
        ),
        ("function_receipt.schema.json", function_receipt_schema()),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schema")
            .join(file);
        let rendered = format!(
            "{}\n",
            serde_json::to_string_pretty(&schema).expect("schema should serialize")
        );
        if std::env::var_os(UPDATE_ENV).is_some() {
            fs::write(&path, &rendered).expect("schema should write");
        }
        let checked_in: Option<Value> = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        let current: Value = serde_json::from_str(&rendered).expect("rendered schema parses");
        if checked_in.as_ref() != Some(&current) {
            stale.push(format!("---BEGIN {file}---\n{rendered}---END {file}---"));
        }
    }
    assert!(
        stale.is_empty(),
        "stale schemas; regenerate with {UPDATE_ENV}=1 cargo test -p norn-ir --test function_manifest\n{}",
        stale.join("\n")
    );
}

/// Every fixture is already in Norn1 canonical form: decoding and
/// re-canonicalizing reproduces its bytes, so the fixtures are the canonical
/// JSON of all three records under all three profiles.
#[test]
fn function_manifest_fixtures_are_canonical_json() {
    for profile in PROFILES {
        for (record, canonical) in [
            ("manifest", canonical_json_of(&manifest(profile))),
            ("invocation", canonical_json_of(&invocation(profile))),
            ("receipt", canonical_json_of(&receipt(profile))),
        ] {
            let name = format!("{record}.{profile}.json");
            assert_eq!(
                text(canonical.expect("canonical")),
                fixture(&name).trim_end(),
                "{name} is not the canonical form of its record"
            );
        }
    }
}

fn identities(profile: &str) -> Value {
    let manifest = manifest(profile);
    let invocation = invocation(profile);
    let receipt = receipt(profile);
    json!({
        "manifest": manifest.content_digest().expect("manifest").0,
        "executionProfile": manifest.execution_profile_digest().expect("profile").0,
        "invocation": invocation.content_digest().expect("invocation").0,
        "inputIdentity": invocation.input_identity().expect("inputs").0,
        "dependencyToken": invocation.dependency.digest().expect("token").0,
        "actionKey": invocation.action_key(&manifest).expect("action key").0,
        "receipt": receipt.content_digest().expect("receipt").0,
        "resultIdentity": receipt.result_identity().expect("result").0,
    })
}

#[test]
fn function_manifest_identities_match_the_golden() {
    let golden: Value = serde_json::from_str(&fixture("golden.json")).expect("golden parses");
    let computed: Value = PROFILES
        .iter()
        .map(|profile| ((*profile).to_owned(), identities(profile)))
        .collect::<serde_json::Map<_, _>>()
        .into();
    assert_eq!(
        computed,
        golden,
        "identities changed; a changed identity is a new contract version, not an edit. Computed:\n{}",
        serde_json::to_string_pretty(&computed).expect("render")
    );
}

/// The records bind each other: an invocation names its manifest's digest,
/// and a receipt names its dispatch, profile, inputs, and token.
#[test]
fn function_manifest_records_bind_each_other() {
    for profile in PROFILES {
        let manifest = manifest(profile);
        let invocation = invocation(profile);
        let receipt = receipt(profile);
        assert_eq!(invocation.manifest, manifest.content_digest().expect("m"));
        assert_eq!(receipt.dispatch, invocation.content_digest().expect("i"));
        assert_eq!(
            receipt.execution_profile,
            manifest.execution_profile_digest().expect("p")
        );
        assert_eq!(
            receipt.input_identity,
            invocation.input_identity().expect("in")
        );
        assert_eq!(
            receipt.dependency_token,
            invocation.dependency.digest().expect("t")
        );
    }
}

/// Command identity is separate from computation identity: two commands over
/// the same inputs share one action key but not an invocation digest, the
/// tenant scopes the key, and blob versions are informational.
#[test]
fn function_manifest_command_and_action_identities_are_separate() {
    let manifest = manifest("container");
    let first = invocation("container");
    let mut second = first.clone();
    second.command_id = "cmd-thumbnail-doc-9-copy".to_owned();
    second.run.id = "run-copy".to_owned();
    assert_ne!(
        first.content_digest().expect("first"),
        second.content_digest().expect("second")
    );
    assert_eq!(
        first.action_key(&manifest).expect("first"),
        second.action_key(&manifest).expect("second")
    );

    let mut other_tenant = first.clone();
    other_tenant.tenant = "tenant_globex".to_owned();
    assert_ne!(
        first.action_key(&manifest).expect("first"),
        other_tenant.action_key(&manifest).expect("other"),
        "the default cache scope is the tenant"
    );

    let mut new_version_label = first.clone();
    if let Some(blob) = new_version_label.blobs.get_mut("image") {
        blob.version = Some("v4".to_owned());
    }
    assert_eq!(
        first.input_identity().expect("first"),
        new_version_label.input_identity().expect("relabelled"),
        "inputs are identified by content digest, not version labels"
    );
    assert_ne!(
        first.content_digest().expect("first"),
        new_version_label.content_digest().expect("relabelled")
    );

    let mut new_content = first.clone();
    if let Some(blob) = new_content.blobs.get_mut("image") {
        blob.digest.0 = format!("sha256:{}", "b".repeat(64));
    }
    assert_ne!(
        first.input_identity().expect("first"),
        new_content.input_identity().expect("new content")
    );
    assert_ne!(
        first.action_key(&manifest).expect("first"),
        new_content.action_key(&manifest).expect("new content")
    );
}

/// The token covers membership, order, and absence: adding, reordering, or
/// removing a predicate member changes it.
#[test]
fn function_manifest_dependency_token_covers_membership_and_order() {
    let base = invocation("wasm").dependency;
    let digest = base.digest().expect("base");
    let mut inserted = base.clone();
    inserted.predicates[0].members.push(MemberPin {
        key: "msg-43".to_owned(),
        version: 1,
    });
    let mut reordered = inserted.clone();
    reordered.predicates[0].members.reverse();
    let mut emptied = base;
    emptied.predicates[0].members.clear();
    let digests = [
        digest,
        inserted.digest().expect("inserted"),
        reordered.digest().expect("reordered"),
        emptied.digest().expect("emptied"),
    ];
    for (index, left) in digests.iter().enumerate() {
        for right in digests.iter().skip(index.saturating_add(1)) {
            assert_ne!(left, right);
        }
    }
}

/// Metrics stay outside the result identity: another attempt with different
/// resource use, provenance, or cache origin computed the same result.
#[test]
fn function_manifest_result_identity_excludes_metrics() {
    let first = receipt("wasm");
    let mut retried = first.clone();
    retried.attempt = 2;
    retried.resources.cpu_millis = 9;
    retried.provenance.placement = "pool-eu-1/node-z".to_owned();
    retried.cache = CacheOrigin::Hit {
        action_key: first.dispatch.clone(),
    };
    assert_eq!(
        first.result_identity().expect("first"),
        retried.result_identity().expect("retried")
    );
    assert_ne!(
        first.content_digest().expect("first"),
        retried.content_digest().expect("retried")
    );

    let mut different = first.clone();
    different.outcome = FunctionOutcome::DeadlineExceeded {};
    assert_ne!(
        first.result_identity().expect("first"),
        different.result_identity().expect("different")
    );
}

/// Inputs are exact: a decimal `f64` cannot hold is kept, `1.50` and `1.5`
/// are one value, and no identity passes through `f64`.
#[test]
fn function_manifest_inputs_are_exact_decimals() {
    let with = |amount: &str| {
        let mut invocation = invocation("wasm");
        invocation.inputs.insert(
            "amount".to_owned(),
            RuntimeValue::from_json_text(amount).expect("exact decimal"),
        );
        invocation.input_identity().expect("identity")
    };
    assert_ne!(with("0.3000000000000000000000000001"), with("0.3"));
    assert_eq!(with("1.50"), with("1.5"));
    assert_ne!(with("9007199254740993"), with("9007199254740992"));

    let text = fixture("invocation.wasm.json").replace(
        "\"locale\":\"en\"",
        "\"amount\":0.3000000000000000000000000001,\"locale\":\"en\"",
    );
    let decoded: FunctionInvocation = serde_json::from_str(&text).expect("decodes exactly");
    assert_eq!(
        decoded.input_identity().expect("decoded"),
        with("0.3000000000000000000000000001")
    );
}

#[test]
fn function_manifest_unknown_members_are_refused() {
    let text = fixture("manifest.wasm.json").replacen('{', "{\"extra\":1,", 1);
    assert!(serde_json::from_str::<FunctionManifest>(&text).is_err());
    let text = fixture("receipt.wasm.json").replacen('{', "{\"timestamp\":1,", 1);
    assert!(
        serde_json::from_str::<FunctionReceipt>(&text).is_err(),
        "timestamps stay outside the receipt's deterministic shape"
    );
}
