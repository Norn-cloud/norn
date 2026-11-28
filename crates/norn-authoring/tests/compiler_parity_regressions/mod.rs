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

#[test]
fn regression_schema_preflight_charges_expanded_enum_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());
    let (_, mut vfs) = mount_pinned_packages(&profile)?;

    let big_lit = "a".repeat(2_048);
    let mut schema_src = format!(
        "import {{ Schema }} from \"effect\"\n\
const BigEnum = Schema.Literal(\"{big_lit}\")\n\
export const Wide = Schema.Struct({{\n"
    );
    for i in 0_usize..20 {
        writeln!(schema_src, "  f{i}: BigEnum,")?;
    }
    schema_src.push_str("})\n");

    let mut entity_src = format!(
        "import {{ Schema }} from \"effect\"\nimport {{ Norn }} from \"@norn/authoring\"\n\
const BigEnum = Schema.Literal(\"{big_lit}\")\n\
export const WideType = Norn.type({{ name: \"demo.Wide\", title: \"Wide\", version: \"1.0.0\", \
properties: Schema.Struct({{\n"
    );
    for i in 0_usize..20 {
        writeln!(entity_src, "  f{i}: BigEnum,")?;
    }
    entity_src.push_str("}) })\n");

    let (schema_p, entity_p) = ("/reg/enum_bytes.ts", "/reg/enum_entity.ts");
    vfs.add_file(schema_p, schema_src)?;
    vfs.add_file(entity_p, entity_src)?;
    let program = AuthoringProgram::new(vfs, &[schema_p, entity_p], profile)?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);

    let tight = CompilerLimits {
        max_output_bytes: 16_000,
        ..CompilerLimits::default()
    };
    let ValueSchemasCompileOutcome::Err { diagnostics } =
        compile_value_schemas_program(&checker, &program, schema_p, tight)?
    else {
        return Err("expected shared large enum schema to refuse in preflight".into());
    };
    assert!(
        diagnostics.iter().any(|d| d.code == "NORN-FLOW-UNBOUNDED"),
        "expected NORN-FLOW-UNBOUNDED for schema enum bytes, got: {diagnostics:?}"
    );

    let entity_out = compile_definitions_program(&checker, &program, &[entity_p], |_| {
        DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope: demo_scope(),
                source_path: "reg/enum_entity.ts".to_owned(),
                registry_package_path: None,
            },
            limits: tight,
            ..DefinitionsCompileOptions::default()
        }
    })?;
    let Some(DefinitionsCompileOutcome::Err { diagnostics }) = entity_out.first() else {
        return Err("expected shared large enum entity properties to refuse in preflight".into());
    };
    assert!(
        diagnostics.iter().any(|d| d.code == "NORN-FLOW-UNBOUNDED"),
        "expected NORN-FLOW-UNBOUNDED for entity enum bytes, got: {diagnostics:?}"
    );
    Ok(())
}

