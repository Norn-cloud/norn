//! Bounded public-cut regressions; all fixture content is authored here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "synthetic policy fixtures"
)]

use norn_skeleton_guard::ci::provenance::{self, CONTROL, MANIFEST, RECORDS};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

struct Fixture {
    dir: tempfile::TempDir,
    records: Vec<Value>,
    software: Vec<Value>,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self {
            dir: tempfile::tempdir().unwrap(),
            records: vec![],
            software: vec![],
        };
        f.add(
            "LICENSE",
            b"Independently authored Apache-2.0 fixture rights",
            "code",
        );
        f
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn write(&self, path: &str, data: &[u8]) {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
    fn reference(&self, path: &str) -> Value {
        json!({"path":path,"sha256":hex::encode(Sha256::digest(fs::read(self.root().join(path)).unwrap()))})
    }
    fn add(&mut self, path: &str, data: &[u8], category: &str) -> usize {
        self.write(path, data);
        self.records.push(json!({"path":path,"sha256":self.reference(path)["sha256"],"origin":"authored","category":category,"license":"Apache-2.0","sources":[],"analyzed_sources":[],"parents":[],"rights_evidence":[self.reference("LICENSE")],"notices":[],"connector":null}));
        self.records.len().checked_sub(1).unwrap()
    }
    fn derived(&mut self, path: &str, parent: &str, category: &str) -> usize {
        let i = self.add(
            path,
            format!("synthetic output {path}").as_bytes(),
            category,
        );
        self.records[i]["origin"] = json!("derived");
        self.records[i]["parents"] = json!([parent]);
        i
    }
    fn source(&mut self, license: &str, redistribution: &str) -> Value {
        let path = "registry/sources/toy/MANIFEST";
        let source = json!({"sources":[{"id":"toy-upstream","version":"1.2.3","terms":{"license":license,"redistribution":redistribution},"pin":null}]});
        self.add(
            path,
            &serde_json::to_vec(&source).unwrap(),
            "provenance-evidence",
        );
        json!({"id":"toy-upstream","revision":"1.2.3","evidence":self.reference(path)})
    }
    fn save(&self) {
        if self.root().join(CONTROL).exists() {
            fs::remove_dir_all(self.root().join(CONTROL)).unwrap();
        }
        self.write(
            MANIFEST,
            &serde_json::to_vec(&json!({"version":2,"software_sources":self.software})).unwrap(),
        );
        for record in &self.records {
            let mut body = record.clone();
            let path = body.as_object_mut().unwrap().remove("path").unwrap();
            self.write(
                &format!("{RECORDS}{}.json", path.as_str().unwrap()),
                &serde_json::to_vec(&body).unwrap(),
            );
        }
    }
    fn report(&self) -> provenance::Report {
        self.save();
        provenance::check_distribution(self.root())
    }
    fn passes(&self) {
        assert!(
            self.report().errors().is_empty(),
            "{:?}",
            self.report().errors()
        );
    }
    fn rejects(&self, message: &str) {
        let r = self.report();
        assert!(
            r.errors().iter().any(|e| e.contains(message)),
            "expected {message}: {:?}",
            r.errors()
        );
    }
}

#[test]
fn complete_lineage_includes_generated_release_and_embedded_copies() {
    let mut f = Fixture::new();
    f.add(
        "input.json",
        b"independently authored toy",
        "synthetic-fixture",
    );
    f.derived("generated.json", "input.json", "synthetic-golden");
    f.derived("release/copy.json", "generated.json", "generated");
    f.derived("embedded/copy.rs", "release/copy.json", "generated");
    f.passes();
    f.write("input.json", b"tampered");
    let r = f.report();
    assert_eq!(r.rejected_paths.len(), 4);
    assert!(
        r.rejected_paths["embedded/copy.rs"]
            .iter()
            .any(|r| r.contains("rejected ancestor"))
    );
    f.write("input.json", b"independently authored toy");
    let head = bind_fixture(&f);
    assert!(bound_report(&f, &head).errors().is_empty());
    f.write("input.json", b"tampered after immutable binding");
    assert!(
        bound_report(&f, &head)
            .errors()
            .iter()
            .any(|e| e.contains("bound byte drift"))
    );
}

#[test]
fn missing_untracked_and_ignored_distribution_members_reject() {
    let mut f = Fixture::new();
    f.add("tracked", b"toy", "code");
    f.write("release/undeclared", b"hidden input");
    f.rejects("missing shipped-artifact");
    fs::remove_file(f.root().join("release/undeclared")).unwrap();
    fs::remove_file(f.root().join("tracked")).unwrap();
    f.rejects("missing artifact");
}

#[test]
fn unknown_ambiguous_and_cyclic_ancestry_fails_closed() {
    for parents in [json!(["unknown"]), json!(["b", "b"]), json!(["a"])] {
        let mut f = Fixture::new();
        let a = f.derived("a", "LICENSE", "generated");
        f.add("b", b"different input", "code");
        f.records[a]["parents"] = parents;
        assert!(!f.report().errors().is_empty());
    }
    let mut f = Fixture::new();
    f.derived("a", "b", "generated");
    f.derived("b", "a", "generated");
    f.rejects("cyclic ancestry");
}

/// #121: each refusal the single manifest gave survives per-file records.
#[test]
fn per_file_records_refuse_wrong_hash_missing_and_unlisted() {
    let mut f = Fixture::new();
    let i = f.add("crates/a/src/lib.rs", b"// a", "code");
    f.passes();
    f.records[i]["sha256"] = json!("0".repeat(64));
    f.rejects("content hash drift"); // explicit legacy distribution still refuses
    let head = bind_fixture(&f);
    assert!(bound_report(&f, &head).errors().is_empty());
    f.write("crates/a/src/lib.rs", b"// post-commit drift");
    assert!(
        bound_report(&f, &head)
            .errors()
            .iter()
            .any(|e| e.contains("bound byte drift"))
    );
    let mut f = Fixture::new();
    f.add("crates/a/src/lib.rs", b"// a", "code");
    f.save();
    fs::remove_file(f.root().join(format!("{RECORDS}crates/a/src/lib.rs.json"))).unwrap();
    let r = provenance::check_distribution(f.root());
    assert!(r.rejected_paths["crates/a/src/lib.rs"][0].contains("missing shipped-artifact"));
    let mut f = Fixture::new();
    f.add("crates/a/src/lib.rs", b"// a", "code");
    f.save();
    fs::remove_file(f.root().join("crates/a/src/lib.rs")).unwrap();
    let r = provenance::check_distribution(f.root());
    assert!(
        r.rejected_paths["crates/a/src/lib.rs"]
            .iter()
            .any(|e| e.contains("missing artifact"))
    );
    let f = Fixture::new();
    f.save();
    for stray in [
        format!("{CONTROL}extra.json"),
        format!("{RECORDS}LICENSE.yaml"),
    ] {
        f.write(&stray, b"{}");
        let r = provenance::check_distribution(f.root());
        assert!(
            r.rejected_paths[&stray]
                .iter()
                .any(|e| e.contains("unknown provenance control"))
        );
        fs::remove_file(f.root().join(&stray)).unwrap();
    }
    // The path is the record's file name, never a field it can claim.
    f.write(
        &format!("{RECORDS}LICENSE.json"),
        &serde_json::to_vec(&f.records[0]).unwrap(),
    );
    let r = provenance::check_distribution(f.root());
    assert!(
        r.rejected_paths[&format!("{RECORDS}LICENSE.json")][0]
            .contains("malformed provenance record")
    );
}

#[test]
fn unknown_conditional_and_prohibited_rights_never_clear_payload() {
    for (license, redistribution) in [
        ("GPL-3.0", "license-conditions"),
        ("MIT", "license-conditions"),
        ("MIT", "permitted"),
        ("Apache-2.0", "unverified"),
        ("Apache-2.0", "prohibited"),
        ("unverified", "permitted"),
    ] {
        let mut f = Fixture::new();
        let source = f.source(license, redistribution);
        f.passes(); // Factual rejected-source records do not inherit payload rights.
        let i = f.add(
            "payload",
            b"a licence label is not an override",
            "standards-data",
        );
        f.records[i]["origin"] = json!("derived");
        f.records[i]["sources"] = json!([source]);
        f.rejects("source toy-upstream rejected");
        f.derived("release/payload", "payload", "generated");
        assert!(f.report().rejected_paths.contains_key("release/payload"));
    }
}

#[test]
fn source_ids_revisions_and_evidence_are_exact() {
    for field in ["id", "revision", "evidence"] {
        let mut f = Fixture::new();
        let mut source = f.source("Apache-2.0", "permitted");
        source[field] = if field == "evidence" {
            json!({"path":"LICENSE","sha256":"0".repeat(64)})
        } else {
            json!("nonexistent")
        };
        let i = f.add("analysis.dl", b"authored query", "authored-query");
        f.records[i]["analyzed_sources"] = json!([source]);
        assert!(!f.report().errors().is_empty());
    }
}

#[test]
fn authored_queries_proofs_verdicts_may_analyze_rejected_sources() {
    let mut f = Fixture::new();
    let source = f.source("GPL-3.0", "license-conditions");
    for (path, category) in [
        ("query.dl", "authored-query"),
        ("law.rs", "authored-law"),
        ("verdict.json", "verdict"),
    ] {
        let i = f.add(
            path,
            format!("Norn authored analysis {path}").as_bytes(),
            category,
        );
        f.records[i]["analyzed_sources"] = json!([source.clone()]);
    }
    f.passes();
    assert_eq!(f.report().rejected_sources.len(), 1);
    for category in [
        "vendor-schema",
        "vendor-data",
        "extraction-database",
        "accounting-domain",
        "inventory-domain",
        "certified-domain-law",
        "private-business",
    ] {
        let mut bad = Fixture::new();
        bad.add("renamed.json", b"never admitted", category);
        bad.rejects("forbidden payload");
    }
}

#[test]
fn known_source_hash_cannot_be_laundered_by_renaming_and_authored_label() {
    let mut f = Fixture::new();
    let i = f.add("renamed.json", b"toy upstream data", "code");
    let lock = json!([{"sourceId":"source.toy","releaseId":"1.0.0","license":"GPL-3.0","redistribution":"vendored","files":[{"path":"original.json","sha256":f.records[i]["sha256"]}]}]);
    f.add(
        "registry/standards/sources.lock.json",
        &serde_json::to_vec(&lock).unwrap(),
        "provenance-evidence",
    );
    f.rejects("requires derivation from source.toy");
}

#[test]
fn upstream_bytes_must_match_pinned_catalog_hash_not_only_candidate_record() {
    let mut f = Fixture::new();
    let i = f.add("payload", b"toy pinned bytes", "standards-data");
    let lock = json!([{"sourceId":"source.toy","releaseId":"1.0.0","license":"Apache-2.0","redistribution":"vendored","files":[{"path":"payload","sha256":f.records[i]["sha256"]}]}]);
    let path = "registry/standards/sources.lock.json";
    f.add(
        path,
        &serde_json::to_vec(&lock).unwrap(),
        "provenance-evidence",
    );
    f.records[i]["origin"] = json!("upstream");
    f.records[i]["sources"] =
        json!([{"id":"source.toy","revision":"1.0.0","evidence":f.reference(path)}]);
    f.passes();
    f.write("payload", b"changed bytes and updated self assertion");
    f.records[i]["sha256"] = f.reference("payload")["sha256"].clone();
    f.rejects("differs from pinned source");
}

#[test]
fn synthetic_goldens_need_synthetic_data_ancestry() {
    let mut f = Fixture::new();
    let i = f.add("toy.json", b"independent toy input", "synthetic-fixture");
    f.derived("golden.json", "toy.json", "synthetic-golden");
    f.passes();
    f.records[i]["category"] = json!("extraction-database");
    f.rejects("nonsynthetic data ancestry");
    assert!(f.report().rejected_paths.contains_key("golden.json"));
}

#[test]
fn opaque_databases_and_archives_cannot_hide_as_authored_code() {
    for bytes in [
        b"SQLite format 3\0more".as_slice(),
        b"PK\x03\x04zip",
        b"\x1f\x8bgzip",
        b"PAR1parquet",
    ] {
        let mut f = Fixture::new();
        f.add("renamed.txt", bytes, "code");
        f.rejects("opaque container");
    }
}

#[test]
fn renamed_rejected_derivation_taints_identical_copies_and_descendants() {
    let mut f = Fixture::new();
    f.add("bad.json", b"toy data", "extraction-database");
    f.add("renamed.txt", b"toy data", "code");
    f.derived("release/copy", "renamed.txt", "generated");
    let r = f.report();
    assert!(r.rejected_paths.contains_key("renamed.txt"));
    assert!(r.rejected_paths.contains_key("release/copy"));
}

#[test]
fn structured_payload_cannot_use_authored_code_to_hide_extraction_or_schema() {
    for payload in [
        br#"{"format":"norn.behaviour-source/1","files":[]}"#.as_slice(),
        br#"{"doctype":"DocType","fields":[]}"#,
        br#"{"kind":"source_schema","payload":{"format":"norn.source-schema/1"}}"#,
    ] {
        let mut f = Fixture::new();
        f.add("renamed.json", payload, "code");
        assert!(!f.report().errors().is_empty());
    }
}

#[test]
fn embedded_import_package_members_and_derived_from_must_close() {
    for payload in [
        json!({"provenance":{"derivedFrom":["input"]}}),
        json!({"provenance":{"source":"import","sourcePath":"input"}}),
    ] {
        let mut f = Fixture::new();
        f.add("input", b"toy input", "code");
        let i = f.add("output", &serde_json::to_vec(&payload).unwrap(), "code");
        f.rejects("missing from lineage");
        f.records[i]["origin"] = json!("derived");
        f.records[i]["parents"] = json!(["input"]);
        f.passes();
    }
    let mut f = Fixture::new();
    f.add("release/item", b"toy member", "code");
    let reference = f.reference("release/item");
    let payload = json!({"format":"norn.registry.package/1","artifacts":[{"path":"item","sha256":reference["sha256"]}]});
    let i = f.add(
        "release/package.json",
        &serde_json::to_vec(&payload).unwrap(),
        "code",
    );
    f.rejects("distribution member missing");
    f.records[i]["origin"] = json!("derived");
    f.records[i]["parents"] = json!(["release/item"]);
    f.passes();
    let mut payload = payload;
    payload["artifacts"][0]["sha256"] = json!("0".repeat(64));
    f.write(
        "release/package.json",
        &serde_json::to_vec(&payload).unwrap(),
    );
    f.records[i]["sha256"] = f.reference("release/package.json")["sha256"].clone();
    f.rejects("member hash drift");
}

#[test]
fn derived_catalogs_do_not_turn_restrictive_lineage_into_authored_data() {
    let mut f = Fixture::new();
    let i = f.add("output", b"toy derived output", "code");
    let catalog = json!({"sourceId":"source.toy-derived","releaseId":"1.0.0","license":"Apache-2.0 AND GPL-3.0","redistribution":"vendored","files":[{"path":"output","sha256":f.records[i]["sha256"]}]});
    f.add(
        "registry/standards/derived.source.json",
        &serde_json::to_vec(&catalog).unwrap(),
        "provenance-evidence",
    );
    f.rejects("requires derivation from source.toy-derived");
    assert!(
        f.report()
            .rejected_sources
            .contains_key("source.toy-derived")
    );
}

#[test]
fn mit_software_requires_source_notice_and_hash() {
    for id in ["software.zen", "software.ascent", "software.act"] {
        let mut f = Fixture::new();
        f.add("pin.json", b"Norn factual toy software pin", "code");
        f.add("MIT-LICENSE",b"Copyright Toy. Permission is hereby granted, free of charge, to any person obtaining a copy of this software","code");
        let evidence = f.reference("pin.json");
        f.software.push(json!({"id":id,"revision":"0123456789012345678901234567890123456789","license":"MIT","evidence":evidence}));
        let i = f.add("vendor/toy.rs", b"// toy software stand-in", "software");
        f.records[i]["origin"] = json!("upstream");
        f.records[i]["license"] = json!("MIT");
        f.records[i]["sources"] = json!([{"id":id,"revision":"0123456789012345678901234567890123456789","evidence":evidence}]);
        f.records[i]["notices"] = json!([f.reference("MIT-LICENSE")]);
        f.passes();
        f.records[i]["category"] = json!("standards-data");
        f.rejects("software rights cannot clear schema/data");
        f.records[i]["category"] = json!("software");
        f.records[i]["notices"] = json!([]);
        f.rejects("notice");
        f.records[i]["notices"] = json!([f.reference("MIT-LICENSE")]);
        f.write("MIT-LICENSE", b"changed");
        f.rejects("tampered evidence or notice");
    }
}

#[test]
fn mapping_requires_versions_sparse_fields_terms_and_install_validation() {
    let mut f = Fixture::new();
    let source = f.source("GPL-3.0", "license-conditions");
    f.add(
        "terms.txt",
        b"Factual API access terms, not redistribution",
        "code",
    );
    f.add("install.rs", b"// toy validator evidence", "code");
    let i = f.add("mapping.json", b"Toy.field -> Norn.field", "mapping");
    f.records[i]["analyzed_sources"] = json!([source]);
    f.records[i]["connector"] = json!({"versions":[{"source_id":"toy-upstream","revisions":["1.2.3"]}],"api_terms":[f.reference("terms.txt")],"mapped_fields":["Toy.field"],"install_validation":f.reference("install.rs")});
    f.passes();
    f.records[i]["connector"]["versions"][0]["revisions"] = json!(["^1.2"]);
    f.rejects("unpinned");
    f.records[i]["connector"]["versions"][0]["revisions"] = json!(["1.2.3"]);
    f.records[i]["connector"]["mapped_fields"] = json!(["Toy.*"]);
    f.rejects("sparse");
    f.records[i]["connector"]["mapped_fields"] = json!(["Toy.field"]);
    f.records[i]["connector"]["api_terms"] = json!([]);
    f.rejects("API terms");
}

#[test]
fn strict_records_reject_unknown_fields_categories_origins_and_directory_claims() {
    for (field, value) in [
        ("category", json!("unknown")),
        ("origin", json!("unknown")),
        ("bypass", json!(true)),
    ] {
        let mut f = Fixture::new();
        f.records[0][field] = value;
        assert!(!f.report().errors().is_empty());
    }
    let mut f = Fixture::new();
    f.records[0]["path"] = json!(".");
    f.rejects("non-exact");
    f.records[0]["path"] = json!(MANIFEST);
    f.rejects("self hash");
}

#[cfg(unix)]
#[test]
fn symlink_to_outside_candidate_is_never_read() {
    let f = Fixture::new();
    std::os::unix::fs::symlink("/etc/passwd", f.root().join("outside")).unwrap();
    f.rejects("symlink refused");
}

const COVERAGE_INDEX: &str = "verification/coverage-v1/manifest/index.json";
const COVERAGE_LEDGER: &str = "verification/coverage-v1/manifest/subsystems/quantity-ledger/obligations/2b6e3370813437d16485a55299d0d399cbbc4a57dc45f9202e8652184984af26.json";

fn policy_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.add(".github/workflows/core.yml", b"on: pull_request\njobs:\n  core:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo synthetic\n", "code");
    for path in [
        "scripts/ci/core.sh",
        ".config/nextest.toml",
        "scripts/ci/validate-runner-policy.py",
        "scripts/proof_enrollment.py",
        "verification/coverage-v1/validate.py",
        "verification/coverage-v1/negative_canaries.py",
        "scripts/coverage_manifest.py",
        "scripts/verify-promoted-subsystems.sh",
        "scripts/check-proof-holes.py",
    ] {
        fixture.add(
            path,
            b"independently authored synthetic gate contract",
            "code",
        );
    }
    fixture.add(COVERAGE_INDEX, b"{}", "code");
    fixture.add(
        "verification/coverage-v1/manifest/subsystems/quantity-ledger/index.json",
        br#"{"obligations":{"$entries":"obligations"}}"#,
        "code",
    );
    fixture.add(
        COVERAGE_LEDGER,
        br#"{"order":0,"value":"synthetic-preserved-obligation"}"#,
        "code",
    );
    fixture.add("crates/norn-skeleton-guard/baseline.json", b"{}", "code");
    fixture.add(
        "crates/norn-ir/src/lib.rs",
        b"pub enum ArtifactKind { Entity, Law }",
        "code",
    );
    fixture.add(
        "verification/stripmine-2026-08-22/proofs/fstar/ArtifactPromotionProof.fst",
        b"let authority_bearing kind =\n  match kind with\n  | ArtifactKind_Entity\n  | ArtifactKind_Law -> true\nlet next = ()\n",
        "generic-proof",
    );
    fixture.add(
        "crates/norn-checker/tests/non_code_literals.baseline.json",
        b"[]",
        "code",
    );
    fixture.save();
    index_fixture(&fixture);
    fixture
}

