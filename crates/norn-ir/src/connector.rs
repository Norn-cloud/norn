//! Typed ConnectorIR root: the payload of a `connector` artifact (T1-IR-1d,
//! #2509).
//!
//! Connectors lower to MappingIR plus ordinary observe/perform actions (ruling
//! 2026-09-25 §11; kind consolidation is #2633, MappingIR is #2600). Until then
//! this root mirrors today's validator exactly; nested objects keep unchecked
//! members in `additional` (see [`crate::members`]).

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

use crate::action::ActionCommandOperation;
use crate::members::{
    IrVersion01, ProposalMetadata, WholeNumber, deserialize_u32, present_non_null,
};
use crate::rule::JsonObject;

/// Typed payload of a `connector` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorRoot {
    pub connector_version: IrVersion01,
    pub auth: ConnectorAuth,
    /// Non-empty set of source resources, unique by `resourceId`.
    pub resources: Vec<ConnectorResource>,
    /// Optional runtime source-version qualification before connector use.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ConnectorSupportedVersions")]
    pub supported_versions: Option<ConnectorSupportedVersions>,
    /// Optional declarative write capabilities. Existing read-only connector
    /// definitions omit this member and retain their previous wire form.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<ConnectorCommand>")]
    pub commands: Option<Vec<ConnectorCommand>>,
    /// Source-to-object mappings over declared resources.
    pub mappings: Vec<ConnectorMapping>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ConnectorCursor")]
    pub cursor: Option<ConnectorCursor>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "DeadLetterPolicy")]
    pub dead_letter_policy: Option<DeadLetterPolicy>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<String>")]
    pub custom_step_hook_ids: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<ConnectorCustomStepHook>")]
    pub custom_step_hooks: Option<Vec<ConnectorCustomStepHook>>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ProposalMetadata")]
    pub proposal: Option<ProposalMetadata>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// How the connector authenticates to its source. Secrets are references only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ConnectorAuth {
    None {
        #[serde(flatten)]
        additional: JsonObject,
    },
    Oauth2 {
        auth_id: String,
        authorization_url: String,
        token_url: String,
        scopes: Vec<String>,
        /// Optional secret reference used by hosts that exchange credentials
        /// for an access token. The secret itself is never stored in IR.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_secret_ref: Option<String>,
        #[serde(flatten)]
        additional: JsonObject,
    },
    ApiKey {
        auth_id: String,
        placement: ApiKeyPlacement,
        name: String,
        secret_ref: String,
        /// Static prefix prepended to the resolved secret, e.g. `token `.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        /// Optional validation/format contract for the resolved key material.
        /// Omission preserves legacy raw-key connector profiles.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<ApiKeyFormat>,
        #[serde(flatten)]
        additional: JsonObject,
    },
}

/// Where an API key is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyPlacement {
    /// Place the key in an HTTP header.
    Header,
    /// Place the key in a query parameter.
    Query,
}

/// Structural validation rule for a resolved API key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ApiKeyFormat {
    /// Accept any visible key value permitted by its placement.
    Raw,
    /// Require two non-empty visible components separated by this string.
    Pair { separator: String },
}

/// One source resource the connector reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ConnectorResource {
    Paginated {
        resource_id: String,
        endpoint: String,
        method: HttpMethod,
        page_param: String,
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "String")]
        page_size_param: Option<String>,
        /// Numbering convention; omitted roots default to one-based pages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_mode: Option<ConnectorPageMode>,
        /// Starting page number or row offset; defaults by `pageMode`.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_u32",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "Option<WholeNumber>")]
        page_start: Option<u32>,
        /// Declared page size, used for requests and short-page exhaustion.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_u32",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "Option<WholeNumber>")]
        page_size: Option<u32>,
        items_path: String,
        /// Treat a missing item path as an empty page. A present non-array
        /// value is always an invalid response.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        missing_items_is_empty: Option<bool>,
        /// Fixed query parameters declared by the source profile. String
        /// values may contain `{doctype}` and `{name}` bindings.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        query_params: BTreeMap<String, String>,
        #[serde(flatten)]
        additional: JsonObject,
    },
    Cursor {
        resource_id: String,
        endpoint: String,
        cursor_param: String,
        next_cursor_path: String,
        items_path: String,
        /// Treat a missing item path as an empty page. A present non-array
        /// value is always an invalid response.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        missing_items_is_empty: Option<bool>,
        /// Fixed source query parameters; values may include path bindings.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        query_params: BTreeMap<String, String>,
        #[serde(flatten)]
        additional: JsonObject,
    },
    Delta {
        resource_id: String,
        endpoint: String,
        since_param: String,
        updated_at_path: String,
        items_path: String,
        /// Treat a missing item path as an empty page. A present non-array
        /// value is always an invalid response.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        missing_items_is_empty: Option<bool>,
        /// Fixed source query parameters; values may include path bindings.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        query_params: BTreeMap<String, String>,
        /// Optional compound-key paging contract for safe delta scans.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keyset: Option<Box<ConnectorKeysetProfile>>,
        #[serde(flatten)]
        additional: JsonObject,
    },
    Webhook {
        resource_id: String,
        event_type: String,
        payload_path: String,
        #[serde(flatten)]
        additional: JsonObject,
    },
    CsvImport {
        resource_id: String,
        delimiter: CsvDelimiter,
        columns: Vec<String>,
        #[serde(flatten)]
        additional: JsonObject,
    },
}

