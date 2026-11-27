// The cell host's telemetry: what a request, a sync, an alarm and a live
// publish cost and how they ended, exported over OTLP (otlp.mjs). It wraps the
// Durable Object's methods from outside, so the host's own code is unchanged
// and nothing is recorded when export is off (OTEL-OFF-1).
//
// Attributes come from fixed tables only (OTEL-NO-TENANT-1): route templates
// (otlp.mjs CELL_ROUTES), allowlisted methods, environments and refusal codes,
// status classes and outcomes. Never the scope, a principal, an object id, a
// path segment or a body.
//
// OTEL-NEVER-FAILS-1: a wrapper reads only what the handler already produced
// (its status, and a refusal's body text the host noted with `noteRefusal`),
// records nothing on the handler's path, and hands recording and export to a
// background task the triggering event passes to waitUntil. Every hook is
// guarded; a telemetry failure never changes a result or adds a retry.
import { COUNT_BUCKETS, ENVIRONMENTS, cellRoute, methodOf, statusClass, telemetry } from "./otlp.mjs";

export const SERVICE = "norn-cell";
const CELL_HEADER = "x-norn-cell";
/** A refusal body longer than this is not parsed for its code. */
const MAX_REFUSAL_TEXT = 8192;

/** Every refusal code the host and kernel write (the norn-kernel, norn-cell-do, norn-cell, norn-api, norn-authz and norn-decisions crates, worker.mjs, subscribe.mjs), by family; any other is "other". */
const REFUSAL_CODES = new Set(Object.entries({
  action:
    "aggregate_bound_exceeded aggregate_bytes_exceeded aggregate_stale_read aggregate_unavailable " +
    "approved_intent_changed authorized_binding_mismatch blank_field conflict derivation_failed duplicate_precondition " +
    "expression_budget_exceeded id_too_long idempotency_conflict idempotency_key_too_long " +
    "idempotency_without_receipt input_bound_exceeded invalid_input invalid_transition_identity " +
    "legacy_receipt logical_time_rollback missing_precondition missing_scope owner_version_conflict " +
    "owner_version_overflow permission_denied precondition_failed precondition_stale_read " +
    "receipt_binding_mismatch receipt_body_forbidden receipt_effect_unsupported receipt_not_accepted " +
    "receipt_not_pending receipt_reference_invalid receipt_replayed receipt_result_type " +
    "receipt_run_mismatch receipt_unknown receipts_unconfigured slice_object_missing " +
    "slice_unavailable target_not_found transition_binding_mismatch transition_protocol_conflict " +
    "unsupported_effect write_mode_failed effect_kind_mismatch",
  analytics:
    "dataset_cold engine frontier_behind frontier_moved invalid resource_exhausted too_large " +
    "unauthorized unavailable unconfigured unknown unsettled",
  approval:
    "attempt_ended consumption_invalid definition_changed expired refusal_invalid refused rejected",
  artifact_set: "action.effect_kind_mismatch",
  authn: "required",
  authz:
    "access.actions_empty access.context_reserved access.decimal_comparison " +
    "access.generic_rule_invalid access.id_empty access.marking_action_invalid " +
    "access.marking_id_invalid access.marking_resource_invalid access.markings_empty " +
    "access.name_invalid access.principal_attribute_conflict access.principal_operator_invalid " +
    "access.principal_projection_required access.principal_reserved access.projection_required " +
    "access.rel_unsupported access.resource_operator_invalid access.roles_empty access.rule_duplicate " +
    "access.scope_org_required denied deny.absent_scope deny.access deny.access_error deny.capability " +
    "deny.delegation deny.invalid_request entitlement.action_mismatch entitlement.actions_invalid " +
    "entitlement.grantee_type_invalid entitlement.identifier_invalid entitlement.link_invalid " +
    "entitlement.linked_record_mismatch entitlement.owner_mismatch " +
    "entitlement.scope_attribute_invalid entitlement.scope_attribute_mismatch " +
    "entitlement.scope_invalid entitlement.target_type_mismatch entitlement.value_invalid " +
    "grant.invalid policy.invalid policy.lint policy.parse schema.invalid schema.parse " +
    "schema.scope_type",
  bulk:
    "above_max attempt_busy attempt_unknown below_min digest_mismatch invalid_finding " +
    "missing_column not_boolean not_date not_decimal not_in_enum not_integer not_timestamp " +
    "out_of_range required residue_exceeded scale_exceeded too_long too_short unmapped " +
    "unreadable unsupported_encoding unsupported_law unsupported_mapping unsupported_marking " +
    "unwritable",
  cell:
    "base_mismatch not_provisioned provision_conflict provision_refused reserved_artifact " +
    "fenced scope_conflict sqlite unopenable verifying",
  commit:
    "conflict dangling_link duplicate_key duplicate_link duplicate_link_read effective_values " +
    "event_limit host identifier_too_long immutable_object invalid_link_read invalid_predicate_read " +
    "invariant_budget_exceeded invariant_model invariant_violation link_cardinality link_conflict " +
    "link_read_conflict link_read_too_large link_required link_target not_pending " +
    "observation_authorization overflow owned_rows_protected predicate_read_conflict " +
    "traversal_limit_exceeded type_mismatch unsupported_link_read unsupported_predicate_read",
  connection:
    "conflict egress grant_forbidden invalid not_found oauth_callback oauth_pending oauth_received " +
    "oauth_state provider_refused provider_unsupported reconnect_needed unavailable vault vault_key",
  connector: "bind read run",
  dataset: "invalid manifest not_found request_conflict stale_landing stale_pins uploads_unconfigured",
  decision:
    "envelope_exceeded expired guard_error guard_false host idempotency_conflict invalid_output " +
    "invariant_budget_exceeded invariant_violation law_not_satisfied not_found not_in_slice " +
    "not_pending note_too_long object_exists object_missing overflow perform_outside_main " +
    "principal_input_unavailable read_only_world resolution_invalid resolved source_owned_target " +
    "stale_state " +
    "turn_envelope_exceeded unknown_choice unknown_type unresolved_lifecycle",
  defs:
    "dev_sources_only live_data_refused live_data_unbounded live_data_unchecked live_data_violation " +
    "migration_refused not_installed not_provisioned provision_refused receipt_context_unavailable " +
    "refused stale version_not_found",
  definition:
    "already_retired approval_action_unknown approval_duplicate approval_invalid " +
    "approval_role_unknown automation_action_target_mismatch automation_action_unknown " +
    "automation_guard_invalid automation_input_invalid automation_invalid " +
    "automation_lifecycle_state_unknown automation_lifecycle_unknown automation_object_type_unknown " +
    "automation_role_unknown automation_stale_property_invalid " +
    "binding_connector_invalid binding_digest_mismatch binding_invalid_payload " +
    "binding_label_invalid binding_mapping_invalid binding_mapping_unknown_field binding_not_nfc " +
    "binding_role_unknown binding_version_mismatch cursor_invalid flow_action_declared " +
    "flow_artifact_conflict flow_artifact_foreign flow_command_unverifiable flow_function_unknown " +
    "flow_id_mismatch flow_label_invalid flow_runs_live flow_scope_conflict flow_shape_invalid " +
    "id_in_use id_invalid id_reserved invalid_payload kind_unknown layout_invalid not_found " +
    "payload_too_large question_invalid question_unavailable refused signoff_mismatch " +
    "signoff_required signoff_unavailable tile_id_invalid tile_title_invalid version_conflict " +
    "view_invalid",
  entitlement: "linked_record none owner_only",
  guard: "idempotency_conflict not_found",
  ingress: "limit_exceeded",
  lifecycle: "declared_transition guard guard_evaluable immutable_state initial_state transition_source",
  live:
    "invalid_id not_subscribable too_many_sockets too_many_subscriptions unknown_message " +
    "upgrade_required",
  object: "not_found scan_bound_exceeded snapshot_moved snapshot_unavailable unknown_type",
  observations: "too_large",
  privacy: "marked_column",
  query:
    "cursor_invalid invalid limit_invalid op_invalid property_unknown sort_invalid too_many " +
    "value_invalid",
  question: "bucket_bound_exceeded marked_history not_found refused",
  request: "invalid too_large",
  reveal: "idempotency_conflict",
  route: "method_not_allowed not_found",
  run:
    "exists flow_inadmissible flow_not_found not_found result_not_pending start_conflict " +
    "start_invalid start_refused state_invalid turn_refused",
  scope: "denied missing",
  source:
    "auth_unsupported bound cell_busy connector_missing cursor_conflict http_status ingest " +
    "lease_fenced no_cell not_running profile_invalid profile_missing response_shape " +
    "scope_mismatch seal secret_missing secret_unbound sqlite sync_running token_refused unknown",
  subscription: "empty_bound over_profile unknown_type",
  transform: "not_found row_not_found snapshot_moved",
  upload:
    "changed invalid_prefix invalid_request not_completed not_csv not_found not_recorded read_failed " +
    "refused storage_unconfigured too_large",
}).flatMap(([family, codes]) => codes.split(" ").map((code) => `${family}.${code}`)));