#[expect(
    clippy::disallowed_methods,
    reason = "index only independently authored disposable policy test data"
)]
fn index_fixture(fixture: &Fixture) {
    for args in [["init", "--quiet"], ["add", "."]] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(fixture.root())
                .args(args)
                .status()
                .expect("git fixture index")
                .success()
        );
    }
}

#[test]
fn trusted_policy_rejects_changed_workflows_and_gate_contracts_even_with_updated_hashes() {
    let base = policy_fixture();
    assert!(norn_skeleton_guard::ci::run(base.root(), base.root()).is_empty());
    for path in [
        ".github/workflows/core.yml",
        "scripts/ci/core.sh",
        "verification/coverage-v1/negative_canaries.py",
    ] {
        let mut candidate = policy_fixture();
        let record = candidate
            .records
            .iter()
            .position(|record| record["path"] == path)
            .expect("contract record");
        candidate.write(path, b"on: pull_request\njobs:\n  core:\n    runs-on: self-hosted\n    steps:\n      - run: echo changed\n");
        candidate.records[record]["sha256"] = candidate.reference(path)["sha256"].clone();
        candidate.save();
        let errors = norn_skeleton_guard::ci::run(candidate.root(), base.root());
        assert!(
            errors
                .iter()
                .any(|error| error.contains("contract differs from the trusted base")),
            "{errors:?}"
        );
    }
    let mut candidate = policy_fixture();
    candidate.add(
        ".github/workflows/extra.yml",
        b"jobs:\n  core:\n    runs-on: self-hosted\n",
        "code",
    );
    candidate.save();
    index_fixture(&candidate);
    let errors = norn_skeleton_guard::ci::run(candidate.root(), base.root());
    assert!(
        errors
            .iter()
            .any(|error| error.contains("workflow inventory differs")),
        "{errors:?}"
    );
}

