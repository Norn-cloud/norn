//! Golden parity and standalone native compiler integration tests for `norn-authoring`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use norn_authoring::{
    AuthoringProgram, BoundedVfs, CompilerLimits, DashboardsCompileOutcome,
    DefinitionsCompileOptions, DefinitionsCompileOutcome, EnvelopeOptions, FlowCompileOptions,
    FlowCompileOutcome, LimitsEnvelope, LimitsProfile, PENDING_SOURCE_SHA256,
    PINNED_EFFECT_DIST_PREFIX, Profile, ProfileChecker, QuestionsCompileOutcome,
    ValueSchemasCompileOutcome, canonical_json, compact_canonical_json, compile_dashboards_program,
    compile_definitions_program, compile_definitions_vfs, compile_flow_program,
    compile_questions_program, compile_value_schemas_program, flow_limits, from_statement,
    lexical_bounds, may_read_actor, norn1_sha256, parse_sentence, parse_statement,
    render_flow_limits, render_sentence, serialize_statement, to_statement,
};
use serde_json::{Map, Value, json};

fn tool_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/norn-ts")
}

fn demo_scope() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("environmentId".to_owned(), "dev".to_owned()),
        ("orgId".to_owned(), "org_demo".to_owned()),
    ])
}

fn mount_ts_dir(
    vfs: &mut BoundedVfs,
    disk_dir: &Path,
    virtual_prefix: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut entries = Vec::new();
    for entry_res in std::fs::read_dir(disk_dir)? {
        let entry = entry_res?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.as_bytes().ends_with(b".ts") {
            entries.push((name, entry.path()));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut names = Vec::with_capacity(entries.len());
    for (name, full_path) in entries {
        let content = std::fs::read_to_string(&full_path)?;
        vfs.add_file(&format!("{virtual_prefix}/{name}"), content)?;
        names.push(name);
    }
    Ok(names)
}

fn mount_pinned_packages(
    profile: &Profile,
) -> Result<(PathBuf, BoundedVfs), Box<dyn std::error::Error>> {
    let root = tool_root();
    let effect_dir = root.join("node_modules/effect");
    let ts_dir = root.join("node_modules/typescript");
    assert!(
        effect_dir.join("package.json").is_file(),
        "pinned effect package must be installed at {}",
        effect_dir.display()
    );
    assert!(
        ts_dir.join("package.json").is_file(),
        "pinned typescript package must be installed at {}",
        ts_dir.display()
    );
    let mut vfs = BoundedVfs::default();
    let effect_count = vfs.mount_pinned_effect_dir(&effect_dir, profile)?;
    assert!(effect_count > 0, "expected pinned effect declarations");
    let ts_count = vfs.mount_pinned_package_dir("/node_modules/typescript", &ts_dir)?;
    assert!(ts_count > 0, "expected pinned typescript declarations");
    Ok((root, vfs))
}

fn expected_code_header(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    for line in text.lines() {
        if let Some(code) = line.trim().strip_prefix("// expect: ") {
            return Ok(code.trim().to_owned());
        }
    }
    Err(format!("{} has no `// expect: CODE` header", path.display()).into())
}

#[test]
fn canonical_json_and_norn1_hash_determinism() -> Result<(), Box<dyn std::error::Error>> {
    let sample = json!({
        "z": 1,
        "a": [true, null, -0.0],
        "m": { "b": "hello", "a": "world" }
    });
    let pretty_a = canonical_json(&sample)?;
    let pretty_b = canonical_json(&sample)?;
    assert_eq!(pretty_a, pretty_b);
    assert!(pretty_a.ends_with('\n'));
    assert!(pretty_a.contains("\n  \"a\": ["));
    assert!(pretty_a.contains("    0\n"));

    let compact = compact_canonical_json(&sample)?;
    assert_eq!(
        compact,
        r#"{"a":[true,null,0],"m":{"a":"world","b":"hello"},"z":1}"#
    );

    let digest_a = norn1_sha256(&sample).ok_or("expected norn1_sha256 digest")?;
    let digest_b = norn1_sha256(&sample).ok_or("expected norn1_sha256 digest")?;
    assert_eq!(digest_a, digest_b);
    assert_eq!(digest_a.len(), 64);
    assert!(
        digest_a
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "expected lower-case 64-hex SHA-256 digest, got: {digest_a}"
    );

    let js_parity = json!({
        "10": 1e-7,
        "2": 1.5e-7,
        "01": 1e-6,
        "a": 1e21
    });
    assert_eq!(
        compact_canonical_json(&js_parity)?,
        r#"{"2":1.5e-7,"10":1e-7,"01":0.000001,"a":1e+21}"#
    );

    let proto = json!({ "__proto__": "forbid" });
    assert!(canonical_json(&proto).is_err());
    Ok(())
}

#[test]
fn zen_reads_and_law_statement_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    assert!(may_read_actor("actor.role == 'admin'"));
    assert!(may_read_actor("self.owner == actor.id"));
    assert!(!may_read_actor("self.note == 'actor.role'"));
    assert!(!may_read_actor("self.amount >= 0"));

    assert!(
        lexical_bounds("self.total > 0 and self.status == 'open'"),
        "expected simple expression to pass lexical bounds"
    );
    let deeply_nested = format!("{}1{}", "(".repeat(130), ")".repeat(130));
    assert!(
        !lexical_bounds(&deeply_nested),
        "expected 130 nested parentheses to exceed MAX_DEPTH (128)"
    );

    let law_ir = json!({
        "lawVersion": "0.1",
        "id": "demo.invoice.positive_total",
        "owner": "demo",
        "severity": "error",
        "rationale": "Invoices must carry a positive total.",
        "provenance": {
            "source": "authoring_sdk",
            "sourcePath": "demo/laws.ts"
        },
        "scope": {
            "root": { "kind": "entity", "id": "demo.Invoice" },
            "objectTypes": ["demo.Invoice"]
        },
        "subject": "demo.Invoice",
        "condition": "self.status == 'issued'",
        "modality": { "kind": "always" },
        "response": {
            "kind": "predicate",
            "expression": "self.total > 0"
        }
    });
    let stmt = to_statement(&law_ir)?;
    let sentence = render_sentence(&stmt)?;
    let reparsed_sentence = parse_sentence(&sentence, stmt.metadata.clone())?;
    assert_eq!(reparsed_sentence, stmt);

    let wire = serialize_statement(&stmt)?;
    let reparsed_wire = parse_statement(&wire)?;
    assert_eq!(reparsed_wire, stmt);
    let round_tripped = from_statement(&reparsed_wire)?;
    assert_eq!(canonical_json(&round_tripped)?, canonical_json(&law_ir)?);
    Ok(())
}

const STANDALONE_DEFS_TS: &str = r#"import { Norn } from "@norn/authoring"

export const Source = Norn.sourceSchema({
  id: "demo.erp.source",
  title: "ERP Source",
  version: "1.0.0",
  source: { system: "erp", instance: "prod" },
  observedAt: "2026-01-01T00:00:00Z",
  adapter: { name: "erp_sync", version: "1.0.0" },
  rawMetadata: {
    uri: "s3://demo/erp/schema.json",
    sha256: "0000000000000000000000000000000000000000000000000000000000000000",
  },
  datasets: [{ id: "orders", nativeName: "orders", kind: "table" }],
  fields: [
    {
      dataset: "orders",
      name: "id",
      ordinal: 1,
      nativeType: "varchar",
      typeCandidate: { type: "string", origin: "declared" },
    },
  ],
})

export const Setting = Norn.setting({
  id: "demo.setting.tax_rate",
  title: "Tax Rate",
  version: "1.0.0",
  settingKey: "tax.rate",
  valueType: "number",
  value: "0.12",
  effectiveFrom: "2026-01-01T00:00:00Z",
  review: { state: "active" },
  scopeRequired: ["orgId"],
})

export const ErpMapping = Norn.mapping({
  id: "demo.erp.orders",
  title: "ERP Orders",
  version: "1.0.0",
  source: Source,
  interfaces: [],
  maps: [],
})
"#;

const STANDALONE_INEXACT_TS: &str = r#"import { Norn } from "@norn/authoring"

export const BadSetting = Norn.setting({
  id: "demo.setting.bad_rate",
  title: "Bad Rate",
  version: "1.0.0",
  settingKey: "tax.bad",
  valueType: "number",
  value: "1e-5",
  effectiveFrom: "2026-01-01T00:00:00Z",
  review: { state: "active" },
  scopeRequired: ["orgId"],
})
"#;

#[test]
fn standalone_definitions_and_source_schema_binding_without_effect_package()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile);

    let mut vfs = BoundedVfs::default();
    vfs.add_file("/app/defs.ts", STANDALONE_DEFS_TS)?;
    vfs.add_file("/app/inexact.ts", STANDALONE_INEXACT_TS)?;

    let outcomes = compile_definitions_vfs(
        &checker,
        vfs,
        &["/app/defs.ts", "/app/inexact.ts"],
        None,
        |file| DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope: demo_scope(),
                source_path: file.trim_start_matches('/').to_owned(),
                registry_package_path: None,
            },
            ..DefinitionsCompileOptions::default()
        },
    )?;
    assert_eq!(outcomes.len(), 2);

    let Some(DefinitionsCompileOutcome::Ok(ok_out)) = outcomes.first() else {
        return Err(format!("expected defs.ts to compile, got: {:?}", outcomes.first()).into());
    };
    assert_eq!(ok_out.artifacts.len(), 3);
    assert!(ok_out.warnings.is_empty());
    let source_art = ok_out
        .artifacts
        .iter()
        .find(|a| a.get("kind").and_then(Value::as_str) == Some("source_schema"))
        .ok_or("missing source_schema artifact")?;
    let mapping_art = ok_out
        .artifacts
        .iter()
        .find(|a| a.get("kind").and_then(Value::as_str) == Some("mapping"))
        .ok_or("missing mapping artifact")?;
    let expected_source_sha =
        norn1_sha256(source_art).ok_or("missing norn1_sha256 for source_schema")?;
    assert_ne!(PENDING_SOURCE_SHA256, expected_source_sha);
    assert_eq!(
        mapping_art
            .pointer("/payload/source/sha256")
            .and_then(Value::as_str),
        Some(expected_source_sha.as_str())
    );
    let setting_art = ok_out
        .artifacts
        .iter()
        .find(|a| a.get("kind").and_then(Value::as_str) == Some("setting"))
        .ok_or("missing setting artifact")?;
    assert_eq!(
        setting_art
            .pointer("/payload/value")
            .and_then(Value::as_str),
        Some("0.12")
    );
    assert_ne!(
        PENDING_SOURCE_SHA256,
        norn1_sha256(setting_art).ok_or("missing norn1_sha256 for setting")?
    );

    let Some(DefinitionsCompileOutcome::Err { diagnostics }) = outcomes.get(1) else {
        return Err("expected binary JS number in Norn.setting to be refused".into());
    };
    assert!(
        diagnostics.iter().any(|d| d.code == "NORN-FLOW-LITERAL"),
        "expected NORN-FLOW-LITERAL for VAL-EXACT-1 refusal, got: {diagnostics:?}"
    );
    Ok(())
}

