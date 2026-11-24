#[path = "../../../registry/core/src/value_types.rs"]
mod value_types;

use std::collections::BTreeSet;

use norn_checker::receipt::{
    ReceiptSources, admission_receipt_sha256, check_artifact_set_with_receipt,
    verify_admission_receipt,
};
use norn_checker::{
    ArtifactValidation, EntityArtifactSetContext, EntityContext, EntityModel, check_artifact_set,
    check_entity_model, check_property_type_defs, derive_type_env,
    entity_instance::EffectiveProperty,
};
use norn_expr::env::TypeEnvVersion;
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, ObjectTypeDef, Property, PropertyConstraints, PropertyId,
    PropertyType, PropertyTypeCatalogRootV2, PropertyTypeDef, PropertyTypeName,
    ResourceLimitProfile, RuntimeValue,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn extract_catalog(pack: ArtifactEnvelope) -> TestResult<Vec<PropertyTypeDef>> {
    pack.validate()
        .map_err(|err| format!("ordinary entity admission: {err:?}"))?;
    let ArtifactPayload::Entity(payload) = pack.payload else {
        return Err("expected entity catalog".into());
    };
    let root: PropertyTypeCatalogRootV2 = serde_json::from_str(&serde_json::to_string(&payload)?)?;
    Ok(root.property_types)
}

fn combined_catalog() -> TestResult<Vec<PropertyTypeDef>> {
    let mut catalog = extract_catalog(value_types::core_pack()?)?;
    catalog.extend(extract_catalog(value_types::std_pack()?)?);
    Ok(catalog)
}

fn property(name: &str, catalog: &[PropertyTypeDef]) -> EffectiveProperty {
    EffectiveProperty::from_declared_property(
        &Property {
            id: PropertyId("value".to_owned()),
            description: None,
            property_type: PropertyType::Ref {
                name: PropertyTypeName(name.to_owned()),
                additivity: norn_ir::PropertyAdditivity::Undeclared {},
                unit_source: Vec::new(),
            },
            constraints: PropertyConstraints::default(),
            default: None,
            markings: Vec::new(),
            computed: None,
            protected: false,
        },
        catalog,
    )
}

fn assert_generated_catalog(relative_path: &str, pack: ArtifactEnvelope) -> TestResult {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    let exact = RuntimeValue::from_json(serde_json::to_value(&pack)?)
        .map_err(|err| format!("exact runtime value: {err:?}"))?;
    let rendered = format!("{}\n", serde_json::to_string_pretty(&exact)?);
    if std::env::var_os("NORN_CHECKER_UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, &rendered)?;
    }
    assert_eq!(
        std::fs::read_to_string(&path)?,
        rendered,
        "{relative_path} drifted from registry/core/src/value_types.rs"
    );
    let defs = extract_catalog(pack)?;
    assert_eq!(check_property_type_defs(&defs, &BTreeSet::new()), []);
    Ok(())
}

#[test]
fn core_and_std_value_types_match_generated_admitted_catalogs() -> TestResult {
    assert_generated_catalog(
        "../../registry/core/artifacts/core.value_types.entity.json",
        value_types::core_pack()?,
    )?;
    assert_generated_catalog(
        "../../registry/core/artifacts/std.value_types.entity.json",
        value_types::std_pack()?,
    )?;
    Ok(())
}

#[test]
fn iso4217_table_matches_canon_records_and_is_strictly_sorted() -> TestResult {
    assert_eq!(value_types::ISO_4217.len(), 176);
    for pair in value_types::ISO_4217.windows(2) {
        let [prev, next] = pair else {
            return Err("expected 2-element window".into());
        };
        assert!(
            prev < next,
            "ISO_4217 must be strictly sorted and unique: {prev:?} >= {next:?}",
        );
    }
    for code in value_types::ISO_4217 {
        assert_eq!(code.len(), 3, "currency code {code} must be 3 chars");
        assert!(
            code.bytes().all(|b| b.is_ascii_uppercase()),
            "currency code {code} must be ASCII uppercase"
        );
        assert!(
            code != "XTS" && code != "XXX",
            "ISO_4217 must exclude testing/no-currency sentinel {code}"
        );
    }

    let canon_records = include_str!("../../../registry/packs/canon/records.ts");
    let (_, rest) = canon_records
        .split_once("export const ISO_4217 = [")
        .ok_or("ISO_4217 export in registry/packs/canon/records.ts")?;
    let (body, _) = rest
        .split_once("] as const")
        .ok_or("closing `] as const` for ISO_4217")?;
    let canon_codes: Vec<&str> = body.split('"').skip(1).step_by(2).collect();
    assert_eq!(
        canon_codes.as_slice(),
        value_types::ISO_4217.as_slice(),
        "ISO_4217 in registry/core/src/value_types.rs drifted from registry/packs/canon/records.ts"
    );
    Ok(())
}

