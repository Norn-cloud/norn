/**
 * `<NornAppRenderer>`: renders one page of an admitted Widget UX app (#1762 milestone C).
 *
 * The workspace comes first (`matchWorkspaceState`): nothing is read until the server's mode answer
 * decided one, and a page reads only through that workspace's reader. Then the renderer checks that
 * it can draw the page at all: a contract that needs a renderer capability this renderer does not
 * declare (`renderer_capabilities`, L6), a binding or control form it does not draw, or a catalog
 * island it was not given is refused whole, saying what is missing. Only then does the page's
 * machine run (`useWidgetMachine`) and its tree draw (`nodes.tsx`).
 *
 * The renderer holds no permission logic: it reflects the server's answers (reads, refusals,
 * projections, approvals) and the admitted document, nothing else.
 *
 * A live page is keyed by its decided scope: another workspace gets another page, its own machine,
 * reads and calls, never one carried from the last.
 */
import { Button } from "@ui-sdk/react/button"
import { useMemo, useRef, type ReactNode } from "react"
import type { ActionEffects } from "./agent.ts"
import { missingCapabilities } from "./capabilities.ts"
import { islandsOf, type IslandRegistry } from "./islands.ts"
import type { Clock } from "./machine/driver.ts"
import type { EngineLoader } from "./machine/evaluate.ts"
import type { Machine } from "./machine/machine.ts"
import { Node, type PageScope } from "./nodes.tsx"
import type { WidgetCalls, WidgetLive, WidgetReader } from "./reads.ts"
import { useWidgetMachine } from "./runtime.ts"
import { matchWorkspaceState, type WorkspaceState } from "./state.ts"
import type { WidgetPage, WidgetUx } from "./ux.ts"

export interface NornAppRendererProps<Scope, Reader extends WidgetReader, Recorded extends WidgetReader> {
  /** The admitted contract (`WidgetRoot.ux`). */
  readonly ux: WidgetUx
  /** The page to draw; the app's entry page, or the first, when omitted. */
  readonly pageId?: string
  /** The page's context parameters, as the host's route supplied them. */
  readonly context?: Readonly<Record<string, unknown>>
  /** The workspace the server's mode answer decided (`WorkspaceProvider`). */
  readonly workspace: WorkspaceState<Scope, Reader, Recorded>
  readonly calls: WidgetCalls
  /** Live feeds; a page that follows a read live is refused without them. */
  readonly live?: WidgetLive
  /** Loads the checker's expression engine; called after the first paint, never before. */
  readonly engine: EngineLoader
  /** The contract's catalog islands, by entry id (a generated `RequiredCatalogRegistry`). */
  readonly islands?: IslandRegistry
  /** The object type the cell serves for an entity artifact, from the admitted definitions. */
  readonly objectTypeOf: (artifactId: string) => string | undefined
  /** The action id the cell calls for an action artifact, from the admitted definitions. */
  readonly actionIdOf: (artifactId: string) => string | undefined
  /**
   * What the server says an action artifact's effects are (`uiEffects`, and `ledgered`); `undefined`
   * while it has not said (no catalog, or its read loading or failed), which marks every control
   * that starts the action as needing a person (L6 fails closed).
   */
  readonly actionEffects?: (artifactId: string) => ActionEffects | undefined
  readonly locale?: string
  readonly dir?: "ltr" | "rtl"
  readonly navigate?: (itemId: string) => void
  /** The mode read's Retry, for a workspace that is unavailable. */
  readonly onRetryWorkspace?: () => void
  readonly clock?: Clock
}

const NO_ISLANDS: IslandRegistry = Object.freeze({})
const EMPTY_CONTEXT: Readonly<Record<string, unknown>> = Object.freeze({})
const NO_EFFECTS = (): undefined => undefined
const NO_NAVIGATION = () => {}

/** The page `pageId` names, or the app's entry page, or the first. */
function pageOf(ux: WidgetUx, pageId: string | undefined): WidgetPage | undefined {
  const wanted = pageId ?? ux.app?.entryPageId
  return wanted === undefined ? ux.pages[0] : ux.pages.find((page) => page.pageId === wanted)
}

/** What the renderer would need, and lacks, to draw `ux` whole. */
export function refusalsOf(ux: WidgetUx, islands: IslandRegistry, live: boolean): string[] {
  const missing = missingCapabilities(ux)
  for (const { entry } of islandsOf(ux.catalog)) {
    if (!Object.hasOwn(islands, entry.entryId)) missing.push(`island:${entry.entryId}`)
  }
  if (!live && ux.pages.some((page) => page.machine?.events.some((event) => event.source.kind === "subscription"))) missing.push("live:feed")
  return missing
}

/**
 * The one "unavailable" state the renderer draws: the workspace's mode read, or the page's
 * expression engine, did not answer. A person's Retry asks again.
 */
function Unavailable({ what, title, message, onRetry, dir }: { readonly what: "workspace" | "engine"; readonly title: string; readonly message: string; readonly onRetry?: (() => void) | undefined; readonly dir: "ltr" | "rtl" }) {
  return (
    <div role="alert" dir={dir} data-unavailable={what} {...(what === "workspace" ? { "data-workspace": "unavailable" } : {})} className="grid gap-2">
      <p className="text-body">{title}</p>
      <p className="text-meta text-muted-foreground">{message}</p>
      {onRetry !== undefined && (
        <Button
          type="button"
          className="inline-flex h-9 items-center justify-self-start rounded-full border border-border bg-background px-4 text-meta text-foreground outline-none transition-colors hover:bg-accent focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring max-sm:min-h-[44px]"
          onClick={onRetry}
        >
          Retry
        </Button>
      )}
    </div>
  )
}