#[test]
fn golden_flow_parity_matches_ts_compiler_and_limits() -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());
    let (root, mut vfs) = mount_pinned_packages(&profile)?;

    let flows_dir = root.join("fixtures/flows");
    mount_ts_dir(
        &mut vfs,
        &flows_dir.join("support"),
        "/tools/norn-ts/fixtures/flows/support",
    )?;
    let flow_names = [
        "sequence",
        "route",
        "typed-failure",
        "retry-timer",
        "terminal-fact",
    ];
    let mut flow_roots = Vec::with_capacity(flow_names.len().saturating_add(1));
    for name in flow_names {
        let disk = flows_dir.join(format!("{name}.source.ts"));
        let vpath = format!("/tools/norn-ts/fixtures/flows/{name}.source.ts");
        vfs.add_file(&vpath, std::fs::read_to_string(&disk)?)?;
        flow_roots.push(vpath);
    }
    let vert_dir = root.join("fixtures/vertical");
    mount_ts_dir(
        &mut vfs,
        &vert_dir.join("support"),
        "/tools/norn-ts/fixtures/vertical/support",
    )?;
    let vert_vpath = "/tools/norn-ts/fixtures/vertical/route-message.source.ts".to_owned();
    vfs.add_file(
        &vert_vpath,
        std::fs::read_to_string(vert_dir.join("route-message.source.ts"))?,
    )?;
    flow_roots.push(vert_vpath.clone());

    let root_slices: Vec<&str> = flow_roots.iter().map(String::as_str).collect();
    let program = AuthoringProgram::new(vfs, &root_slices, profile)?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);

    let limits_profile = LimitsProfile {
        id: "norn.limits".to_owned(),
        version: 1,
        envelope: LimitsEnvelope {
            max_commands: 16,
            max_events: 16,
            max_output_bytes: 65_536,
            max_timers: 16,
        },
    };

    for name in flow_names {
        verify_one_flow_golden(&checker, &program, &flows_dir, name, &limits_profile)?;
    }
    verify_vertical_flow_golden(&checker, &program, &vert_dir, &vert_vpath)?;
    Ok(())
}

