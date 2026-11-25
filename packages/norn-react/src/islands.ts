/**
 * Catalog islands (UI-CATALOG-1, #1762): custom UI a page places through its catalog, in two tiers.
 *
 * - **Tier 1, presentational** (`trust: "opaque_island"`): it receives only its authorized props,
 *   each a ZEN expression over the page machine's fields, and an `emit` for its declared events.
 * - **Tier 2, stateful** (`trust: "contract_checked"`): it also receives the page's server states
 *   as closed unions (`ReadState`, `ActionCallState`), and must draw them through `matchReadState` /
 *   `matchActionState`, so a state it leaves out does not compile.
 *
 * `norn codegen --ts` emits `RequiredCatalogRegistry<W, States>` from the admitted widget: the
 * registry a renderer must be given for widget `W`, one island per declared entry, each typed by
 * its entry. A missing island, an extra one, or one whose props or events differ from its entry
 * fails to compile. `IslandStates` is the `States` this renderer passes a Tier 2 island.
 */
import type { BindingCallState, BindingReadState } from "./reads.ts"
import type { CatalogEntry, Trust } from "./ux.ts"

/** The server states a Tier 2 island receives: every data binding's read, every action binding's call. */
export interface IslandStates {
  readonly reads: Readonly<Record<string, BindingReadState>>
  readonly calls: Readonly<Record<string, BindingCallState>>
}

export type IslandTier = "presentational" | "stateful"

/** The tier of a catalog entry; a `native` entry is drawn by the renderer itself, never an island. */
export function tierOf(trust: Trust): IslandTier | null {
  if (trust === "opaque_island") return "presentational"
  if (trust === "contract_checked") return "stateful"
  return null
}

/** What the renderer hands an island at run time; the generated registry types each one exactly. */
export interface IslandInput {
  readonly props: Readonly<Record<string, unknown>>
  readonly emit: (event: string, payload?: Readonly<Record<string, unknown>>) => void
  readonly states?: IslandStates
}

/**
 * The islands the renderer is given, by entry id. Pass a `RequiredCatalogRegistry` (generated) here:
 * it is assignable to this, and holds the compile-time guarantee.
 */
export type IslandRegistry = Readonly<Record<string, (island: never) => unknown>>

/** The entries of a contract's catalog that are islands, with their tiers. */
export function islandsOf(catalog: readonly CatalogEntry[] | undefined): readonly { readonly entry: CatalogEntry; readonly tier: IslandTier }[] {
  return (catalog ?? []).flatMap((entry) => {
    const tier = tierOf(entry.trust)
    return tier === null ? [] : [{ entry, tier }]
  })
}
