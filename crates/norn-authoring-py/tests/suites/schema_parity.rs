//! Golden parity integration tests for Slice 2 native Python-to-Canonical-IR
//! elaboration (`hr_pilot.py`, checked-in `core.Agent` & `core.TimeInterval`
//! goldens, `Norn1` Unicode ordering/NFC conformance, object invariants,
//! `Money`/`Rate`, and `PackManifest`/`DeploymentSpec`).

use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

use norn_authoring_py::{
    CellHostKind, DeploymentSpec, EnvelopeOptions, FnRef, JsonValue, MaskSpec, PackCategory,
    PackConflictPolicy, PackManifest, SecretRef, SettingValue, canonical_json,
    canonical_norn1_bytes, canonical_norn1_sha256, compose_packs, elaborate_file, elaborate_source,
};

type TestResult = Result<(), Box<dyn Error>>;

fn demo_options(source_path: &str) -> EnvelopeOptions {
    let mut scope = BTreeMap::new();
    scope.insert("environmentId".to_owned(), "dev".to_owned());
    scope.insert("orgId".to_owned(), "org_demo".to_owned());
    EnvelopeOptions::new(scope, source_path)
}

fn obj_field<'a>(val: &'a JsonValue, key: &str) -> Result<&'a JsonValue, Box<dyn Error>> {
    let JsonValue::Object(map) = val else {
        return Err(format!("expected JSON object when accessing '{key}'").into());
    };
    map.get(key)
        .ok_or_else(|| format!("missing JSON key '{key}'").into())
}

fn as_str<'a>(val: &'a JsonValue, label: &str) -> Result<&'a str, Box<dyn Error>> {
    let JsonValue::String(s) = val else {
        return Err(format!("expected JSON string for '{label}'").into());
    };
    Ok(s.as_str())
}

#[test]
fn hr_pilot_elaborates_byte_identically_to_python_reference() -> TestResult {
    let hr_pilot = include_str!("../../../../tools/norn-py/examples/hr_pilot.py");
    let opts = demo_options("tools/norn-py/examples/hr_pilot.py");

    let run1 = elaborate_source(hr_pilot, &opts)?;
    let run2 = elaborate_source(hr_pilot, &opts)?;
    assert_eq!(run1, run2, "elaboration must be 100% deterministic");

    assert_eq!(run1.artifacts.len(), 6);
    assert_eq!(run1.norn1_bytes.len(), 5995);
    assert_eq!(
        run1.norn1_sha256,
        "a05807beea0d213d81f8ff1dd06184117642ac170dd93702e33c6529e304e858"
    );

    let expected_pins: &[(&str, &str)] = &[
        (
            "hr.privacy.markings",
            "67421ddff02c8f809cda10ea15d58b795e40d81399b0778998b5a877942bfd82",
        ),
        (
            "hr_pilot.TimeSpan.interface",
            "1c8c91ae123c3017ccc9b1387f81101fe0574207112f439e2fe226ebe2f4fef6",
        ),
        (
            "hr.Employee.entity",
            "4613c65aacb98a9988106f61ee714ef74a68cca3f9a80fed5026dfc269212deb",
        ),
        (
            "hr.LeaveRequest.entity",
            "3c4baf60146966293bb7115cd492aa8b5682030a4dd1b365af34342ff1017859",
        ),
        (
            "hr.Shift.entity",
            "98a4d655bb922df01f443b5da60f2b740dbeebe2a157bda887c1a194a4d78546",
        ),
        (
            "hr.shift.valid_window_and_hours.law",
            "790756166f48bdc996bd7aa6e4d434db38d07b17d8b3434b3345b076e1af8ef8",
        ),
    ];

    for (idx, &(expected_id, expected_sha)) in expected_pins.iter().enumerate() {
        let artifact = run1
            .artifacts
            .get(idx)
            .ok_or_else(|| format!("missing artifact {idx}"))?;
        let actual_id = as_str(obj_field(artifact, "artifactId")?, "artifactId")?;
        assert_eq!(actual_id, expected_id);
        assert_eq!(
            canonical_norn1_sha256(artifact)?,
            expected_sha,
            "Norn1 SHA-256 mismatch for {expected_id}"
        );
    }

    assert!(run1.source_map.contains_key("Employee"));
    assert!(run1.source_map.contains_key("HR_MARKINGS"));
    assert!(run1.source_map.contains_key("SOLVE_ROSTER"));
    assert!(run1.source_map.contains_key("shift_bounds"));

    // Verify bounded file-based elaboration matches source-based elaboration identically.
    let file_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/norn-py/examples/hr_pilot.py");
    let from_file = elaborate_file(&file_path, &opts)?;
    assert_eq!(from_file.canonical_text, run1.canonical_text);
    assert_eq!(from_file.norn1_sha256, run1.norn1_sha256);
    Ok(())
}

