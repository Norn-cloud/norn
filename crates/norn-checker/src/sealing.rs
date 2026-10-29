//! Pure, deterministic sealing rules for customization overlays, moved verbatim
//! from `norn-ir` (T1-CHK-1, #2524). The sealing IR types stay in
//! `norn_ir::sealing`.
use norn_ir::sealing::{
    CustomizationOverlayOperation, ExtensionPointDefinition, SealAnnotation, SealError, SealInput,
    SealOutcome, SealValidationResult,
};
use norn_ir::{Diagnostic, DiagnosticRemediation, DiagnosticSeverity, JsonValue};
use std::collections::BTreeMap;

fn seal_validation_result(diagnostics: Vec<Diagnostic>) -> SealValidationResult {
    SealValidationResult {
        ok: diagnostics.is_empty(),
        diagnostics,
    }
}

/// Runs a seal pass without IO. Policy denials are reported as Diagnostics.
pub fn seal(input: SealInput) -> Result<SealOutcome, SealError> {
    Ok(match input {
        SealInput::CustomizationOverlay {
            base_seals,
            extension_points,
            operations,
        } => validate_customization_overlay(&base_seals, &extension_points, &operations),
        SealInput::RuntimeCapabilityBackstop {
            requested_node_id,
            seals,
            capabilities,
        } => validate_runtime_capability_backstop(&requested_node_id, &seals, &capabilities),
        SealInput::SealUpgrade {
            previous_base_seals,
            upgraded_base_seals,
            extension_points,
            overlay_operations,
        } => validate_seal_upgrade(
            &previous_base_seals,
            &upgraded_base_seals,
            &extension_points,
            &overlay_operations,
        ),
    })
}

pub fn validate_customization_overlay(
    base_seals: &[SealAnnotation],
    extension_points: &[ExtensionPointDefinition],
    operations: &[CustomizationOverlayOperation],
) -> SealValidationResult {
    let mut diagnostics = Vec::new();
    let seals = index_seals(base_seals, "baseSeals", &mut diagnostics);
    let points = index_extension_points(extension_points, "extensionPoints", &mut diagnostics);
    validate_overlay_operations(&seals, &points, operations, &mut diagnostics);
    seal_validation_result(diagnostics)
}

fn validate_overlay_operations(
    seals: &SealIndex<'_>,
    points: &ExtensionPointIndex<'_>,
    operations: &[CustomizationOverlayOperation],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, operation) in operations.iter().enumerate() {
        match operation {
            operation @ CustomizationOverlayOperation::Add { .. } => {
                validate_additive(operation, seals, points, index, diagnostics)
            }
            CustomizationOverlayOperation::Unseal { .. } => diagnostics.push(diag(
                "sealing.monotonic.unseal_forbidden",
                "downstream overlays cannot unseal an upstream seal",
                format!("operations[{index}]"),
            )),
            CustomizationOverlayOperation::Mutate {
                base_node_id,
                payload,
                extension_point_id,
                required_capabilities,
                ..
            } => {
                let base_seal = seals.get(base_node_id.as_str());
                if extension_point_id.is_none() {
                    let denial = match base_seal {
                        Some(seal) if seal.sealed => Some((
                            "sealing.overlay.sealed_mutation",
                            "sealed base nodes cannot be mutated without an exposed extension point",
                        )),
                        Some(seal) if !seal.allows_mutation => Some((
                            "sealing.overlay.base_mutation_denied",
                            "base nodes are immutable unless an annotation explicitly allows direct mutation",
                        )),
                        None => Some((
                            "sealing.overlay.base_mutation_denied",
                            "base nodes are immutable unless an annotation explicitly allows direct mutation",
                        )),
                        _ => None,
                    };
                    if let Some((code, message)) = denial {
                        diagnostics.push(diag(
                            code,
                            message,
                            format!("operations[{index}].baseNodeId"),
                        ));
                        continue;
                    }
                }
                if let Some(point_id) = extension_point_id {
                    match authorized_extension_point(base_node_id, point_id, seals, points) {
                        Some(point) => {
                            if !has_capabilities(required_capabilities, &point.scope_capabilities) {
                                diagnostics.push(diag(
                                    "sealing.extension_point.capability_missing",
                                    "overlay mutation lacks extension-point-granted capabilities",
                                    format!("operations[{index}].requiredCapabilities"),
                                ));
                            }
                            validate_contract_payload(
                                payload,
                                &point.contract,
                                index,
                                diagnostics,
                            );
                        }
                        None => diagnostics.push(diag(
                            "sealing.extension_point.unresolved",
                            "overlay mutation references an extension point not exposed by the target seal",
                            format!("operations[{index}].extensionPointId"),
                        )),
                    }
                }
            }
        }
    }
}

