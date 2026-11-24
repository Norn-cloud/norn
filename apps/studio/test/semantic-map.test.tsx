import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import "../src/bones/registry.ts"
import { BoneSkeleton } from "../src/components/bone-skeleton.tsx"
import type { Catalog, Concept, SourceSystem } from "../src/lib/catalog.ts"
import {
  buildSemanticMap,
  nodeMatchesLens,
  nodeMatchesQuery,
  provenanceForNode,
} from "../src/lib/semantic-map.ts"
import type { Inbox } from "../src/inbox.tsx"

function concept(partial: Partial<Concept> & { readonly name: string; readonly title: string }): Concept {
  return {
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
    id: "sys_alpha",
    name: "Alpha Source",
    datasets: ["alpha_items"],
    feeds: ["app.Item", "app.Batch"],
    schema: undefined,
  }
  const betaSystem: SourceSystem = {
    id: "sys_beta",
    name: "Beta Source",
    datasets: ["beta_sites"],
    feeds: ["app.Site"],
    schema: undefined,
  }
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
    feeds: [{ system: "sys_alpha", dataset: "alpha_items", facts: new Map(), ignored: [] }],
  })
  const batch = concept({
    name: "app.Batch",
    title: "Batch",
    fields: [{ id: "batch_id", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" } }],
    feeds: [{ system: "sys_alpha", dataset: "alpha_items", facts: new Map(), ignored: [] }],
  })
  const site = concept({
    name: "app.Site",
    title: "Site",
    summary: "Physical location node",
    fields: [{ id: "label", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" } }],
    // Catalog feeds may name a connected system by display name ("Beta Source") as well as id.
    feeds: [{ system: "Beta Source", dataset: "beta_sites", facts: new Map(), ignored: [] }],
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

test("buildSemanticMap groups flat concepts into deterministic Islands with bundled cross-island bridges", () => {
  const projection = buildSemanticMap(sampleCatalog())
  expect(projection.islands.map((i) => i.title)).toEqual(["Alpha Source", "Beta Source", "Core model"])
  expect(projection.totals.islands).toBe(3)
  expect(projection.totals.objectTypes).toBe(4)
  expect(projection.totals.laws).toBe(1)

  // Intra-island link (Item -> Batch) is counted in total links, while cross-island link (Item -> Site) becomes a bridge.
  expect(projection.totals.links).toBe(2)
  expect(projection.bridges).toHaveLength(1)
  expect(projection.bridges[0]).toMatchObject({
    fromIsland: "system:sys_alpha",
    toIsland: "system:sys_beta",
    count: 1,
  })
  expect(projection.bridges[0]!.edges[0]!.relationshipId).toBe("located_at")
})

test("buildSemanticMap surfaces honest held-back and source-error attention counts without inventing verified state", () => {
  const inbox = {
    sections: [],
    items: [
      {
        kind: "held_back",
        objectType: "app.Item",
        count: 3,
        approximate: false,
        title: "Item quantity must stay positive",
        target: "/objects/app.Item?population=held_back",
        ruleRef: { kind: "law", id: "law.item_positive" },
        records: [],
      },
      {
        kind: "source_error",
        id: "sys_beta",
        target: "/connections",
        values: { system: "sys_beta", status: "error" },
      },
    ],
  } as unknown as Inbox

  const projection = buildSemanticMap(sampleCatalog(), inbox)
  const alpha = projection.islandsById.get("system:sys_alpha")!
  const beta = projection.islandsById.get("system:sys_beta")!
  const core = projection.islandsById.get("core:model")!

  expect(alpha.heldCount).toBe(3)
  expect(alpha.attention).toBe("critical")
  expect(beta.sourceErrors).toBe(1)
  expect(beta.attention).toBe("critical")
  expect(core.attention).toBe("none")

  const item = projection.nodesById.get("app.Item")!
  const batch = projection.nodesById.get("app.Batch")!
  expect(nodeMatchesLens(item, "governance")).toBe(true)
  expect(nodeMatchesLens(item, "attention")).toBe(true)
  expect(nodeMatchesLens(batch, "governance")).toBe(false)
  expect(nodeMatchesLens(batch, "attention")).toBe(false)
  expect(nodeMatchesQuery(item, "restricted.code")).toBe(true)
  expect(nodeMatchesQuery(batch, "restricted.code")).toBe(false)

  const dossier = provenanceForNode(projection, "app.Site")!
  expect(dossier.incoming).toHaveLength(1)
  expect(dossier.incoming[0]!.sourceConcept).toBe("app.Item")
  expect(dossier.outgoing).toHaveLength(0)
  expect(dossier.sourceFeeds).toEqual([{ system: "Beta Source", dataset: "beta_sites", mappedFields: 0 }])
})

test("BoneSkeleton renders registered boneyard-js UI-shape bones when loading and children when loaded", () => {
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
})
