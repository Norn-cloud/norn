//! Negative and fail-closed refusal integration tests for Slice 2 native
//! Python-to-Canonical-IR lowering, porting every refusal case from
//! `tools/norn-py/tests/test_norn_py.py`.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;

use norn_authoring_py::{
    CODE_CANONICAL, CODE_DEPLOYMENT_ENDPOINT, CODE_DEPLOYMENT_SCOPE, CODE_ELABORATION,
    CODE_EXPRESSION, CODE_FLOAT_LITERAL, CODE_FLOW_LITERAL, CODE_FORBIDDEN_CALL,
    CODE_FORBIDDEN_SYNTAX, CODE_INTERFACE_LIFECYCLE, CODE_PACK_CONFLICT, CODE_PACK_CYCLE,
    CODE_PACK_DUPLICATE, CODE_PACK_MISSING_DEP, CODE_RAW_EXPR, CODE_RAW_IR, CODE_RAW_SECRET,
    CODE_SCHEMA, CODE_TYPE_MISMATCH, CODE_UNBOUNDED, CODE_UNSAFE_ANNOTATION, CellHostKind,
    DeploymentSpec, EnvelopeOptions, JsonValue, MAX_SOURCE_BYTES, PackCategory, PackConflictPolicy,
    PackManifest, PyAuthoringError, SandboxLimits, SettingValue, canonical_json,
    canonical_norn1_bytes, compose_packs, elaborate_source, elaborate_source_with_limits,
    validate_annotation_source,
};

type TestResult = Result<(), Box<dyn Error>>;

fn expect_err<T>(
    result: Result<T, PyAuthoringError>,
    context: &str,
) -> Result<PyAuthoringError, Box<dyn Error>> {
    match result {
        Err(err) => Ok(err),
        Ok(_) => Err(format!("expected refusal for `{context}`, got Ok").into()),
    }
}

fn demo_opts() -> EnvelopeOptions {
    let mut scope = BTreeMap::new();
    scope.insert("orgId".to_owned(), "o".to_owned());
    scope.insert("environmentId".to_owned(), "dev".to_owned());
    EnvelopeOptions::new(scope, "test.py")
}

#[test]
fn annotation_resolver_blocks_malicious_deep_and_bad_decimal_annotations() -> TestResult {
    let malicious = [
        "__import__('os').system('echo pwned')",
        "(lambda: 1)()",
        "open('/etc/passwd')",
        "eval('1 + 1')",
        "[x for x in (1, 2)]",
        "().__class__.__bases__[0].__subclasses__()",
        r#"Annotated[Field[Decimal], FieldMeta(default=Decimal("not-a-num"))]"#,
    ];
    for ann in malicious {
        let err = expect_err(validate_annotation_source(ann, None), ann)?;
        assert_eq!(
            err.code(),
            CODE_UNSAFE_ANNOTATION,
            "annotation `{ann}` expected {CODE_UNSAFE_ANNOTATION}, got {err}"
        );
    }

    let mut deep = String::new();
    for _ in 0..70 {
        deep.push_str("Optional[");
    }
    deep.push_str("str");
    for _ in 0..70 {
        deep.push(']');
    }
    let err_deep = expect_err(validate_annotation_source(&deep, None), "deep annotation")?;
    assert_eq!(err_deep.code(), CODE_UNSAFE_ANNOTATION);
    Ok(())
}

#[test]
fn elaboration_blocks_transitive_module_attrs_and_arbitrary_base_classes() -> TestResult {
    let opts = demo_opts();

    let err_typing = expect_err(
        elaborate_source("import typing\nX = typing.sys\n", &opts),
        "typing.sys",
    )?;
    assert_eq!(err_typing.code(), CODE_ELABORATION);
    assert!(err_typing.message().contains("AttributeError"));

    let err_base = expect_err(
        elaborate_source("class Evil(dict):\n    pass\n", &opts),
        "class Evil(dict)",
    )?;
    assert_eq!(err_base.code(), CODE_FORBIDDEN_SYNTAX);
    Ok(())
}

