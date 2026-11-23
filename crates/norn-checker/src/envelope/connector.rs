//! Connector artifact payload validation.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{Diagnostic, JsonValue};

use super::helpers::{
    expect_object, invalid_diagnostic, require_literal_field, require_non_empty_field,
    validate_json_object_field, validate_optional_non_empty_json_field, validate_proposal_metadata,
    validate_string_array, validate_string_array_field, validate_unique_string_array_field,
};
use crate::connector_path::ConnectorValuePath;

pub(super) fn validate_connector_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    require_literal_field(
        payload,
        "connectorVersion",
        &["0.1"],
        "payload.connectorVersion",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_connector_auth(payload.get("auth"), diagnostics, artifact_id);
    crate::source_commands::validate_connector_profile(payload, diagnostics, artifact_id);
    if let Some(commands) = payload.get("commands") {
        validate_connector_commands(commands, diagnostics, artifact_id);
    }

    let mut resource_ids = BTreeSet::new();
    match payload.get("resources") {
        Some(JsonValue::Array(resources))
            if !resources.is_empty()
                || payload
                    .get("commands")
                    .and_then(JsonValue::as_array)
                    .is_some_and(|c| !c.is_empty()) =>
        {
            for (index, resource) in resources.iter().enumerate() {
                validate_connector_resource(
                    resource,
                    index,
                    diagnostics,
                    artifact_id,
                    &mut resource_ids,
                );
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "resources must be a non-empty array",
            "payload.resources",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "resources must be an array",
            "payload.resources",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "resources is required",
            "payload.resources",
            artifact_id,
        )),
    }

    validate_connector_mappings(
        payload.get("mappings"),
        diagnostics,
        artifact_id,
        &resource_ids,
    );
    validate_catalog_connector_extensions(payload.get("extensions"), diagnostics, artifact_id);
    validate_connector_poll_profile(
        payload.get("extensions"),
        payload.get("resources"),
        diagnostics,
        artifact_id,
    );

    if let Some(cursor) = payload.get("cursor") {
        validate_connector_cursor_descriptor(cursor, diagnostics, artifact_id, &resource_ids);
    }
    if let Some(policy) = payload.get("deadLetterPolicy") {
        validate_dead_letter_policy(policy, diagnostics, artifact_id);
    }
    if let Some(hook_ids) = payload.get("customStepHookIds") {
        validate_string_array(
            hook_ids,
            "payload.customStepHookIds",
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    if let Some(hooks) = payload.get("customStepHooks") {
        validate_connector_custom_step_hooks(hooks, diagnostics, artifact_id);
    }
    if let Some(proposal) = payload.get("proposal") {
        validate_proposal_metadata(proposal, "payload.proposal", diagnostics, artifact_id, CODE);
    }
}

fn validate_connector_poll_profile(
    extensions: Option<&JsonValue>,
    resources: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(JsonValue::Object(extensions)) = extensions else {
        return;
    };
    let Some(value) = extensions.get(norn_ir::connector::CONNECTOR_POLL_EXTENSION_KEY) else {
        return;
    };
    let path = format!(
        "payload.extensions.{}",
        norn_ir::connector::CONNECTOR_POLL_EXTENSION_KEY
    );
    let profile = serde_json::to_string(value).ok().and_then(|text| {
        serde_json::from_str::<norn_ir::connector::ConnectorPollProfile>(&text).ok()
    });
    let Some(profile) = profile else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "the norn.poll/1 extension must be an ordered feed profile",
            path,
            artifact_id,
        ));
        return;
    };
    if profile.feeds.is_empty() || profile.feeds.len() > 32 {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "the poll profile must contain between 1 and 32 feeds",
            format!("{path}.feeds"),
            artifact_id,
        ));
        return;
    }

    let mut resources_by_id = BTreeMap::new();
    if let Some(JsonValue::Array(resources)) = resources {
        for resource in resources {
            let JsonValue::Object(resource) = resource else {
                continue;
            };
            let (Some(JsonValue::String(resource_id)), Some(JsonValue::String(kind))) =
                (resource.get("resourceId"), resource.get("kind"))
            else {
                continue;
            };
            let endpoint = resource.get("endpoint").and_then(JsonValue::as_string);
            resources_by_id.insert(
                resource_id.clone(),
                (kind.as_str(), endpoint, resource.get("queryParams")),
            );
        }
    }

    let poll_ctx = PollProfileContext {
        feeds: &profile.feeds,
        path: &path,
        resources_by_id: &resources_by_id,
        artifact_id,
    };
    let mut seen_resources = BTreeSet::new();
    for (index, feed) in profile.feeds.iter().enumerate() {
        validate_connector_poll_feed(&poll_ctx, index, feed, &mut seen_resources, diagnostics);
    }
}