/** A refusal code when the host writes it, else "other". */
export const refusalCodeOf = (code) => (typeof code === "string" && REFUSAL_CODES.has(code) ? code : "other");

/** A refusal's code from a `norn/refusal@2` body the host already holds as text, bounded; "other" when it names none we know. */
export function refusalCode(text) {
  if (typeof text !== "string" || text.length > MAX_REFUSAL_TEXT) return "other";
  try {
    return refusalCodeOf(JSON.parse(text)?.code);
  } catch {
    return "other";
  }
}

/** What a request does, from its method and route template. */
export function kindOf(method, route) {
  if (route === "/v1/subscribe") return "subscribe";
  if (method === "GET" || method === "HEAD") return "read";
  switch (route) {
    case "/v1/actions/{action}": return "action";
    case "/v1/decisions/{decision}:decide": return "decision";
    case "/v1/cell:provision": return "provision";
    case "/v1/cell/definitions:plan": return "plan";
    case "/v1/cell/definitions:apply": return "apply";
    case "/v1/definitions:admit": return "admission";
    case "/v1/definitions:install": return "install";
    case "/v1/sources/{source}:sync": return "sync";
    case "other": return "other";
  }
  if (route.startsWith("/v1/questions")) return "question";
  if (route.endsWith(":load") || route.startsWith("/v1/uploads")) return "load";
  return "write";
}

