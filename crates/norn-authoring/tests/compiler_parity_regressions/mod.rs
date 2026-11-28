//! Regression tests for bounded compilation, canonicalization failure
//! propagation, and escaped-byte budgeting.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use norn_authoring::{
    AuthoringProgram, BoundedVfs, CompilerLimits, DefinitionsCompileOptions,
    DefinitionsCompileOutcome, EnvelopeOptions, PINNED_EFFECT_DIST_PREFIX, Profile, ProfileChecker,
    ValueSchemasCompileOutcome, compile_definitions_program, compile_definitions_vfs,
    compile_value_schemas_program,
};

use super::{STANDALONE_DEFS_TS, demo_scope, mount_pinned_packages};

#[test]
fn regression_schema_dag_refuses_without_oom() -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());
    let (_, mut vfs) = mount_pinned_packages(&profile)?;

    let mut dag_src = String::from(
        "import { Schema } from \"effect\"\nexport const S0 = Schema.Struct({ v: Schema.String })\n",
    );
    for i in 1_usize..=25 {
        let prev = i.saturating_sub(1);
        writeln!(
            dag_src,
            "export const S{i} = Schema.Struct({{ a: S{prev}, b: S{prev} }})"
        )?;
    }

    let dag_p = "/reg/dag.ts";
    vfs.add_file(dag_p, dag_src)?;
    let program = AuthoringProgram::new(vfs, &[dag_p], profile)?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);

    let ValueSchemasCompileOutcome::Err { diagnostics } =
        compile_value_schemas_program(&checker, &program, dag_p, CompilerLimits::default())?
    else {
        return Err("expected 25-step schema DAG to refuse without OOM".into());
    };
    assert!(
        diagnostics.iter().any(|d| d.code == "NORN-FLOW-UNBOUNDED"),
        "expected NORN-FLOW-UNBOUNDED for schema DAG, got: {diagnostics:?}"
    );
    Ok(())
}

#[test]
fn regression_decision_callback_iterative_and_branch_bounds()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());
    let (_, mut vfs) = mount_pinned_packages(&profile)?;

    let mut flat_src = String::from(
        "import { Schema } from \"effect\"\nimport { Norn } from \"@norn/authoring\"\n\
export const Flat = Norn.decision({ id: \"demo.rule.flat\", title: \"Flat\", version: \"1.0.0\", \
contract: \"sub_decision\", scopeRequired: [\"orgId\"], \
input: Schema.Struct({ x: Schema.Int }), output: Schema.Struct({ y: Schema.Int }) }, input => {\n\
  const v0 = input.x\n",
    );
    for i in 1_usize..150 {
        let prev = i.saturating_sub(1);
        writeln!(flat_src, "  const v{i} = v{prev}")?;
    }
    flat_src.push_str("  return { y: v149 }\n})\n");

    let mut branch_src = String::from(
        "import { Schema } from \"effect\"\nimport { Norn } from \"@norn/authoring\"\n\
export const Deep = Norn.decision({ id: \"demo.rule.deep\", title: \"Deep\", version: \"1.0.0\", \
contract: \"sub_decision\", scopeRequired: [\"orgId\"], \
input: Schema.Struct({ x: Schema.Int }), output: Schema.Struct({ y: Schema.Int }) }, input => {\n",
    );
    for i in 0_usize..12 {
        writeln!(
            branch_src,
            "  if (input.x > {i}) {{ const b{i} = 1 }} else {{ const b{i} = 0 }}"
        )?;
    }
    branch_src.push_str("  return { y: input.x }\n})\n");

    let (flat_p, branch_p) = ("/reg/flat.ts", "/reg/branch.ts");
    vfs.add_file(flat_p, flat_src)?;
    vfs.add_file(branch_p, branch_src)?;
    let program = AuthoringProgram::new(vfs, &[flat_p, branch_p], profile)?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);

    let flat_out = compile_definitions_program(&checker, &program, &[flat_p], |_| {
        DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope: demo_scope(),
                source_path: "reg/flat.ts".to_owned(),
                registry_package_path: None,
            },
            ..DefinitionsCompileOptions::default()
        }
    })?;
    assert!(
        matches!(flat_out.first(), Some(DefinitionsCompileOutcome::Ok(_))),
        "expected 150 flat const bindings to compile, got: {flat_out:?}"
    );

    let branch_out = compile_definitions_program(&checker, &program, &[branch_p], |_| {
        DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope: demo_scope(),
                source_path: "reg/branch.ts".to_owned(),
                registry_package_path: None,
            },
            ..DefinitionsCompileOptions::default()
        }
    })?;
    let Some(DefinitionsCompileOutcome::Err { diagnostics }) = branch_out.first() else {
        return Err("expected exponential decision branches to fail closed".into());
    };
    assert!(
        diagnostics.iter().any(|d| d.code == "NORN-FLOW-UNBOUNDED"),
        "expected NORN-FLOW-UNBOUNDED for branch fanout, got: {diagnostics:?}"
    );
    Ok(())
}