type PollResourceSummary<'a> = (&'a str, Option<&'a str>, Option<&'a JsonValue>);

struct PollProfileContext<'a> {
    feeds: &'a [norn_ir::connector::ConnectorPollFeed],
    path: &'a str,
    resources_by_id: &'a BTreeMap<String, PollResourceSummary<'a>>,
    artifact_id: Option<&'a str>,
}

fn validate_connector_poll_feed<'a>(
    ctx: &PollProfileContext<'_>,
    index: usize,
    feed: &'a norn_ir::connector::ConnectorPollFeed,
    seen_resources: &mut BTreeSet<&'a str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let feed_path = format!("{}.feeds[{index}]", ctx.path);
    let valid_id = !feed.resource_id.trim().is_empty()
        && feed.resource_id.len() <= 256
        && !feed.resource_id.chars().any(char::is_control)
        && seen_resources.insert(feed.resource_id.as_str());
    if !valid_id {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "poll feed resource ids must be non-empty and unique",
            format!("{feed_path}.resourceId"),
            ctx.artifact_id,
        ));
        return;
    }
    match ctx.resources_by_id.get(feed.resource_id.as_str()) {
        Some(("cursor" | "paginated" | "delta", _, _)) => {}
        _ => {
            diagnostics.push(invalid_diagnostic(
                CODE,
                "poll feeds must reference a declared readable resource",
                format!("{feed_path}.resourceId"),
                ctx.artifact_id,
            ));
        }
    }
    if let Some(record_key_path) = &feed.record_key_path
        && let Some(error) = poll_value_path_error(record_key_path)
    {
        diagnostics.push(invalid_diagnostic(
            CODE,
            format!("recordKeyPath is invalid: {error}"),
            format!("{feed_path}.recordKeyPath"),
            ctx.artifact_id,
        ));
    }
    if let Some(fanout) = &feed.fanout {
        if !ctx.feeds.iter().take(index).any(|parent| {
            parent.resource_id == fanout.parent_resource_id && parent.fanout.is_none()
        }) {
            diagnostics.push(invalid_diagnostic(
                CODE,
                "a fanout parent must be an earlier feed in the profile",
                format!("{feed_path}.fanout.parentResourceId"),
                ctx.artifact_id,
            ));
        }
        let binding = fanout.path_binding.trim();
        let request_has_binding = ctx
            .resources_by_id
            .get(feed.resource_id.as_str())
            .is_some_and(|(_, endpoint, query)| {
                let placeholder = format!("{{{binding}}}");
                endpoint.is_some_and(|endpoint| endpoint.contains(&placeholder))
                    || query.and_then(JsonValue::as_object).is_some_and(|query| {
                        query
                            .values()
                            .filter_map(JsonValue::as_string)
                            .any(|value| value.contains(&placeholder))
                    })
            });
        if binding != fanout.path_binding
            || binding.is_empty()
            || binding.len() > 128
            || binding.chars().any(char::is_control)
            || !request_has_binding
        {
            diagnostics.push(invalid_diagnostic(
                CODE,
                "fanout requires a valid binding name and a matching child request binding",
                format!("{feed_path}.fanout"),
                ctx.artifact_id,
            ));
        }
        if let Some(error) = poll_value_path_error(&fanout.key_path) {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("fanout keyPath is invalid: {error}"),
                format!("{feed_path}.fanout.keyPath"),
                ctx.artifact_id,
            ));
        }
    }
}

fn poll_value_path_error(path: &str) -> Option<String> {
    if path.is_empty() {
        return Some("path must not be empty".to_owned());
    }
    if path.len() > 1_024 {
        return Some("path exceeds 1,024 bytes".to_owned());
    }
    ConnectorValuePath::parse(path)
        .err()
        .map(|error| error.to_string())
}

