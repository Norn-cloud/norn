import { useQuery } from "@tanstack/react-query"
import { GitBranch } from "lucide-react"
import { useEffect, useMemo, useState } from "react"
import { Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Skeleton } from "@/components/ui/skeleton"
import { defineLifecycleView, type LifecycleFinding, type LifecycleViewDocument, type LifecycleViewOptions, type NornLifecycleView } from "../../../../packages/norn-demo-ui/src/elements/lifecycle-view.ts"
import viewDocument from "../../../../packages/norn-demo-ui/src/elements/lifecycle-view.widget.json"
import type { Concept, Field, Lifecycle } from "../lib/catalog.ts"
import { loadChecker, type Checker } from "../lib/canvas/checker.ts"
import { readAtScope, readStudioScope, scopedHostFetch, useStudioScope, type StudioScope } from "../lib/definitions.ts"
import { facetsPath } from "../lib/facets.ts"
import { liveAnswer } from "../lib/live-reads.ts"
import { useMode } from "../lib/live-transport.ts"
import { useLocale } from "../lib/locale.tsx"
import { declaredLifecycleStates } from "../lib/objects.ts"
import type { Predicate } from "../lib/records-query.ts"
import { transport } from "../lib/transport.ts"
import { useView } from "../lib/view.tsx"

/**
 * An object type's lifecycle with each state's live count (UI-LIFECYCLE-2), hosted inside
 * Studio's workbench Panel/Well primitives. The element `<norn-lifecycle-view>` draws it; this
 * host gives it the lifecycle as the catalog declares it, one live read of the type's scoped
 * facets (`object_facets.rs`) on the lifecycle property, and the checker's counterexamples for
 * pages that act on this lifecycle. A finding belongs here when the widget page it names declares
 * this object type and property in its page machine's `lifecycle`; nothing is read from its
 * message. The checker loads after the view paints, and its findings join once it is here.
 */
const DOCUMENT = viewDocument as unknown as LifecycleViewDocument
type Presentation = Pick<LifecycleViewOptions, "lifecycle" | "title" | "findings"> & {
  readonly labels: readonly (readonly [string, string])[]
}

/** Format a lifecycle state, transition, or page state id in words, preferring declared labels. */
export function lifecycleLabel(id: string, field?: Field): string {
  return field?.valueLabels?.get(id) ?? sentence(id)
}

export function LifecycleView({ concept, lifecycle, filter = null, scope: boundScope }: {
  readonly concept: Concept
  readonly lifecycle: Lifecycle
  readonly filter?: Predicate | null
  readonly scope?: StudioScope
}) {
  const { locale } = useLocale()
  const { view: mode } = useView()
  const modeRead = useMode()
  const scopeRead = useStudioScope()
  const live = modeRead.data?.kind === "live"
  const scope = live ? (boundScope ?? scopeRead.data ?? null) : null
  const orgId = scope?.orgId
  const environmentId = scope?.environmentId
  const waitingForScope = modeRead.isPending || (live && boundScope === undefined && scopeRead.isPending)
  const definitions = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const checker = useQuery({ queryKey: ["checker"], queryFn: () => loadChecker(), staleTime: Infinity, retry: false })
  const [host, setHost] = useState<HTMLDivElement | null>(null)
  const field = concept.fields.find((candidate) => candidate.id === lifecycle.property)
  const { name, title } = concept
  const { property } = lifecycle
  const artifacts = definitions.data?.document
  const findings = useMemo(() => findingsFor(name, property, artifacts, checker.data), [name, property, artifacts, checker.data])
  // Catalog re-reads can allocate equivalent definitions. Only changed options should restart reads.
  const presentation = JSON.stringify({
    lifecycle: {
      property,
      states: declaredLifecycleStates(lifecycle, field),
      initial: lifecycle.initial,
      terminal: lifecycle.terminal ?? [],
      transitions: (lifecycle.transitions ?? []).map(({ id, from, to }) => ({ id, from, to })),
      endings: lifecycle.endings ?? [],
    },
    title,
    findings,
    labels: [...field?.valueLabels ?? []],
  } satisfies Presentation)
  const path = facetsPath(concept.name, filter, [lifecycle.property])
  const propertyLabel = field?.label ?? sentence(lifecycle.property)
  const filtered = filter !== null && filter.all.length > 0

  useEffect(() => {
    if (host === null || waitingForScope) return
    defineLifecycleView()
    const view = document.createElement("norn-lifecycle-view") as NornLifecycleView
    const { labels, ...display } = JSON.parse(presentation) as Presentation
    const stateLabels = new Map(labels)
    const activeScope = orgId === undefined || environmentId === undefined ? null : { orgId, environmentId }
    const call = activeScope === null ? null : scopedHostFetch((input, init) => fetch(input, init), "/api/host", async () => activeScope)
    view.options = {
      document: DOCUMENT,
      ...display,
      hideTitle: true,
      // When live, read the cell's scoped facets route; when recorded or without a cell, draw the declared lifecycle without calling the network.
      ...(call !== null && activeScope !== null ? {
        read: async (signal?: AbortSignal) => readAtScope(
          activeScope,
          (s) => readStudioScope(undefined, s),
          async () => {
            const response = await call(path, { headers: { accept: "application/json" }, signal })
            const text = await response.text()
            let body: unknown
            try { body = text === "" ? undefined : JSON.parse(text) } catch { body = undefined }
            return { status: response.status, body }
          },
          signal,
        ),
        changes: (listener, onRefused) => liveAnswer(path, activeScope, listener, onRefused),
      } : {}),
      locale: locale === "ar" ? "ar" : "en",
      technical: mode === "technical",
      // A state's label from the definition; a transition or page state id in words, as the definition names it.
      label: (id) => stateLabels.get(id) ?? sentence(id),
    }
    host.replaceChildren(view)
    return () => view.remove()
  }, [host, waitingForScope, presentation, path, orgId, environmentId, locale, mode])

  return (
    <Panel icon={GitBranch} label={`${locale === "ar" ? "دورة الحياة" : "Lifecycle"} · ${propertyLabel}`}
      action={(filtered || mode === "technical") && (
        <span className="flex items-center gap-2">
          {filtered && <Badge variant="outline">{locale === "ar" ? "مُصفّاة" : "Filtered"}</Badge>}
          {mode === "technical" && <span className="font-mono text-xs">{concept.name}.{lifecycle.property}</span>}
        </span>
      )}>
      <Well className="p-4">
        {waitingForScope
          ? <div className="grid gap-3" aria-label="Reading lifecycle"><Skeleton className="h-5 w-48" /><Skeleton className="h-28 w-full" /></div>
          : <div ref={setHost} data-testid="lifecycle-view" />}
      </Well>
    </Panel>
  )
}