pub fn validate_runtime_capability_backstop(
    requested_node_id: &str,
    seals: &[SealAnnotation],
    capabilities: &[String],
) -> SealValidationResult {
    let mut diagnostics = Vec::new();
    let seals = index_seals(seals, "seals", &mut diagnostics);
    let denied = seals.get(requested_node_id).is_some_and(|seal| {
        seal.sealed && !has_capabilities(capabilities, &seal.scope_capabilities)
    });
    if denied {
        diagnostics.push(diag(
            "sealing.runtime.capability_denied",
            "custom code receives no handle to sealed nodes unless an extension point grants capabilities",
            "capabilities".to_string(),
        ));
    }
    seal_validation_result(diagnostics)
}

pub fn validate_seal_upgrade(
    previous_base_seals: &[SealAnnotation],
    upgraded_base_seals: &[SealAnnotation],
    extension_points: &[ExtensionPointDefinition],
    overlay_operations: &[CustomizationOverlayOperation],
) -> SealValidationResult {
    let mut diagnostics = Vec::new();
    let previous = index_seals(previous_base_seals, "previousBaseSeals", &mut diagnostics);
    let upgraded = index_seals(upgraded_base_seals, "upgradedBaseSeals", &mut diagnostics);

    for previous in previous.values() {
        if previous.sealed
            && upgraded
                .get(previous.node_id.as_str())
                .is_none_or(|next| weakens_seal_policy(previous, next))
        {
            diagnostics.push(diag(
                "sealing.upgrade.monotonic_violation",
                "base upgrades cannot weaken or remove an existing seal without kernel migration authority",
                format!("upgradedBaseSeals.{}", previous.node_id),
            ));
        }
    }

    let points = index_extension_points(extension_points, "extensionPoints", &mut diagnostics);
    validate_overlay_operations(&upgraded, &points, overlay_operations, &mut diagnostics);
    seal_validation_result(diagnostics)
}

fn validate_additive(
    operation: &CustomizationOverlayOperation,
    seals: &SealIndex<'_>,
    points: &ExtensionPointIndex<'_>,
    index: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let CustomizationOverlayOperation::Add {
        node_id,
        node_kind,
        tenant_namespace,
        scope,
        payload,
        extension_point_id,
        required_capabilities,
    } = operation
    else {
        unreachable!("validate_additive only accepts additive operations")
    };

    if tenant_namespace.trim().is_empty() {
        diagnostics.push(diag(
            "sealing.overlay.tenant_namespace_required",
            "additive tenant customization requires a tenant namespace",
            format!("operations[{index}].tenantNamespace"),
        ));
    } else if node_id
        .strip_prefix(tenant_namespace)
        .and_then(|suffix| suffix.strip_prefix('.'))
        .is_none_or(|suffix| suffix.is_empty())
        || scope.tenant_id.as_deref() != Some(tenant_namespace)
    {
        diagnostics.push(diag(
            "sealing.overlay.tenant_namespace_mismatch",
            "additive customization node and scope must belong to the declared tenant namespace",
            format!("operations[{index}]"),
        ));
    }

    let Some(point_id) = extension_point_id else {
        return;
    };
    let Some(point) = points.get(point_id.as_str()).copied().and_then(|point| {
        authorized_extension_point(&point.owner_node_id, point_id, seals, points)
    }) else {
        diagnostics.push(diag(
            "sealing.extension_point.unresolved",
            "additive contribution references an extension point not exposed by its owner seal",
            format!("operations[{index}].extensionPointId"),
        ));
        return;
    };
    if point.allowed_contribution_kind != *node_kind {
        diagnostics.push(diag(
            "sealing.extension_point.kind_mismatch",
            "extension-point contribution kind does not match the operation",
            format!("operations[{index}].nodeKind"),
        ));
    }
    if !has_capabilities(required_capabilities, &point.scope_capabilities) {
        diagnostics.push(diag(
            "sealing.extension_point.capability_missing",
            "additive contribution lacks extension-point-granted capabilities",
            format!("operations[{index}].requiredCapabilities"),
        ));
    }
    validate_contract_payload(payload, &point.contract, index, diagnostics);
}

