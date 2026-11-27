/**
 * What `<NornAppRenderer>` can draw, and what a contract asks of it (UI-VIEW-1, L6).
 *
 * `requiredCapabilities` is the renderer capability catalog a contract requires, as
 * `<family>:<name>` strings: the checker's own `renderer_capabilities`
 * (`crates/norn-ui-checker/src/ui_sdk.rs`), walked here over the same document so the renderer
 * decides before it draws, with no WASM on the page's first paint. `test/capabilities.test.ts`
 * holds the two to identical answers through the checker's WASM export (`rendererCapabilities`).
 *
 * The renderer fails closed: a page that needs a capability outside `RENDERER_CAPABILITIES`, or a
 * binding or control form the renderer does not draw (`unsupported`), is refused with what is
 * missing, never drawn in part.
 */
import { UI_SDK } from "../../../tools/norn-ts/generated/ui-sdk.ts"
import { nodesOf, UI_SDK_PREFIX, type ComponentKind, type WidgetNode, type WidgetUx } from "./ux.ts"

/** The agent capabilities every renderer needs (L6, `AGENT_CAPABILITIES` in ui_sdk.rs). */
export const AGENT_CAPABILITIES = ["agent:confirm", "agent:redaction"] as const

/**
 * UI SDK primitives this renderer draws with `@ui-sdk/react`: the layers a page holds open in its
 * states (`heldIn`). A page's own controls draw as UI SDK buttons and inputs (`component:control`).
 */
export const RENDERED_PRIMITIVES = ["alert-dialog", "dialog"] as const
export type RenderedPrimitive = (typeof RENDERED_PRIMITIVES)[number]

/**
 * The foundation behaviours of {@link RENDERED_PRIMITIVES}, as the pinned catalog
 * (`crates/norn-ui-checker/catalog/ui-sdk.json`) lists them; `test/capabilities.test.ts` holds this
 * list to the catalog, so a UI SDK bump that changes them fails there.
 */
export const RENDERED_BEHAVIORS = [
  "focus/tabbable", "motion/presence", "platform/anchoring", "platform/env", "platform/keyboard", "platform/layers",
  "platform/moves", "platform/platform", "platform/position", "platform/scrollbar", "platform/sides", "platform/style",
  "popups/bridge", "popups/create-popup", "popups/dismissal", "popups/frame-guards", "popups/hover", "popups/modal",
  "popups/modal-attribute", "popups/popup", "popups/popup-behavior", "popups/popup-elements", "popups/popup-features",
  "popups/popup-payload", "popups/popup-presets", "popups/popup-slots", "popups/popup-steps", "popups/presentation",
  "shortcuts/create-shortcuts", "shortcuts/shortcut-order", "shortcuts/shortcut-slot", "shortcuts/shortcuts",
  "text/accessible-name",
] as const

/**
 * Component kinds this renderer draws. `form`, `child_grid` and `composition` are refused, and so is
 * `dialog` (a dialog scope with `blocking` and `dismissalMethods`) until it draws through a UI SDK
 * dialog layer; a page's layers are UI SDK primitives held open in its states (`heldIn`).
 */
export const RENDERED_COMPONENTS: readonly ComponentKind[] = ["action", "catalog", "control", "detail", "field", "list", "navigation", "section"]

const OUTCOMES = ["accepted", "refused", "pending_approval", "answer_moved", "network_failure", "not_served", "stale", "subscription_refused", "timeout", "cancelled"] as const

/** Every capability this renderer declares. */
export const RENDERER_CAPABILITIES: ReadonlySet<string> = new Set([
  ...AGENT_CAPABILITIES,
  ...RENDERED_COMPONENTS.map((kind) => `component:${kind}`),
  ...RENDERED_PRIMITIVES.map((name) => `ui-sdk:${name}`),
  ...RENDERED_BEHAVIORS.map((name) => `behavior:${name}`),
  "view:collection", "view:record",
  "event:page_opened", "event:user", "event:outcome", "event:subscription", "event:timer",
  ...OUTCOMES.map((outcome) => `outcome:${outcome}`),
  "effect:start:action", "effect:start:read", "effect:cancel", "effect:navigate",
  // A context read (UI-CONTEXT-READ-1), read through the host's reader (`BindingRead` `record`).
  "read:context",
])

const primitives: ReadonlySet<string> = new Set(Object.keys(UI_SDK.components))

/** The UI SDK primitive a node draws, when it names one the pinned catalog has (`primitive_of`). */
export function primitiveOf(node: WidgetNode): string | null {
  if (node.component.kind !== "catalog" || node.component.entry.widget !== undefined) return null
  const { entryId } = node.component.entry
  if (!entryId.startsWith(UI_SDK_PREFIX)) return null
  const name = entryId.slice(UI_SDK_PREFIX.length)
  return primitives.has(name) ? name : null
}

/** The capabilities `ux` requires, sorted as the checker sorts them (`BTreeSet<String>`). */
export function requiredCapabilities(ux: WidgetUx): string[] {
  const out = new Set<string>(AGENT_CAPABILITIES)
  if ((ux.contextReads ?? []).length > 0) out.add("read:context")
  for (const page of ux.pages) {
    for (const node of nodesOf(page.root)) {
      out.add(`component:${node.component.kind}`)
      const primitive = primitiveOf(node)
      if (primitive !== null) out.add(`ui-sdk:${primitive}`)
      for (const behavior of node.behaviors ?? []) out.add(`behavior:${behavior}`)
    }
    if (page.view !== undefined) out.add(`view:${page.view.kind}`)
    const machine = page.machine
    if (machine === undefined) continue
    for (const event of machine.events) {
      out.add(`event:${event.source.kind}`)
      if (event.source.kind === "outcome") out.add(`outcome:${event.source.outcome}`)
    }
    for (const transition of machine.transitions) {
      for (const effect of transition.effects ?? []) out.add(effect.kind === "start" ? `effect:start:${effect.call.kind}` : `effect:${effect.kind}`)
    }
  }
  return [...out].sort((left, right) => (left < right ? -1 : left > right ? 1 : 0))
}

/**
 * What the renderer cannot draw although the capability catalog does not name it: a data binding
 * read it has no reader for, a control input it has no data for, a catalog island it was not given.
 */
export function unsupported(ux: WidgetUx): string[] {
  const out: string[] = []
  for (const binding of ux.dataBindings) {
    const query = binding.query
    if (query.kind === "object_list" && query.filters.length === 0 && query.sort.length === 0) continue
    if (query.kind === "object_runtime_detail") continue
    out.push(`read:${query.kind}${query.kind === "object_list" ? ":filtered" : ""} (${binding.bindingId})`)
  }
  for (const page of ux.pages) {
    for (const node of nodesOf(page.root)) {
      if (node.component.kind === "control" && node.component.input === "choice") out.push(`control:choice (${node.nodeId})`)
      if (node.component.kind === "catalog" && node.component.entry.widget !== undefined) out.push(`catalog:pinned-widget (${node.nodeId})`)
    }
  }
  return out
}

/** What `ux` requires that the renderer declaring `declared` lacks; empty means it may draw it. */
export function missingCapabilities(ux: WidgetUx, declared: ReadonlySet<string> = RENDERER_CAPABILITIES): string[] {
  return [...requiredCapabilities(ux).filter((capability) => !declared.has(capability)), ...unsupported(ux)]
}