#[test]
fn trusted_policy_preserves_retained_core_obligations() {
    let base = policy_fixture();
    let mut candidate = policy_fixture();
    let path = COVERAGE_LEDGER;
    drop_file(&mut candidate, path);
    let errors = norn_skeleton_guard::ci::run(candidate.root(), base.root());
    assert!(
        errors
            .iter()
            .any(|error| error.contains("retained core obligations were removed")),
        "{errors:?}"
    );
}

#[test]
fn trusted_policy_reads_entry_obligations_and_refuses_malformed_or_removed_entries() {
    let base = policy_fixture();
    for change in ["none", "missing", "empty", "identity", "order", "extra"] {
        let mut candidate = policy_fixture();
        drop_file(&mut candidate, COVERAGE_LEDGER);
        let directory = "verification/coverage-v1/manifest/subsystems/quantity-ledger";
        let id = "synthetic-preserved-obligation";
        let hash = "2b6e3370813437d16485a55299d0d399cbbc4a57dc45f9202e8652184984af26";
        let name = if change == "identity" { "wrong" } else { hash };
        let mut value = json!({"order": 0, "value": id});
        if change == "order" {
            value["order"] = json!(true);
        }
        if change == "extra" {
            value["extra"] = json!(true);
        }
        let path = format!("{directory}/obligations/{name}.json");
        candidate.add(&path, &serde_json::to_vec(&value).unwrap(), "code");
        if matches!(change, "missing" | "empty") {
            drop_file(&mut candidate, &path);
            if change == "empty" {
                fs::remove_dir(candidate.root().join(format!("{directory}/obligations"))).unwrap();
            }
        }
        candidate.save();
        index_fixture(&candidate);
        let baseline = if change == "empty" {
            candidate.root()
        } else {
            base.root()
        };
        let errors = norn_skeleton_guard::ci::run(candidate.root(), baseline);
        if matches!(change, "none" | "empty") {
            assert!(errors.is_empty(), "{errors:?}");
        } else {
            assert!(!errors.is_empty(), "accepted {change}");
        }
    }
}