/** How a request ended: committed, pending (parked for approval), refused or error. */
export function outcomeOf(status) {
  if (status === 202) return "pending";
  if (status < 400) return "ok";
  if (status < 500) return "refused";
  return "error";
}

/** The environment part of the router's cell header, when it is a configured stage name. */
function environmentOf(header) {
  try {
    const environment = JSON.parse(header)?.[1];
    return ENVIRONMENTS.has(environment) ? environment : "other";
  } catch {
    return "other";
  }
}

/** Response -> refusal body text the host noted (never read from the stream). */
const refusals = new WeakMap();

/**
 * The host notes a refusal's body, which it already holds as a string, so the
 * refusal's code can be counted later without reading the response stream.
 * Returns `response`; a no-op when export is off, never throws.
 */
export function noteRefusal(env, response, body) {
  try {
    if (response.status >= 400 && typeof body === "string" && body.length <= MAX_REFUSAL_TEXT && telemetry(env, SERVICE) !== null) refusals.set(response, body);
  } catch {
    // Best effort.
  }
  return response;
}

/** Wait one timer turn, so recording never runs on the handler's path. */
const later = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Hand `work` to the event's waitUntil: run after a timer turn, then export. Never throws. */
function background(t, ctx, work) {
  try {
    const task = later().then(() => {
      try {
        work();
      } catch {
        t.log("telemetry record failed");
      }
      return t.pump();
    }).catch(() => {});
    if (typeof ctx?.waitUntil === "function") ctx.waitUntil(task);
  } catch {
    // Best effort.
  }
}

