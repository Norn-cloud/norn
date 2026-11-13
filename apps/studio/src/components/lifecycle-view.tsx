import { useQuery } from "@tanstack/react-query"
import { useEffect, useState } from "react"
import { defineLifecycleView, type LifecycleFinding, type NornLifecycleView } from "../../../../packages/norn-demo-ui/src/elements/lifecycle-view.ts"
import type { Concept, Lifecycle } from "../lib/catalog.ts"
import { loadChecker } from "../lib/canvas/checker.ts"
import { useStudioScope } from "../lib/definitions.ts"
import { liveAnswer } from "../lib/live-reads.ts"
import { useLocale } from "../lib/locale.tsx"
import { transport } from "../lib/transport.ts"

/**
 * An object type's lifecycle with each state's live count (UI-LIFECYCLE-2). The element
 * `<norn-lifecycle-view>` draws it; this host gives it the lifecycle as the catalog declares it,
 * one live read of the type's facets on the lifecycle property, and the checker's
 * counterexamples for pages that act on this lifecycle. A finding belongs here when the widget it
 * names declares this object type and property in its page machine's `lifecycle`; nothing is read
 * from its message. The checker loads after the view paints, and its findings join once it is here.
 */
export function LifecycleView({ concept, lifecycle }: { readonly concept: Concept; readonly lifecycle: Lifecycle }) {
  const { locale } = useLocale()
  const scope = useStudioScope().data ?? null
  const definitions = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const checker = useQuery({ queryKey: ["checker"], queryFn: () => loadChecker(), staleTime: Infinity, retry: false })
  const [host, setHost] = useState<HTMLDivElement | null>(null)
  const field = concept.fields.find((candidate) => candidate.id === lifecycle.property)
  const states = field?.scalar.kind === "enum" ? field.scalar.values : [...new Set([...lifecycle.initial, ...lifecycle.terminal, ...lifecycle.transitions.flatMap(({ from, to }) => [...from, to])])]
  const findings = findingsFor(concept.name, lifecycle.property, definitions.data?.document, checker.data)
  const findingsKey = JSON.stringify(findings.map(({ code, message }) => [code, message]))

  useEffect(() => {
    if (host === null || scope === null) return
    defineLifecycleView()
    const view = document.createElement("norn-lifecycle-view") as NornLifecycleView
    const path = `/v1/objects/${encodeURIComponent(concept.name)}:facets?properties=${encodeURIComponent(lifecycle.property)}`
    view.options = {
      lifecycle: { property: lifecycle.property, states, initial: lifecycle.initial, terminal: lifecycle.terminal, transitions: lifecycle.transitions, endings: lifecycle.endings ?? [] },
      title: concept.title,
      counts: (listener) => liveAnswer(path, scope, listener),
      findings,
      locale: locale === "ar" ? "ar" : "en",
      label: (id) => field?.valueLabels?.get(id) ?? id.replaceAll("_", " "),
    }
    host.replaceChildren(view)
    return () => view.remove()
    // A different type, property, scope, language or set of findings draws a fresh view.
  }, [host, concept.name, lifecycle.property, scope?.orgId, scope?.environmentId, locale, findingsKey])

  return <div ref={setHost} data-testid="lifecycle-view" />
}

type Envelope = { readonly artifactId?: string; readonly kind?: string; readonly payload?: { readonly ux?: { readonly pages?: readonly { readonly machine?: { readonly lifecycle?: { readonly objectType?: string; readonly property?: string } } }[] } } }

/** The checker's `widget.lifecycle.*` findings with a run, for widgets whose page machines act on this lifecycle. */
function findingsFor(objectType: string, property: string, definitions: readonly unknown[] | undefined, checker: ((artifacts: readonly unknown[]) => { readonly diagnostics: { readonly diagnostics: readonly unknown[] } }) | undefined): LifecycleFinding[] {
  if (!definitions || !checker) return []
  const acting = new Set(
    (definitions as readonly Envelope[])
      .filter((artifact) => artifact.kind === "widget" && artifact.payload?.ux?.pages?.some((page) => page.machine?.lifecycle?.objectType === objectType && page.machine.lifecycle.property === property))
      .map((artifact) => artifact.artifactId),
  )
  if (acting.size === 0) return []
  return (checker(definitions).diagnostics.diagnostics as readonly (LifecycleFinding & { readonly artifactId?: string })[]).filter(
    (finding) => finding.code.startsWith("widget.lifecycle.") && finding.witness !== undefined && acting.has(finding.artifactId),
  )
}