#[test]
fn materialized_value_types_validate_scalars_and_iso4217_codes() -> TestResult {
    let catalog = combined_catalog()?;

    let currency = property("iso4217.currency_code", &catalog);
    for valid in ["USD", "EUR", "EGP", "JPY", "GBP", "XAU"] {
        assert!(
            currency
                .check_runtime_value(&RuntimeValue::from(valid))
                .is_ok(),
            "active ISO 4217 code {valid} should be admitted"
        );
    }
    for invalid in ["", "US", "USDD", "usd", "XXX", "XTS", "123"] {
        assert!(
            currency
                .check_runtime_value(&RuntimeValue::from(invalid))
                .is_err(),
            "non-active or malformed currency code {invalid} must be refused"
        );
    }

    let unit = property("ucum.unit_code", &catalog);
    assert!(unit.check_runtime_value(&RuntimeValue::from("kg")).is_ok());
    assert!(
        unit.check_runtime_value(&RuntimeValue::from("{each}"))
            .is_ok()
    );
    assert!(unit.check_runtime_value(&RuntimeValue::from("")).is_err());
    assert!(
        unit.check_runtime_value(&RuntimeValue::from("k g"))
            .is_err()
    );

    let quantity = property("core.quantity", &catalog);
    let valid_qty = RuntimeValue::from_json(json!(12.345_678))
        .map_err(|err| format!("valid decimal: {err:?}"))?;
    assert!(quantity.check_runtime_value(&valid_qty).is_ok());
    let invalid_qty = RuntimeValue::from_json(json!(12.345_678_9))
        .map_err(|err| format!("invalid decimal: {err:?}"))?;
    assert!(quantity.check_runtime_value(&invalid_qty).is_err());

    let date = property("iso8601.date", &catalog);
    assert!(
        date.check_runtime_value(&RuntimeValue::from("2026-11-23"))
            .is_ok()
    );
    assert!(
        date.check_runtime_value(&RuntimeValue::from("2026-11-23T00:00:00Z"))
            .is_err()
    );
    Ok(())
}

#[test]
fn core_rea_interfaces_admit_with_materialized_value_types_and_pinned_dependencies() -> TestResult {
    let base = vec![value_types::core_pack()?, value_types::std_pack()?];
    let artifacts: Vec<ArtifactEnvelope> = [
        include_str!("../../../registry/core/artifacts/core.Agent.interface.json"),
        include_str!("../../../registry/core/artifacts/core.AgentRole.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Resource.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Commitment.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Hold.interface.json"),
        include_str!("../../../registry/core/artifacts/core.EconomicEvent.interface.json"),
        include_str!("../../../registry/core/artifacts/core.LedgerEffect.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Claim.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Process.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Conversion.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Exchange.interface.json"),
    ]
    .into_iter()
    .map(serde_json::from_str)
    .collect::<Result<Vec<_>, _>>()?;

    let registry = BTreeSet::new();
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
    );
    assert_eq!(
        findings,
        [],
        "REA interfaces must admit cleanly over materialized core/std value types"
    );
    Ok(())
}

#[test]
fn unbodied_and_non_scalar_refs_remain_fail_closed_in_invariants() -> TestResult {
    let catalog = combined_catalog()?;
    let registry = BTreeSet::from([PropertyTypeName("external.unbodied_code".to_owned())]);

    let unbodied: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.UnbodiedInvariant",
        "properties": [
            { "id": "ext", "type": { "kind": "ref", "name": "external.unbodied_code" } }
        ],
        "invariants": [
            { "id": "reads_ext", "expression": "self.ext != ''", "message": "must be non-empty" }
        ]
    }))?;

    let findings = check_entity_model(
        &EntityModel {
            object_types: &[unbodied],
            ..EntityModel::EMPTY
        },
        &EntityContext {
            catalog: &catalog,
            registry: &registry,
            base: EntityModel::EMPTY,
            tenant: None,
        },
    );
    assert!(
        findings.iter().any(|d| d.code.as_str()
            == "object_type.validation.invalid_invariant_expression"
            && d.message.contains("opaque")),
        "unbodied registry ref must remain fail-closed (opaque): {findings:?}"
    );

    let non_scalar: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.NonScalarInvariant",
        "properties": [
            { "id": "health", "type": { "kind": "ref", "name": "core.health_information" } }
        ],
        "invariants": [
            { "id": "reads_health", "expression": "self.health != null", "message": "must exist" }
        ]
    }))?;

    let findings = check_entity_model(
        &EntityModel {
            object_types: &[non_scalar],
            ..EntityModel::EMPTY
        },
        &EntityContext {
            catalog: &catalog,
            registry: &BTreeSet::new(),
            base: EntityModel::EMPTY,
            tenant: None,
        },
    );
    assert!(
        findings.iter().any(|d| d.code.as_str()
            == "object_type.validation.invalid_invariant_expression"
            && d.message.contains("opaque")),
        "non-scalar json-ext ref must remain fail-closed (opaque): {findings:?}"
    );
    Ok(())
}

