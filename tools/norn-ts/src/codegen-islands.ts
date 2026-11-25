/**
 * `norn codegen --ts`, catalog islands (#1762 milestone C): the registry a renderer must be given to
 * draw an admitted widget's custom UI, as types.
 *
 * Each widget artifact of the set that declares catalog islands (`WidgetUx.catalog` entries with
 * `trust: "opaque_island"` or `"contract_checked"`) becomes one member of `WidgetIslands`: each
 * entry's tier, its typed props and its typed events. `RequiredCatalogRegistry<W, States>` maps
 * that to the island components `@norn/react`'s `<NornAppRenderer>` takes: every entry, none extra,
 * a presentational (Tier 1) island taking only `{ props, emit }`, a stateful (Tier 2) one also the
 * page's server states (`States`, `@norn/react`'s `IslandStates`). A missing island, an extra one,
 * or one whose props or events differ from its entry fails to compile. A `native` entry is drawn by
 * the renderer itself and is no island.
 *
 * A set with no islands emits nothing here, so its file is unchanged. Generation fails closed like
 * the rest of codegen: a value type outside the closed set stops the run.
 */
import { CodegenError } from "./codegen.ts"

type Node = { readonly [key: string]: unknown }
const record = (value: unknown): value is Node => typeof value === "object" && value !== null && !Array.isArray(value)
const compare = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0)
const key = (name: string): string => JSON.stringify(name)

/** The TypeScript type of one machine value type (`WidgetValueType`), as the page's engine yields it. */
const VALUE_TYPES: Readonly<Record<string, string>> = {
  text: "string",
  integer: "number",
  // The engine's JSON value; an exact decimal reads as a JS number here, as the page's fields do.
  number: "number",
  boolean: "boolean",
  date: "string",
  timestamp: "string",
  duration: "string",
  json: "NornJson",
}

const TIERS: Readonly<Record<string, string>> = { opaque_island: "presentational", contract_checked: "stateful" }

function members(fields: unknown, at: string): string {
  if (fields === undefined) return "Readonly<Record<string, never>>"
  if (!Array.isArray(fields)) throw new CodegenError(at, "members are not a list")
  if (fields.length === 0) return "Readonly<Record<string, never>>"
  const lines = (fields as unknown[]).map((field) => {
    if (!record(field) || typeof field.fieldId !== "string" || typeof field.type !== "string") throw new CodegenError(at, "a member has no fieldId or type")
    const type = VALUE_TYPES[field.type]
    if (type === undefined) throw new CodegenError(`${at}/${field.fieldId}`, `value type \`${field.type}\` is outside the closed set`)
    return { id: field.fieldId, text: `readonly ${key(field.fieldId)}: ${field.required === true ? type : `${type} | null`}` }
  }).sort((a, b) => compare(a.id, b.id))
  return `{ ${lines.map((line) => line.text).join("; ")} }`
}

/** The islands one widget's contract declares, as a `WidgetIslands` member; null when it declares none. */
function widgetIslands(artifact: Node): { readonly id: string; readonly text: string } | null {
  const at = String(artifact.artifactId)
  const payload = artifact.payload
  const ux = record(payload) ? payload.ux : undefined
  const catalog = record(ux) ? ux.catalog : undefined
  if (catalog === undefined) return null
  if (!Array.isArray(catalog)) throw new CodegenError(at, "the widget's catalog is not a list")
  const entries = (catalog as unknown[]).flatMap((entry) => {
    if (!record(entry) || typeof entry.entryId !== "string" || typeof entry.trust !== "string") throw new CodegenError(at, "a catalog entry has no entryId or trust")
    const tier = TIERS[entry.trust]
    if (tier === undefined) return []
    const where = `${at}#/ux/catalog/${entry.entryId}`
    const events = entry.events === undefined ? [] : entry.events
    if (!Array.isArray(events)) throw new CodegenError(where, "events are not a list")
    const eventLines = (events as unknown[]).map((event) => {
      if (!record(event) || typeof event.eventId !== "string") throw new CodegenError(where, "an event has no eventId")
      return { id: event.eventId, text: `readonly ${key(event.eventId)}: ${members(event.payload, `${where}/${event.eventId}`)}` }
    }).sort((a, b) => compare(a.id, b.id))
    const eventsType = eventLines.length === 0 ? "Readonly<Record<string, never>>" : `{ ${eventLines.map((line) => line.text).join("; ")} }`
    return [{ id: entry.entryId, text: `    readonly ${key(entry.entryId)}: { readonly tier: "${tier}"; readonly props: ${members(entry.props, `${where}/props`)}; readonly events: ${eventsType} }` }]
  }).sort((a, b) => compare(a.id, b.id))
  if (entries.length === 0) return null
  return { id: at, text: `  readonly ${key(at)}: {\n${entries.map((entry) => entry.text).join("\n")}\n  }` }
}

/** The widget artifacts of `set` that declare islands; codegen's digest covers them. */
export function islandWidgets(set: readonly unknown[]): Node[] {
  return set.filter((value): value is Node => record(value) && value.kind === "widget" && widgetIslands(value) !== null)
}

/** The islands block of the `.d.ts`, or no lines when the set declares no islands. */
export function islandsBlock(set: readonly unknown[]): string[] {
  const widgets = islandWidgets(set).map((artifact) => widgetIslands(artifact)!).sort((a, b) => compare(a.id, b.id))
  if (widgets.length === 0) return []
  return [
    "/** Each widget id → each catalog island it declares: its tier, its typed props and its typed events. */",
    `export type WidgetIslands = {\n${widgets.map((widget) => widget.text).join("\n")}\n}`,
    "",
    "/** An island's `emit`: one of its declared events, with that event's typed payload. */",
    "export type IslandEmit<Events> = <K extends keyof Events & string>(event: K, payload: Events[K]) => void",
    "",
    "/**",
    " * The islands a renderer must be given to draw widget `W` (`@norn/react`'s `NornAppRenderer` `islands`):",
    " * every entry, none extra, each typed by its entry. A presentational island takes `{ props, emit }`; a",
    " * stateful one also `states`, the page's server states as `States` (`@norn/react`'s `IslandStates`).",
    " */",
    "export type RequiredCatalogRegistry<W extends keyof WidgetIslands, States> = {",
    "  readonly [E in keyof WidgetIslands[W]]: WidgetIslands[W][E] extends { readonly tier: \"stateful\"; readonly props: infer P; readonly events: infer V }",
    "    ? (island: { readonly props: P; readonly emit: IslandEmit<V>; readonly states: States }) => unknown",
    "    : WidgetIslands[W][E] extends { readonly props: infer P; readonly events: infer V }",
    "      ? (island: { readonly props: P; readonly emit: IslandEmit<V> }) => unknown",
    "      : never",
    "}",
    "",
  ]
}
