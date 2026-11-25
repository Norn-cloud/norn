/**
 * The record page's catalog islands (#1762 C2): Studio's own UI for each state of the admitted
 * record page (`registry/studio/record-page.ts`), typed by the registry `norn codegen --ts` emits
 * for it (`record-islands.d.ts`), so a missing, extra or mistyped island does not compile.
 *
 * Each island is Tier 2: it receives the page's server states as closed unions and draws the
 * record read through `matchReadState`. It decides nothing: the page machine decided which state
 * the page is in (each island is held in its own), and a person's Retry or Read again is the
 * island's declared event, which the machine takes.
 */
import { createContext, use, useEffect, useRef, type ReactNode } from "react"
import { LoadState, ReadErrorState, WorkspaceUnavailable } from "@/components/load-state"
import type { IslandStates } from "../../../../packages/norn-react/src/islands.ts"
import { matchReadState } from "../../../../packages/norn-react/src/state.ts"
import type { BindingReadState } from "../../../../packages/norn-react/src/reads.ts"
import type { ObjectReads } from "../lib/transport.ts"
import { unavailableWorkspaceOf } from "../lib/definitions.ts"
import { useMode } from "../lib/live-transport.ts"
import { useMessage } from "../lib/translations.ts"
import type { RequiredCatalogRegistry, WidgetIslands } from "./record-islands.d.ts"
import { RecordAbsent, RecordBody, RecordHeader, useReadAgain, type RecordRoute } from "./record-view.tsx"

/** The admitted record page's widget id, as its generated registry names it. */
export const RECORD_PAGE = "norn.studio.record.widget" satisfies keyof WidgetIslands

/** The record the page's route names, for its islands. */
export const RecordRouteContext = createContext<RecordRoute>({ name: "", id: "" })

/**
 * The record the page shows: the read's document while it is `ok`, and, while it is read again, the
 * one it showed before (the island stays mounted from `showing` into `refreshing`, so the record
 * never blinks away under Read again).
 */
function useShown(read: BindingReadState | undefined): { readonly data: ObjectReads | undefined; readonly refreshing: boolean } {
  const last = useRef<ObjectReads | undefined>(undefined)
  if (read?.kind === "ok") last.current = read.document as ObjectReads
  return { data: last.current, refreshing: read?.kind === "pending" }
}

/** The first read's skeleton. The page machine's deadline, not the skeleton, decides when it has waited too long. */
function Reading() {
  const say = useMessage()
  return <LoadState status="pending" skeleton="detail" loadingLabel={say("Reading the record")} timeoutMs={60_000}>{() => null}</LoadState>
}

/**
 * What the record read failed with, as the cell said it, and a person's Retry. When the workspace,
 * not the cell, did not answer, it is the shell's Workspace unavailable, whose Retry decides the
 * workspace again and then reads the record again.
 */
function Failed({ read, retry }: { readonly read: BindingReadState | undefined; readonly retry: () => void }) {
  const say = useMessage()
  const workspace = read?.kind === "unavailable" ? unavailableWorkspaceOf(read.error.cause) : undefined
  if (workspace !== undefined) return <WorkspaceFailed error={workspace} retry={retry} />
  const error = read === undefined ? new Error("The record was not read.") : matchReadState(read, {
    pending: (): Error => new Error("The record is being read."),
    ok: () => new Error("The record was read."),
    refused: ({ refusal }) => {
      const said = (refusal.document as { readonly message?: unknown } | null)?.message
      return new Error(typeof said === "string" && said !== "" ? said : `The cell refused this read (${refusal.code}).`)
    },
    not_served: ({ error }) => error,
    unavailable: ({ error }) => error,
    answer_moved: ({ error }) => error,
    timed_out: ({ error }) => error,
  })
  return <div data-record-read={read?.kind ?? "none"}><ReadErrorState error={error} title={say("The record could not be read")} onRetry={retry} /></div>
}

/**
 * The workspace, not the cell, did not answer the record read: the shell's Workspace unavailable,
 * whose own Retry decides the workspace again. Once the workspace is decided again (its mode read
 * answers anew), the record is read again; while it is not, the page's workspace is unavailable and
 * the shell shows that instead of the page.
 */
function WorkspaceFailed({ error, retry }: { readonly error: unknown; readonly retry: () => void }) {
  const mode = useMode()
  const failedAt = useRef(mode.dataUpdatedAt)
  useEffect(() => {
    if (mode.dataUpdatedAt > failedAt.current) retry()
  }, [mode.dataUpdatedAt])
  return <div data-record-read="workspace"><WorkspaceUnavailable error={error} /></div>
}

function Header({ read, emit }: { readonly read: BindingReadState | undefined; readonly emit: () => void }) {
  const route = use(RecordRouteContext)
  const readAgain = useReadAgain(route, emit)
  const { data, refreshing } = useShown(read)
  return data === undefined ? null : <RecordHeader route={route} data={data} refreshing={refreshing} readAgain={readAgain} />
}

function Body({ read, emit }: { readonly read: BindingReadState | undefined; readonly emit: () => void }) {
  const route = use(RecordRouteContext)
  const readAgain = useReadAgain(route, emit)
  const { data } = useShown(read)
  return data === undefined ? null : <RecordBody route={route} data={data} readAgain={readAgain} />
}

function Absent({ emit }: { readonly emit: () => void }) {
  const route = use(RecordRouteContext)
  return <RecordAbsent route={route} readAgain={useReadAgain(route, emit)} />
}

type Registry = RequiredCatalogRegistry<typeof RECORD_PAGE, IslandStates>

export const RECORD_ISLANDS: Registry = {
  "record-reading": (): ReactNode => <Reading />,
  "record-failed": ({ emit, states }): ReactNode => <Failed read={states.reads.record} retry={() => emit("retry", {})} />,
  "record-absent": ({ emit }): ReactNode => <Absent emit={() => emit("readAgain", {})} />,
  "record-header": ({ emit, states }): ReactNode => <Header read={states.reads.record} emit={() => emit("readAgain", {})} />,
  "record-body": ({ emit, states }): ReactNode => <Body read={states.reads.record} emit={() => emit("readAgain", {})} />,
}
