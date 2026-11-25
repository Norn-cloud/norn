import { expect, test } from "bun:test"
import { readdirSync, readFileSync } from "node:fs"
import { resolve } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import "../src/bones/registry.ts"
import { BoneSkeleton } from "../src/components/bone-skeleton.tsx"
import { Skeleton } from "../src/components/ui/skeleton.tsx"
import type { Catalog, Concept, Source, SourceSystem } from "../src/lib/catalog.ts"
import {
  buildSemanticMap,
  islandMatchesLens,
  nodeMatchesLens,
  nodeMatchesQuery,
  provenanceForNode,
} from "../src/lib/semantic-map.ts"
import type { Inbox } from "../src/inbox.tsx"

function concept(partial: Partial<Concept> & { readonly name: string; readonly title: string }): Concept {
  return {
    artifactId: `entity:${partial.name}`,
    titleLang: "en",
    titleNeedsReview: false,
    kind: "concept",
    definition: "object type",
    summary: undefined,
    summaryLang: "en",
    description: undefined,
    parents: [],
    fields: [],
    relationships: [],
    rules: [],
    laws: [],
    feeds: [],
    lifecycles: [],
    ...partial,
  }
}

function sampleCatalog(): Catalog {
  const alphaSystem: SourceSystem = {
    id: "mapping.alpha_primary",
    name: "Shared Connector",
    datasets: ["alpha_items"],
    feeds: ["app.Item", "app.Batch"],
    schema: undefined,
  }
  const betaSystem: SourceSystem = {
    id: "mapping.beta_secondary",
    name: "Shared Connector",
    datasets: ["beta_sites"],
    feeds: ["app.Site"],
    schema: undefined,
  }
  const secondarySystem: SourceSystem = {
    id: "mapping.gamma_secondary",
    name: "Gamma Feed",
    datasets: ["gamma_items"],
    feeds: ["app.Item"],
    schema: undefined,
  }
  const itemFacts = new Map<string, readonly Source[]>([
    ["code", [{ kind: "field", field: "item_code" }]],
    ["state", [{ kind: "gap", reason: "not_in_source", note: "Computed downstream" }]],
    ["empty_field", [{ kind: "empty" }]],
    ["nested_empty", [{ kind: "nested", dataset: "lines", collection: "items", where: "true", value: { kind: "empty" } }]],
  ])
  const item = concept({
    name: "app.Item",
    title: "Item",
    summary: "Tracked unit in the model",
    fields: [
      { id: "code", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" }, markings: ["restricted.code"] },
      { id: "state", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" } },
    ],
    relationships: [
      { id: "located_at", target: "app.Site", description: undefined, count: "one" },
      { id: "belongs_to", target: "app.Batch", description: undefined, count: "at most one" },
    ],
    laws: [
      {
        id: "law.item_positive",
        artifactId: "law:item_positive",
        title: "Item quantity must stay positive",
        subject: "app.Item",
        rationale: "Negative quantities cannot be committed.",
        severity: "error",
        modality: "always",
        response: "invariant",
        owner: "ops",
      },
    ],
    feeds: [
      { system: "Shared Connector", mappingId: "mapping.alpha_primary", dataset: "alpha_items", facts: itemFacts, ignored: [] },
      { system: "Gamma Feed", mappingId: "mapping.gamma_secondary", dataset: "gamma_items", facts: new Map(), ignored: [] },
    ],
  })
  const batch = concept({
    name: "app.Batch",
    title: "Batch",
    fields: [{ id: "batch_id", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" } }],
    // Shares the same admitted law artifact (e.g. inherited from an interface) to verify deduplicated KPI counts.
    laws: [
      {
        id: "law.item_positive",
        artifactId: "law:item_positive",
        title: "Item quantity must stay positive",
        subject: "app.Item",
        rationale: "Negative quantities cannot be committed.",
        severity: "error",
        modality: "always",
        response: "invariant",
        owner: "ops",
      },
    ],
    feeds: [{ system: "Shared Connector", mappingId: "mapping.alpha_primary", dataset: "alpha_items", facts: new Map(), ignored: [] }],
  })
  const site = concept({
    name: "app.Site",
    title: "Site",
    summary: "Physical location node",
    fields: [{ id: "label", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" } }],
    feeds: [
      { system: "Shared Connector", mappingId: "mapping.beta_secondary", dataset: "beta_sites", facts: new Map(), ignored: [] },
      { system: "Gamma Feed", mappingId: "mapping.gamma_secondary", dataset: "gamma_sites", facts: new Map(), ignored: [] },
    ],
  })
  const unmapped = concept({
    name: "core.Note",
    title: "Note",
    kind: "core idea",
    fields: [{ id: "body", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" } }],
  })
  const concepts = [item, batch, site, unmapped]
  return {
    concepts,
    systems: [alphaSystem, betaSystem, secondarySystem],
    byName: new Map(concepts.map((c) => [c.name, c])),
  }
}

test("buildSemanticMap preserves distinct same-named mappings by mappingId, bundles bridges, and never claims no alerts when unserved/recorded", () => {
  const projection = buildSemanticMap(sampleCatalog(), { kind: "unavailable" })
  expect(projection.attentionStatus).toBe("unavailable")
  expect(projection.totals.alerts).toBeNull()
  expect(projection.islands.map((i) => i.id)).toEqual([
    "system:mapping.alpha_primary",
    "system:mapping.beta_secondary",
    "core:model",
  ])
  // Duplicate display names are disambiguated rather than overwriting each other.
  expect(projection.islands[0]!.title).toContain("mapping.alpha_primary")
  expect(projection.islands[1]!.title).toContain("mapping.beta_secondary")
  for (const island of projection.islands) {
    expect(island.attention).toBe("unknown")
  }

  // Intra-island link (Item -> Batch) is counted in total links; cross-island link (Item -> Site) becomes a bridge.
  expect(projection.totals.links).toBe(2)
  expect(projection.bridges).toHaveLength(1)
  expect(projection.bridges[0]).toMatchObject({
    fromIsland: "system:mapping.alpha_primary",
    toIsland: "system:mapping.beta_secondary",
    count: 1,
  })
})

test("buildSemanticMap reflects server rule-holding sums, avoids false green on bounded inbox slices, surfaces secondary feed sync errors, deduplicates shared laws, and excludes nested empty mappings", () => {
  // Strictly typed against `Inbox` (`Schemas["NornInbox"]`) — no cast through `unknown`.
  const inbox: Inbox = {
    scanLimit: 500,
    sections: [
      { kind: "held_back", count: 3, shown: 2, approximate: true },
      { kind: "decision", count: 0, shown: 0, approximate: false },
      { kind: "source_error", count: 2, shown: 2, approximate: false },
    ],
    items: [
      {
        kind: "held_back",
        objectType: "app.Item",
        count: 2,
        approximate: false,
        title: "Rule A",
        target: "/objects/app.Item?population=held_back",
        ruleRef: { kind: "law", artifactId: "law:item_positive", lawId: "law.a", version: "sha256:aaa" },
        records: [
          { id: "item-1", objectType: "app.Item", target: "/objects/app.Item/item-1" },
          { id: "item-2", objectType: "app.Item", target: "/objects/app.Item/item-2" },
        ],
      },
      {
        kind: "held_back",
        objectType: "app.Item",
        count: 1,
        approximate: false,
        title: "Rule B",
        target: "/objects/app.Item?population=held_back",
        ruleRef: { kind: "law", artifactId: "law:item_code", lawId: "law.b", version: "sha256:bbb" },
        records: [
          { id: "item-1", objectType: "app.Item", target: "/objects/app.Item/item-1" },
        ],
      },
      {
        kind: "source_error",
        id: "conn_beta",
        target: "/connections",
        values: { mapping_id: "mapping.beta_secondary", system: "Shared Connector", status: "failed" },
      },
      {
        kind: "source_error",
        id: "conn_gamma",
        target: "/connections",
        values: { mapping_id: "mapping.gamma_secondary", system: "Gamma Feed", status: "failed" },
      },
    ],
  }

  const projection = buildSemanticMap(sampleCatalog(), { kind: "live", inbox })
  // One admitted law governing both app.Item and app.Batch counts once in both island and workspace totals.
  expect(projection.totals.laws).toBe(1)
  // Workspace alert summary comes from server sections (rule holdings, explicitly flagged as bounded).
  expect(projection.totals.alerts).toEqual({
    ruleHoldings: 3,
    ruleHoldingsApproximate: true,
    sourceErrors: 2,
    sourceErrorsApproximate: false,
    bounded: true,
    scanLimit: 500,
  })

  const alpha = projection.islandsById.get("system:mapping.alpha_primary")!
  const beta = projection.islandsById.get("system:mapping.beta_secondary")!
  const core = projection.islandsById.get("core:model")!

  expect(alpha.lawCount).toBe(1)
  expect(alpha.ruleHoldings).toBe(3)
  // Secondary feed mapping.gamma_secondary feeds app.Item inside alpha's island, so its sync error surfaces on alpha.
  expect(alpha.sourceErrors).toBe(1)
  expect(alpha.attention).toBe("critical")
  // Beta has both primary beta_secondary and secondary gamma_secondary failing, and zero rule holdings.
  expect(beta.ruleHoldings).toBe(0)
  expect(beta.sourceErrors).toBe(2)
  expect(beta.attention).toBe("critical")
  // Bounded inbox must NEVER mark unseen islands or unseen nodes green ("none"); core & its node stay "unknown".
  expect(core.attention).toBe("unknown")
  expect(projection.nodesById.get("core.Note")!.attention).toBe("unknown")

  // Attention lens must NOT dim beta when searching by primary title OR by failed secondary feed ("Gamma Feed").
  expect(islandMatchesLens(beta, "attention", "")).toBe(true)
  expect(islandMatchesLens(beta, "attention", "Gamma Feed")).toBe(true)
  expect(islandMatchesLens(core, "attention", "")).toBe(false)

  // Source-only bounded inbox (with the real server scanLimit present) must keep unseen nodes "unknown", never green ("none").
  const sourceBoundedInbox: Inbox = {
    scanLimit: 500,
    sections: [
      { kind: "held_back", count: 0, shown: 0, approximate: false },
      { kind: "decision", count: 0, shown: 0, approximate: false },
      { kind: "source_error", count: 2, shown: 1, approximate: true },
    ],
    items: [
      {
        kind: "source_error",
        id: "conn_gamma_only",
        target: "/connections",
        values: { mapping_id: "mapping.gamma_secondary", system: "Gamma Feed", status: "failed" },
      },
    ],
  }
  const sourceBoundedMap = buildSemanticMap(sampleCatalog(), { kind: "live", inbox: sourceBoundedInbox })
  expect(sourceBoundedMap.totals.alerts?.bounded).toBe(true)
  expect(sourceBoundedMap.islandsById.get("core:model")!.attention).toBe("unknown")
  expect(sourceBoundedMap.nodesById.get("core.Note")!.attention).toBe("unknown")
  // Beta has zero rule holdings and only secondary feed Gamma Feed failing: searching "Gamma Feed" keeps beta lit under Attention.
  const betaSecondaryOnly = sourceBoundedMap.islandsById.get("system:mapping.beta_secondary")!
  expect(betaSecondaryOnly.ruleHoldings).toBe(0)
  expect(betaSecondaryOnly.sourceErrors).toBe(1)
  expect(islandMatchesLens(betaSecondaryOnly, "attention", "Gamma Feed")).toBe(true)

  // A complete, empty live inbox (with the constant server scanLimit: 500) is NOT bounded and legitimately reports "none".
  const completeEmptyInbox: Inbox = {
    scanLimit: 500,
    sections: [
      { kind: "held_back", count: 0, shown: 0, approximate: false },
      { kind: "decision", count: 0, shown: 0, approximate: false },
      { kind: "source_error", count: 0, shown: 0, approximate: false },
    ],
    items: [],
  }
  const completeEmptyMap = buildSemanticMap(sampleCatalog(), { kind: "live", inbox: completeEmptyInbox })
  expect(completeEmptyMap.totals.alerts?.bounded).toBe(false)
  expect(completeEmptyMap.islandsById.get("core:model")!.attention).toBe("none")
  expect(completeEmptyMap.nodesById.get("core.Note")!.attention).toBe("none")

  const item = projection.nodesById.get("app.Item")!
  expect(nodeMatchesLens(item, "governance")).toBe(true)
  expect(nodeMatchesLens(item, "attention")).toBe(true)
  expect(nodeMatchesQuery(item, "restricted.code")).toBe(true)

  // Dossier counts only actual mapped fields (excluding gap, empty, and nested empty) and surfaces gap count + artifactId.
  const itemDossier = provenanceForNode(projection, "app.Item")!
  expect(itemDossier.node.artifactId).toBe("entity:app.Item")
  expect(itemDossier.sourceFeeds).toEqual([
    {
      system: "Shared Connector",
      mappingId: "mapping.alpha_primary",
      dataset: "alpha_items",
      mappedFields: 1,
      gapCount: 1,
    },
    {
      system: "Gamma Feed",
      mappingId: "mapping.gamma_secondary",
      dataset: "gamma_items",
      mappedFields: 0,
      gapCount: 0,
    },
  ])
})

// #1705: the inbox returns at most 20 held-back groups; 21 one-record rules must read as at least 20.
test("buildSemanticMap marks counts as lower bounds when the inbox capped its groups or sources", () => {
  const groups = Array.from({ length: 20 }, (_, index) => ({
    kind: "held_back" as const, objectType: "app.Item", count: 1, approximate: false, title: `Rule ${index}`,
    target: "/objects/app.Item?population=held_back",
    ruleRef: { kind: "law" as const, artifactId: `law:${index}`, lawId: `law.${index}`, version: "sha256:1" }, records: [],
  }))
  const inbox: Inbox = {
    scanLimit: 500,
    sections: [
      { kind: "held_back", count: 21, shown: 20, approximate: false },
      { kind: "decision", count: 0, shown: 0, approximate: false },
      { kind: "source_error", count: 0, shown: 0, approximate: false },
    ],
    items: groups,
  }
  const map = buildSemanticMap(sampleCatalog(), { kind: "live", inbox })
  const item = map.nodesById.get("app.Item")!
  expect(item.ruleHoldings).toBe(20)
  expect(item.ruleHoldingsApproximate).toBe(true)
  expect(map.islandsById.get(item.islandId)!.ruleHoldingsApproximate).toBe(true)
  // The section's own count is exact (21); only what the map places from the returned slice is a lower bound.
  expect(map.totals.alerts).toMatchObject({ ruleHoldings: 21, ruleHoldingsApproximate: false, bounded: true })

  const sources: Inbox = {
    scanLimit: 500,
    sections: [
      { kind: "held_back", count: 0, shown: 0, approximate: false },
      { kind: "decision", count: 0, shown: 0, approximate: false },
      { kind: "source_error", count: 21, shown: 20, approximate: false },
    ],
    items: Array.from({ length: 20 }, (_, index) => ({ kind: "source_error" as const, id: `conn_${index}`, target: "/connections", values: { mapping_id: "mapping.gamma_secondary" } })),
  }
  const capped = buildSemanticMap(sampleCatalog(), { kind: "live", inbox: sources })
  const gamma = capped.nodesById.get("app.Item")!
  expect(gamma.sourceErrors).toBe(20)
  expect(gamma.sourceErrorsApproximate).toBe(true)
  expect(capped.islandsById.get(gamma.islandId)!.sourceErrorsApproximate).toBe(true)
  expect(capped.totals.alerts).toMatchObject({ sourceErrors: 21, sourceErrorsApproximate: false, bounded: true })
})

// #1705: a source error's own id is its connection record's id; only a named mapping or an unambiguous system places it.
test("buildSemanticMap never joins a source error on its connection id, and an unplaced error keeps every island unknown", () => {
  const inbox: Inbox = {
    scanLimit: 500,
    sections: [
      { kind: "held_back", count: 0, shown: 0, approximate: false },
      { kind: "decision", count: 0, shown: 0, approximate: false },
      { kind: "source_error", count: 1, shown: 1, approximate: false },
    ],
    // The id reads like a mapping id, and the system name is shared by two mappings: neither places it.
    items: [{ kind: "source_error", id: "mapping.alpha_primary", target: "/connections", values: { system: "Shared Connector", status: "failed" } }],
  }
  const map = buildSemanticMap(sampleCatalog(), { kind: "live", inbox })
  const alpha = map.islandsById.get("system:mapping.alpha_primary")!
  expect(alpha.sourceErrors).toBe(0)
  expect(alpha.attention).toBe("unknown")
  expect([...map.islandsById.values()].every((island) => island.attention === "unknown")).toBe(true)
  expect(map.totals.alerts).toMatchObject({ sourceErrors: 1, sourceErrorsApproximate: false, bounded: true })

  const named = buildSemanticMap(sampleCatalog(), { kind: "live", inbox: { ...inbox, items: [{ kind: "source_error", id: "conn_9", target: "/connections", values: { system: "Gamma Feed" } }] } })
  expect(named.nodesById.get("app.Item")!.sourceErrors).toBe(1)
  expect(named.nodesById.get("app.Item")!.sourceErrorsApproximate).toBe(false)

  // #1724 (Fable r1): a system *name* that equals a mapping id is a name, never that mapping.
  const nameLikeId = buildSemanticMap(sampleCatalog(), { kind: "live", inbox: { ...inbox, items: [{ kind: "source_error", id: "conn_10", target: "/connections", values: { system: "mapping.gamma_secondary" } }] } })
  expect(nameLikeId.nodesById.get("app.Item")!.sourceErrors).toBe(0)
  expect(nameLikeId.totals.alerts).toMatchObject({ sourceErrors: 1, bounded: true })
})

test("every boneyard-js .bones.json capture uses valid 0..100 percentage x/w coordinates with zero horizontal overflow", () => {
  const dir = resolve(import.meta.dir, "../src/bones")
  const files = readdirSync(dir).filter((f) => f.endsWith(".bones.json"))
  expect(files.length).toBeGreaterThanOrEqual(6)
  for (const file of files) {
    const parsed = JSON.parse(readFileSync(resolve(dir, file), "utf8")) as {
      breakpoints: Record<string, { width: number; height: number; bones: (number | string | boolean)[][] }>
    }
    const bps = Object.entries(parsed.breakpoints)
    expect(bps.length, `${file} breakpoints`).toBeGreaterThanOrEqual(3)
    for (const [bpKey, bp] of bps) {
      expect(bp.bones.length, `${file}@${bpKey} bone count`).toBeGreaterThanOrEqual(5)
      for (const bone of bp.bones) {
        const [x, y, w, h] = bone as [number, number, number, number]
        expect(x, `${file}@${bpKey} x >= 0`).toBeGreaterThanOrEqual(0)
        expect(w, `${file}@${bpKey} w > 0`).toBeGreaterThan(0)
        expect(x + w, `${file}@${bpKey} x+w <= 100%`).toBeLessThanOrEqual(100.01)
        expect(y, `${file}@${bpKey} y >= 0`).toBeGreaterThanOrEqual(0)
        expect(h, `${file}@${bpKey} h > 0`).toBeGreaterThan(0)
      }
    }
  }
})

test("BoneSkeleton and fallback Skeleton respect loading state and prefers-reduced-motion", () => {
  const loadingHtml = renderToStaticMarkup(
    <BoneSkeleton name="semantic-map" loading label="Building semantic map">
      <div>Loaded map</div>
    </BoneSkeleton>,
  )
  expect(loadingHtml).toContain('data-slot="bone-skeleton"')
  expect(loadingHtml).toContain('data-bone="semantic-map"')
  expect(loadingHtml).toContain('aria-busy="true"')
  expect(loadingHtml).not.toContain("Loaded map")

  const loadedHtml = renderToStaticMarkup(
    <BoneSkeleton name="semantic-map" loading={false} label="Building semantic map">
      <div>Loaded map</div>
    </BoneSkeleton>,
  )
  expect(loadedHtml).toContain("Loaded map")
  expect(loadedHtml).not.toContain('aria-busy="true"')

  const fallbackHtml = renderToStaticMarkup(<Skeleton className="h-4 w-24" />)
  expect(fallbackHtml).toContain("motion-reduce:animate-none")
})
