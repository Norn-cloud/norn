/** Core IR and checker types generated from the Rust OpenAPI projection. */
import type { components } from "./generated/norn-api.ts"
export type { components } from "./generated/norn-api.ts"
export type Schemas = components["schemas"]

/** The generic governed-action client. */
export { createIntentKey, createNornClient, NornUnavailable, NornUnexpectedResponse, readRefusal } from "./client.ts"
export type {
  ActionCatalog, ActionContract, ActionId, ActionInput, ActionMapShape, ActionOutput, ActionReceipt, ActionSummary,
  CallOptions, CallResult, Diagnostic, Fetch, Immutable, Intent, NornClient, NornClientOptions, ObjectFields, ObjectMapShape,
  PendingDecision, Refusal, RefusalCategory, RefusalReason, Scope, TraceContext, UiEffects, UntypedActions,
} from "./client.ts"

/** The live-read client (`norn.live.v1`). */
export { LIVE_PROTOCOL, LiveClient } from "./live.ts"
export type {
  LiveClientRefusal, LiveListener, LiveOptions, LiveRefusal, LiveSnapshot, LiveState, LiveSubscribeOptions, SocketLike,
} from "./live.ts"