/**
 * Record one finished request from bounded metadata: the method, the path's
 * template, the router's environment header, the status, and a refusal's noted
 * body. `seen` is that metadata, captured before the handler ran.
 */
export function recordRequest(t, seen, status, refusal, startedMs, endedMs) {
  const route = cellRoute(seen.pathname);
  const method = methodOf(seen.method);
  const kind = kindOf(method, route);
  const outcome = status === null ? "error" : outcomeOf(status);
  const environment = environmentOf(seen.cell);
  const ms = Math.max(0, endedMs - startedMs);
  const status_class = status === null ? "5xx" : statusClass(status);
  const code = status !== null && status >= 400 ? refusalCode(refusal) : "";
  const base = { route, method, kind, environment };
  t.count("norn_cell_requests_total", { ...base, status_class, outcome });
  t.observe("norn_cell_request_duration_ms", { ...base, outcome }, ms);
  if (code !== "") t.count("norn_cell_refusals_total", { route, kind, code, status_class, environment });
  if (kind === "action") t.count("norn_cell_actions_total", { outcome, environment });
  if (kind === "decision") t.count("norn_cell_decisions_total", { outcome, environment });
  if (kind === "provision" || kind === "plan" || kind === "apply" || kind === "admission" || kind === "install") {
    t.observe("norn_cell_definitions_stage_ms", { stage: kind, outcome, environment }, ms);
  }
  t.span(`cell ${kind} ${route}`, { "http.route": route, "http.request.method": method, "http.response.status_code": status ?? 500, "norn.kind": kind, "norn.outcome": outcome, "norn.refusal.code": code, "norn.environment": environment }, startedMs, endedMs, outcome === "error");
}

const SYNC_OUTCOMES = new Set(["ok", "partial", "failed"]);

/** Record one finished sync from its counts (runs in the background task). */
export function recordSync(t, record, endedMs) {
  const outcome = SYNC_OUTCOMES.has(record?.outcome) ? record.outcome : "other";
  // ok, partial (some rows quarantined) or failed, with the failure's code.
  const code = Array.isArray(record?.error) ? refusalCodeOf(record.error[0]) : "";
  const count = (value) => (Number.isFinite(value) && value >= 0 ? value : 0);
  const wallMs = count(record?.wallMs);
  t.count("norn_cell_sync_runs_total", { outcome, code });
  t.observe("norn_cell_sync_duration_ms", { outcome }, wallMs);
  t.count("norn_cell_sync_rows_read_total", { outcome }, count(record?.rowsRead));
  t.count("norn_cell_sync_rows_landed_total", { outcome }, count(record?.rowsLanded));
  t.count("norn_cell_sync_rows_quarantined_total", { outcome }, count(record?.rowsQuarantined));
  t.observe("norn_cell_sync_rows_landed", { outcome }, count(record?.rowsLanded), "1", COUNT_BUCKETS);
  t.span("cell sync", { "norn.outcome": outcome, "norn.sync.rows_landed": count(record?.rowsLanded), "norn.sync.rows_quarantined": count(record?.rowsQuarantined), "norn.refusal.code": code }, endedMs - wallMs, endedMs, outcome === "failed");
}

/**
 * A finished source sync, from the counts runSync already logs (never what it
 * read). Its duration is the sync lag: from the sync starting to its rows
 * being committed and readable. Nothing is recorded on the caller's path: the
 * counts are copied and recorded in the background task handed to
 * `ctx.waitUntil`. Never throws.
 */
