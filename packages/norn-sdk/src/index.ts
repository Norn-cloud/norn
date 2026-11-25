/** Core IR and checker types generated from the Rust OpenAPI projection. */
import type { components } from "./generated/norn-api.ts"
export type { components } from "./generated/norn-api.ts"
export type Schemas = components["schemas"]

/** The generic governed-action client, and typed reads and writes of any route of the contract. */
export { createIntentKey, createNornClient, EMPTY_SUCCESS, FILE_MEDIA, NO_HOST_ERROR, NornUnavailable, NornUnexpectedResponse, readRefusal } from "./client.ts"
export type {
  ActionCatalog, ActionContract, ActionId, ActionInput, ActionMapShape, ActionOutput, ActionReceipt, ActionSummary,
  CallOptions, CallResult, Diagnostic, ExactDocument, Fetch, FileResult, FileRoute, GetRoute, Immutable, Intent, NornClient,
  NornClientOptions, ObjectFields, ObjectMapShape, Operation, PendingDecision, PostRoute, Refusal, RefusalCategory, RefusalReason,
  RouteArgs, RouteCall, RouteResult, Scope, Success, SuccessMedia, TraceContext, UiEffects, UntypedActions,
} from "./client.ts"

/** Exact JSON: numbers kept as their source text. */
export { parseExactJson } from "./exact-json.ts"
export type { ExactJson } from "./exact-json.ts"

/** The live-read client (`norn.live.v1`). */
export { LIVE_PROTOCOL, LIVE_UNAVAILABLE, LiveClient } from "./live.ts"
export type {
  LiveClientMessage, LiveClientRefusal, LiveListener, LiveOptions, LiveRefusal, LiveServerMessage, LiveSnapshot, LiveState,
  LiveSubscribeOptions, SocketLike,
} from "./live.ts"
