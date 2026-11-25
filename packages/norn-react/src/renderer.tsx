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
 */
import { useMemo, useRef, type ReactNode } from "react"
import type { ActionEffects } from "./agent.ts"
import { missingCapabilities } from "./capabilities.ts"
import { islandsOf, type IslandRegistry } from "./islands.ts"
import type { Clock } from "./machine/driver.ts"
import type { CheckerEvaluator } from "./machine/evaluate.ts"
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
  readonly engine: () => Promise<CheckerEvaluator>
  /** The contract's catalog islands, by entry id (a generated `RequiredCatalogRegistry`). */
  readonly islands?: IslandRegistry
  /** The object type the cell serves for an entity artifact, from the admitted definitions. */
  readonly objectTypeOf: (artifactId: string) => string | undefined
  /** The action id the cell calls for an action artifact, from the admitted definitions. */
  readonly actionIdOf: (artifactId: string) => string | undefined
  /** What the server says an action artifact's effects are (`uiEffects`, and `ledgered`). */
  readonly actionEffects?: (artifactId: string) => ActionEffects | undefined
  readonly locale?: string
  readonly dir?: "ltr" | "rtl"
  readonly navigate?: (itemId: string) => void
  /** The mode read's Retry, for a workspace that is unavailable. */
  readonly onRetryWorkspace?: () => void
  readonly clock?: Clock
}

const NO_ISLANDS: IslandRegistry = Object.freeze({})
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
    if (!(entry.entryId in islands)) missing.push(`island:${entry.entryId}`)
  }
  if (!live && ux.pages.some((page) => page.machine?.events.some((event) => event.source.kind === "subscription"))) missing.push("live:feed")
  return missing
}

function Refused({ missing, dir }: { readonly missing: readonly string[]; readonly dir: "ltr" | "rtl" }) {
  return (
    <div role="alert" dir={dir} data-norn-refused="" className="grid gap-2 rounded-[var(--radius-panel)] border border-border bg-card p-4">
      <p className="text-body font-medium">This page needs what this renderer does not draw.</p>
      <ul className="text-meta text-muted-foreground">{missing.map((capability) => <li key={capability} data-missing={capability}><code>{capability}</code></li>)}</ul>
    </div>
  )
}

function Page<Reader extends WidgetReader>(props: Omit<NornAppRendererProps<unknown, Reader, Reader>, "workspace"> & { readonly page: WidgetPage; readonly reader: Reader }) {
  const { ux, page, reader } = props
  const machine = page.machine!
  const runtime = useWidgetMachine({
    ux,
    machine,
    reader,
    calls: props.calls,
    ...(props.live === undefined ? {} : { live: props.live }),
    engine: props.engine,
    context: props.context ?? {},
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
  return (
    // Not a landmark itself: the page's own root node names its region, so no two regions share a name.
    <div
      dir={props.dir ?? "ltr"}
      lang={props.locale ?? "en"}
      className="grid gap-4"
      data-norn-page={page.pageId}
      data-state={runtime.snapshot.stateId}
      data-role={role}
      data-engine={runtime.engine}
      data-workspace={reader.kind}
    >
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
  return matchWorkspaceState(workspace, {
    deciding: () => <p role="status" aria-busy="true" dir={dir} data-workspace="deciding" className="text-meta text-muted-foreground">Deciding the workspace…</p>,
    unavailable: ({ error }) => (
      <div role="alert" dir={dir} data-workspace="unavailable" className="grid gap-2">
        <p className="text-body font-medium">Workspace unavailable</p>
        <p className="text-meta text-muted-foreground">{error.message}</p>
        {props.onRetryWorkspace !== undefined && <button type="button" className="justify-self-start text-meta underline" onClick={props.onRetryWorkspace}>Retry</button>}
      </div>
    ),
    // Keyed by workspace kind: a page never carries one workspace's machine into another.
    recorded: ({ reader }) => <Page key="recorded" {...props} page={page} reader={reader as WidgetReader} />,
    live: ({ reader }) => <Page key="live" {...props} page={page} reader={reader as WidgetReader} />,
  })
}