export const sentence = (id: string) => { const words = id.replaceAll(/[_-]/g, " "); return words.charAt(0).toUpperCase() + words.slice(1) }

type PageMachine = {
  readonly initialStateId?: string
  readonly transitions?: readonly { readonly transitionId: string; readonly target: string }[]
  readonly lifecycle?: { readonly objectType?: string; readonly property?: string }
}
type Envelope = {
  readonly artifactId?: string
  readonly kind?: string
  readonly payload?: { readonly ux?: { readonly pages?: readonly { readonly machine?: PageMachine }[] } }
}

// RFC 6901 array index: "0" or [1-9][0-9]* (leading zeros like "01" are forbidden), followed only by valid JSON Pointer tokens.
const PAGE_POINTER = /^\/payload\/ux\/pages\/(0|[1-9]\d*)(?:\/(?:[^~/]|~[01])*)*$/

/**
 * The checker's `widget.lifecycle.*` findings with a witness, for widget pages whose machines act
 * on this lifecycle. Attribution requires a canonical RFC 6901 page pointer; a finding from a
 * matching widget with a missing, non-canonical, or out-of-bounds pointer attaches no witness and
 * is returned as unattributed so it can never be attributed to the wrong page.
 */
export function findingsFor(
  objectType: string,
  property: string,
  definitions: readonly unknown[] | undefined,
  checker: Checker | undefined,
): LifecycleFinding[] {
  if (!definitions || !checker) return []
  const envelopes = definitions as readonly Envelope[]
  const widgets = new Map<string, readonly { readonly machine?: PageMachine }[]>()
  for (const artifact of envelopes) {
    if (artifact.kind !== "widget" || typeof artifact.artifactId !== "string") continue
    const pages = artifact.payload?.ux?.pages
    if (!pages?.some((page) => page.machine?.lifecycle?.objectType === objectType && page.machine.lifecycle.property === property)) continue
    widgets.set(artifact.artifactId, pages)
  }
  if (widgets.size === 0) return []
  const results: LifecycleFinding[] = []
  for (const finding of checker(definitions).diagnostics.diagnostics) {
    if (!finding.code.startsWith("widget.lifecycle.") || finding.witness == null || !Array.isArray(finding.witness.steps)) continue
    if (finding.artifactId === null) continue
    const pages = widgets.get(finding.artifactId)
    if (!pages) continue
    const pointer = finding.span?.pointer
    const match = typeof pointer === "string" ? PAGE_POINTER.exec(pointer) : null
    const pageIndex = match ? Number(match[1]) : -1
    if (!match || !Number.isSafeInteger(pageIndex) || pageIndex < 0 || pageIndex >= pages.length) {
      results.push({
        code: finding.code,
        message: finding.message,
        witness: null,
        unattributed: true,
      })
      continue
    }
    const machine = pages[pageIndex]?.machine
    if (machine?.lifecycle?.objectType !== objectType || machine.lifecycle.property !== property) continue
    // Where the run leaves the page: the target of its last page move in the widget's own machine.
    const lastPage = [...finding.witness.steps].reverse().flatMap((step) => step.moves).find((move) => move.side === "ui")?.label
    const pageState = lastPage === undefined ? machine.initialStateId : machine.transitions?.find((transition) => transition.transitionId === lastPage)?.target
    results.push({
      code: finding.code,
      message: finding.message,
      witness: finding.witness,
      ...(pageState === undefined ? {} : { pageState }),
    })
  }
  return results
}