#[test]
fn schema_refuses_interface_lifecycle_raw_ir_and_bad_type_names() -> TestResult {
    let opts = demo_opts();

    let bad_iface_lc = r#"from __future__ import annotations
from enum import StrEnum
from norn_py import Concept, Field, lifecycle, transition

class St(StrEnum):
    A = "a"
    B = "b"

class BadIface(Concept, name="test.BadLifecycleIface", title="Bad", version="0.1.0", interface=True):
    status: Field[St]
    state = lifecycle("status", St, initial=(St.A,), transitions=(transition("go", from_=(St.A,), to=St.B),))
"#;
    let err_lc = expect_err(elaborate_source(bad_iface_lc, &opts), "interface lifecycle")?;
    assert_eq!(err_lc.code(), CODE_INTERFACE_LIFECYCLE);

    let raw_cases: &[(&str, &str)] = &[
        ("from norn_py import Expr\nX = Expr()\n", CODE_RAW_EXPR),
        (
            "from norn_py import Expr\nclass FakeExpr(Expr[bool]):\n    pass\n",
            CODE_RAW_EXPR,
        ),
        (
            "from norn_py import PropertyTypeSpec\nX = PropertyTypeSpec()\n",
            CODE_RAW_IR,
        ),
        (
            "from norn_py import MaskSpec\nX = MaskSpec()\n",
            CODE_RAW_IR,
        ),
        (
            r#"from norn_py import MarkingDecl
M = MarkingDecl(id="pii.x", label="X", severity="high", category="pii", mask={"kind": "redacted"})
"#,
            CODE_RAW_IR,
        ),
        (
            r#"from norn_py import Concept, Field
class BadName(Concept, name="not_pascal", title="Bad", version="0.1.0"):
    id: Field[str]
"#,
            CODE_FLOW_LITERAL,
        ),
        (
            r#"from norn_py import Concept, Field, field_default
class BadIfaceDefault(Concept, name="test.BadIfaceDefault", title="Bad", version="0.1.0", interface=True):
    code: Field[str] = field_default("x")
"#,
            CODE_FLOW_LITERAL,
        ),
    ];

    for &(src, expected_code) in raw_cases {
        let err = expect_err(elaborate_source(src, &opts), src)?;
        assert_eq!(
            err.code(),
            expected_code,
            "snippet `{src}` expected {expected_code}, got {err}"
        );
    }
    Ok(())
}

#[test]
fn schema_refuses_unsupported_link_metadata_and_dual_defaults() -> TestResult {
    let opts = demo_opts();
    let preamble = r#"from __future__ import annotations
from typing import Annotated
from norn_py import Concept, Field, FieldMeta, Link, Marking, field_default, link

class Node(Concept, name="test.LinkNode", title="Node", version="0.1.0"):
    id: Field[str]
"#;

    let cases: &[(&str, &str)] = &[
        (
            "iface_acyclic",
            r#"class Bad(Concept, name="test.Bad", title="Bad", version="0.1.0", interface=True):
    parent: Annotated[Field[Link[Node]], link(Node, acyclic=True)]
"#,
        ),
        (
            "iface_inverse",
            r#"class Bad(Concept, name="test.Bad", title="Bad", version="0.1.0", interface=True):
    parent: Annotated[Field[Link[Node]], link(Node, inverse="children")]
"#,
        ),
        (
            "link_marking",
            r#"class Bad(Concept, name="test.Bad", title="Bad", version="0.1.0"):
    parent: Annotated[Field[Link[Node]], link(Node), Marking("pii.x")]
"#,
        ),
        (
            "link_default",
            r#"class Bad(Concept, name="test.Bad", title="Bad", version="0.1.0"):
    parent: Annotated[Field[Link[Node]], link(Node)] = field_default("x")
"#,
        ),
        (
            "link_field_meta",
            r#"class Bad(Concept, name="test.Bad", title="Bad", version="0.1.0"):
    parent: Annotated[Field[Link[Node]], link(Node), FieldMeta(description="ignored")]
"#,
        ),
        (
            "dup_prop_meta",
            r#"class Bad(Concept, name="test.Bad", title="Bad", version="0.1.0"):
    name: Annotated[Field[str], FieldMeta(description="first", default="a"), FieldMeta()]
"#,
        ),
        (
            "dup_link_meta",
            r#"class Bad(Concept, name="test.Bad", title="Bad", version="0.1.0"):
    parent: Annotated[Field[Link[Node]], link(Node), FieldMeta(description="x"), FieldMeta()]
"#,
        ),
        (
            "dual_default",
            r#"class Bad(Concept, name="test.Bad", title="Bad", version="0.1.0"):
    code: Annotated[Field[str], FieldMeta(default="a")] = field_default("b")
"#,
        ),
        (
            "nullable_required_link",
            r#"class Bad(Concept, name="test.Bad", title="Bad", version="0.1.0"):
    parent: Annotated[Field[Link[Node] | None], link(Node, required=True)]
"#,
        ),
    ];

    for &(label, body) in cases {
        let full = format!("{preamble}\n{body}");
        let err = expect_err(elaborate_source(&full, &opts), label)?;
        assert_eq!(
            err.code(),
            CODE_SCHEMA,
            "case `{label}` expected {CODE_SCHEMA}, got {err}"
        );
    }
    Ok(())
}