#[test]
fn regression_decision_flat_if_return_chain_linear_and_bounded()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());
    let (_, mut vfs) = mount_pinned_packages(&profile)?;

    let mut ok_src = String::from(
        "import { Schema } from \"effect\"\nimport { Norn } from \"@norn/authoring\"\n\
export const ChainOk = Norn.decision({ id: \"demo.rule.chain_ok\", title: \"Chain\", version: \"1.0.0\", \
contract: \"sub_decision\", scopeRequired: [\"orgId\"], \
input: Schema.Struct({ x: Schema.Int }), output: Schema.Struct({ y: Schema.Int }) }, input => {\n",
    );
    for i in 0_usize..60 {
        writeln!(ok_src, "  if (input.x > {i}) return {{ y: {i} }}")?;
    }
    ok_src.push_str("  return { y: 0 }\n})\n");

    let mut over_src = String::from(
        "import { Schema } from \"effect\"\nimport { Norn } from \"@norn/authoring\"\n\
export const ChainOver = Norn.decision({ id: \"demo.rule.chain_over\", title: \"ChainOver\", version: \"1.0.0\", \
contract: \"sub_decision\", scopeRequired: [\"orgId\"], \
input: Schema.Struct({ x: Schema.Int }), output: Schema.Struct({ y: Schema.Int }) }, input => {\n",
    );
    for i in 0_usize..80 {
        writeln!(over_src, "  if (input.x > {i}) return {{ y: {i} }}")?;
    }
    over_src.push_str("  return { y: 0 }\n})\n");

    let (ok_p, over_p) = ("/reg/chain_ok.ts", "/reg/chain_over.ts");
    vfs.add_file(ok_p, ok_src)?;
    vfs.add_file(over_p, over_src)?;
    let program = AuthoringProgram::new(vfs, &[ok_p, over_p], profile)?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);

    let ok_out =
        compile_definitions_program(&checker, &program, &[ok_p], |_| DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope: demo_scope(),
                source_path: "reg/chain_ok.ts".to_owned(),
                registry_package_path: None,
            },
            ..DefinitionsCompileOptions::default()
        })?;
    assert!(
        matches!(ok_out.first(), Some(DefinitionsCompileOutcome::Ok(_))),
        "expected 60-branch flat if-return chain to compile within budget, got: {ok_out:?}"
    );

    let over_out = compile_definitions_program(&checker, &program, &[over_p], |_| {
        DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope: demo_scope(),
                source_path: "reg/chain_over.ts".to_owned(),
                registry_package_path: None,
            },
            limits: CompilerLimits {
                max_expansion_work: 1_000,
                ..CompilerLimits::default()
            },
            ..DefinitionsCompileOptions::default()
        }
    })?;
    let Some(DefinitionsCompileOutcome::Err { diagnostics }) = over_out.first() else {
        return Err("expected 80-branch flat if-return chain to trip work budget".into());
    };
    assert!(
        diagnostics.iter().any(|d| d.code == "NORN-FLOW-UNBOUNDED"),
        "expected NORN-FLOW-UNBOUNDED for flat if-return chain, got: {diagnostics:?}"
    );
    Ok(())
}

#[test]
fn regression_literal_streaming_budget_for_objects_and_arrays()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile);

    let key_pad = "k".repeat(64);
    let mut obj_src = String::from("import { Norn } from \"@norn/authoring\"\nconst OBJ = {\n");
    for i in 0_usize..120 {
        writeln!(obj_src, "  \"{key_pad}_{i}\": 1,")?;
    }
    obj_src.push_str(
        "}\nexport const ObjLit = Norn.setting({ id: \"demo.setting.obj_lit\", title: \"Obj\", version: \"1.0.0\", \
settingKey: \"demo.obj\", valueType: \"json\", value: [OBJ, OBJ, OBJ], \
effectiveFrom: \"2026-01-01T00:00:00Z\", review: { state: \"active\" }, scopeRequired: [\"orgId\"] })\n",
    );

    let mut arr_src = String::from("import { Norn } from \"@norn/authoring\"\nconst ARR = [");
    for i in 0_usize..200 {
        if i > 0 {
            arr_src.push_str(", ");
        }
        arr_src.push('1');
    }
    arr_src.push_str(
        "]\nexport const ArrLit = Norn.setting({ id: \"demo.setting.arr_lit\", title: \"Arr\", version: \"1.0.0\", \
settingKey: \"demo.arr\", valueType: \"json\", value: ARR, \
effectiveFrom: \"2026-01-01T00:00:00Z\", review: { state: \"active\" }, scopeRequired: [\"orgId\"] })\n",
    );

    let mut vfs = BoundedVfs::default();
    vfs.add_file("/r/obj_lit.ts", obj_src)?;
    vfs.add_file("/r/arr_lit.ts", arr_src)?;
    let out = compile_definitions_vfs(
        &checker,
        vfs,
        &["/r/obj_lit.ts", "/r/arr_lit.ts"],
        None,
        |file| DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope: demo_scope(),
                source_path: file.trim_start_matches('/').to_owned(),
                registry_package_path: None,
            },
            limits: if file == "/r/obj_lit.ts" {
                CompilerLimits {
                    max_output_bytes: 4_000,
                    ..CompilerLimits::default()
                }
            } else {
                CompilerLimits {
                    max_expansion_work: 150,
                    ..CompilerLimits::default()
                }
            },
            ..DefinitionsCompileOptions::default()
        },
    )?;
    for idx in 0_usize..2 {
        let Some(DefinitionsCompileOutcome::Err { diagnostics }) = out.get(idx) else {
            return Err(format!("literal case {idx} unexpectedly compiled").into());
        };
        assert!(
            diagnostics.iter().any(|d| d.code == "NORN-FLOW-UNBOUNDED"),
            "literal case {idx} expected NORN-FLOW-UNBOUNDED, got: {diagnostics:?}"
        );
    }
    Ok(())
}
