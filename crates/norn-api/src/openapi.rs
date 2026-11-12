//! Generated `OpenAPI` components for the core IR and checker wire contract.
use crate::contracts::ContractSchemas;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// The security scheme's name in `components.securitySchemes`.
pub const HOST_TOKEN_SCHEME: &str = "hostToken";

/// Operations a caller may reach without a host token, by `operationId`.
/// None exist. A route that is deliberately public is named here; it then
/// carries no requirement, and the test below requires exactly that.
const PUBLIC_OPERATIONS: &[&str] = &[];

/// Declares the host-token bearer scheme and requires it on every operation
/// not named in [`PUBLIC_OPERATIONS`].
///
/// Every route in this document resolves its principal from the
/// `Authorization: Bearer` header and denies without one. The requirement is
/// stamped here, not read from the dispatch: whether a route is really
/// public is decided by the server, and this list has to follow it.
pub(crate) struct HostTokenSecurity;

impl Modify for HostTokenSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let scheme = HttpBuilder::new()
            .scheme(HttpAuthScheme::Bearer)
            .description(Some(
                "The cell host's own credential, sent to the cell host directly as \
                 `Authorization: Bearer <token>`. A published host accepts only a token signed \
                 (EdDSA) by the identity service it is configured to trust, issued for at most \
                 five minutes and bound to the cell's organization; the token is not bound to an \
                 environment. The local development host accepts the session token it writes to \
                 the owner-only file `.norn/dev.token`. A caller that reaches the cell through a \
                 fronting application (for example Studio's proxy) authenticates to that application by its own means and does not present this \
                 token. Without a valid token no route serves: a route that reaches \
                 authorization refuses with 403 `authz.denied`, and an unknown route, a wrong \
                 method or a malformed query can refuse before that.",
            ))
            .build();
        openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::new)
            .add_security_scheme(HOST_TOKEN_SCHEME, SecurityScheme::Http(scheme));
        let requirement = SecurityRequirement::new(HOST_TOKEN_SCHEME, Vec::<String>::new());
        for item in openapi.paths.paths.values_mut() {
            let fixed = [
                &mut item.get,
                &mut item.put,
                &mut item.post,
                &mut item.delete,
                &mut item.options,
                &mut item.head,
                &mut item.patch,
                &mut item.trace,
                &mut item.query,
            ];
            for operation in fixed
                .into_iter()
                .flatten()
                .chain(item.additional_operations.values_mut())
            {
                let public = operation
                    .operation_id
                    .as_deref()
                    .is_some_and(|id| PUBLIC_OPERATIONS.contains(&id));
                operation.security = (!public).then(|| vec![requirement.clone()]);
            }
        }
    }
}

/// The public core's schema contract; the cell store is embedded through Rust.
#[derive(Debug, OpenApi)]
#[openapi(info(title = "Norn core contracts"), modifiers(&ContractSchemas, &HostTokenSecurity), paths(crate::actions::call_action, crate::question_routes::get_answer, crate::question_routes::get_numeric_trace, crate::question_routes::evaluate_question, crate::question_routes::trace_ad_hoc, crate::question_routes::question_catalog, crate::definition_routes::admit_definition, crate::definition_routes::install_definition, crate::definition_routes::retire_definition, crate::definition_routes::list_definitions, crate::definition_routes::get_definition, crate::routes::get_defs, crate::object_routes::get_object_query, crate::object_routes::get_queried_object, crate::object_links::get_object_links, crate::object_links::get_object_lineage), components(schemas(crate::contracts::NornExplainResponse, crate::actions::ActionRequest, crate::actions::ActionReceipt, crate::actions::ActionCommit, crate::actions::ActionCallOutcome, crate::refusal::Refusal, crate::refusal::RefusalDiagnostic, crate::refusal::RefusalSpan, crate::refusal::RefusalRestatement, crate::question_routes::EvaluateRequest, crate::question_routes::AdHocTraceRequest, crate::question_routes::QuestionCatalog, crate::question_routes::CatalogObjectType, crate::question_routes::CatalogProperty, crate::definition_routes::AdmitDefinitionRequest, crate::definition_routes::InstallDefinitionRequest, crate::definition_routes::RetireDefinitionRequest, crate::definition_routes::AdmitDefinitionResponse, crate::definition_routes::InstallDefinitionResponse, crate::definition_routes::DefinitionReceipt, crate::definition_routes::DefinitionPage, crate::definition_routes::DefinitionSummary, crate::definition_routes::DefinitionDocument, crate::definition_routes::SignoffRequest, crate::definition_routes::SignoffDocument, crate::contracts::NornCheckReceipt, crate::contracts::NornCheckerDiagnostics, crate::answers::NornQueryResult, crate::answers::NornHistoricalQuestionResult, crate::objects::NornObjectPage, crate::objects::NornObjectRecord, crate::objects::NornLinkPage, crate::objects::NornInstanceSourceProvenance)))]
pub struct ApiDoc;