/// Remove `path` and its record from a fixture, as a pull request would.
fn drop_file(fixture: &mut Fixture, path: &str) {
    fixture.records.retain(|record| record["path"] != path);
    fs::remove_file(fixture.root().join(path)).unwrap();
    fixture.save();
    index_fixture(fixture);
}

#[test]
fn trusted_policy_refuses_a_dropped_subsystem_a_missing_index_and_stray_entries() {
    let base = policy_fixture();
    let refused = |candidate: &Fixture, reason: &str| {
        let errors = norn_skeleton_guard::ci::run(candidate.root(), base.root());
        assert!(
            errors.iter().any(|error| error.contains(reason)),
            "{reason}: {errors:?}"
        );
    };
    let mut dropped = policy_fixture();
    drop_file(&mut dropped, COVERAGE_LEDGER);
    drop_file(
        &mut dropped,
        "verification/coverage-v1/manifest/subsystems/quantity-ledger/index.json",
    );
    fs::remove_dir_all(
        dropped
            .root()
            .join("verification/coverage-v1/manifest/subsystems/quantity-ledger"),
    )
    .unwrap();
    refused(
        &dropped,
        "quantity-ledger: retained core obligations were removed",
    );
    let mut headless = policy_fixture();
    drop_file(&mut headless, COVERAGE_INDEX);
    refused(&headless, "manifest/index.json is missing");
    let mut stray = policy_fixture();
    stray.add(
        "verification/coverage-v1/manifest/subsystems/notes.txt",
        b"synthetic",
        "code",
    );
    stray.save();
    index_fixture(&stray);
    refused(&stray, "unknown entry notes.txt");
}

#[test]
fn trusted_policy_rejects_artifact_promotion_drift() {
    let mut fixture = policy_fixture();
    assert!(norn_skeleton_guard::ci::run(fixture.root(), fixture.root()).is_empty());
    let path = "verification/stripmine-2026-08-22/proofs/fstar/ArtifactPromotionProof.fst";
    let record = fixture
        .records
        .iter()
        .position(|record| record["path"] == path)
        .expect("proof record");
    fixture.write(
        path,
        b"let authority_bearing kind =\n  match kind with\n  | ArtifactKind_Entity -> true\nlet next = ()\n",
    );
    fixture.records[record]["sha256"] = fixture.reference(path)["sha256"].clone();
    fixture.save();
    let errors = norn_skeleton_guard::ci::run(fixture.root(), fixture.root());
    assert!(
        errors
            .iter()
            .any(|error| error.contains("missing=[\"Law\"]")),
        "{errors:?}"
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "commit only independently authored disposable policy test data"
)]
fn commit_fixture(fixture: &Fixture) -> String {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(fixture.root())
            .args(args)
            .output()
            .expect("git fixture commit")
    };
    assert!(git(&["add", "."]).status.success());
    let identity = [
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@example.invalid",
    ];
    let commit = [
        &identity[..],
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    ]
    .concat();
    assert!(git(&commit).status.success());
    String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned()
}

/// A same-shape candidate whose only finding is a changed gate file: its older
/// commit is the unchanged tree and its head commit changes the gate.
fn gate_change_candidate() -> (Fixture, String, String) {
    let mut candidate = policy_fixture();
    let older = commit_fixture(&candidate);
    let path = "scripts/ci/core.sh";
    let record = candidate
        .records
        .iter()
        .position(|record| record["path"] == path)
        .expect("gate record");
    candidate.write(path, b"#!/usr/bin/env bash\necho reviewed gate change\n");
    candidate.records[record]["sha256"] = candidate.reference(path)["sha256"].clone();
    candidate.save();
    let head = commit_fixture(&candidate);
    (candidate, older, head)
}