#[test]
fn regression_literal_budget_proto_and_escape_cap() -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile);

    let big = "x".repeat(4_096);
    let huge_src = format!(
        "import {{ Norn }} from \"@norn/authoring\"\nconst BIG = \"{big}\"\n\
export const Huge = Norn.setting({{ id: \"demo.setting.huge\", title: \"Huge\", version: \"1.0.0\", \
settingKey: \"demo.huge\", valueType: \"json\", value: [BIG, BIG, BIG, BIG, BIG, BIG], \
effectiveFrom: \"2026-01-01T00:00:00Z\", review: {{ state: \"active\" }}, scopeRequired: [\"orgId\"] }})\n"
    );
    let proto_labels = "import { Norn } from \"@norn/authoring\"\n\
export const BadLabels = Norn.labels({ id: \"demo.bad.labels\", title: \"Bad\", version: \"1.0.0\", \
actions: { \"demo.act\": { label: { \"__proto__\": \"bad\" } } } })\n";

    let mut vfs = BoundedVfs::default();
    vfs.add_file("/r/huge.ts", huge_src)?;
    vfs.add_file("/r/proto_labels.ts", proto_labels)?;
    vfs.add_file("/r/ok.ts", STANDALONE_DEFS_TS)?;
    let out = compile_definitions_vfs(
        &checker,
        vfs,
        &["/r/huge.ts", "/r/proto_labels.ts", "/r/ok.ts"],
        None,
        |file| {
            let scope = if file == "/r/ok.ts" {
                BTreeMap::from([("__proto__".to_owned(), "bad".to_owned())])
            } else {
                demo_scope()
            };
            DefinitionsCompileOptions {
                envelope: EnvelopeOptions {
                    scope,
                    source_path: file.trim_start_matches('/').to_owned(),
                    registry_package_path: None,
                },
                limits: CompilerLimits {
                    max_output_bytes: 12_000,
                    ..CompilerLimits::default()
                },
                ..DefinitionsCompileOptions::default()
            }
        },
    )?;
    for (idx, expected_code) in [
        (0_usize, "NORN-FLOW-UNBOUNDED"),
        (1, "NORN-FLOW-LITERAL"),
        (2, "NORN-FLOW-LITERAL"),
    ] {
        let Some(DefinitionsCompileOutcome::Err { diagnostics }) = out.get(idx) else {
            return Err(format!("case {idx} unexpectedly compiled").into());
        };
        assert!(
            diagnostics.iter().any(|d| d.code == expected_code),
            "case {idx} expected {expected_code}, got: {diagnostics:?}"
        );
    }

    let ctrl = "\\u0000".repeat(500);
    let esc_src = format!(
        "import {{ Norn }} from \"@norn/authoring\"\n\
export const Esc = Norn.setting({{ id: \"demo.setting.esc\", title: \"Esc\", version: \"1.0.0\", \
settingKey: \"demo.esc\", valueType: \"json\", value: \"{ctrl}\", \
effectiveFrom: \"2026-01-01T00:00:00Z\", review: {{ state: \"active\" }}, scopeRequired: [\"orgId\"] }})\n"
    );
    let mut esc_vfs = BoundedVfs::default();
    esc_vfs.add_file("/r/esc.ts", esc_src)?;
    let esc_out = compile_definitions_vfs(&checker, esc_vfs, &["/r/esc.ts"], None, |_| {
        DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope: demo_scope(),
                source_path: "r/esc.ts".to_owned(),
                registry_package_path: None,
            },
            limits: CompilerLimits {
                max_output_bytes: 1_500,
                ..CompilerLimits::default()
            },
            ..DefinitionsCompileOptions::default()
        }
    })?;
    let Some(DefinitionsCompileOutcome::Err { diagnostics }) = esc_out.first() else {
        return Err("expected control-heavy string to trip escaped-byte budget".into());
    };
    assert!(
        diagnostics.iter().any(|d| d.code == "NORN-FLOW-UNBOUNDED"),
        "expected NORN-FLOW-UNBOUNDED for escaped string cap, got: {diagnostics:?}"
    );
    Ok(())
}