/// HTTP method of a paginated resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum HttpMethod {
    /// Read or call using HTTP GET.
    #[serde(rename = "GET")]
    Get,
    /// Create or call using HTTP POST.
    #[serde(rename = "POST")]
    Post,
    /// Replace/update a source resource using HTTP PUT.
    #[serde(rename = "PUT")]
    Put,
    /// Delete or cancel a source resource using HTTP DELETE.
    #[serde(rename = "DELETE")]
    Delete,
}

/// Pagination numbering convention for a paginated resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorPageMode {
    /// The parameter is a one-based page number (the default for older roots).
    Page,
    /// The parameter is a zero-based row offset.
    Offset,
}

/// A typed command capability admitted by a connector definition.
///
/// `endpoint` is relative to the host-bound connection URL and may contain
/// `{doctype}` and `{name}` path bindings. `doctypes` may be empty when the
/// action itself already narrows the target document type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorCommand {
    /// Optional action id to narrow this capability; omitted means any action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// Closed source operation matched to ActionIR.
    pub operation: ActionCommandOperation,
    /// Optional target document type allowlist; empty means action-authorized types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub doctypes: Vec<String>,
    /// Relative endpoint path, with `{doctype}` / `{name}` path bindings.
    pub endpoint: String,
    /// HTTP verb used for this command.
    pub method: HttpMethod,
    /// Allowlisted input fields that may be copied into the outbound body.
    /// `*` admits all action fields; caller-supplied guard fields remain rejected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_fields: Vec<String>,
    /// Fixed fields supplied by the connector declaration. Reserved fields
    /// from lifecycle/idempotency contracts always override these values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fixed_body: JsonObject,
    /// Optional remote idempotency and receipt contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<ConnectorIdempotencyProfile>,
    /// Optional source document version and lifecycle contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<ConnectorLifecycleProfile>,
    /// Source error classifications for known write conflicts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ConnectorConflictRule>,
    /// Child tables that this command may merge by declared ownership.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge_profiles: Vec<ConnectorChildMergeProfile>,
}

/// Runtime check that the remote source reports a supported set of app majors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorSupportedVersions {
    /// Relative endpoint used to read source versions.
    pub endpoint: String,
    /// JSON Pointer to the response object containing app versions.
    pub response_path: String,
    /// Allowed major versions for each reported application.
    pub components: Vec<ConnectorVersionComponent>,
    /// Require all components to report the same major version.
    pub require_matching_majors: bool,
}

/// One source application and its accepted major versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorVersionComponent {
    /// Source application's reported name (for example `frappe`).
    pub app: String,
    /// Accepted numeric major versions.
    #[serde(deserialize_with = "deserialize_major_versions")]
    #[schemars(with = "Vec<WholeNumber>")]
    pub majors: Vec<u16>,
}

/// Merge rules for one source child-table field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorChildMergeProfile {
    /// Parent field holding the child rows.
    pub field: String,
    /// Child row field that identifies rows owned by Norn.
    pub ownership_field: String,
    /// Child row identity field used to preserve names on updates.
    pub row_key_field: String,
    /// Unique business-key fields checked against desired and foreign rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_key_fields: Vec<String>,
}

/// Declarative remote idempotency contract for one command capability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorIdempotencyProfile {
    /// Source field that stores the command's stable idempotency key.
    pub key_field: String,
    /// Optional header carrying the key on writes and reconciliation reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    /// Optional static prefix applied before the header key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_prefix: Option<String>,
    /// Child-table receipt columns and parent linkage.
    pub receipt: ConnectorReceiptContract,
    /// Lookup used to reconcile a create by its unique idempotency key.
    pub create_lookup: ConnectorLookup,
    /// Lookup used to find a prior immutable receipt, globally or for a target.
    pub receipt_lookup: ConnectorLookup,
}