fn verify_one_flow_golden(
    checker: &ProfileChecker,
    program: &AuthoringProgram,
    flows_dir: &Path,
    name: &str,
    limits_profile: &LimitsProfile,
) -> Result<(), Box<dyn std::error::Error>> {
    let vpath = format!("/tools/norn-ts/fixtures/flows/{name}.source.ts");
    let opts = FlowCompileOptions {
        envelope: EnvelopeOptions {
            scope: demo_scope(),
            source_path: format!("tools/norn-ts/fixtures/flows/{name}.flow.ts"),
            registry_package_path: None,
        },
        include_flow_source_map: true,
        source_root: "/".to_owned(),
        tool_root: "/tools/norn-ts".to_owned(),
        forbid_unsafe_zen: false,
        limits: CompilerLimits::default(),
    };
    let outcomes = compile_flow_program(checker, program, &[vpath.as_str()], &opts)?;
    let Some(FlowCompileOutcome::Ok(out)) = outcomes.into_iter().next() else {
        return Err(format!("flow fixture `{name}` failed to compile").into());
    };
    let actual_json = canonical_json(&Value::Array(out.artifacts.clone()))?;
    let golden_json = std::fs::read_to_string(flows_dir.join(format!("{name}.ir.json")))?;
    assert_eq!(
        actual_json, golden_json,
        "flow golden mismatch for `{name}`"
    );

    let repeat = compile_flow_program(checker, program, &[vpath.as_str()], &opts)?;
    let Some(FlowCompileOutcome::Ok(repeat_out)) = repeat.into_iter().next() else {
        return Err(format!("repeat compile of `{name}` failed").into());
    };
    assert_eq!(
        canonical_json(&Value::Array(repeat_out.artifacts))?,
        actual_json,
        "non-deterministic flow compilation for `{name}`"
    );

    let limits = flow_limits(&out.definition, &out.artifacts, limits_profile);
    assert!(
        limits.checks.iter().all(|c| c.ok),
        "flow `{name}` exceeded limits: {:?}",
        limits.checks
    );
    let rendered = render_flow_limits(&[limits]);
    assert!(rendered.contains(&out.definition.meta.id));
    Ok(())
}