#[test]
fn core_agent_and_time_interval_match_checked_in_goldens_byte_for_byte() -> TestResult {
    let agent_src = r#"from __future__ import annotations

from enum import StrEnum
from typing import Annotated

from norn_py import Concept, DependencyPin, Field, ref_type


class AgentKind(StrEnum):
    PERSON = "person"
    ORGANIZATION = "organization"
    SYSTEM = "system"


class Agent(
    Concept,
    name="core.Agent",
    title="Agent",
    version="0.2.0",
    interface=True,
    role="thing",
    dependencies=(
        DependencyPin(
            artifact_id="core.value_types.entity",
            version="0.2.0",
            kind="entity",
        ),
    ),
    description=(
        "An actor that holds resources, takes part in economic events and "
        "makes commitments: a person, an organisation or a system (REA agent)."
    ),
):
    display_name: Annotated[Field[str], ref_type("core.display_name")]
    kind: Field[AgentKind]
"#;

    let agent_opts = EnvelopeOptions::new(BTreeMap::new(), "registry/core/src/rea.ts")
        .with_registry_package_path("registry/core");
    let agent_res = elaborate_source(agent_src, &agent_opts)?;
    assert_eq!(agent_res.artifacts.len(), 1);
    let agent_art = agent_res
        .artifacts
        .first()
        .ok_or("missing Agent artifact")?;
    let expected_agent =
        include_str!("../../../../registry/core/artifacts/core.Agent.interface.json");
    assert_eq!(canonical_json(agent_art)?, expected_agent);
    let parsed_agent_golden = norn_canonical::parse(expected_agent)?;
    assert_eq!(
        canonical_norn1_sha256(agent_art)?,
        canonical_norn1_sha256(&parsed_agent_golden)?
    );

    let ti_src = r#"from __future__ import annotations

from typing import Annotated

from norn_py import Concept, Expr, Field, Sym, Timestamp, invariant, ref_type, timestamp_before


class TimeInterval(
    Concept,
    name="core.TimeInterval",
    title="Time interval",
    version="0.1.0",
    interface=True,
    role="happening",
    description=(
        "A proper half-open temporal interval [starts_at, ends_at) in UTC "
        "with an optional canonical IANA timezone label."
    ),
):
    starts_at: Annotated[Field[Timestamp], ref_type("core.time.instant")]
    ends_at: Annotated[Field[Timestamp], ref_type("core.time.instant")]
    timezone: Annotated[Field[str | None], ref_type("core.time.iana_timezone")]

    @invariant("proper_interval", message="starts_at must be strictly before ends_at")
    def _proper_interval(s: Sym[TimeInterval]) -> Expr[bool]:
        return timestamp_before(s.starts_at, s.ends_at)
"#;

    let ti_opts = EnvelopeOptions::new(BTreeMap::new(), "registry/core/src/time.ts")
        .with_registry_package_path("registry/core");
    let ti_res = elaborate_source(ti_src, &ti_opts)?;
    assert_eq!(ti_res.artifacts.len(), 1);
    let ti_art = ti_res
        .artifacts
        .first()
        .ok_or("missing TimeInterval artifact")?;
    let expected_ti =
        include_str!("../../../../registry/core/artifacts/core.TimeInterval.interface.json");
    assert_eq!(canonical_json(ti_art)?, expected_ti);
    let parsed_ti_golden = norn_canonical::parse(expected_ti)?;
    assert_eq!(
        canonical_norn1_bytes(ti_art)?,
        canonical_norn1_bytes(&parsed_ti_golden)?
    );
    assert_eq!(
        canonical_norn1_sha256(ti_art)?,
        canonical_norn1_sha256(&parsed_ti_golden)?
    );
    Ok(())
}