/// Receipt child-table schema and the columns needed for lookup and recovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorReceiptContract {
    /// Source child-row doctype used in child-table filters.
    pub doctype: String,
    /// Parent document field that stores the receipt rows.
    pub table_field: String,
    /// Child row field storing the stable idempotency key.
    pub key_field: String,
    /// Child row field storing the operation name.
    pub operation_field: String,
    /// Optional child row identity field required on persisted receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_field: Option<String>,
    /// Child row field linking to the parent document.
    pub parent_field: String,
    /// Child row field linking to the parent's doctype.
    pub parent_type_field: String,
    /// Child row field linking to the parent table field.
    pub parent_table_field: String,
    /// Optional child row timestamp field used for validation or audit output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_field: Option<String>,
    /// Optional child row field storing the action id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id_field: Option<String>,
    /// Optional child row field storing the document type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doctype_field: Option<String>,
    /// Optional child row field storing the target document name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_name_field: Option<String>,
}

/// Configured query used to find existing records or receipts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorLookup {
    /// Relative request endpoint, with optional path bindings.
    pub endpoint: String,
    /// JSON Pointer or simple JSONPath path to the result array.
    pub items_path: String,
    /// Projection fields to request from the source.
    pub fields: Vec<String>,
    /// Query parameter carrying the serialized filters array.
    pub filters_param: String,
    /// Query parameter carrying the serialized field projection.
    pub fields_param: String,
    /// Query parameter carrying the result limit.
    pub limit_param: String,
    /// Maximum number of results needed to detect ambiguity.
    #[serde(deserialize_with = "deserialize_u16")]
    #[schemars(with = "WholeNumber")]
    pub max_results: u16,
    /// Declared filters evaluated for each reconciliation context.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<ConnectorLookupFilter>,
}

/// One equality filter in a configured lookup. When `entity` is present, the
/// query encoder emits the four-element child-table tuple; otherwise it emits
/// a normal three-element filter tuple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorLookupFilter {
    /// Optional child doctype prefix for source-specific child-table filters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Source field filtered by this declaration.
    pub field: String,
    /// Filter operator; defaults to equality.
    #[serde(default = "default_filter_operator")]
    pub operator: String,
    /// Runtime context member used as the filter value.
    pub value: ConnectorLookupValue,
}

fn default_filter_operator() -> String {
    "=".to_owned()
}

/// Runtime value inserted into a declarative lookup filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorLookupValue {
    /// Stable command idempotency key.
    IdempotencyKey,
    /// Operation name such as `create` or `cancel`.
    Operation,
    /// Action identifier.
    ActionId,
    /// Target source doctype.
    DocumentType,
    /// Target source document name; omitted for global lookup.
    TargetName,
    /// Parent doctype for a receipt row.
    ParentType,
    /// Parent child-table field for a receipt row.
    ParentField,
}

/// Numeric source-document lifecycle fields and statuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorLifecycleProfile {
    /// JSON Pointer to the source document in a response.
    pub document_path: String,
    /// Identity field within the selected document.
    pub identity_field: String,
    /// Version field used for optimistic mutation guards.
    pub version_field: String,
    /// Numeric document status field.
    pub status_field: String,
    /// Numeric status for a newly created/initial document.
    #[serde(deserialize_with = "deserialize_nonnegative_i64")]
    #[schemars(with = "WholeNumber")]
    pub initial_status: i64,
    /// Numeric status after submit.
    #[serde(deserialize_with = "deserialize_nonnegative_i64")]
    #[schemars(with = "WholeNumber")]
    pub submitted_status: i64,
    /// Numeric status after cancellation.
    #[serde(deserialize_with = "deserialize_nonnegative_i64")]
    #[schemars(with = "WholeNumber")]
    pub cancelled_status: i64,
}

/// A source-specific conflict response mapped to a stable connector outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorConflictRule {
    /// HTTP status that activates this classification.
    #[serde(deserialize_with = "deserialize_u16")]
    #[schemars(with = "WholeNumber")]
    pub status: u16,
    /// Optional source error type discriminator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// Stable connector conflict category.
    pub category: ConnectorConflictCategory,
}

