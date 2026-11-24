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
  const itemFacts = new Map<string, readonly Source[]>([
    ["code", [{ kind: "field", field: "item_code" }]],
    ["state", [{ kind: "gap", reason: "not_in_source", note: "Computed downstream" }]],
    ["empty_field", [{ kind: "empty" }]],
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
        title: "Item quantity must stay positive",
        subject: "app.Item",
        rationale: "Negative quantities cannot be committed.",
        severity: "error",
        modality: "always",
        response: "invariant",
        owner: "ops",
      },
    ],
    feeds: [{ system: "Shared Connector", mappingId: "mapping.alpha_primary", dataset: "alpha_items", facts: itemFacts, ignored: [] }],
  })
  const batch = concept({
    name: "app.Batch",
    title: "Batch",
    fields: [{ id: "batch_id", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" } }],
    feeds: [{ system: "Shared Connector", mappingId: "mapping.alpha_primary", dataset: "alpha_items", facts: new Map(), ignored: [] }],
  })
  const site = concept({
    name: "app.Site",
    title: "Site",
    summary: "Physical location node",
    fields: [{ id: "label", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" } }],
    feeds: [{ system: "Shared Connector", mappingId: "mapping.beta_secondary", dataset: "beta_sites", facts: new Map(), ignored: [] }],
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
    systems: [alphaSystem, betaSystem],
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

test("buildSemanticMap uses server inbox sections for alert totals, joins source_error by mapping_id, highlights sync-error islands under Attention lens, and counts only real mapped fields", () => {
  const inbox = {
    scanLimit: 500,
    sections: [
      { kind: "held_back", count: 2, approximate: true },
      { kind: "source_error", count: 1, approximate: false },
    ],
    items: [
      {
        kind: "held_back",
        objectType: "app.Item",
        count: 2,
        approximate: false,
        title: "Rule A",
        target: "/objects/app.Item?population=held_back",
        ruleRef: { kind: "law", id: "law.a" },
        records: [],
      },
      {
        kind: "held_back",
        objectType: "app.Item",
        count: 1,
        approximate: false,
        title: "Rule B",
        target: "/objects/app.Item?population=held_back",
        ruleRef: { kind: "law", id: "law.b" },
        records: [],
      },
      {
        kind: "source_error",
        id: "conn_beta",
        target: "/connections",
        values: { mapping_id: "mapping.beta_secondary", system: "Shared Connector", status: "failed" },
      },
    ],
  } as unknown as Inbox

  const projection = buildSemanticMap(sampleCatalog(), { kind: "live", inbox })
  // Workspace alert summary comes from server sections (not double-counted from per-rule groups).
  expect(projection.totals.alerts).toEqual({
    heldBack: 2,
    heldBackApproximate: true,
    sourceErrors: 1,
    sourceErrorsApproximate: false,
    scanLimit: 500,
  })

  const alpha = projection.islandsById.get("system:mapping.alpha_primary")!
  const beta = projection.islandsById.get("system:mapping.beta_secondary")!
  const core = projection.islandsById.get("core:model")!

  expect(alpha.ruleHoldings).toBe(3)
  expect(alpha.attention).toBe("critical")
  expect(beta.sourceErrors).toBe(1)
  expect(beta.attention).toBe("critical")
  expect(core.attention).toBe("none")

  // Attention lens must NOT dim beta even though beta's only alert is a source sync error.
  expect(islandMatchesLens(beta, "attention", "")).toBe(true)
  expect(islandMatchesLens(core, "attention", "")).toBe(false)

  const item = projection.nodesById.get("app.Item")!
  expect(nodeMatchesLens(item, "governance")).toBe(true)
  expect(nodeMatchesLens(item, "attention")).toBe(true)
  expect(nodeMatchesQuery(item, "restricted.code")).toBe(true)

  // Dossier counts only actual mapped fields (excluding gap & empty) and surfaces gap count + artifactId.
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
  ])
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