fn verify_vertical_flow_golden(
    checker: &ProfileChecker,
    program: &AuthoringProgram,
    vert_dir: &Path,
    vert_vpath: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let vert_opts = FlowCompileOptions {
        envelope: EnvelopeOptions {
            scope: BTreeMap::from([
                ("environmentId".to_owned(), "main".to_owned()),
                ("orgId".to_owned(), "tenant_acme".to_owned()),
            ]),
            source_path: "tools/norn-ts/fixtures/vertical/route-message.source.ts".to_owned(),
            registry_package_path: None,
        },
        include_flow_source_map: false,
        source_root: "/".to_owned(),
        tool_root: "/tools/norn-ts".to_owned(),
        forbid_unsafe_zen: false,
        limits: CompilerLimits::default(),
    };
    let vert_outcomes = compile_flow_program(checker, program, &[vert_vpath], &vert_opts)?;
    let Some(FlowCompileOutcome::Ok(vert_out)) = vert_outcomes.into_iter().next() else {
        return Err("vertical flow fixture `route-message` failed to compile".into());
    };
    let vert_actual = canonical_json(&Value::Array(vert_out.artifacts))?;
    let vert_golden = std::fs::read_to_string(vert_dir.join("route-message.ir.json"))?;
    assert_eq!(vert_actual, vert_golden, "vertical flow golden mismatch");
    Ok(())
}