/// The base-owned trusted lane over `candidate`, with API evidence as recorded by
/// the workflow: an optional `/approve-gates <commit>` comment by a user with `permission`.
fn checked_with(
    candidate: &Fixture,
    head_repository: &str,
    head: &str,
    approved: Option<(&str, &str)>,
) -> Vec<String> {
    let comments: Vec<Value> = approved
        .map(|(commit, permission)| json!({"user":"owner","body":format!("/approve-gates {commit}"),"created_at":"2026-10-30T12:00:00Z","updated_at":"2026-10-30T12:00:00Z","permission":permission}))
        .into_iter()
        .collect();
    let evidence = json!({"repository":"Norn-cloud/norn","head_repository":head_repository,"head_sha":head,"comments":comments});
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("maintainer-approval.json");
    fs::write(&path, evidence.to_string()).unwrap();
    let base = policy_fixture();
    norn_skeleton_guard::ci::run_with_evidence(candidate.root(), base.root(), Some(&path))
}

fn rejected(errors: &[String], reason: &str) {
    assert!(errors.iter().any(|error| error == "scripts/ci/core.sh: gate contract differs from the trusted base"), "{errors:?}");
    assert!(
        errors
            .iter()
            .any(|error| error.starts_with("maintainer path: ") && error.contains(reason)),
        "{errors:?}"
    );
}

#[test]
fn trusted_policy_admits_a_gate_change_an_admin_approved_on_its_exact_head() {
    let (candidate, _, head) = gate_change_candidate();
    let errors = checked_with(&candidate, "Norn-cloud/norn", &head, Some((&head, "admin")));
    assert!(errors.is_empty(), "{errors:?}");
    // Without evidence the same candidate is rejected exactly as before.
    let base = policy_fixture();
    let errors = norn_skeleton_guard::ci::run(candidate.root(), base.root());
    assert_eq!(
        errors,
        ["scripts/ci/core.sh: gate contract differs from the trusted base"]
    );
}

#[test]
fn trusted_policy_rejects_an_approval_of_a_stale_head() {
    let (candidate, older, head) = gate_change_candidate();
    rejected(
        &checked_with(
            &candidate,
            "Norn-cloud/norn",
            &head,
            Some((&older, "admin")),
        ),
        "no admin commented",
    );
    // Evidence recorded for the older head cannot vouch for the candidate checkout.
    rejected(
        &checked_with(
            &candidate,
            "Norn-cloud/norn",
            &older,
            Some((&older, "admin")),
        ),
        "the candidate is",
    );
}

#[test]
fn trusted_policy_rejects_an_approval_from_a_non_admin() {
    let (candidate, _, head) = gate_change_candidate();
    rejected(
        &checked_with(&candidate, "Norn-cloud/norn", &head, Some((&head, "write"))),
        "no admin commented",
    );
}

#[test]
fn trusted_policy_rejects_a_fork_gate_change_even_with_an_exact_head_approval() {
    let (candidate, _, head) = gate_change_candidate();
    rejected(
        &checked_with(
            &candidate,
            "contributor/norn",
            &head,
            Some((&head, "admin")),
        ),
        "only Norn-cloud/norn branches",
    );
}

#[test]
fn trusted_policy_rejects_a_gate_change_without_an_approval_comment() {
    let (candidate, _, head) = gate_change_candidate();
    rejected(
        &checked_with(&candidate, "Norn-cloud/norn", &head, None),
        "no admin commented",
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "build merge-queue history over independently authored disposable policy test data"
)]
fn git_in(fixture: &Fixture, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(fixture.root())
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git fixture");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Rewrite `path` with its manifest record, as a reviewed pull request would.
fn change(candidate: &mut Fixture, path: &str, data: &[u8]) {
    let record = candidate
        .records
        .iter()
        .position(|record| record["path"] == path)
        .expect("record");
    candidate.write(path, data);
    candidate.records[record]["sha256"] = candidate.reference(path)["sha256"].clone();
    candidate.save();
}

/// Queue-style history: base, then one `--no-ff` merge per pull request head,
/// where each head branches from the base and applies `edit`. Returns the
/// base, the pull request heads and the group head.
fn merge_group(edits: &[&dyn Fn(&mut Fixture)]) -> (Fixture, String, Vec<String>, String) {
    let mut candidate = policy_fixture();
    let base = commit_fixture(&candidate);
    git_in(&candidate, &["branch", "-M", "queue"]);
    let mut heads = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let branch = format!("pr-{index}");
        git_in(&candidate, &["checkout", "--quiet", "-b", &branch, &base]);
        edit(&mut candidate);
        git_in(&candidate, &["add", "."]);
        git_in(
            &candidate,
            &["commit", "--quiet", "--allow-empty", "-m", &branch],
        );
        heads.push(git_in(&candidate, &["rev-parse", "HEAD"]));
        git_in(&candidate, &["checkout", "--quiet", "queue"]);
        git_in(
            &candidate,
            &["merge", "--quiet", "--no-ff", "--no-edit", &branch],
        );
    }
    let head = git_in(&candidate, &["rev-parse", "HEAD"]);
    (candidate, base, heads, head)
}

fn gate_edit(candidate: &mut Fixture) {
    change(
        candidate,
        "scripts/ci/core.sh",
        b"#!/usr/bin/env bash\necho reviewed gate change\n",
    );
}

/// A pull request that changes no gate file (here, no file at all: every byte
/// is in the single-line manifest, so two edits of it would conflict).
fn data_edit(_: &mut Fixture) {}

/// A queued pull request's head, head repository and optional
/// `/approve-gates` (commit, commenter permission).
type Pull<'a> = (&'a str, &'a str, Option<(&'a str, &'a str)>);