/// Serialize the contract using the existing typed `OpenAPI` projection.
///
/// # Errors
/// Returns a serialization failure.
pub fn openapi_json() -> Result<String, serde_json::Error> {
    let mut doc = ApiDoc::openapi();
    ContractSchemas.modify(&mut doc);
    serde_json::to_string_pretty(&doc)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "generated schema correspondence"
)]
mod action_contract_tests {
    #[test]
    fn generated_action_values_and_receipts_retain_typed_fields() {
        let document: serde_json::Value =
            serde_json::from_str(&super::openapi_json().unwrap()).unwrap();
        let schemas = &document["components"]["schemas"];
        assert!(schemas["NornActionInput"]["additionalProperties"].is_object());
        assert!(
            schemas["NornActionInput"]["additionalProperties"]["description"]
                .as_str()
                .unwrap()
                .contains("exact-decimal")
        );
        assert!(
            schemas["NornActionInput"]["description"]
                .as_str()
                .unwrap()
                .contains("travels as its exact decimal text")
        );
        assert!(schemas.get("NornActionJsonValue").is_none());
        let record = &schemas["NornActionCallRecord"]["properties"];
        for name in [
            "actionId",
            "actor",
            "turn",
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "rowOperations",
        ] {
            assert!(record.get(name).is_some(), "typed receipt field {name}");
        }
        assert_eq!(
            record["object"]["$ref"],
            "#/components/schemas/NornActionFactKey"
        );
        assert!(record["rowOperations"]["items"].is_object());
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "generated schema correspondence"
)]
mod security_contract_tests {
    const METHODS: &[&str] = &[
        "get", "put", "post", "delete", "options", "head", "patch", "trace", "query",
    ];

    /// The document agrees with the declared list: every operation carries
    /// exactly the host-token requirement unless `PUBLIC_OPERATIONS` names
    /// it, and a named one carries none. This does not prove the server
    /// enforces it; each route's own refusal tests do that.
    #[test]
    fn every_operation_requires_the_declared_host_token() {
        let document: serde_json::Value =
            serde_json::from_str(&super::openapi_json().unwrap()).unwrap();
        let scheme = &document["components"]["securitySchemes"][super::HOST_TOKEN_SCHEME];
        assert_eq!(scheme["type"], "http");
        assert_eq!(scheme["scheme"], "bearer");
        let required = serde_json::json!([{ super::HOST_TOKEN_SCHEME: [] }]);
        let mut seen = 0_usize;
        for (path, item) in document["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                if !METHODS.contains(&method.as_str()) {
                    continue;
                }
                seen += 1;
                let id = operation["operationId"].as_str().unwrap_or_default();
                if super::PUBLIC_OPERATIONS.contains(&id) {
                    assert!(operation.get("security").is_none(), "{method} {path}");
                } else {
                    assert_eq!(operation["security"], required, "{method} {path}");
                }
            }
        }
        assert!(seen >= 15, "the document lists its operations: {seen}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "generated schema correspondence")]
mod free_form_contract_tests {
    use serde_json::Value;

    /// Every object schema says what members it has: `properties`, or
    /// `additionalProperties` for free-form members, or a composition. A bare
    /// `object` generates as an object with no members (`Record<string,
    /// never>` in TypeScript), which no reader can use.
    #[test]
    fn no_object_schema_is_bare() {
        fn walk(node: &Value, at: &str, bare: &mut Vec<String>) {
            match node {
                Value::Object(members) => {
                    let object = match members.get("type") {
                        Some(Value::String(kind)) => kind == "object",
                        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "object"),
                        _ => false,
                    };
                    // `properties: {}` alone names no member either.
                    let named = members
                        .get("properties")
                        .and_then(Value::as_object)
                        .is_some_and(|properties| !properties.is_empty());
                    let described = named
                        || ["additionalProperties", "oneOf", "anyOf", "allOf", "$ref"]
                            .iter()
                            .any(|key| members.contains_key(*key));
                    if object && !described {
                        bare.push(at.to_owned());
                    }
                    for (key, child) in members {
                        walk(child, &format!("{at}/{key}"), bare);
                    }
                }
                Value::Array(items) => {
                    for (index, child) in items.iter().enumerate() {
                        walk(child, &format!("{at}/{index}"), bare);
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
            }
        }
        let document: Value = serde_json::from_str(&super::openapi_json().unwrap()).unwrap();
        let mut bare = Vec::new();
        walk(&document, "#", &mut bare);
        assert!(bare.is_empty(), "bare object schemas: {bare:#?}");
    }
}