#[test]
fn object_invariants_money_rate_defaults_and_anonymous_subclasses() -> TestResult {
    let src = r#"from __future__ import annotations

from decimal import Decimal
from typing import Annotated, Literal

from norn_py import (
    Concept,
    Expr,
    Field,
    FieldMeta,
    Money,
    Rate,
    Sym,
    UnitRef,
    decimal_type,
    field_default,
    invariant,
    rate_type,
)


class Order(Concept, name="sales.TestOrder", title="Test order", version="0.1.0"):
    id: Field[str]
    currency: Field[str]
    count: Field[int] = field_default(1)
    active: Field[bool] = field_default(True)
    total: Annotated[Field[Decimal], decimal_type(2)]
    salary: Annotated[Field[Money[Literal["currency"]]], decimal_type(2, currency="currency")]
    hourly_rate: Annotated[
        Field[Rate[Literal["currency"], Literal["h"]]],
        rate_type(2, currency="currency", per_unit=UnitRef("ucum", "h")),
        FieldMeta(description="Hourly billing rate"),
    ]

    @invariant("non_negative_total", message="total must be non-negative")
    def _non_neg(s: Sym[Order]) -> Expr[bool]:
        return s.total >= Decimal("0.00")

    @invariant("large_order_warn", message="order unusually large", classification="quality")
    def _warn_large(s: Sym[Order]) -> Expr[bool]:
        return s.total <= Decimal("100000.00")


class AnonymousHelper(Order):
    pass
"#;

    let res = elaborate_source(src, &demo_options("order.py"))?;
    assert_eq!(
        res.artifacts.len(),
        1,
        "anonymous Concept subclass must not emit a duplicate artifact"
    );
    let art = res.artifacts.first().ok_or("missing Order artifact")?;
    let payload = obj_field(art, "payload")?;
    let obj = obj_field(payload, "objectType")?;

    let JsonValue::Array(invariants) = obj_field(obj, "invariants")? else {
        return Err("expected invariants array".into());
    };
    assert_eq!(invariants.len(), 2);
    let inv0 = invariants.first().ok_or("missing inv0")?;
    assert_eq!(as_str(obj_field(inv0, "id")?, "id")?, "non_negative_total");
    assert_eq!(
        as_str(obj_field(inv0, "expression")?, "expression")?,
        "self.total >= 0.00"
    );
    let JsonValue::Object(inv0_map) = inv0 else {
        return Err("expected inv0 object".into());
    };
    assert!(!inv0_map.contains_key("classification"));

    let inv1 = invariants.get(1).ok_or("missing inv1")?;
    assert_eq!(
        as_str(obj_field(inv1, "classification")?, "classification")?,
        "quality"
    );
    assert_eq!(
        as_str(obj_field(inv1, "expression")?, "expression")?,
        "self.total <= 100000.00"
    );

    let JsonValue::Array(props) = obj_field(obj, "properties")? else {
        return Err("expected properties array".into());
    };
    let count_prop = props.get(2).ok_or("missing count prop")?;
    assert_eq!(
        as_str(obj_field(count_prop, "default")?, "count default")?,
        "1"
    );
    let active_prop = props.get(3).ok_or("missing active prop")?;
    assert_eq!(obj_field(active_prop, "default")?, &JsonValue::Bool(true));
    let rate_prop = props.get(6).ok_or("missing hourly_rate prop")?;
    assert_eq!(
        as_str(obj_field(rate_prop, "description")?, "rate description")?,
        "Hourly billing rate"
    );
    Ok(())
}

#[test]
fn canonical_norn1_nfc_utf16_ordering_and_control_escaping_parity() -> TestResult {
    // 1. NFD -> NFC + C0 control escape vector from crates/norn-canonical & test_norn_py.py
    let mut nested = BTreeMap::new();
    nested.insert("b".to_owned(), JsonValue::Number("1".to_owned()));
    nested.insert("a".to_owned(), JsonValue::Number("0".to_owned()));

    let mut rust_vec = BTreeMap::new();
    rust_vec.insert("z".to_owned(), JsonValue::Number("0".to_owned()));
    rust_vec.insert("e".to_owned(), JsonValue::Number("123".to_owned()));
    rust_vec.insert(
        "decomposed".to_owned(),
        JsonValue::String("e\u{0301}".to_owned()),
    );
    rust_vec.insert(
        "control".to_owned(),
        JsonValue::String("\u{0000}\n\t".to_owned()),
    );
    rust_vec.insert("nested".to_owned(), JsonValue::Object(nested));

    let expected_bytes =
        "{\"control\":\"\\u0000\\n\\t\",\"decomposed\":\"\u{00e9}\",\"e\":123,\"nested\":{\"a\":0,\"b\":1},\"z\":0}"
            .as_bytes();
    assert_eq!(
        canonical_norn1_bytes(&JsonValue::Object(rust_vec))?,
        expected_bytes
    );

    // 2. UTF-16 (canonical_json, TS parity) vs Unicode code-point (canonical_norn1_bytes, Rust parity)
    // U+E000 (0xE000) vs U+10000 (UTF-16 surrogate 0xD800 0xDC00).
    let mut pair = BTreeMap::new();
    pair.insert("\u{10000}".to_owned(), JsonValue::String("supp".to_owned()));
    pair.insert("\u{e000}".to_owned(), JsonValue::String("bmp".to_owned()));
    let pair_val = JsonValue::Object(pair);

    assert_eq!(
        canonical_norn1_bytes(&pair_val)?,
        "{\"\u{e000}\":\"bmp\",\"\u{10000}\":\"supp\"}".as_bytes()
    );
    let pretty = canonical_json(&pair_val)?;
    let pos_supp = pretty.find('\u{10000}').ok_or("missing U+10000")?;
    let pos_bmp = pretty.find('\u{e000}').ok_or("missing U+E000")?;
    assert!(
        pos_supp < pos_bmp,
        "UTF-16 canonical_json must sort U+10000 before U+E000"
    );

    // 3. NFD vs NFC identical bytes & SHA-256
    let mut m1 = BTreeMap::new();
    m1.insert("b".to_owned(), JsonValue::Number("1".to_owned()));
    m1.insert("a".to_owned(), JsonValue::String("e\u{0301}".to_owned()));
    let mut m2 = BTreeMap::new();
    m2.insert("a".to_owned(), JsonValue::String("\u{00e9}".to_owned()));
    m2.insert("b".to_owned(), JsonValue::Number("1".to_owned()));
    assert_eq!(
        canonical_norn1_bytes(&JsonValue::Object(m1.clone()))?,
        canonical_norn1_bytes(&JsonValue::Object(m2.clone()))?
    );
    assert_eq!(
        canonical_norn1_sha256(&JsonValue::Object(m1))?,
        canonical_norn1_sha256(&JsonValue::Object(m2))?
    );
    Ok(())
}