export function syncFinished(env, record, ctx) {
  try {
    const t = telemetry(env, SERVICE);
    if (t === null) return;
    const ended = Date.now();
    const counts = { outcome: record?.outcome, error: Array.isArray(record?.error) ? [record.error[0]] : undefined, wallMs: record?.wallMs, rowsRead: record?.rowsRead, rowsLanded: record?.rowsLanded, rowsQuarantined: record?.rowsQuarantined };
    background(t, ctx, () => recordSync(t, counts, ended));
  } catch {
    // OTEL-NEVER-FAILS-1.
  }
}

/** Telemetry for `self`, or null; never throws. */
const of = (self) => {
  try {
    return telemetry(self.env, SERVICE);
  } catch {
    return null;
  }
};

/**
 * Wrap the cell object's handlers. `fetch` is timed and classified; `publish`
 * counts its fan-out and the snapshots it sends; `alarm`, when the class has
 * one (TIMER-ALARM-1), records how late it fired against the deadline it was
 * armed for (`this.wake?.armed`, falling back to `this.armed`).
 */
export function instrument(Cell) {
  const proto = Cell.prototype;
  const fetch = proto.fetch;
  proto.fetch = async function instrumentedFetch(request) {
    const t = of(this);
    if (t === null) return fetch.call(this, request);
    let seen = null;
    let started = 0;
    try {
      seen = { method: request.method, pathname: new URL(request.url).pathname, cell: request.headers.get(CELL_HEADER) };
      started = Date.now();
    } catch {
      // Unrecorded.
    }
    let status = null;
    let refusal;
    try {
      const response = await fetch.call(this, request);
      try {
        status = response.status;
        refusal = refusals.get(response);
      } catch {
        // Unrecorded.
      }
      return response;
    } finally {
      if (seen !== null) {
        let ended = started;
        try {
          ended = Date.now();
        } catch {
          // Unrecorded duration.
        }
        background(t, this.ctx, () => recordRequest(t, seen, status, refusal, started, ended));
      }
    }
  };
  const publish = proto.publish;
  if (typeof publish === "function") {
    proto.publish = function instrumentedPublish(...args) {
      const t = of(this);
      let sockets = 0;
      try {
        if (t !== null) sockets = this.ctx.getWebSockets("member").length;
      } catch {
        // Unrecorded.
      }
      const result = publish.apply(this, args);
      if (t !== null && sockets > 0) {
        // The host's publish resolves to the number of changed snapshots it sent.
        Promise.resolve(result).then((sent) => {
          background(t, this.ctx, () => {
            t.count("norn_cell_live_publishes_total", {});
            t.observe("norn_cell_live_fanout", {}, sockets, "1", COUNT_BUCKETS);
            if (Number.isInteger(sent) && sent > 0) t.count("norn_cell_live_snapshots_total", {}, sent);
          });
        }, () => {});
      }
      return result;
    };
  }
  const alarm = proto.alarm;
  if (typeof alarm === "function") {
    proto.alarm = async function instrumentedAlarm(info) {
      const t = of(this);
      if (t === null) return alarm.call(this, info);
      let armed = null;
      let started = 0;
      let retry = false;
      try {
        armed = typeof this.wake?.armed === "number" ? this.wake.armed : typeof this.armed === "number" ? this.armed : null;
        started = Date.now();
        retry = (info?.retryCount ?? 0) > 0;
      } catch {
        // Unrecorded.
      }
      let failed = true;
      try {
        const result = await alarm.call(this, info);
        failed = false;
        return result;
      } finally {
        try {
          const ended = Date.now();
          background(t, this.ctx, () => {
            const lag = armed === null ? -1 : Math.max(0, started - armed);
            if (armed !== null) t.observe("norn_cell_alarm_lag_ms", { retry }, lag);
            t.count("norn_cell_alarms_total", { outcome: failed ? "error" : "ok", retry });
            t.span("cell alarm", { "norn.outcome": failed ? "error" : "ok", "norn.alarm.retry": retry, "norn.alarm.lag_ms": lag }, started, ended, failed);
          });
        } catch {
          // OTEL-NEVER-FAILS-1: the alarm's own result stands.
        }
      }
    };
  }
  return Cell;
}