/// The base-owned lane over a merge group with the workflow-recorded evidence:
/// each pull request head with an optional `/approve-gates` comment.
fn group_checked(candidate: &Fixture, base: &str, head: &str, pulls: &[Pull<'_>]) -> Vec<String> {
    let pull_requests: Vec<Value> = pulls
        .iter()
        .map(|&(sha, repository, approved)| {
            let comments: Vec<Value> = approved
                .map(|(commit, permission)| json!({"user":"owner","body":format!("/approve-gates {commit}"),"created_at":"2026-10-30T12:00:00Z","updated_at":"2026-10-30T12:00:00Z","permission":permission}))
                .into_iter()
                .collect();
            json!({"number":1,"head_repository":repository,"head_sha":sha,"comments":comments})
        })
        .collect();
    let evidence = json!({"repository":"Norn-cloud/norn","merge_group":{"base_sha":base,"head_sha":head},"pull_requests":pull_requests});
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("maintainer-approval.json");
    fs::write(&path, evidence.to_string()).unwrap();
    let trusted = policy_fixture();
    norn_skeleton_guard::ci::run_with_evidence(candidate.root(), trusted.root(), Some(&path))
}

#[test]
fn merge_group_admits_an_approved_gate_change_behind_data_only_pulls() {
    let (candidate, base, heads, head) = merge_group(&[&data_edit, &gate_edit]);
    let pulls = [
        (heads[0].as_str(), "contributor/norn", None),
        (
            heads[1].as_str(),
            "Norn-cloud/norn",
            Some((heads[1].as_str(), "admin")),
        ),
    ];
    let errors = group_checked(&candidate, &base, &head, &pulls);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn merge_group_without_gate_changes_needs_no_evidence() {
    let (candidate, base, _, head) = merge_group(&[&data_edit]);
    assert!(group_checked(&candidate, &base, &head, &[]).is_empty());
}

#[test]
fn merge_group_rejects_unapproved_fork_and_stale_gate_changes() {
    let (candidate, base, heads, head) = merge_group(&[&gate_edit]);
    let pr = heads[0].as_str();
    for (pulls, reason) in [
        (vec![(pr, "Norn-cloud/norn", None)], "no admin commented"),
        (
            vec![(pr, "Norn-cloud/norn", Some((pr, "write")))],
            "no admin commented",
        ),
        (
            vec![(pr, "contributor/norn", Some((pr, "admin")))],
            "only Norn-cloud/norn branches",
        ),
        (
            vec![(pr, "Norn-cloud/norn", Some((base.as_str(), "admin")))],
            "no admin commented",
        ),
        (vec![], "no pull request evidence"),
    ] {
        rejected(&group_checked(&candidate, &base, &head, &pulls), reason);
    }
    // Evidence for another group head cannot vouch for the candidate.
    let approved = [(pr, "Norn-cloud/norn", Some((pr, "admin")))];
    rejected(
        &group_checked(&candidate, &base, pr, &approved),
        "the candidate is",
    );
}

#[test]
fn merge_group_rejects_gates_that_differ_from_the_approved_head() {
    let (mut candidate, base, heads, _) = merge_group(&[&gate_edit]);
    // An extra gate edit on top of the reviewed merge is not what the admin approved.
    change(
        &mut candidate,
        "scripts/ci/core.sh",
        b"#!/usr/bin/env bash\necho unreviewed\n",
    );
    git_in(&candidate, &["commit", "--quiet", "-am", "unreviewed"]);
    let amended = git_in(&candidate, &["rev-parse", "HEAD"]);
    let pr = heads[0].as_str();
    let pulls = [(pr, "Norn-cloud/norn", Some((pr, "admin")))];
    rejected(
        &group_checked(&candidate, &base, &amended, &pulls),
        "is not a two-parent merge",
    );
    let merged = git_in(&candidate, &["rev-parse", "HEAD~1"]);
    git_in(&candidate, &["reset", "--quiet", "--soft", &merged]);
    git_in(&candidate, &["commit", "--quiet", "--amend", "--no-edit"]);
    let tampered = git_in(&candidate, &["rev-parse", "HEAD"]);
    rejected(
        &group_checked(&candidate, &base, &tampered, &pulls),
        "differ from pull request head",
    );
}

#[test]
fn merge_group_rejects_squashed_history() {
    let mut candidate = policy_fixture();
    let base = commit_fixture(&candidate);
    gate_edit(&mut candidate);
    let head = commit_fixture(&candidate);
    let pulls = [(
        head.as_str(),
        "Norn-cloud/norn",
        Some((head.as_str(), "admin")),
    )];
    rejected(
        &group_checked(&candidate, &base, &head, &pulls),
        "is not a two-parent merge",
    );
}

#[test]
fn merge_group_admits_exactly_the_longest_walk_and_rejects_a_longer_one() {
    for (merges, admitted) in [(64, true), (65, false)] {
        let mut edits: Vec<&dyn Fn(&mut Fixture)> = vec![&data_edit; merges - 1];
        edits.push(&gate_edit);
        let (candidate, base, heads, head) = merge_group(&edits);
        let pr = heads[merges - 1].as_str();
        let errors = group_checked(
            &candidate,
            &base,
            &head,
            &[(pr, "Norn-cloud/norn", Some((pr, "admin")))],
        );
        if admitted {
            assert!(errors.is_empty(), "{errors:?}");
        } else {
            rejected(&errors, "longer than 64 merges");
        }
    }
}

#[test]
fn merge_group_admits_a_shared_head_when_any_of_its_pull_requests_is_approved() {
    let (candidate, base, heads, head) = merge_group(&[&gate_edit]);
    let pr = heads[0].as_str();
    let pulls = [
        (pr, "contributor/norn", None),
        (pr, "Norn-cloud/norn", Some((pr, "admin"))),
    ];
    let errors = group_checked(&candidate, &base, &head, &pulls);
    assert!(errors.is_empty(), "{errors:?}");
    rejected(
        &group_checked(&candidate, &base, &head, &pulls[..1]),
        "only Norn-cloud/norn branches",
    );
}

fn bind_fixture(f: &Fixture) -> String {
    f.save();
    index_fixture(f);
    commit_fixture(f)
}

fn bound_report(f: &Fixture, head: &str) -> provenance::Report {
    provenance::check_bound(
        f.root(),
        &provenance::BoundSource {
            source: f.root(),
            head,
            base: f.root(),
            repository: "Norn-cloud/norn",
        },
        false,
    )
}

fn rights_document(head: &str, base: &str) -> Value {
    json!({"repository":"Norn-cloud/norn", "head_repository":"Norn-cloud/norn",
        "head_sha":head, "base_sha":base, "comments":[{"id":1,"user":"owner","permission":"admin",
        "body":format!("/ack-rights {head}"), "created_at":"2026-11-03T00:00:00Z", "updated_at":"2026-11-03T00:00:00Z"}]})
}

#[test]
fn bound_authored_edits_keep_standing_metadata_and_require_live_exact_head_rights() {
    let f = policy_fixture();
    let base = commit_fixture(&f);
    f.write(
        "crates/norn-ir/src/lib.rs",
        b"pub enum ArtifactKind { Entity, Law }\n// ordinary authored edit\n",
    );
    let head = commit_fixture(&f); // no record refresh
    assert!(bound_report(&f, &head).errors().is_empty());
    let document = rights_document(&head, &base);
    assert!(norn_skeleton_guard::ci::rights_acknowledgment(&document, &head).is_ok());
    for mutation in [
        "missing",
        "repository",
        "stale",
        "edited",
        "nonadmin",
        "revoked",
        "duplicate",
    ] {
        let mut doc = document.clone();
        match mutation {
            "missing" => doc["comments"] = json!([]),
            "repository" => doc["repository"] = json!("other/repo"),
            "stale" => doc["head_sha"] = json!(base),
            "edited" => doc["comments"][0]["updated_at"] = json!("2026-11-03T01:00:00Z"),
            "nonadmin" => doc["comments"][0]["permission"] = json!("write"),
            "revoked" => {
                let mut revoke = doc["comments"][0].clone();
                revoke["id"] = json!(2);
                revoke["body"] = json!(format!("/revoke-rights {head}"));
                doc["comments"].as_array_mut().unwrap().push(revoke);
            }
            "duplicate" => {
                let duplicate = doc["comments"][0].clone();
                doc["comments"].as_array_mut().unwrap().push(duplicate);
            }
            _ => unreachable!(),
        }
        assert!(
            norn_skeleton_guard::ci::rights_acknowledgment(&doc, &head).is_err(),
            "{mutation}"
        );
    }
    // A gate approval cannot serve as a rights acknowledgment.
    let mut gate_only = document;
    gate_only["comments"][0]["body"] = json!(format!("/approve-gates {head}"));
    assert!(norn_skeleton_guard::ci::rights_acknowledgment(&gate_only, &head).is_err());
}

#[test]
fn immutable_binding_refuses_post_commit_payload_record_and_staged_inventory_changes() {
    for path in ["payload", &format!("{RECORDS}payload.json")] {
        let mut f = Fixture::new();
        f.add("payload", b"authored input", "code");
        let head = bind_fixture(&f);
        f.write(path, b"post-commit mutation");
        git_in(&f, &["add", "."]);
        assert!(
            bound_report(&f, &head)
                .errors()
                .iter()
                .any(|e| e.contains("bound byte drift"))
        );
    }
    let mut staged = Fixture::new();
    staged.add("payload", b"authored input", "code");
    let head = bind_fixture(&staged);
    staged.write("payload", b"staged bytes");
    git_in(&staged, &["add", "."]);
    staged.write("payload", b"authored input");
    assert!(
        bound_report(&staged, &head)
            .errors()
            .iter()
            .any(|e| e.contains("index differs"))
    );
    let mut f = Fixture::new();
    f.add("payload", b"authored input", "code");
    let head = bind_fixture(&f);
    f.write("extra", b"staged addition");
    git_in(&f, &["add", "."]);
    assert!(!bound_report(&f, &head).errors().is_empty());
}

#[test]
fn bound_distribution_requires_independent_members_bytes_and_control_files() {
    let mut f = Fixture::new();
    let i = f.add("payload", b"authored input", "code");
    f.records[i]["sha256"] = json!("0".repeat(64));
    let head = bind_fixture(&f);
    let output = tempfile::tempdir().unwrap();
    for (path, entry) in norn_skeleton_guard::ci::git::tree(f.root(), &head).unwrap() {
        let target = output.path().join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, entry.bytes).unwrap();
    }
    let check = || {
        provenance::check_bound(
            output.path(),
            &provenance::BoundSource {
                source: f.root(),
                head: &head,
                base: f.root(),
                repository: "Norn-cloud/norn",
            },
            true,
        )
    };
    assert!(check().errors().is_empty());
    assert!(
        !provenance::check_distribution(output.path())
            .errors()
            .is_empty()
    );
    for path in ["payload", MANIFEST, &format!("{RECORDS}payload.json")] {
        let target = output.path().join(path);
        let before = fs::read(&target).unwrap();
        fs::write(&target, b"mutation").unwrap();
        assert!(!check().errors().is_empty(), "{path}");
        fs::remove_file(&target).unwrap();
        assert!(!check().errors().is_empty(), "missing {path}");
        fs::write(&target, before).unwrap();
    }
    fs::write(output.path().join("forged-export-inventory.json"), b"{}").unwrap();
    assert!(!check().errors().is_empty());
    assert!(
        !provenance::check_distribution(output.path())
            .errors()
            .is_empty()
    );
}

#[test]
fn actual_bytes_preserve_source_and_rejection_closure_despite_misleading_legacy_hashes() {
    for legacy in [None, Some("0".repeat(64)), Some("f".repeat(64))] {
        let mut f = Fixture::new();
        let i = f.add("renamed.json", b"known source bytes", "code");
        let actual = f.reference("renamed.json")["sha256"].clone();
        f.add("registry/standards/sources.lock.json", &serde_json::to_vec(&json!([{
            "sourceId":"source.toy", "releaseId":"1.0.0", "license":"GPL-3.0", "redistribution":"vendored",
            "files":[{"path":"original.json", "sha256":actual}]
        }])).unwrap(), "provenance-evidence");
        if let Some(hash) = &legacy {
            f.records[i]["sha256"] = json!(hash);
        } else {
            f.records[i].as_object_mut().unwrap().remove("sha256");
        }
        let head = bind_fixture(&f);
        assert!(
            bound_report(&f, &head)
                .errors()
                .iter()
                .any(|e| e.contains("requires derivation from source.toy"))
        );

        let mut f = Fixture::new();
        f.add("bad", b"rejected identical bytes", "extraction-database");
        f.add("renamed", b"rejected identical bytes", "code");
        f.derived("descendant", "renamed", "generated");
        for (index, record) in f.records.iter_mut().enumerate() {
            if legacy.is_none() {
                record.as_object_mut().unwrap().remove("sha256");
            } else {
                record["sha256"] = json!(format!("{index:064x}"));
            }
        }
        let head = bind_fixture(&f);
        let report = bound_report(&f, &head);
        assert!(
            report.rejected_paths["renamed"]
                .iter()
                .any(|e| e.contains("content matches a rejected"))
        );
        assert!(
            report.rejected_paths["descendant"]
                .iter()
                .any(|e| e.contains("rejected ancestor"))
        );

        let mut f = Fixture::new();
        let i = f.add("payload", b"changed upstream bytes", "standards-data");
        let pin = hex::encode(Sha256::digest(b"original upstream bytes"));
        let path = "registry/standards/sources.lock.json";
        f.add(path, &serde_json::to_vec(&json!([{"sourceId":"source.toy", "releaseId":"1.0.0", "license":"Apache-2.0", "redistribution":"vendored", "files":[{"path":"payload","sha256":pin}]}])).unwrap(), "provenance-evidence");
        f.records[i]["origin"] = json!("upstream");
        f.records[i]["sources"] =
            json!([{"id":"source.toy","revision":"1.0.0","evidence":f.reference(path)}]);
        if legacy.is_none() {
            f.records[i].as_object_mut().unwrap().remove("sha256");
        } else {
            f.records[i]["sha256"] = json!(pin);
        } // lie that bytes still match the catalog
        let head = bind_fixture(&f);
        assert!(
            bound_report(&f, &head)
                .errors()
                .iter()
                .any(|e| e.contains("differs from pinned source"))
        );
    }
}

#[test]
fn bound_mode_preserves_missing_record_ancestry_and_nested_evidence_refusals() {
    let mut f = Fixture::new();
    f.add("input", b"authored input", "synthetic-fixture");
    f.derived("generated", "input", "synthetic-golden");
    f.derived("release", "generated", "generated");
    let head = bind_fixture(&f);
    f.write("input", b"uncommitted drift");
    assert!(!bound_report(&f, &head).errors().is_empty());
    // Once committed, a deterministic rights contradiction still taints ancestry.
    f.records[1]["category"] = json!("extraction-database");
    let head = bind_fixture(&f);
    assert!(
        bound_report(&f, &head)
            .rejected_paths
            .contains_key("release")
    );
    f.records[1]["category"] = json!("synthetic-fixture");
    f.records[1]["rights_evidence"][0]["sha256"] = json!("0".repeat(64));
    let head = bind_fixture(&f);
    assert!(
        bound_report(&f, &head)
            .errors()
            .iter()
            .any(|e| e.contains("tampered evidence"))
    );
    f.records.remove(1);
    let head = bind_fixture(&f);
    assert!(
        bound_report(&f, &head)
            .errors()
            .iter()
            .any(|e| e.contains("missing shipped-artifact"))
    );
}

#[test]
fn merge_group_rights_cover_clean_same_file_hunks_and_refuse_unreviewed_integration() {
    let mut f = policy_fixture();
    f.add("notes", b"first\n2\n3\n4\n5\n6\n7\n8\n9\nlast\n", "code");
    let base = bind_fixture(&f);
    git_in(&f, &["branch", "-M", "queue"]);
    let trusted_dir = tempfile::tempdir().unwrap();
    let trusted = trusted_dir.path().join("base");
    git_in(
        &f,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            trusted.to_str().unwrap(),
            &base,
        ],
    );
    let mut heads = Vec::new();
    for (branch, content) in [
        ("pr-a", "FIRST\n2\n3\n4\n5\n6\n7\n8\n9\nlast\n"),
        ("pr-b", "first\n2\n3\n4\n5\n6\n7\n8\n9\nLAST\n"),
    ] {
        git_in(&f, &["checkout", "--quiet", "-b", branch, &base]);
        f.write("notes", content.as_bytes());
        heads.push(commit_fixture(&f));
        git_in(&f, &["checkout", "--quiet", "queue"]);
        git_in(&f, &["merge", "--quiet", "--no-ff", "--no-edit", branch]);
    }
    let head = git_in(&f, &["rev-parse", "HEAD"]);
    let doc = json!({"repository":"Norn-cloud/norn", "merge_group":{"base_sha":base,"head_sha":head},
        "pull_requests":heads.iter().map(|h| rights_document(h, &base)).collect::<Vec<_>>()});
    let admission = |doc: &Value, head: &str| {
        norn_skeleton_guard::ci::rights_admission(doc, f.root(), &trusted, head)
    };
    assert!(admission(&doc, &head).is_ok());
    let evidence = trusted_dir.path().join("group-evidence.json");
    fs::write(&evidence, doc.to_string()).unwrap();
    let report = norn_skeleton_guard::ci::check_with_rights(
        f.root(),
        &trusted,
        &head,
        "Norn-cloud/norn",
        None,
        &evidence,
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(
        provenance::check_bound(
            f.root(),
            &provenance::BoundSource {
                source: f.root(),
                head: &head,
                base: &trusted,
                repository: "Norn-cloud/norn"
            },
            false
        )
        .errors()
        .is_empty()
    );
    let mut missing = doc.clone();
    missing["pull_requests"][1]["comments"] = json!([]);
    assert!(admission(&missing, &head).is_err());
    // Rewrite the merge result with a metadata-only addition: no new parent,
    // but a tree that no acknowledged head or clean integration accounts for.
    f.write(&format!("{RECORDS}notes.json"), b"{}");
    git_in(&f, &["add", "."]);
    git_in(&f, &["commit", "--quiet", "--amend", "--no-edit"]);
    let tampered = git_in(&f, &["rev-parse", "HEAD"]);
    let mut modified = doc;
    modified["merge_group"]["head_sha"] = json!(tampered);
    assert!(
        admission(&modified, &tampered)
            .unwrap_err()
            .contains("integration-only")
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "exercise the real trusted CLI over synthetic event data"
)]
fn trusted_pr_event_and_cli_require_separate_rights_evidence_on_the_inspected_head() {
    let f = policy_fixture();
    let base = commit_fixture(&f);
    let temporary = tempfile::tempdir().unwrap();
    let trusted = temporary.path().join("base");
    git_in(
        &f,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            trusted.to_str().unwrap(),
            &base,
        ],
    );
    f.write(
        "crates/norn-ir/src/lib.rs",
        b"pub enum ArtifactKind { Entity, Law }\n// authored change without a hash refresh\n",
    );
    let head = commit_fixture(&f);
    let evidence = temporary.path().join("rights.json");
    fs::write(&evidence, rights_document(&head, &base).to_string()).unwrap();
    let report = norn_skeleton_guard::ci::check_with_rights(
        f.root(),
        &trusted,
        &head,
        "Norn-cloud/norn",
        None,
        &evidence,
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let binding = report.provenance.binding.unwrap();
    assert_eq!(binding.head, head);
    assert_eq!(binding.base, base);
    assert_eq!(binding.policy, base);
    assert_eq!(binding.changed_paths, ["crates/norn-ir/src/lib.rs"]);
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "ci",
            "trusted",
            "--root",
            f.root().to_str().unwrap(),
            "--base-root",
            trusted.to_str().unwrap(),
            "--head",
            &head,
            "--repository",
            "Norn-cloud/norn",
            "--rights-evidence",
            evidence.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let output: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(output["provenance"]["binding"]["head"], head);
    fs::remove_file(&evidence).unwrap();
    assert!(
        !norn_skeleton_guard::ci::run_with_rights(
            f.root(),
            &trusted,
            &head,
            "Norn-cloud/norn",
            None,
            &evidence
        )
        .is_empty()
    );
    for args in [
        vec!["ci", "provenance"],
        vec![
            "ci",
            "provenance",
            "--distribution",
            "--head",
            &head,
            "--repository",
            "Norn-cloud/norn",
        ],
        vec![
            "ci",
            "provenance",
            "--legacy-distribution",
            "--distribution",
            "--head",
            &head,
        ],
    ] {
        assert!(
            !std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
                .args(args)
                .current_dir(f.root())
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}

#[test]
fn legacy_digest_is_optional_but_present_values_keep_the_strict_shape() {
    for value in [
        json!(null),
        json!("wrong"),
        json!("A".repeat(64)),
        json!(42),
    ] {
        let mut f = Fixture::new();
        let i = f.add("payload", b"authored content", "code");
        f.records[i]["sha256"] = value;
        let head = bind_fixture(&f);
        assert!(!bound_report(&f, &head).errors().is_empty());
    }
}

#[test]
fn overlapping_rights_metadata_changes_require_resolution_even_if_source_merges_cleanly() {
    let mut f = Fixture::new();
    f.add("payload", b"authored payload", "code");
    let base = bind_fixture(&f);
    let path = format!("{RECORDS}payload.json");
    let original: Value = serde_json::from_slice(&fs::read(f.root().join(&path)).unwrap()).unwrap();
    let mut heads = Vec::new();
    for (branch, field, value) in [
        ("metadata-a", "category", json!("generic-proof")),
        ("metadata-b", "notices", json!([f.reference("LICENSE")])),
    ] {
        git_in(&f, &["checkout", "--quiet", "-b", branch, &base]);
        let mut record = original.clone();
        record[field] = value;
        f.write(
            &path,
            serde_json::to_string_pretty(&record).unwrap().as_bytes(),
        );
        heads.push(commit_fixture(&f));
    }
    assert!(
        norn_skeleton_guard::ci::git::clean_merge(f.root(), &heads[0], &heads[1], &base)
            .unwrap_err()
            .contains("metadata disagreement")
    );
}