#[test]
fn golden_definitions_and_schema_parity_matches_ts_compiler()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());
    let (root, mut vfs) = mount_pinned_packages(&profile)?;

    let defs_dir = root.join("fixtures/definitions");
    let schema_dir = root.join("fixtures/schema");
    let def_cases = [
        (&defs_dir, "fixtures/definitions", "crm", false),
        (&defs_dir, "fixtures/definitions", "jewelry-pricing", true),
        (&schema_dir, "fixtures/schema", "invoice", false),
        (&schema_dir, "fixtures/schema", "precision", false),
    ];
    let closed_vpath = "/tools/norn-ts/fixtures/schema/closed.source.ts";
    vfs.add_file(
        closed_vpath,
        std::fs::read_to_string(schema_dir.join("closed.source.ts"))?,
    )?;

    let mut case_meta = Vec::with_capacity(def_cases.len());
    for (dir, rel, stem, compact_golden) in def_cases {
        let vpath = format!("/tools/norn-ts/{rel}/{stem}.source.ts");
        let source_path = format!("tools/norn-ts/{rel}/{stem}.source.ts");
        vfs.add_file(
            &vpath,
            std::fs::read_to_string(dir.join(format!("{stem}.source.ts")))?,
        )?;
        case_meta.push((
            dir.join(format!("{stem}.ir.json")),
            vpath,
            source_path,
            compact_golden,
        ));
    }
    let mut root_files: Vec<&str> = vec![closed_vpath];
    for (_, vpath, _, _) in &case_meta {
        root_files.push(vpath.as_str());
    }

    let program = AuthoringProgram::new(vfs, &root_files, profile)?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);

    for (golden_path, vpath, source_path, compact_golden) in &case_meta {
        let outcomes = compile_definitions_program(&checker, &program, &[vpath.as_str()], |_| {
            DefinitionsCompileOptions {
                envelope: EnvelopeOptions {
                    scope: demo_scope(),
                    source_path: source_path.clone(),
                    registry_package_path: None,
                },
                tool_root: "/tools/norn-ts".to_owned(),
                ..DefinitionsCompileOptions::default()
            }
        })?;
        let Some(DefinitionsCompileOutcome::Ok(out)) = outcomes.into_iter().next() else {
            return Err(format!("definition fixture `{source_path}` failed to compile").into());
        };
        let actual = if *compact_golden {
            format!(
                "{}\n",
                compact_canonical_json(&Value::Array(out.artifacts))?
            )
        } else {
            canonical_json(&Value::Array(out.artifacts))?
        };
        let expected = std::fs::read_to_string(golden_path)?;
        assert_eq!(actual, expected, "golden mismatch for `{source_path}`");
    }

    let closed_outcome =
        compile_value_schemas_program(&checker, &program, closed_vpath, CompilerLimits::default())?;
    let ValueSchemasCompileOutcome::Ok { schemas } = closed_outcome else {
        return Err("closed.source.ts failed to compile value schemas".into());
    };
    let mut schema_map = Map::new();
    for (name, schema) in schemas {
        schema_map.insert(name, schema.to_json());
    }
    let actual_closed = canonical_json(&Value::Object(schema_map))?;
    let expected_closed = std::fs::read_to_string(schema_dir.join("closed.schemas.json"))?;
    assert_eq!(
        actual_closed, expected_closed,
        "closed.schemas.json mismatch"
    );
    Ok(())
}

#[test]
fn golden_questions_and_dashboards_parity_matches_ts_compiler()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());
    let (root, mut vfs) = mount_pinned_packages(&profile)?;

    let q_vpath = "/tools/norn-ts/fixtures/questions/scrap-yield.ts";
    let d_vpath = "/tools/norn-ts/fixtures/dashboards/refinery.ts";
    vfs.add_file(
        q_vpath,
        std::fs::read_to_string(root.join("fixtures/questions/scrap-yield.ts"))?,
    )?;
    vfs.add_file(
        d_vpath,
        std::fs::read_to_string(root.join("fixtures/dashboards/refinery.ts"))?,
    )?;

    let program = AuthoringProgram::new(vfs, &[q_vpath, d_vpath], profile)?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);

    let q_outcome =
        compile_questions_program(&checker, &program, q_vpath, CompilerLimits::default())?;
    let QuestionsCompileOutcome::Ok { questions } = q_outcome else {
        return Err("scrap-yield.ts failed to compile questions".into());
    };
    let q_expected =
        std::fs::read_to_string(root.join("fixtures/questions/scrap-yield.question.json"))?;
    assert_eq!(
        format!("{}\n", compact_canonical_json(&Value::Array(questions))?),
        q_expected,
        "scrap-yield.question.json mismatch"
    );

    let d_outcome =
        compile_dashboards_program(&checker, &program, d_vpath, CompilerLimits::default())?;
    let DashboardsCompileOutcome::Ok { dashboards } = d_outcome else {
        return Err("refinery.ts failed to compile dashboards".into());
    };
    let d_expected: Value = serde_json::from_str(&std::fs::read_to_string(
        root.join("fixtures/dashboards/refinery.dashboard.json"),
    )?)?;
    assert_eq!(
        Value::Array(dashboards),
        d_expected,
        "refinery.dashboard.json mismatch"
    );
    Ok(())
}