type SealIndex<'a> = BTreeMap<&'a str, &'a SealAnnotation>;
type ExtensionPointIndex<'a> = BTreeMap<&'a str, &'a ExtensionPointDefinition>;

fn index_seals<'a>(
    seals: &'a [SealAnnotation],
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> SealIndex<'a> {
    let mut indexed = BTreeMap::new();
    for (index, seal) in seals.iter().enumerate() {
        if indexed.contains_key(seal.node_id.as_str()) {
            diagnostics.push(diag(
                "sealing.input.duplicate_seal",
                "seal annotations must contain at most one entry per node",
                format!("{path}[{index}].nodeId"),
            ));
            continue;
        }
        indexed.insert(seal.node_id.as_str(), seal);
    }
    indexed
}

fn index_extension_points<'a>(
    extension_points: &'a [ExtensionPointDefinition],
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> ExtensionPointIndex<'a> {
    let mut indexed = BTreeMap::new();
    for (index, point) in extension_points.iter().enumerate() {
        if indexed.contains_key(point.extension_point_id.as_str()) {
            diagnostics.push(diag(
                "sealing.input.duplicate_extension_point",
                "extension-point definitions must contain at most one entry per ID",
                format!("{path}[{index}].extensionPointId"),
            ));
            continue;
        }
        indexed.insert(point.extension_point_id.as_str(), point);
    }
    indexed
}

fn authorized_extension_point<'a>(
    owner_node_id: &str,
    point_id: &str,
    seals: &SealIndex<'a>,
    points: &ExtensionPointIndex<'a>,
) -> Option<&'a ExtensionPointDefinition> {
    let point = points.get(point_id).copied()?;
    let owner_seal = seals.get(owner_node_id).copied()?;
    (point.owner_node_id == owner_node_id
        && owner_seal
            .extension_point_ids
            .iter()
            .any(|exposed_id| exposed_id == point_id))
    .then_some(point)
}

fn weakens_seal_policy(previous: &SealAnnotation, upgraded: &SealAnnotation) -> bool {
    !upgraded.sealed
        || (!previous.allows_mutation && upgraded.allows_mutation)
        || previous
            .scope_capabilities
            .iter()
            .any(|required| !upgraded.scope_capabilities.contains(required))
        || upgraded
            .extension_point_ids
            .iter()
            .any(|exposed| !previous.extension_point_ids.contains(exposed))
}

fn has_capabilities(actual: &[String], required: &[String]) -> bool {
    required
        .iter()
        .all(|capability| actual.contains(capability))
}

fn validate_contract_payload(
    payload: &BTreeMap<String, JsonValue>,
    contract: &BTreeMap<String, JsonValue>,
    operation_index: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let schema = JsonValue::Object(contract.clone());
    let value = JsonValue::Object(payload.clone());
    if let Err(reason) = validate_contract_schema(&schema, "$")
        .and_then(|_| validate_contract_value(&value, &schema, "$"))
    {
        diagnostics.push(diag(
            "sealing.extension_point.contract_violation",
            &format!("overlay payload violates the extension-point contract: {reason}"),
            format!("operations[{operation_index}].payload"),
        ));
    }
}

fn validate_contract_schema(schema: &JsonValue, path: &str) -> Result<(), String> {
    let JsonValue::Object(keywords) = schema else {
        return match schema {
            JsonValue::Bool(_) => Ok(()),
            JsonValue::Null
            | JsonValue::Number(_)
            | JsonValue::String(_)
            | JsonValue::Array(_)
            | JsonValue::Object(_) => Err(format!("{path} must be an object or boolean schema")),
        };
    };

    for (keyword, value) in keywords {
        match keyword.as_str() {
            "$schema" | "$id" | "$comment" | "title" | "description" | "default" | "examples"
            | "deprecated" | "readOnly" | "writeOnly" => {}
            "type" => {
                let supported = match value {
                    JsonValue::String(kind) => is_supported_contract_type(kind),
                    JsonValue::Array(kinds) => {
                        !kinds.is_empty()
                            && kinds.iter().all(
                                |kind| matches!(kind, JsonValue::String(kind) if is_supported_contract_type(kind)),
                            )
                    }
                    JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::Object(_) => false,
                };
                if !supported {
                    return Err(format!("{path}.type must name a supported JSON value type"));
                }
            }
            "required" => {
                if !matches!(
                    value,
                    JsonValue::Array(fields)
                        if fields.iter().all(|field| matches!(field, JsonValue::String(_)))
                ) {
                    return Err(format!("{path}.required must be an array of strings"));
                }
            }
            "properties" => {
                let JsonValue::Object(properties) = value else {
                    return Err(format!("{path}.properties must be an object"));
                };
                for (name, property_schema) in properties {
                    validate_contract_schema(
                        property_schema,
                        &format!("{path}.properties.{name}"),
                    )?;
                }
            }
            "additionalProperties" | "items" => {
                validate_contract_schema(value, &format!("{path}.{keyword}"))?;
            }
            "enum" => {
                if !matches!(value, JsonValue::Array(values) if !values.is_empty()) {
                    return Err(format!("{path}.enum must be a non-empty array"));
                }
            }
            "const" => {}
            _ => {
                return Err(format!(
                    "{path} uses unsupported contract keyword `{keyword}`"
                ));
            }
        }
    }
    Ok(())
}