/// Typed HTTP query and ordering contract for a keyset resource sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorKeysetProfile {
    /// Query parameter carrying conjunctive filters.
    pub filters_param: String,
    /// Query parameter carrying the compound cursor disjunction.
    pub or_filter_param: String,
    /// Query parameter carrying projected fields.
    pub fields_param: String,
    /// Query parameter carrying ordering.
    pub order_param: String,
    /// Query parameter carrying row offset.
    pub page_param: String,
    /// Query parameter carrying maximum page size.
    pub size_param: String,
    /// Field used as the high-water component.
    pub watermark_field: String,
    /// Stable field that orders rows sharing the high-water value.
    pub tie_breaker_field: String,
    /// Fields requested from the source.
    pub fields: Vec<String>,
    /// Maximum number of rows per source page.
    #[serde(deserialize_with = "deserialize_u32")]
    #[schemars(with = "WholeNumber")]
    pub page_size: u32,
}

fn deserialize_whole_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    WholeNumber::deserialize(deserializer).map(|number| number.0)
}

fn deserialize_u16<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    u16::try_from(deserialize_whole_number(deserializer)?).map_err(serde::de::Error::custom)
}

fn deserialize_major_versions<'de, D>(deserializer: D) -> Result<Vec<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<WholeNumber>::deserialize(deserializer)?
        .into_iter()
        .map(|number| u16::try_from(number.0).map_err(serde::de::Error::custom))
        .collect()
}

fn deserialize_optional_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_u32(deserializer).map(Some)
}

fn deserialize_nonnegative_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    i64::try_from(deserialize_whole_number(deserializer)?).map_err(serde::de::Error::custom)
}

/// Typed conflict categories surfaced from a source response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorConflictCategory {
    /// Source rejected a command because its idempotency key already exists.
    Duplicate,
    /// Source rejected a declared unique-key constraint.
    Unique,
    /// Source rejected a stale optimistic version.
    Version,
}

/// Field delimiter of a CSV import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CsvDelimiter {
    #[serde(rename = ",")]
    Comma,
    #[serde(rename = ";")]
    Semicolon,
    #[serde(rename = "\t")]
    Tab,
}

/// One source-path-to-object mapping over a declared resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ConnectorMapping {
    /// Map a source value onto an object field.
    Field {
        resource_id: String,
        source_path: String,
        target_object_kind: String,
        target_field: String,
        #[serde(flatten)]
        additional: JsonObject,
    },
    /// Map a source value onto an object's external identity.
    ExternalId {
        resource_id: String,
        source_path: String,
        target_object_kind: String,
        external_source: String,
        #[serde(flatten)]
        additional: JsonObject,
    },
}

/// Durable sync cursor over a declared resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorCursor {
    pub cursor_id: String,
    pub resource_id: String,
    pub storage_key: String,
    /// Members the validator does not check, kept verbatim (for example
    /// `initialValue`).
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Versioned declarative feed plan carried at `ConnectorRoot.extensions["norn.poll/1"]`.
///
/// The feeds are processed in order. A fanout feed enumerates one child
/// resource for every key found in its previously completed parent feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorPollProfile {
    pub feeds: Vec<ConnectorPollFeed>,
}

/// One ordered source feed in a connector poll profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorPollFeed {
    pub resource_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanout: Option<ConnectorPollFanout>,
    /// Stable key path for detecting a repeated record across pages, even if
    /// other fields in the record changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_key_path: Option<String>,
}

/// Parent-key binding used to enumerate one child resource per parent record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectorPollFanout {
    /// Resource id of an earlier feed in the same profile.
    pub parent_resource_id: String,
    /// Endpoint path binding supplied to the child resource request.
    pub path_binding: String,
    /// JSON Pointer or simple `$.a.b` path in each parent item.
    pub key_path: String,
}

/// Reserved connector extension key for the generic poll profile.
pub const CONNECTOR_POLL_EXTENSION_KEY: &str = "norn.poll/1";

/// Where records that keep failing go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeadLetterPolicy {
    /// Positive failure count before a record is dead-lettered.
    pub max_failures: WholeNumber,
    pub route: DeadLetterRoute,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Dead-letter destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeadLetterRoute {
    Ledger,
    Queue,
}

/// A capability-scoped custom step hook installed from an extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorCustomStepHook {
    pub hook_id: String,
    pub install_id: String,
    pub extension_version: String,
    pub required_services: Vec<String>,
    pub scope_capabilities: Vec<String>,
    pub input_schema: JsonObject,
    pub output_schema: JsonObject,
    pub error_schema: JsonObject,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Closed Shopify GraphQL protocol at the connector perimeter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShopifyGraphqlProfile {
    /// Pinned versioned Admin API surface.
    pub api_version: String,
}

/// One approved catalogue projector bound to an exact MappingIR artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogProjectionProfile {
    /// Reviewed mapping identity; installation is the approval boundary.
    pub mapping: crate::mapping::SourceBinding,
    /// Explicit host-configured service principal used for action authorization.
    pub actor: String,
}