#[test]
fn single_object_invariant_scope_still_refuses_cross_object_link_reads() -> TestResult {
    let catalog = combined_catalog()?;
    let parent: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.Owner",
        "properties": [
            { "id": "code", "type": { "kind": "ref", "name": "core.code" } }
        ]
    }))?;
    let child: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.Item",
        "properties": [
            { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } }
        ],
        "links": [
            { "id": "owner", "target": "test.Owner", "cardinality": { "to": "one", "from": "many", "required": true } }
        ],
        "invariants": [
            { "id": "cross_owner", "expression": "self.quantity > 0 and self.owner.code != ''", "message": "cross-object must refuse" }
        ]
    }))?;

    let findings = check_entity_model(
        &EntityModel {
            object_types: &[parent, child],
            ..EntityModel::EMPTY
        },
        &EntityContext {
            catalog: &catalog,
            registry: &BTreeSet::new(),
            base: EntityModel::EMPTY,
            tenant: None,
        },
    );
    assert!(
        findings.iter().any(|d| d.code.as_str()
            == "object_type.validation.invalid_invariant_expression"
            && d.message.contains("reads a link or inverse role")),
        "cross-object invariant must remain refused: {findings:?}"
    );
    Ok(())
}

fn interface_envelope(
    artifact_id: &str,
    name: &str,
    interface: &serde_json::Value,
) -> TestResult<ArtifactEnvelope> {
    Ok(serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": "entity",
        "namespace": "toy",
        "name": name,
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "authoring_sdk", "sourcePath": "test.ts" },
        "payload": {
            "entityVersion": "2.0",
            "interface": interface
        }
    }))?)
}

fn receipt_sources_for(artifacts: &[ArtifactEnvelope]) -> TestResult<ReceiptSources> {
    let values = artifacts
        .iter()
        .map(|artifact| {
            let wire = serde_json::to_value(artifact)?;
            RuntimeValue::from_json(wire).map_err(|err| format!("runtime value: {err:?}").into())
        })
        .collect::<TestResult<Vec<_>>>()?;
    ReceiptSources::new(&values, &[]).map_err(|err| format!("receipt sources: {err:?}").into())
}

#[test]
fn retained_env_v2_decimal_binding_receipt_verifies_and_relabelling_to_v3_refuses() -> TestResult {
    let conversion = interface_envelope(
        "core.Conversion.interface",
        "Conversion",
        &json!({
            "name": "core.Conversion",
            "properties": [
                { "id": "input", "type": { "kind": "decimal-binding", "group": "conversion" } },
                { "id": "output", "type": { "kind": "decimal-binding", "group": "conversion" } },
                { "id": "loss", "type": { "kind": "decimal-binding", "group": "conversion" } }
            ],
            "invariants": [
                {
                    "id": "conserve",
                    "expression": "self.input == self.output + self.loss",
                    "message": "conversion balances"
                }
            ]
        }),
    )?;
    let artifacts = [conversion];
    let limits = ResourceLimitProfile::norn_limits_v1();
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };

    let checked =
        check_artifact_set_with_receipt(&receipt_sources_for(&artifacts)?, &context, &limits)?;
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let fresh_env = checked
        .receipt
        .type_env
        .as_ref()
        .ok_or("fresh receipt must carry typeEnv")?;
    assert_eq!(fresh_env.version, 3);
    assert_eq!(fresh_env.version, norn_expr::TYPE_ENV_VERSION);
    let fresh_body = String::from_utf8(checked.receipt.canonical_json()?)?;
    let fresh_hash = admission_receipt_sha256(&fresh_body)?;
    verify_admission_receipt(&fresh_body, &fresh_hash, &artifacts, &[], &limits)?;

    // Historical main-era norn.expr.env/2 receipt (without decimalBindingOwners)
    // must still verify cleanly against the exact same DecimalBinding artifact set.
    let derived =
        derive_type_env(&artifacts, &[], &limits).map_err(|err| format!("derive env: {err:?}"))?;
    let v2_bytes = String::from_utf8(derived.canonical_json_at_version(TypeEnvVersion::V2)?)?;
    let v3_bytes = String::from_utf8(derived.canonical_json_at_version(TypeEnvVersion::V3)?)?;
    assert!(!v2_bytes.contains("decimalBindingOwners"));
    assert!(v3_bytes.contains("decimalBindingOwners"));
    let v2_ref = derived.reference_at_version(TypeEnvVersion::V2)?;
    assert_eq!(v2_ref.version, 2);
    assert_ne!(v2_ref.sha256, fresh_env.sha256);

    let mut retained_v2 = checked.receipt;
    retained_v2.type_env = Some(v2_ref);
    let retained_v2_body = String::from_utf8(retained_v2.canonical_json()?)?;
    let retained_v2_hash = admission_receipt_sha256(&retained_v2_body)?;
    verify_admission_receipt(
        &retained_v2_body,
        &retained_v2_hash,
        &artifacts,
        &[],
        &limits,
    )?;

    // Relabelling a historical /2 hash as /3 must refuse.
    let mut relabelled: serde_json::Value = serde_json::from_str(&retained_v2_body)?;
    if let Some(type_env) = relabelled
        .get_mut("typeEnv")
        .and_then(serde_json::Value::as_object_mut)
    {
        type_env.insert("version".to_owned(), json!(3));
    }
    let relabelled_body = String::from_utf8(norn_canonical::canonical_json_of(&relabelled)?)?;
    let relabelled_hash = admission_receipt_sha256(&relabelled_body)?;
    assert!(
        verify_admission_receipt(&relabelled_body, &relabelled_hash, &artifacts, &[], &limits)
            .is_err(),
        "relabelled /2 hash claiming /3 must refuse"
    );
    Ok(())
}