#[test]
fn annotated_base_and_metadata_mismatches_refused_with_type_mismatch() -> TestResult {
    let opts = demo_opts();
    let preamble = r#"from __future__ import annotations
from decimal import Decimal
from typing import Annotated, Literal
from norn_py import Concept, Field, Link, Money, Quantity, Rate, UnitRef, decimal_type, integer, link, rate_type

class TargetA(Concept, name="test.TargetA", title="A", version="0.1.0"):
    id: Field[str]

class TargetB(Concept, name="test.TargetB", title="B", version="0.1.0"):
    id: Field[str]
"#;

    let mismatch_cases: &[(&str, &str)] = &[
        ("unit_on_str", r#"x: Annotated[str, UnitRef("ucum", "h")]"#),
        ("dec_lit_on_int", r#"x: Annotated[int, Decimal("2")]"#),
        ("dec_spec_on_int", "x: Annotated[int, decimal_type(2)]"),
        (
            "dec_spec_on_str",
            r#"x: Annotated[str, decimal_type(2, unit=UnitRef("ucum", "h"))]"#,
        ),
        (
            "unit_code_mismatch",
            r#"x: Annotated[Field[Quantity[Literal["h"]]], decimal_type(2, unit=UnitRef("ucum", "d"))]"#,
        ),
        (
            "link_target_mismatch",
            "ref: Annotated[Field[Link[TargetA]], link(TargetB)]",
        ),
        ("bool_as_int", "flag: Annotated[bool, integer()]"),
        (
            "money_currency_mismatch",
            r#"amount: Annotated[Field[Money[Literal["EUR"]]], decimal_type(2, currency="USD")]"#,
        ),
        (
            "money_non_literal",
            r#"amount: Annotated[Field[Money[str]], decimal_type(2, currency="currency")]"#,
        ),
        (
            "rate_unit_mismatch",
            r#"hourly_rate: Annotated[Field[Rate[Literal["USD"], Literal["h"]]], rate_type(2, currency="USD", per_unit=UnitRef("ucum", "d"))]"#,
        ),
        (
            "rate_currency_mismatch",
            r#"hourly_rate: Annotated[Field[Rate[Literal["EUR"], Literal["h"]]], rate_type(2, currency="USD", per_unit=UnitRef("ucum", "h"))]"#,
        ),
    ];

    for &(label, field_line) in mismatch_cases {
        let src = format!(
            "{preamble}\nclass Bad(Concept, name=\"test.Bad\", title=\"Bad\", version=\"0.1.0\"):\n    {field_line}\n"
        );
        let err = expect_err(elaborate_source(&src, &opts), label)?;
        assert_eq!(
            err.code(),
            CODE_TYPE_MISMATCH,
            "mismatch case `{label}` expected {CODE_TYPE_MISMATCH}, got {err}"
        );
    }

    let err_ann_float = expect_err(
        validate_annotation_source("Field[float]", None),
        "standalone float annotation",
    )?;
    assert_eq!(err_ann_float.code(), CODE_FLOAT_LITERAL);

    let float_ann = format!(
        "{preamble}\nclass BadFloat(Concept, name=\"test.BadFloat\", title=\"Bad\", version=\"0.1.0\"):\n    x: Field[float]\n"
    );
    let err_float = expect_err(elaborate_source(&float_ann, &opts), "float annotation")?;
    assert_eq!(err_float.code(), CODE_FORBIDDEN_CALL);
    Ok(())
}

fn sample_pack(id: &str, requires: Vec<&str>, entities: Vec<&str>) -> PackManifest {
    PackManifest {
        id: id.to_owned(),
        version: "0.1.0".to_owned(),
        category: PackCategory::Domain,
        title: id.to_owned(),
        requires: requires.into_iter().map(str::to_owned).collect(),
        entities: entities.into_iter().map(str::to_owned).collect(),
        laws: Vec::new(),
        markings: Vec::new(),
        functions: Vec::new(),
        description: None,
    }
}

#[test]
fn canonical_json_refuses_nfc_dups_proto_depth_and_malformed_numbers() -> TestResult {
    let mut dup = BTreeMap::new();
    dup.insert("e\u{0301}".to_owned(), JsonValue::Number("1".to_owned()));
    dup.insert("\u{00e9}".to_owned(), JsonValue::Number("2".to_owned()));
    let err_dup = expect_err(canonical_norn1_bytes(&JsonValue::Object(dup)), "nfc dup")?;
    assert_eq!(err_dup.code(), CODE_CANONICAL);

    let mut proto = BTreeMap::new();
    proto.insert("__proto__".to_owned(), JsonValue::Bool(true));
    let err_proto = expect_err(canonical_json(&JsonValue::Object(proto)), "__proto__")?;
    assert_eq!(err_proto.code(), CODE_CANONICAL);

    let mut deep = JsonValue::String("leaf".to_owned());
    for _ in 0..140 {
        deep = JsonValue::Array(vec![deep]);
    }
    let err_depth = expect_err(canonical_json(&deep), "json depth > 128")?;
    assert_eq!(err_depth.code(), CODE_CANONICAL);

    for bad_num in [
        "",
        "NaN",
        "Infinity",
        "-Infinity",
        "+1",
        "01",
        "-01",
        "1.",
        ".5",
        "1_000",
        "1e",
        "1e+",
        "123abc",
        "123 ",
    ] {
        let val = JsonValue::Number(bad_num.to_owned());
        let err = expect_err(canonical_json(&val), bad_num)?;
        assert_eq!(
            err.code(),
            CODE_CANONICAL,
            "malformed JSON number `{bad_num}` expected {CODE_CANONICAL}, got {err}"
        );
    }
    Ok(())
}

#[test]
fn pack_composition_refuses_missing_dep_cycle_duplicate_and_conflict() -> TestResult {
    let orphan = sample_pack("norn.pack.loc-eg", vec!["norn.pack.hr"], vec![]);
    assert_eq!(
        expect_err(
            compose_packs(&[orphan], PackConflictPolicy::FailClosed, None),
            "missing dep"
        )?
        .code(),
        CODE_PACK_MISSING_DEP
    );

    let p_a = sample_pack("pack.a", vec!["pack.b"], vec![]);
    let p_b = sample_pack("pack.b", vec!["pack.a"], vec![]);
    assert_eq!(
        expect_err(
            compose_packs(&[p_a, p_b], PackConflictPolicy::FailClosed, None),
            "pack cycle"
        )?
        .code(),
        CODE_PACK_CYCLE
    );

    let base = sample_pack("pack.base", vec![], vec!["hr.Employee"]);
    assert_eq!(
        expect_err(
            compose_packs(
                &[base.clone(), base.clone()],
                PackConflictPolicy::FailClosed,
                None
            ),
            "dup pack"
        )?
        .code(),
        CODE_PACK_DUPLICATE
    );

    let conflict = sample_pack("pack.other", vec![], vec!["hr.Employee"]);
    assert_eq!(
        expect_err(
            compose_packs(&[base, conflict], PackConflictPolicy::FailClosed, None),
            "pack conflict"
        )?
        .code(),
        CODE_PACK_CONFLICT
    );
    Ok(())
}

#[test]
fn deployment_spec_refuses_bad_scope_raw_secret_and_malformed_settings() -> TestResult {
    let base = sample_pack("pack.base", vec![], vec!["hr.Employee"]);

    let bad_scope = DeploymentSpec {
        id: "d".to_owned(),
        org_id: "   ".to_owned(),
        environment_id: "dev".to_owned(),
        packs: vec![base.clone()],
        cell_host: CellHostKind::Rivet,
        worker_endpoints: BTreeMap::new(),
        secret_refs: Vec::new(),
        initial_settings: BTreeMap::new(),
    };
    assert_eq!(
        expect_err(bad_scope.validate(None), "whitespace scope")?.code(),
        CODE_DEPLOYMENT_SCOPE
    );

    let mut secret_settings = BTreeMap::new();
    secret_settings.insert(
        "whatsapp_api_token".to_owned(),
        SettingValue::Str("raw-secret-never-allowed".to_owned()),
    );
    let bad_secret = DeploymentSpec {
        id: "d".to_owned(),
        org_id: "org".to_owned(),
        environment_id: "dev".to_owned(),
        packs: vec![base.clone()],
        cell_host: CellHostKind::Rivet,
        worker_endpoints: BTreeMap::new(),
        secret_refs: Vec::new(),
        initial_settings: secret_settings,
    };
    assert_eq!(
        expect_err(bad_secret.validate(None), "raw secret")?.code(),
        CODE_RAW_SECRET
    );

    let bad_setting_cases: &[(&str, SettingValue, &str)] = &[
        (
            "nan_dec",
            SettingValue::Decimal("NaN".to_owned()),
            CODE_EXPRESSION,
        ),
        (
            "inf_dec",
            SettingValue::Decimal("Infinity".to_owned()),
            CODE_EXPRESSION,
        ),
        (
            "float_int",
            SettingValue::Int("1.5".to_owned()),
            CODE_SCHEMA,
        ),
        ("lead_zero", SettingValue::Int("01".to_owned()), CODE_SCHEMA),
        ("sign_only", SettingValue::Int("-".to_owned()), CODE_SCHEMA),
        ("empty_int", SettingValue::Int(String::new()), CODE_SCHEMA),
    ];
    for (label, val, expected_code) in bad_setting_cases {
        let mut settings = BTreeMap::new();
        settings.insert("limit".to_owned(), val.clone());
        let spec = DeploymentSpec {
            id: "d".to_owned(),
            org_id: "org".to_owned(),
            environment_id: "dev".to_owned(),
            packs: vec![base.clone()],
            cell_host: CellHostKind::Rivet,
            worker_endpoints: BTreeMap::new(),
            secret_refs: Vec::new(),
            initial_settings: settings,
        };
        let err = expect_err(spec.validate(None), label)?;
        assert_eq!(
            err.code(),
            *expected_code,
            "setting case `{label}` expected {expected_code}, got {err}"
        );
    }
    Ok(())
}

#[test]
fn deployment_spec_refuses_invalid_worker_endpoint_urls_without_leaking_credentials() -> TestResult
{
    let base = sample_pack("pack.base", vec![], vec!["hr.Employee"]);
    for bad_url in [
        "http://localhost.evil.example/solve",
        "http://127.0.0.1.evil.example/solve",
        "http://evil.example\\@localhost/solve",
        "http://evil.example\\@127.0.0.1/solve",
        "https://alice:s3cr3t@example.com/solve",
        "http://user:pass@localhost/solve",
        "http://[::1/solve",
        "https://[bogus]/",
        "https://[1::2::3]/solve",
        "http://[0::1]/solve",
        "https://example.com:notaport/solve",
        "https://example.com:99999/solve",
        "https://foo bar/solve",
        "https://foo$bar/solve",
        "https:///no-host",
        "http://example.com/solve",
        "https://example.com/solve?token=abc",
        "https://example.com/solve#frag",
        "https://example.com:0/solve",
        "https://example.com/solve\u{0085}x",
    ] {
        let mut endpoints = BTreeMap::new();
        endpoints.insert("norn.solver.rostering@0.1.0".to_owned(), bad_url.to_owned());
        let bad_ep = DeploymentSpec {
            id: "d".to_owned(),
            org_id: "org".to_owned(),
            environment_id: "dev".to_owned(),
            packs: vec![base.clone()],
            cell_host: CellHostKind::Rivet,
            worker_endpoints: endpoints,
            secret_refs: Vec::new(),
            initial_settings: BTreeMap::new(),
        };
        let err = expect_err(bad_ep.validate(None), bad_url)?;
        assert_eq!(
            err.code(),
            CODE_DEPLOYMENT_ENDPOINT,
            "bad URL expected {CODE_DEPLOYMENT_ENDPOINT}"
        );
        assert!(
            !err.message().contains("s3cr3t") && !err.message().contains("alice"),
            "refusal diagnostic leaked URL credentials: {}",
            err.message()
        );
    }
    Ok(())
}

#[test]
fn eval_refuses_exponential_value_cloning_and_deep_or_cyclic_inheritance() -> TestResult {
    let opts = demo_opts();

    // 1. Exponential binding chain (2^30 nodes if cloned naively) must fail fast.
    let mut bomb = String::from("v0 = [0]\n");
    for idx in 1_usize..=30 {
        let prev = idx.saturating_sub(1);
        let _ = writeln!(bomb, "v{idx} = [v{prev}, v{prev}]");
    }
    let err_bomb = expect_err(elaborate_source(&bomb, &opts), "exponential value bomb")?;
    assert_eq!(err_bomb.code(), CODE_UNBOUNDED);

    // 2. Deep inheritance chain (> MAX_INHERITANCE_DEPTH = 64) must fail closed.
    let mut deep_chain = String::from(
        "from __future__ import annotations\nfrom norn_py import Concept, Field\n\nclass C0(Concept):\n    f0: Field[str]\n",
    );
    for idx in 1_usize..=66 {
        let prev = idx.saturating_sub(1);
        if idx == 66 {
            let _ = writeln!(
                deep_chain,
                "class C{idx}(C{prev}, name=\"test.DeepLeaf\", title=\"Deep\", version=\"0.1.0\"):\n    leaf: Field[str]"
            );
        } else {
            let _ = writeln!(deep_chain, "class C{idx}(C{prev}):\n    pass");
        }
    }
    let err_deep = expect_err(elaborate_source(&deep_chain, &opts), "deep inheritance")?;
    assert_eq!(err_deep.code(), CODE_UNBOUNDED);

    // 3. Rebound self-cycle (`class Loop(Concept)` followed by `class Loop(Loop, ...)`) fails closed.
    let cycle_src = r#"from __future__ import annotations
from norn_py import Concept, Field

class Loop(Concept):
    a: Field[str]

class Loop(Loop, name="test.CyclicLoop", title="Loop", version="0.1.0"):
    b: Field[str]
"#;
    let err_cycle = expect_err(elaborate_source(cycle_src, &opts), "cyclic inheritance")?;
    assert_eq!(err_cycle.code(), CODE_SCHEMA);
    Ok(())
}

#[test]
fn class_chained_lifecycle_assignment_respects_eval_budget() -> TestResult {
    let mut src = String::from(
        r#"from __future__ import annotations
from enum import StrEnum
from norn_py import Concept, Field, lifecycle, transition

class St(StrEnum):
    A = "a"
    B = "b"

class Huge(Concept, name="test.HugeChain", title="Huge", version="0.1.0"):
    status: Field[St]
    "#,
    );
    for idx in 0_usize..200 {
        let _ = write!(src, "lc{idx} = ");
    }
    src.push_str("lifecycle(\"status\", St, initial=(St.A,), transitions=(");
    for idx in 0_usize..150 {
        if idx > 0 {
            src.push_str(", ");
        }
        let _ = write!(src, "transition(\"t{idx}\", from_=(St.A,), to=St.B)");
    }
    src.push_str("))\n");

    let err = expect_err(
        elaborate_source(&src, &demo_opts()),
        "chained lifecycle assignment budget",
    )?;
    assert_eq!(err.code(), CODE_UNBOUNDED);
    Ok(())
}

#[test]
fn standalone_annotation_and_elaboration_enforce_early_budgets() -> TestResult {
    let oversized_ann = "a".repeat(MAX_SOURCE_BYTES.saturating_add(1));
    let err_bytes = expect_err(
        validate_annotation_source(&oversized_ann, None),
        "oversized standalone annotation",
    )?;
    assert_eq!(err_bytes.code(), CODE_UNBOUNDED);

    // High AST node count (> 100,000 nodes) under 1 MiB source bytes.
    let mut wide_ann = String::from("Union[");
    for idx in 0..50_005 {
        if idx > 0 {
            wide_ann.push(',');
        }
        wide_ann.push_str("str");
    }
    wide_ann.push(']');
    let err_nodes = expect_err(
        validate_annotation_source(&wide_ann, None),
        "high-node standalone annotation",
    )?;
    assert_eq!(err_nodes.code(), CODE_UNBOUNDED);

    let tight = SandboxLimits {
        max_source_bytes: 16,
        ..SandboxLimits::default()
    };
    let err_early = expect_err(
        elaborate_source_with_limits("x = '0123456789abcdef0123456789'\n", &demo_opts(), tight),
        "early source byte cap",
    )?;
    assert_eq!(err_early.code(), CODE_UNBOUNDED);
    Ok(())
}

#[cfg(unix)]
#[test]
fn read_regular_file_bounded_refuses_symlinks_dirs_and_oversized_files() -> TestResult {
    use std::fs;
    use std::os::unix::fs::symlink;

    use norn_authoring_py::{CODE_IO, read_regular_file_bounded};

    let tmp_dir =
        std::env::temp_dir().join(format!("norn-authoring-py-io-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir)?;

    let regular = tmp_dir.join("real.py");
    fs::write(&regular, "x = 1\n")?;
    let link_path = tmp_dir.join("link.py");
    symlink(&regular, &link_path)?;
    let oversized = tmp_dir.join("big.py");
    fs::write(&oversized, "0123456789abcdef")?;

    let err_link = expect_err(
        read_regular_file_bounded(&link_path, "link.py", 1024),
        "symlink",
    )?;
    assert_eq!(err_link.code(), CODE_IO);

    let trailing_slash = format!("{}/", link_path.display());
    let err_trailing = expect_err(
        read_regular_file_bounded(std::path::Path::new(&trailing_slash), "link.py/", 1024),
        "symlink with trailing slash",
    )?;
    assert_eq!(err_trailing.code(), CODE_IO);

    let err_dir = expect_err(
        read_regular_file_bounded(&tmp_dir, "dir", 1024),
        "directory",
    )?;
    assert_eq!(err_dir.code(), CODE_IO);

    let err_big = expect_err(
        read_regular_file_bounded(&oversized, "big.py", 8),
        "oversized regular file",
    )?;
    assert_eq!(err_big.code(), CODE_UNBOUNDED);

    let ok_src = read_regular_file_bounded(&regular, "real.py", 1024)?;
    assert_eq!(ok_src, "x = 1\n");

    let _ = fs::remove_dir_all(&tmp_dir);
    Ok(())
}