#[test]
fn compiler_rejection_parity_preserves_profile_and_schema_refusals()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());
    let (root, mut vfs) = mount_pinned_packages(&profile)?;

    let corpus_dir = root.join("corpus");
    mount_ts_dir(&mut vfs, &corpus_dir.join("support"), "/corpus/support")?;
    let reject_names = mount_ts_dir(&mut vfs, &corpus_dir.join("reject"), "/corpus/reject")?;
    let invalid_dir = root.join("fixtures/schema/invalid");
    let invalid_names = mount_ts_dir(&mut vfs, &invalid_dir, "/schema/invalid")?;
    let invalid_closed_dir = root.join("fixtures/schema/invalid-closed");
    let invalid_closed_names =
        mount_ts_dir(&mut vfs, &invalid_closed_dir, "/schema/invalid-closed")?;

    let mut roots: Vec<String> = Vec::new();
    for name in &reject_names {
        roots.push(format!("/corpus/reject/{name}"));
    }
    for name in &invalid_names {
        roots.push(format!("/schema/invalid/{name}"));
    }
    for name in &invalid_closed_names {
        roots.push(format!("/schema/invalid-closed/{name}"));
    }
    let root_slices: Vec<&str> = roots.iter().map(String::as_str).collect();
    let program = AuthoringProgram::new(vfs, &root_slices, profile)?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);

    let flow_reject_paths: Vec<String> = reject_names
        .iter()
        .map(|name| format!("/corpus/reject/{name}"))
        .collect();
    let flow_reject_slices: Vec<&str> = flow_reject_paths.iter().map(String::as_str).collect();
    let flow_outcomes = compile_flow_program(
        &checker,
        &program,
        &flow_reject_slices,
        &FlowCompileOptions::default(),
    )?;
    for (name, outcome) in reject_names.iter().zip(flow_outcomes) {
        let FlowCompileOutcome::Err { diagnostics } = outcome else {
            return Err(format!("reject corpus fixture `{name}` unexpectedly compiled").into());
        };
        assert!(
            !diagnostics.is_empty(),
            "reject corpus fixture `{name}` emitted no diagnostics"
        );
    }

    for name in &invalid_names {
        let vpath = format!("/schema/invalid/{name}");
        let expected_code = expected_code_header(&invalid_dir.join(name))?;
        let outcomes = compile_definitions_program(&checker, &program, &[vpath.as_str()], |_| {
            DefinitionsCompileOptions {
                envelope: EnvelopeOptions {
                    scope: demo_scope(),
                    source_path: format!("tools/norn-ts/fixtures/schema/invalid/{name}"),
                    registry_package_path: None,
                },
                ..DefinitionsCompileOptions::default()
            }
        })?;
        let Some(DefinitionsCompileOutcome::Err { diagnostics }) = outcomes.into_iter().next()
        else {
            return Err(format!("invalid schema fixture `{name}` unexpectedly compiled").into());
        };
        assert!(
            diagnostics.iter().any(|d| d.code == expected_code),
            "invalid schema `{name}` expected `{expected_code}`, got: {diagnostics:?}"
        );
        assert!(diagnostics.iter().all(|d| d.start.line > 0));
    }

    for name in &invalid_closed_names {
        let vpath = format!("/schema/invalid-closed/{name}");
        let expected_code = expected_code_header(&invalid_closed_dir.join(name))?;
        let outcome = compile_value_schemas_program(
            &checker,
            &program,
            vpath.as_str(),
            CompilerLimits::default(),
        )?;
        let ValueSchemasCompileOutcome::Err { diagnostics } = outcome else {
            return Err(
                format!("invalid-closed schema fixture `{name}` unexpectedly compiled").into(),
            );
        };
        assert!(
            diagnostics.iter().any(|d| d.code == expected_code),
            "invalid-closed schema `{name}` expected `{expected_code}`, got: {diagnostics:?}"
        );
    }
    Ok(())
}

mod compiler_parity_regressions;