#[test]
fn new_env_v3_receipts_distinguish_inherited_vs_redeclared_decimal_binding_owners() -> TestResult {
    let limits = ResourceLimitProfile::norn_limits_v1();
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let parent = interface_envelope(
        "toy.Parent.interface",
        "Parent",
        &json!({
            "name": "toy.Parent",
            "properties": [
                { "id": "x", "type": { "kind": "decimal-binding", "group": "g" } },
                { "id": "y", "type": { "kind": "decimal-binding", "group": "g" } }
            ]
        }),
    )?;
    let child_inherited = interface_envelope(
        "toy.Child.interface",
        "Child",
        &json!({
            "name": "toy.Child",
            "extends": ["toy.Parent"],
            "properties": []
        }),
    )?;
    let child_redeclared = interface_envelope(
        "toy.Child.interface",
        "Child",
        &json!({
            "name": "toy.Child",
            "extends": ["toy.Parent"],
            "properties": [
                { "id": "x", "type": { "kind": "decimal-binding", "group": "g" } }
            ]
        }),
    )?;

    let set_inherited = [parent.clone(), child_inherited];
    let set_redeclared = [parent, child_redeclared];
    let inherited_checked =
        check_artifact_set_with_receipt(&receipt_sources_for(&set_inherited)?, &context, &limits)?;
    let redeclared_checked =
        check_artifact_set_with_receipt(&receipt_sources_for(&set_redeclared)?, &context, &limits)?;
    assert!(inherited_checked.receipt.admitted);
    assert!(redeclared_checked.receipt.admitted);

    let inherited_ref = inherited_checked
        .receipt
        .type_env
        .as_ref()
        .ok_or("inherited receipt missing typeEnv")?;
    let redeclared_ref = redeclared_checked
        .receipt
        .type_env
        .as_ref()
        .ok_or("redeclared receipt missing typeEnv")?;
    assert_eq!(inherited_ref.version, 3);
    assert_eq!(redeclared_ref.version, 3);
    assert_ne!(
        inherited_ref.sha256, redeclared_ref.sha256,
        "norn.expr.env/3 receipts must distinguish inherited vs redeclared DecimalBinding owners"
    );

    let inherited_v2 = derive_type_env(&set_inherited, &[], &limits)
        .map_err(|err| format!("derive inherited: {err:?}"))?
        .reference_at_version(TypeEnvVersion::V2)?;
    let redeclared_v2 = derive_type_env(&set_redeclared, &[], &limits)
        .map_err(|err| format!("derive redeclared: {err:?}"))?
        .reference_at_version(TypeEnvVersion::V2)?;
    assert_eq!(
        inherited_v2, redeclared_v2,
        "historical /2 projection stays unchanged without decimalBindingOwners"
    );

    for (receipt, set) in [
        (&inherited_checked.receipt, set_inherited.as_slice()),
        (&redeclared_checked.receipt, set_redeclared.as_slice()),
    ] {
        let body = String::from_utf8(receipt.canonical_json()?)?;
        let hash = admission_receipt_sha256(&body)?;
        verify_admission_receipt(&body, &hash, set, &[], &limits)?;
    }
    Ok(())
}