function Refused({ missing, dir }: { readonly missing: readonly string[]; readonly dir: "ltr" | "rtl" }) {
  return (
    <div role="alert" dir={dir} data-norn-refused="" className="grid gap-2 rounded-[var(--radius-panel)] border border-border bg-card p-4">
      <p className="text-body">This page needs what this renderer does not draw.</p>
      <ul className="text-meta text-muted-foreground">{missing.map((capability) => <li key={capability} data-missing={capability}><code>{capability}</code></li>)}</ul>
    </div>
  )
}

function Page<Reader extends WidgetReader>(props: Omit<NornAppRendererProps<unknown, Reader, Reader>, "workspace"> & { readonly page: WidgetPage & { readonly machine: Machine }; readonly reader: Reader }) {
  const { ux, page, reader } = props
  const machine = page.machine
  const runtime = useWidgetMachine({
    ux,
    machine,
    reader,
    calls: props.calls,
    ...(props.live === undefined ? {} : { live: props.live }),
    engine: props.engine,
    context: props.context ?? EMPTY_CONTEXT,
    objectTypeOf: props.objectTypeOf,
    actionIdOf: props.actionIdOf,
    ...(props.navigate === undefined ? {} : { navigate: props.navigate }),
    ...(props.clock === undefined ? {} : { clock: props.clock }),
  })
  const raisedBy = useRef<string | null>(null)
  const scope: PageScope = {
    raisedBy,
    ux,
    page,
    machine,
    runtime,
    islands: props.islands ?? NO_ISLANDS,
    locale: props.locale ?? "en",
    actionEffects: props.actionEffects ?? NO_EFFECTS,
    navigate: props.navigate ?? NO_NAVIGATION,
  }
  const role = machine.states.find((state) => state.stateId === runtime.snapshot.stateId)?.role
  const dir = props.dir ?? "ltr"
  const steps = runtime.waiting === 1 ? "One step waits" : `${runtime.waiting} steps wait`
  return (
    // Not a landmark itself: the page's own root node names its region, so no two regions share a name.
    <div
      dir={dir}
      lang={props.locale ?? "en"}
      className="grid gap-4"
      data-norn-page={page.pageId}
      data-state={runtime.snapshot.stateId}
      data-role={role}
      data-engine={runtime.engine}
      data-workspace={reader.kind}
      data-waiting={runtime.waiting}
    >
      {runtime.engine === "failed" && (
        <Unavailable what="engine" dir={dir} title="Page engine unavailable" onRetry={runtime.retryEngine}
          message={`This page's expression engine did not load. ${runtime.waiting === 0 ? "Steps that need it wait" : `${steps} for it`} and run, in order, once Retry loads it; nothing that needs it has run.`} />
      )}
      {runtime.fault !== null && (
        <p role="alert" data-fault="" className="text-meta text-destructive-text">{`That step could not run: ${runtime.fault.message} The page stayed where it was.`}</p>
      )}
      <Node scope={scope} node={page.root} />
    </div>
  )
}

/** Renders one page of an admitted Widget UX app in the decided workspace, or says why it cannot. */
export function NornAppRenderer<Scope, Reader extends WidgetReader, Recorded extends WidgetReader>(props: NornAppRendererProps<Scope, Reader, Recorded>): ReactNode {
  const { ux, workspace } = props
  const dir = props.dir ?? "ltr"
  const islands = props.islands ?? NO_ISLANDS
  const page = pageOf(ux, props.pageId)
  const missing = useMemo(() => refusalsOf(ux, islands, props.live !== undefined), [ux, islands, props.live])
  if (page === undefined) return <Refused missing={[`page:${props.pageId ?? "entry"}`]} dir={dir} />
  if (page.machine === undefined) return <Refused missing={["machine"]} dir={dir} />
  if (missing.length > 0) return <Refused missing={missing} dir={dir} />
  const activePage = { ...page, machine: page.machine }
  return matchWorkspaceState(workspace, {
    deciding: () => <p role="status" aria-busy="true" dir={dir} data-workspace="deciding" className="text-meta text-muted-foreground">Deciding the workspace…</p>,
    unavailable: ({ error }) => <Unavailable what="workspace" dir={dir} title="Workspace unavailable" message={error.message} onRetry={props.onRetryWorkspace} />,
    // Keyed by the decided workspace and page: never carries one workspace or page's machine, reads, calls or focus return into another.
    recorded: ({ reader }) => <Page key={`recorded:${activePage.pageId}`} {...props} page={activePage} reader={reader as WidgetReader} />,
    live: ({ scope, reader }) => <Page key={`${pageKey({ kind: "live", scope })}:${activePage.pageId}`} {...props} page={activePage} reader={reader as WidgetReader} />,
  })
}

/** The key a decided workspace's page is drawn under: its kind and, live, its scope. */
export function pageKey(workspace: { readonly kind: "recorded" } | { readonly kind: "live"; readonly scope: unknown }): string {
  return workspace.kind === "recorded" ? "recorded" : `live:${JSON.stringify(workspace.scope)}`
}