fn validate_contract_value(
    value: &JsonValue,
    schema: &JsonValue,
    path: &str,
) -> Result<(), String> {
    match schema {
        JsonValue::Bool(true) => return Ok(()),
        JsonValue::Bool(false) => return Err(format!("{path} is denied by the contract")),
        JsonValue::Object(_) => {}
        JsonValue::Null | JsonValue::Number(_) | JsonValue::String(_) | JsonValue::Array(_) => {
            return Err(format!("{path} has an invalid contract schema"));
        }
    }
    let JsonValue::Object(keywords) = schema else {
        return Err(format!("{path} has an invalid contract schema"));
    };

    if let Some(expected) = keywords.get("type")
        && !contract_type_matches(value, expected)
    {
        return Err(format!("{path} must match the declared type"));
    }
    if let Some(expected) = keywords.get("const")
        && value != expected
    {
        return Err(format!("{path} must match the declared const value"));
    }
    if let Some(JsonValue::Array(allowed)) = keywords.get("enum")
        && !allowed.contains(value)
    {
        return Err(format!("{path} must match one of the declared enum values"));
    }

    match value {
        JsonValue::Object(object) => {
            if let Some(JsonValue::Array(required)) = keywords.get("required") {
                for field in required {
                    let JsonValue::String(field) = field else {
                        unreachable!("contract schema validated required fields")
                    };
                    if !object.contains_key(field) {
                        return Err(format!("{path}.{field} is required"));
                    }
                }
            }

            let properties = keywords.get("properties").and_then(JsonValue::as_object);
            for (name, property_value) in object {
                if let Some(property_schema) =
                    properties.and_then(|properties| properties.get(name))
                {
                    validate_contract_value(
                        property_value,
                        property_schema,
                        &format!("{path}.{name}"),
                    )?;
                    continue;
                }
                match keywords.get("additionalProperties") {
                    Some(JsonValue::Bool(false)) => {
                        return Err(format!("{path}.{name} is not an allowed property"));
                    }
                    Some(additional_schema @ JsonValue::Object(_))
                    | Some(additional_schema @ JsonValue::Bool(true)) => {
                        validate_contract_value(
                            property_value,
                            additional_schema,
                            &format!("{path}.{name}"),
                        )?;
                    }
                    _ => {}
                }
            }
        }
        JsonValue::Array(items) => {
            if let Some(item_schema) = keywords.get("items") {
                for (index, item) in items.iter().enumerate() {
                    validate_contract_value(item, item_schema, &format!("{path}[{index}]"))?;
                }
            }
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::String(_) => {}
    }
    Ok(())
}

fn is_supported_contract_type(kind: &str) -> bool {
    matches!(
        kind,
        "null" | "boolean" | "number" | "integer" | "string" | "array" | "object"
    )
}

fn contract_type_matches(value: &JsonValue, expected: &JsonValue) -> bool {
    let matches_kind = |kind: &str| match (kind, value) {
        ("null", JsonValue::Null)
        | ("boolean", JsonValue::Bool(_))
        | ("number", JsonValue::Number(_))
        | ("string", JsonValue::String(_))
        | ("array", JsonValue::Array(_))
        | ("object", JsonValue::Object(_)) => true,
        ("integer", JsonValue::Number(number)) => number.is_finite() && number.fract() == 0.0,
        _ => false,
    };
    match expected {
        JsonValue::String(kind) => matches_kind(kind),
        JsonValue::Array(kinds) => kinds
            .iter()
            .any(|kind| kind.as_string().is_some_and(&matches_kind)),
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::Object(_) => false,
    }
}

fn diag(code: &str, message: &str, path: String) -> Diagnostic {
    Diagnostic {
        code: code.to_string(),
        severity: DiagnosticSeverity::Error,
        message: message.to_string(),
        artifact_id: None,
        path: Some(path),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn_ir::ScopeRef;
    use norn_ir::sealing::{ExtensionContributionKind, SEALING_IR_VERSION, SealableNodeKind};

    fn base() -> SealAnnotation {
        SealAnnotation {
            seal_version: SEALING_IR_VERSION.to_string(),
            node_id: "retail.StockLedger.quantity".to_string(),
            node_kind: SealableNodeKind::Field,
            sealed: true,
            allows_mutation: false,
            extension_point_ids: vec![],
            scope_capabilities: vec!["retail.stock_ledger.extension".to_string()],
        }
    }
    #[test]
    fn denies_base_mutation() {
        let x = validate_customization_overlay(
            &[base()],
            &[],
            &[CustomizationOverlayOperation::Mutate {
                node_id: "tenant.mutate".to_string(),
                base_node_id: "retail.StockLedger.quantity".to_string(),
                payload: BTreeMap::new(),
                extension_point_id: None,
                required_capabilities: vec![],
            }],
        );
        assert!(!x.ok);
        assert_eq!(x.diagnostics[0].code, "sealing.overlay.sealed_mutation");
    }
    #[test]
    fn allows_additive_tenant_customization() {
        let x = seal(SealInput::CustomizationOverlay {
            base_seals: vec![base()],
            extension_points: vec![],
            operations: vec![CustomizationOverlayOperation::Add {
                node_id: "tenant.batch".to_string(),
                node_kind: ExtensionContributionKind::Field,
                tenant_namespace: "tenant".to_string(),
                scope: ScopeRef {
                    tenant_id: Some("tenant".to_string()),
                    ..ScopeRef::default()
                },
                payload: BTreeMap::new(),
                extension_point_id: None,
                required_capabilities: vec![],
            }],
        })
        .unwrap();
        assert!(x.ok);
    }
    #[test]
    fn denies_unseal_and_runtime_without_capability() {
        let x = validate_customization_overlay(
            &[base()],
            &[],
            &[CustomizationOverlayOperation::Unseal {
                base_node_id: "retail.StockLedger.quantity".to_string(),
                requested_by: "tenant".to_string(),
            }],
        );
        assert_eq!(x.diagnostics[0].code, "sealing.monotonic.unseal_forbidden");
        assert!(
            !validate_runtime_capability_backstop("retail.StockLedger.quantity", &[base()], &[]).ok
        );
    }
    #[test]
    fn denies_upgrade_weakened_seal() {
        let mut weakened = base();
        weakened.sealed = false;
        assert!(!validate_seal_upgrade(&[base()], &[weakened], &[], &[]).ok);
    }

    #[test]
    fn preserves_the_wire_contract_and_rejects_seal_bypass_paths() {
        let add = CustomizationOverlayOperation::Add {
            node_id: "tenant.field".to_string(),
            node_kind: ExtensionContributionKind::Field,
            tenant_namespace: "tenant".to_string(),
            scope: ScopeRef {
                tenant_id: Some("tenant".to_string()),
                ..ScopeRef::default()
            },
            payload: BTreeMap::new(),
            extension_point_id: None,
            required_capabilities: vec![],
        };
        let serialized = serde_json::to_value(SealInput::CustomizationOverlay {
            base_seals: vec![],
            extension_points: vec![],
            operations: vec![add],
        })
        .unwrap();
        assert!(serialized.get("baseSeals").is_some());
        assert!(serialized["operations"][0].get("nodeId").is_some());
        assert!(serialized["operations"][0].get("tenantNamespace").is_some());

        let point = ExtensionPointDefinition {
            extension_point_id: "not.exposed".to_string(),
            owner_node_id: base().node_id,
            allowed_contribution_kind: ExtensionContributionKind::Field,
            contract: BTreeMap::new(),
            scope_capabilities: vec![],
        };
        let mutate = CustomizationOverlayOperation::Mutate {
            node_id: "tenant.mutate".to_string(),
            base_node_id: "retail.StockLedger.quantity".to_string(),
            payload: BTreeMap::new(),
            extension_point_id: Some("not.exposed".to_string()),
            required_capabilities: vec![],
        };
        assert!(!validate_customization_overlay(&[base()], &[point], &[mutate]).ok);
        assert!(!validate_seal_upgrade(&[base()], &[], &[], &[]).ok);

        let mismatched = CustomizationOverlayOperation::Add {
            node_id: "base.field".to_string(),
            node_kind: ExtensionContributionKind::Field,
            tenant_namespace: "tenant".to_string(),
            scope: ScopeRef::default(),
            payload: BTreeMap::new(),
            extension_point_id: None,
            required_capabilities: vec![],
        };
        assert!(!validate_customization_overlay(&[], &[], &[mismatched]).ok);
    }

    fn exposed_base() -> SealAnnotation {
        let mut seal = base();
        seal.extension_point_ids = vec!["retail.StockLedger.fields.additive".to_string()];
        seal
    }

    fn extension_point(contract: BTreeMap<String, JsonValue>) -> ExtensionPointDefinition {
        ExtensionPointDefinition {
            extension_point_id: "retail.StockLedger.fields.additive".to_string(),
            owner_node_id: base().node_id,
            allowed_contribution_kind: ExtensionContributionKind::Field,
            contract,
            scope_capabilities: vec!["retail.stock_ledger.extension".to_string()],
        }
    }

    fn extension_mutation(payload: BTreeMap<String, JsonValue>) -> CustomizationOverlayOperation {
        CustomizationOverlayOperation::Mutate {
            node_id: "tenant.mutate.quantity".to_string(),
            base_node_id: base().node_id,
            payload,
            extension_point_id: Some("retail.StockLedger.fields.additive".to_string()),
            required_capabilities: vec!["retail.stock_ledger.extension".to_string()],
        }
    }

    fn has_diagnostic(result: &SealValidationResult, code: &str) -> bool {
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == code)
    }

    #[test]
    fn rejects_extension_point_not_exposed_by_target_seal() {
        let result = validate_customization_overlay(
            &[base()],
            &[extension_point(BTreeMap::new())],
            &[extension_mutation(BTreeMap::new())],
        );

        assert!(!result.ok);
        assert!(has_diagnostic(
            &result,
            "sealing.extension_point.unresolved"
        ));
    }

    #[test]
    fn seal_upgrade_revalidates_with_extension_point_definitions() {
        let seal = exposed_base();
        let result = validate_seal_upgrade(
            std::slice::from_ref(&seal),
            std::slice::from_ref(&seal),
            &[extension_point(BTreeMap::from([(
                "type".to_string(),
                JsonValue::String("object".to_string()),
            )]))],
            &[extension_mutation(BTreeMap::new())],
        );

        assert!(result.ok, "{:?}", result.diagnostics);
    }

    #[test]
    fn rejects_additive_node_outside_declared_tenant_namespace() {
        let result = validate_customization_overlay(
            &[],
            &[],
            &[CustomizationOverlayOperation::Add {
                node_id: "retail.StockLedger.shadow".to_string(),
                node_kind: ExtensionContributionKind::Field,
                tenant_namespace: "tenant".to_string(),
                scope: ScopeRef {
                    tenant_id: Some("tenant".to_string()),
                    ..ScopeRef::default()
                },
                payload: BTreeMap::new(),
                extension_point_id: None,
                required_capabilities: vec![],
            }],
        );

        assert!(!result.ok);
        assert!(has_diagnostic(
            &result,
            "sealing.overlay.tenant_namespace_mismatch"
        ));
    }

    #[test]
    fn rejects_duplicate_seals_independent_of_input_order() {
        let sealed = base();
        let mut unsealed = sealed.clone();
        unsealed.sealed = false;
        unsealed.scope_capabilities.clear();

        for seals in [
            vec![sealed.clone(), unsealed.clone()],
            vec![unsealed.clone(), sealed.clone()],
        ] {
            let runtime = validate_runtime_capability_backstop(&sealed.node_id, &seals, &[]);
            assert!(!runtime.ok);
            assert!(has_diagnostic(&runtime, "sealing.input.duplicate_seal"));

            let overlay = validate_customization_overlay(&seals, &[], &[]);
            assert!(!overlay.ok);
            assert!(has_diagnostic(&overlay, "sealing.input.duplicate_seal"));
        }
    }

    #[test]
    fn rejects_direct_mutation_without_explicit_permission() {
        let mutation = CustomizationOverlayOperation::Mutate {
            node_id: "tenant.mutate.quantity".to_string(),
            base_node_id: base().node_id,
            payload: BTreeMap::new(),
            extension_point_id: None,
            required_capabilities: vec![],
        };

        let missing = validate_customization_overlay(&[], &[], std::slice::from_ref(&mutation));
        assert!(!missing.ok);
        assert!(has_diagnostic(
            &missing,
            "sealing.overlay.base_mutation_denied"
        ));

        let mut denied = base();
        denied.sealed = false;
        let denied_result = validate_customization_overlay(
            std::slice::from_ref(&denied),
            &[],
            std::slice::from_ref(&mutation),
        );
        assert!(!denied_result.ok);

        denied.allows_mutation = true;
        let allowed = validate_customization_overlay(&[denied], &[], &[mutation]);
        assert!(allowed.ok, "{:?}", allowed.diagnostics);
    }

    #[test]
    fn rejects_duplicate_extension_points_independent_of_input_order() {
        let restrictive = extension_point(BTreeMap::from([(
            "type".to_string(),
            JsonValue::String("object".to_string()),
        )]));
        let mut permissive = restrictive.clone();
        permissive.contract.clear();
        permissive.scope_capabilities.clear();

        for points in [
            vec![restrictive.clone(), permissive.clone()],
            vec![permissive.clone(), restrictive],
        ] {
            let result = validate_customization_overlay(&[exposed_base()], &points, &[]);
            assert!(!result.ok);
            assert!(has_diagnostic(
                &result,
                "sealing.input.duplicate_extension_point"
            ));
        }
    }

    #[test]
    fn rejects_every_monotonic_policy_weakening() {
        let previous = exposed_base();

        let mut capabilities_removed = previous.clone();
        capabilities_removed.scope_capabilities.clear();
        let result = validate_seal_upgrade(
            std::slice::from_ref(&previous),
            &[capabilities_removed],
            &[],
            &[],
        );
        assert!(has_diagnostic(
            &result,
            "sealing.upgrade.monotonic_violation"
        ));

        let mut extension_added = previous.clone();
        extension_added
            .extension_point_ids
            .push("retail.StockLedger.unsafe".to_string());
        let result = validate_seal_upgrade(
            std::slice::from_ref(&previous),
            &[extension_added],
            &[],
            &[],
        );
        assert!(has_diagnostic(
            &result,
            "sealing.upgrade.monotonic_violation"
        ));

        let mut mutation_enabled = previous.clone();
        mutation_enabled.allows_mutation = true;
        let result = validate_seal_upgrade(&[previous], &[mutation_enabled], &[], &[]);
        assert!(has_diagnostic(
            &result,
            "sealing.upgrade.monotonic_violation"
        ));
    }

    #[test]
    fn validates_extension_payload_against_contract() {
        let contract = BTreeMap::from([
            ("type".to_string(), JsonValue::String("object".to_string())),
            (
                "properties".to_string(),
                JsonValue::Object(BTreeMap::from([(
                    "min".to_string(),
                    JsonValue::Object(BTreeMap::from([(
                        "type".to_string(),
                        JsonValue::String("number".to_string()),
                    )])),
                )])),
            ),
            (
                "required".to_string(),
                JsonValue::Array(vec![JsonValue::String("min".to_string())]),
            ),
            ("additionalProperties".to_string(), JsonValue::Bool(false)),
        ]);
        let point = extension_point(contract);
        let seal = exposed_base();

        let rejected = validate_customization_overlay(
            std::slice::from_ref(&seal),
            std::slice::from_ref(&point),
            &[extension_mutation(BTreeMap::from([(
                "min".to_string(),
                JsonValue::String("not-a-number".to_string()),
            )]))],
        );
        assert!(!rejected.ok);
        assert!(has_diagnostic(
            &rejected,
            "sealing.extension_point.contract_violation"
        ));

        let accepted = validate_customization_overlay(
            &[seal],
            &[point],
            &[extension_mutation(BTreeMap::from([(
                "min".to_string(),
                JsonValue::Number(0.0),
            )]))],
        );
        assert!(accepted.ok, "{:?}", accepted.diagnostics);
    }
}