#[test]
fn pack_composition_deployment_spec_and_mask_spec_parity() -> TestResult {
    let hr_pack = PackManifest {
        id: "norn.pack.hr".to_owned(),
        version: "0.1.0".to_owned(),
        category: PackCategory::Domain,
        title: "HR Pack".to_owned(),
        requires: Vec::new(),
        entities: vec!["hr.TestPerson".to_owned()],
        laws: Vec::new(),
        markings: Vec::new(),
        functions: vec![FnRef {
            id: "norn.solver.rostering".to_owned(),
            version: "0.1.0".to_owned(),
            input_type: "SolveRequest".to_owned(),
            output_type: "SolveResponse".to_owned(),
        }],
        description: None,
    };
    let loc_eg = PackManifest {
        id: "norn.pack.loc-eg".to_owned(),
        version: "0.1.0".to_owned(),
        category: PackCategory::Localization,
        title: "Egypt Localization".to_owned(),
        requires: vec!["norn.pack.hr".to_owned()],
        entities: vec!["loc_eg.TestProfile".to_owned()],
        laws: Vec::new(),
        markings: Vec::new(),
        functions: Vec::new(),
        description: None,
    };

    let composed = compose_packs(
        &[hr_pack.clone(), loc_eg.clone()],
        PackConflictPolicy::FailClosed,
        None,
    )?;
    assert_eq!(
        obj_field(&composed, "entities")?,
        &JsonValue::Array(vec![
            JsonValue::String("hr.TestPerson".to_owned()),
            JsonValue::String("loc_eg.TestProfile".to_owned()),
        ])
    );
    assert_eq!(
        obj_field(&composed, "functions")?,
        &JsonValue::Array(vec![JsonValue::String(
            "norn.solver.rostering@0.1.0".to_owned()
        )])
    );

    let mut endpoints = BTreeMap::new();
    endpoints.insert(
        "norn.solver.rostering@0.1.0".to_owned(),
        "https://solver.example.internal/solve".to_owned(),
    );
    let mut settings = BTreeMap::new();
    settings.insert(
        "weekly_overtime_budget_egp".to_owned(),
        SettingValue::decimal("1500.00", None)?,
    );
    settings.insert("avoid_split_shifts".to_owned(), SettingValue::Bool(true));

    let dep = DeploymentSpec {
        id: "dep_demo_cairo".to_owned(),
        org_id: "org_demo".to_owned(),
        environment_id: "prod".to_owned(),
        packs: vec![hr_pack, loc_eg],
        cell_host: CellHostKind::Rivet,
        worker_endpoints: endpoints,
        secret_refs: vec![SecretRef {
            name: "WHATSAPP_VERIFY_TOKEN".to_owned(),
            description: None,
        }],
        initial_settings: settings,
    };
    let wire = dep.to_ir(None)?;
    let wire_settings = obj_field(&wire, "initialSettings")?;
    assert_eq!(
        as_str(
            obj_field(wire_settings, "weekly_overtime_budget_egp")?,
            "budget"
        )?,
        "1500.00"
    );
    assert_eq!(
        obj_field(wire_settings, "avoid_split_shifts")?,
        &JsonValue::Bool(true)
    );
    assert!(!canonical_json(&wire)?.is_empty());

    assert_eq!(
        canonical_norn1_bytes(&MaskSpec::LastN(4).to_ir())?,
        b"{\"count\":4,\"kind\":\"last-n\"}"
    );
    Ok(())
}