fn validate_connector_custom_step_hooks(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    match value {
        JsonValue::Array(hooks) => {
            for (index, hook) in hooks.iter().enumerate() {
                let path = format!("payload.customStepHooks[{index}]");
                let Some(hook) = expect_object(hook, &path, diagnostics, artifact_id, CODE) else {
                    continue;
                };

                require_non_empty_field(
                    hook,
                    "hookId",
                    format!("{path}.hookId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    hook,
                    "installId",
                    format!("{path}.installId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    hook,
                    "extensionVersion",
                    format!("{path}.extensionVersion"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_string_array_field(
                    hook,
                    "requiredServices",
                    format!("{path}.requiredServices"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_string_array_field(
                    hook,
                    "scopeCapabilities",
                    format!("{path}.scopeCapabilities"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_json_object_field(
                    hook,
                    "inputSchema",
                    format!("{path}.inputSchema"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_json_object_field(
                    hook,
                    "outputSchema",
                    format!("{path}.outputSchema"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                validate_json_object_field(
                    hook,
                    "errorSchema",
                    format!("{path}.errorSchema"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::String(_)
        | JsonValue::Object(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "customStepHooks must be an array",
            "payload.customStepHooks",
            artifact_id,
        )),
    }
}

fn validate_connector_auth(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let auth_path = "payload.auth";
    let Some(auth) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "auth is required",
            "payload.auth",
            artifact_id,
        ));
        return;
    };
    let Some(auth) = expect_object(auth, "payload.auth", diagnostics, artifact_id, CODE) else {
        return;
    };
    let kind = require_literal_field(
        auth,
        "kind",
        &["none", "oauth2", "api_key"],
        "payload.auth.kind",
        diagnostics,
        artifact_id,
        CODE,
    );

    match kind.as_deref() {
        Some("oauth2") => {
            require_non_empty_field(
                auth,
                "authId",
                "payload.auth.authId",
                diagnostics,
                artifact_id,
                CODE,
            );
            require_non_empty_field(
                auth,
                "authorizationUrl",
                "payload.auth.authorizationUrl",
                diagnostics,
                artifact_id,
                CODE,
            );
            require_non_empty_field(
                auth,
                "tokenUrl",
                "payload.auth.tokenUrl",
                diagnostics,
                artifact_id,
                CODE,
            );
            validate_string_array_field(
                auth,
                "scopes",
                "payload.auth.scopes",
                diagnostics,
                artifact_id,
                CODE,
            );
            validate_optional_non_empty_json_field(
                auth,
                "clientSecretRef",
                "payload.auth.clientSecretRef",
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        Some("api_key") => {
            validate_connector_api_key_auth(auth, auth_path, diagnostics, artifact_id, CODE);
        }
        _ => {}
    }
}

fn validate_connector_api_key_auth(
    auth: &BTreeMap<String, JsonValue>,
    auth_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    require_non_empty_field(
        auth,
        "authId",
        "payload.auth.authId",
        diagnostics,
        artifact_id,
        code,
    );
    require_literal_field(
        auth,
        "placement",
        &["header", "query"],
        "payload.auth.placement",
        diagnostics,
        artifact_id,
        code,
    );
    require_non_empty_field(
        auth,
        "name",
        "payload.auth.name",
        diagnostics,
        artifact_id,
        code,
    );
    require_non_empty_field(
        auth,
        "secretRef",
        "payload.auth.secretRef",
        diagnostics,
        artifact_id,
        code,
    );
    validate_optional_non_empty_json_field(
        auth,
        "prefix",
        format!("{auth_path}.prefix"),
        diagnostics,
        artifact_id,
        code,
    );
    if let Some(format) = auth.get("format") {
        let format_path = format!("{auth_path}.format");
        if let Some(format) =
            expect_object(format, format_path.clone(), diagnostics, artifact_id, code)
            && require_literal_field(
                format,
                "kind",
                &["raw", "pair"],
                format!("{format_path}.kind"),
                diagnostics,
                artifact_id,
                code,
            )
            .as_deref()
                == Some("pair")
        {
            require_non_empty_field(
                format,
                "separator",
                format!("{format_path}.separator"),
                diagnostics,
                artifact_id,
                code,
            );
        }
    }
}

fn validate_connector_resource(
    value: &JsonValue,
    index: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    resource_ids: &mut BTreeSet<String>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("payload.resources[{index}]");
    let Some(resource) = expect_object(value, path.clone(), diagnostics, artifact_id, CODE) else {
        return;
    };
    validate_optional_non_empty_json_field(
        resource,
        "commandKeyHeader",
        format!("{path}.commandKeyHeader"),
        diagnostics,
        artifact_id,
        CODE,
    );
    if let Some(value) = resource.get("missingItemsIsEmpty")
        && !matches!(value, JsonValue::Bool(_))
    {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "missingItemsIsEmpty must be a boolean",
            format!("{path}.missingItemsIsEmpty"),
            artifact_id,
        ));
    }
    let kind = require_literal_field(
        resource,
        "kind",
        &["paginated", "cursor", "delta", "webhook", "csv_import"],
        format!("{path}.kind"),
        diagnostics,
        artifact_id,
        CODE,
    );
    if let Some(resource_id) = require_non_empty_field(
        resource,
        "resourceId",
        format!("{path}.resourceId"),
        diagnostics,
        artifact_id,
        CODE,
    ) && !resource_ids.insert(resource_id)
    {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "resource ids must be unique",
            format!("{path}.resourceId"),
            artifact_id,
        ));
    }
    if resource.contains_key("queryParams") {
        validate_connector_query_params(
            resource,
            "queryParams",
            format!("{path}.queryParams"),
            diagnostics,
            artifact_id,
        );
    }

    validate_connector_resource_kind(
        resource,
        kind.as_deref(),
        &path,
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_connector_resource_kind(
    resource: &BTreeMap<String, JsonValue>,
    kind: Option<&str>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    match kind {
        Some("paginated") => {
            validate_paginated_connector_resource(resource, path, diagnostics, artifact_id, code);
        }
        Some("cursor") => {
            for field in ["endpoint", "cursorParam", "nextCursorPath", "itemsPath"] {
                require_non_empty_field(
                    resource,
                    field,
                    format!("{path}.{field}"),
                    diagnostics,
                    artifact_id,
                    code,
                );
            }
        }
        Some("delta") => {
            for field in ["endpoint", "sinceParam", "updatedAtPath", "itemsPath"] {
                require_non_empty_field(
                    resource,
                    field,
                    format!("{path}.{field}"),
                    diagnostics,
                    artifact_id,
                    code,
                );
            }
            if let Some(keyset) = resource.get("keyset") {
                validate_connector_keyset_profile(keyset, path, diagnostics, artifact_id);
            }
        }
        Some("webhook") => {
            require_non_empty_field(
                resource,
                "eventType",
                format!("{path}.eventType"),
                diagnostics,
                artifact_id,
                code,
            );
            require_non_empty_field(
                resource,
                "payloadPath",
                format!("{path}.payloadPath"),
                diagnostics,
                artifact_id,
                code,
            );
        }
        Some("csv_import") => {
            require_literal_field(
                resource,
                "delimiter",
                &[",", ";", "\t"],
                format!("{path}.delimiter"),
                diagnostics,
                artifact_id,
                code,
            );
            validate_string_array_field(
                resource,
                "columns",
                format!("{path}.columns"),
                diagnostics,
                artifact_id,
                code,
            );
        }
        _ => {}
    }
}

fn validate_paginated_connector_resource(
    resource: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    require_non_empty_field(
        resource,
        "endpoint",
        format!("{path}.endpoint"),
        diagnostics,
        artifact_id,
        code,
    );
    require_literal_field(
        resource,
        "method",
        &["GET", "POST"],
        format!("{path}.method"),
        diagnostics,
        artifact_id,
        code,
    );
    require_non_empty_field(
        resource,
        "pageParam",
        format!("{path}.pageParam"),
        diagnostics,
        artifact_id,
        code,
    );
    validate_optional_non_empty_json_field(
        resource,
        "pageSizeParam",
        format!("{path}.pageSizeParam"),
        diagnostics,
        artifact_id,
        code,
    );
    require_non_empty_field(
        resource,
        "itemsPath",
        format!("{path}.itemsPath"),
        diagnostics,
        artifact_id,
        code,
    );
}

fn validate_connector_query_params(
    object: &BTreeMap<String, JsonValue>,
    field: &str,
    path: String,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(JsonValue::Object(parameters)) = object.get(field) else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            format!("{path} must be an object of string values"),
            path,
            artifact_id,
        ));
        return;
    };
    for (key, value) in parameters {
        if key.trim().is_empty() || !matches!(value, JsonValue::String(_)) {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("{path} keys and values must be non-empty strings"),
                path,
                artifact_id,
            ));
            return;
        }
    }
}

fn validate_connector_keyset_profile(
    value: &JsonValue,
    resource_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("{resource_path}.keyset");
    let Some(profile) = expect_object(value, path.clone(), diagnostics, artifact_id, CODE) else {
        return;
    };
    for field in [
        "filtersParam",
        "orFilterParam",
        "fieldsParam",
        "orderParam",
        "pageParam",
        "sizeParam",
        "watermarkField",
        "tieBreakerField",
    ] {
        require_non_empty_field(
            profile,
            field,
            format!("{path}.{field}"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    validate_unique_string_array_field(
        profile,
        "fields",
        format!("{path}.fields"),
        diagnostics,
        artifact_id,
        CODE,
    );
    match profile.get("pageSize") {
        Some(JsonValue::Number(size))
            if size.fract() == 0.0 && *size > 0.0 && *size <= f64::from(u32::MAX) => {}
        _ => diagnostics.push(invalid_diagnostic(
            CODE,
            "pageSize must be a positive 32-bit integer",
            format!("{path}.pageSize"),
            artifact_id,
        )),
    }
}

fn validate_connector_commands(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let JsonValue::Array(commands) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "commands must be an array",
            "payload.commands",
            artifact_id,
        ));
        return;
    };
    let mut identities: BTreeSet<(String, String, Vec<String>)> = BTreeSet::new();
    for (index, command_value) in commands.iter().enumerate() {
        validate_connector_command_entry(
            command_value,
            index,
            &mut identities,
            diagnostics,
            artifact_id,
        );
    }
}

fn validate_connector_command_entry(
    command_value: &JsonValue,
    index: usize,
    identities: &mut BTreeSet<(String, String, Vec<String>)>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("payload.commands[{index}]");
    let Some(command) = expect_object(command_value, path.clone(), diagnostics, artifact_id, CODE)
    else {
        return;
    };
    let operation = require_literal_field(
        command,
        "operation",
        &["create", "submit", "update", "cancel"],
        format!("{path}.operation"),
        diagnostics,
        artifact_id,
        CODE,
    );
    let action_id = command
        .get("actionId")
        .and_then(JsonValue::as_string)
        .unwrap_or("")
        .to_owned();
    let doctypes = match command.get("doctypes") {
        Some(JsonValue::Array(doctypes)) => doctypes
            .iter()
            .filter_map(JsonValue::as_string)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    let identity = (
        operation.as_deref().unwrap_or("").to_owned(),
        action_id,
        doctypes,
    );
    if !identities.insert(identity) {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "command profiles must be unique by operation, actionId, and doctypes",
            format!("{path}.operation"),
            artifact_id,
        ));
    }
    let endpoint = require_non_empty_field(
        command,
        "endpoint",
        format!("{path}.endpoint"),
        diagnostics,
        artifact_id,
        CODE,
    );
    if endpoint
        .as_deref()
        .is_some_and(|endpoint| endpoint.starts_with("//") || endpoint.contains("://"))
    {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "endpoint must be relative to the host-bound connector URL",
            format!("{path}.endpoint"),
            artifact_id,
        ));
    }
    let method = require_literal_field(
        command,
        "method",
        &["GET", "POST", "PUT", "DELETE"],
        format!("{path}.method"),
        diagnostics,
        artifact_id,
        CODE,
    );
    if method.as_deref() == Some("GET") {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "connector command mutations cannot use GET",
            format!("{path}.method"),
            artifact_id,
        ));
    }
    validate_connector_command_body_and_policies(command, &path, diagnostics, artifact_id, CODE);
}

fn validate_connector_command_body_and_policies(
    command: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    if command.contains_key("actionId") {
        validate_optional_non_empty_json_field(
            command,
            "actionId",
            format!("{path}.actionId"),
            diagnostics,
            artifact_id,
            code,
        );
    }
    if command.contains_key("doctypes") {
        validate_unique_string_array_field(
            command,
            "doctypes",
            format!("{path}.doctypes"),
            diagnostics,
            artifact_id,
            code,
        );
    }
    if command.contains_key("bodyFields") {
        validate_unique_string_array_field(
            command,
            "bodyFields",
            format!("{path}.bodyFields"),
            diagnostics,
            artifact_id,
            code,
        );
    }
    if command.contains_key("fixedBody") {
        validate_json_object_field(
            command,
            "fixedBody",
            format!("{path}.fixedBody"),
            diagnostics,
            artifact_id,
            code,
        );
    }
    if let Some(idempotency) = command.get("idempotency") {
        validate_connector_idempotency(idempotency, path, diagnostics, artifact_id);
    }
    if let Some(lifecycle) = command.get("lifecycle") {
        validate_connector_lifecycle(lifecycle, path, diagnostics, artifact_id);
    }
    if let Some(conflicts) = command.get("conflicts") {
        validate_connector_conflicts(conflicts, path, diagnostics, artifact_id);
    }
    validate_connector_body_allowlist(command, path, diagnostics, artifact_id);
}

fn validate_connector_body_allowlist(
    command: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let mut reserved = BTreeSet::new();
    if let Some(idempotency) = command.get("idempotency").and_then(JsonValue::as_object) {
        if let Some(field) = idempotency.get("keyField").and_then(JsonValue::as_string) {
            reserved.insert(field.to_owned());
        }
        if let Some(field) = idempotency
            .get("receipt")
            .and_then(JsonValue::as_object)
            .and_then(|receipt| receipt.get("tableField"))
            .and_then(JsonValue::as_string)
        {
            reserved.insert(field.to_owned());
        }
    }
    if let Some(lifecycle) = command.get("lifecycle").and_then(JsonValue::as_object) {
        for field in ["versionField", "statusField"] {
            if let Some(value) = lifecycle.get(field).and_then(JsonValue::as_string) {
                reserved.insert(value.to_owned());
            }
        }
    }
    let fixed = command
        .get("fixedBody")
        .and_then(JsonValue::as_object)
        .map_or_default(|fields| fields.keys().cloned().collect::<BTreeSet<_>>());
    if let Some(JsonValue::Array(fields)) = command.get("bodyFields") {
        for (index, value) in fields.iter().enumerate() {
            let Some(field) = value.as_string() else {
                continue;
            };
            if reserved.contains(field) || fixed.contains(field) {
                diagnostics.push(invalid_diagnostic(
                    CODE,
                    "bodyFields cannot override fixed or reserved connector fields",
                    format!("{path}.bodyFields[{index}]"),
                    artifact_id,
                ));
            }
        }
    }
}

fn validate_connector_idempotency(
    value: &JsonValue,
    command_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("{command_path}.idempotency");
    let Some(profile) = expect_object(value, path.clone(), diagnostics, artifact_id, CODE) else {
        return;
    };
    require_non_empty_field(
        profile,
        "keyField",
        format!("{path}.keyField"),
        diagnostics,
        artifact_id,
        CODE,
    );
    for field in ["headerName", "headerPrefix"] {
        if profile.contains_key(field) {
            validate_optional_non_empty_json_field(
                profile,
                field,
                format!("{path}.{field}"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
    }
    validate_connector_idempotency_receipt(profile, &path, diagnostics, artifact_id, CODE);
    for lookup_name in ["createLookup", "receiptLookup"] {
        let lookup_path = format!("{path}.{lookup_name}");
        match profile.get(lookup_name) {
            Some(lookup) => {
                validate_connector_lookup(lookup, &lookup_path, diagnostics, artifact_id);
            }
            None => diagnostics.push(invalid_diagnostic(
                CODE,
                format!("{lookup_name} is required"),
                lookup_path,
                artifact_id,
            )),
        }
    }
}

fn validate_connector_idempotency_receipt(
    profile: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let receipt_path = format!("{path}.receipt");
    let Some(receipt) = profile.get("receipt") else {
        diagnostics.push(invalid_diagnostic(
            code,
            "receipt is required",
            receipt_path,
            artifact_id,
        ));
        return;
    };
    let Some(receipt) = expect_object(
        receipt,
        receipt_path.clone(),
        diagnostics,
        artifact_id,
        code,
    ) else {
        return;
    };
    for field in [
        "doctype",
        "tableField",
        "keyField",
        "operationField",
        "parentField",
        "parentTypeField",
        "parentTableField",
    ] {
        require_non_empty_field(
            receipt,
            field,
            format!("{receipt_path}.{field}"),
            diagnostics,
            artifact_id,
            code,
        );
    }
    for field in [
        "identityField",
        "actionIdField",
        "doctypeField",
        "targetNameField",
        "timestampField",
    ] {
        if receipt.contains_key(field) {
            validate_optional_non_empty_json_field(
                receipt,
                field,
                format!("{receipt_path}.{field}"),
                diagnostics,
                artifact_id,
                code,
            );
        }
    }
}

fn validate_connector_lookup(
    value: &JsonValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(lookup) = expect_object(value, path, diagnostics, artifact_id, CODE) else {
        return;
    };
    for field in [
        "endpoint",
        "itemsPath",
        "filtersParam",
        "fieldsParam",
        "limitParam",
    ] {
        require_non_empty_field(
            lookup,
            field,
            format!("{path}.{field}"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    validate_unique_string_array_field(
        lookup,
        "fields",
        format!("{path}.fields"),
        diagnostics,
        artifact_id,
        CODE,
    );
    // Request N+1 results so either idempotency lookup can expose a duplicate.
    match lookup.get("maxResults") {
        Some(JsonValue::Number(max))
            if max.fract() == 0.0 && *max >= 2.0 && *max <= f64::from(u16::MAX) => {}
        _ => diagnostics.push(invalid_diagnostic(
            CODE,
            format!(
                "{path}.maxResults must be an integer from 2 through {} to detect duplicate matches",
                u16::MAX
            ),
            format!("{path}.maxResults"),
            artifact_id,
        )),
    }
    validate_connector_lookup_filters(lookup.get("filters"), path, diagnostics, artifact_id, CODE);
}

fn validate_connector_lookup_filters(
    value: Option<&JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    match value {
        None => {}
        Some(JsonValue::Array(filters)) => {
            for (index, filter_value) in filters.iter().enumerate() {
                let filter_path = format!("{path}.filters[{index}]");
                let Some(filter) = expect_object(
                    filter_value,
                    filter_path.clone(),
                    diagnostics,
                    artifact_id,
                    code,
                ) else {
                    continue;
                };
                if filter.contains_key("entity") {
                    validate_optional_non_empty_json_field(
                        filter,
                        "entity",
                        format!("{filter_path}.entity"),
                        diagnostics,
                        artifact_id,
                        code,
                    );
                }
                require_non_empty_field(
                    filter,
                    "field",
                    format!("{filter_path}.field"),
                    diagnostics,
                    artifact_id,
                    code,
                );
                if filter.contains_key("operator") {
                    validate_optional_non_empty_json_field(
                        filter,
                        "operator",
                        format!("{filter_path}.operator"),
                        diagnostics,
                        artifact_id,
                        code,
                    );
                }
                require_literal_field(
                    filter,
                    "value",
                    &[
                        "idempotency_key",
                        "operation",
                        "action_id",
                        "document_type",
                        "target_name",
                        "parent_type",
                        "parent_field",
                    ],
                    format!("{filter_path}.value"),
                    diagnostics,
                    artifact_id,
                    code,
                );
            }
        }
        Some(_) => diagnostics.push(invalid_diagnostic(
            code,
            "filters must be an array",
            format!("{path}.filters"),
            artifact_id,
        )),
    }
}

fn validate_connector_lifecycle(
    value: &JsonValue,
    command_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("{command_path}.lifecycle");
    let Some(lifecycle) = expect_object(value, path.clone(), diagnostics, artifact_id, CODE) else {
        return;
    };
    for field in [
        "documentPath",
        "identityField",
        "versionField",
        "statusField",
    ] {
        require_non_empty_field(
            lifecycle,
            field,
            format!("{path}.{field}"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
    for field in ["initialStatus", "submittedStatus", "cancelledStatus"] {
        if !matches!(lifecycle.get(field), Some(JsonValue::Number(value)) if value.fract() == 0.0) {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("{path}.{field} must be an integer"),
                format!("{path}.{field}"),
                artifact_id,
            ));
        }
    }
}

fn validate_connector_conflicts(
    value: &JsonValue,
    command_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let path = format!("{command_path}.conflicts");
    let JsonValue::Array(conflicts) = value else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "conflicts must be an array",
            path,
            artifact_id,
        ));
        return;
    };
    for (index, conflict_value) in conflicts.iter().enumerate() {
        let conflict_path = format!("{path}[{index}]");
        let Some(conflict) = expect_object(
            conflict_value,
            conflict_path.clone(),
            diagnostics,
            artifact_id,
            CODE,
        ) else {
            continue;
        };
        match conflict.get("status") {
            Some(JsonValue::Number(status))
                if status.fract() == 0.0 && (400.0..=599.0).contains(status) => {}
            _ => diagnostics.push(invalid_diagnostic(
                CODE,
                "status must be an HTTP error status",
                format!("{conflict_path}.status"),
                artifact_id,
            )),
        }
        if conflict.contains_key("errorType") {
            validate_optional_non_empty_json_field(
                conflict,
                "errorType",
                format!("{conflict_path}.errorType"),
                diagnostics,
                artifact_id,
                CODE,
            );
        }
        require_literal_field(
            conflict,
            "category",
            &["duplicate", "unique", "version"],
            format!("{conflict_path}.category"),
            diagnostics,
            artifact_id,
            CODE,
        );
    }
}

fn validate_connector_mappings(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    resource_ids: &BTreeSet<String>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(mappings)) => {
            for (index, mapping) in mappings.iter().enumerate() {
                let path = format!("payload.mappings[{index}]");
                let Some(mapping) =
                    expect_object(mapping, path.clone(), diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                let kind = require_literal_field(
                    mapping,
                    "kind",
                    &["field", "external_id"],
                    format!("{path}.kind"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                if let Some(resource_id) = require_non_empty_field(
                    mapping,
                    "resourceId",
                    format!("{path}.resourceId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                ) {
                    validate_resource_reference(
                        &resource_id,
                        format!("{path}.resourceId"),
                        diagnostics,
                        artifact_id,
                        resource_ids,
                    );
                }
                require_non_empty_field(
                    mapping,
                    "sourcePath",
                    format!("{path}.sourcePath"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                require_non_empty_field(
                    mapping,
                    "targetObjectKind",
                    format!("{path}.targetObjectKind"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );

                match kind.as_deref() {
                    Some("field") => {
                        require_non_empty_field(
                            mapping,
                            "targetField",
                            format!("{path}.targetField"),
                            diagnostics,
                            artifact_id,
                            CODE,
                        );
                    }
                    Some("external_id") => {
                        require_non_empty_field(
                            mapping,
                            "externalSource",
                            format!("{path}.externalSource"),
                            diagnostics,
                            artifact_id,
                            CODE,
                        );
                    }
                    _ => {}
                }
            }
        }
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "mappings must be an array",
            "payload.mappings",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "mappings is required",
            "payload.mappings",
            artifact_id,
        )),
    }
}

fn validate_connector_cursor_descriptor(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    resource_ids: &BTreeSet<String>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(cursor) = expect_object(value, "payload.cursor", diagnostics, artifact_id, CODE)
    else {
        return;
    };
    require_non_empty_field(
        cursor,
        "cursorId",
        "payload.cursor.cursorId",
        diagnostics,
        artifact_id,
        CODE,
    );
    if let Some(resource_id) = require_non_empty_field(
        cursor,
        "resourceId",
        "payload.cursor.resourceId",
        diagnostics,
        artifact_id,
        CODE,
    ) {
        validate_resource_reference(
            &resource_id,
            "payload.cursor.resourceId",
            diagnostics,
            artifact_id,
            resource_ids,
        );
    }
    require_non_empty_field(
        cursor,
        "storageKey",
        "payload.cursor.storageKey",
        diagnostics,
        artifact_id,
        CODE,
    );
}

fn validate_dead_letter_policy(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "connector.validation.invalid_payload";
    let Some(policy) = expect_object(
        value,
        "payload.deadLetterPolicy",
        diagnostics,
        artifact_id,
        CODE,
    ) else {
        return;
    };
    match policy.get("maxFailures") {
        Some(JsonValue::Number(value)) if value.fract() == 0.0 && *value >= 1.0 => {}
        _ => diagnostics.push(invalid_diagnostic(
            CODE,
            "deadLetterPolicy.maxFailures must be a positive integer",
            "payload.deadLetterPolicy.maxFailures",
            artifact_id,
        )),
    }
    require_literal_field(
        policy,
        "route",
        &["ledger", "queue"],
        "payload.deadLetterPolicy.route",
        diagnostics,
        artifact_id,
        CODE,
    );
}
fn validate_resource_reference(
    resource_id: &str,
    path: impl Into<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    resource_ids: &BTreeSet<String>,
) {
    if resource_ids.contains(resource_id) {
        return;
    }
    diagnostics.push(invalid_diagnostic(
        "connector.validation.unknown_resource",
        format!("resourceId '{resource_id}' does not match a declared connector resource"),
        path,
        artifact_id,
    ));
}
fn validate_catalog_connector_extensions(
    extensions: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(JsonValue::Object(extensions)) = extensions else {
        return;
    };
    for key in ["norn.shopify.graphql/1", "norn.catalog.projection/1"] {
        let Some(value) = extensions.get(key) else {
            continue;
        };
        let encoded = serde_json::to_string(value).unwrap_or_default();
        let valid = if key == "norn.shopify.graphql/1" {
            serde_json::from_str::<norn_ir::connector::ShopifyGraphqlProfile>(&encoded)
                .is_ok_and(|profile| matches!(profile.api_version.as_str(), "2026-01" | "2026-04"))
        } else {
            serde_json::from_str::<norn_ir::connector::CatalogProjectionProfile>(&encoded)
                .is_ok_and(|profile| {
                    !profile.actor.trim().is_empty()
                        && !profile.mapping.schema_id.is_empty()
                        && !profile.mapping.version.is_empty()
                        && profile.mapping.sha256.len() == 64
                        && profile
                            .mapping
                            .sha256
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                })
        };
        if !valid {
            diagnostics.push(invalid_diagnostic(
                "connector.validation.invalid_payload",
                "invalid typed catalogue protocol extension",
                format!("payload.extensions.{key}"),
                artifact_id,
            ));
        }
    }
}
