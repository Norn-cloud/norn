import { afterAll, expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import type { ReactNode } from "react"
import { readDocument } from "../src/lib/exact.ts"
import type { ObjectReads, SourceLineage } from "../src/lib/transport.ts"
import type { ObjectFindings } from "../src/evidence/review.ts"
import { CellUnavailable, StudioScopeError, studioScopeKey, WORKSPACE_CHANGED_MESSAGE, type StudioScope } from "../src/lib/definitions.ts"

// These pages read the view from localStorage, which the tests replace; put the original back so later files in the same bun process see their own.
const originalStorage = globalThis.localStorage
afterAll(() => { globalThis.localStorage = originalStorage })

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks; the reads are answered below instead.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { maskedWords, RecordBody, RecordHeader, summaryOf } = await import("../src/record/record-view.tsx")
const { recordPageOf, SourcesItemPage } = await import("../src/record/record-page.tsx")
const { untilDecided, workspaceOf, WorkspaceProvider } = await import("../src/lib/workspace.tsx")
type Workspace = import("../src/lib/workspace.tsx").Workspace
type Fetch = (input: string, init?: RequestInit) => Promise<Response>
const RECORDED = (() => {
  const workspace = workspaceOf({ status: "success", data: { kind: "recorded" } })
  if (workspace.kind !== "recorded") throw new Error("expected the recorded workspace")
  return workspace
})()
/** The workspace a live mode decided at `scope`, its reads sent through `fetcher`. */
function liveAt(scope: StudioScope, fetcher?: Fetch) {
  const workspace = workspaceOf({ status: "success", data: { kind: "live", scope, catalog: { document: [], numbers: new Map(), origin: "live" }, counts: {}, publishedQuestions: new Set<string>() } }, fetcher)
  if (workspace.kind !== "live") throw new Error("expected a live workspace")
  return workspace
}
const { ObjectTypePage } = await import("../src/sources.tsx")
const { ViewProvider } = await import("../src/lib/view.tsx")
const { ProvenanceChain } = await import("../src/components/provenance.tsx")

const fixture = (name: string) => readDocument(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer", name), "utf8"), "recorded")

/**
 * The record at its route as the record page's islands draw it once its read answered (the header and
 * the body, `src/record/record-view.tsx`), with the recorded reads and any `cached` entries, in the
 * recorded workspace; `workspace: null` decides it from the cached mode read instead. `whole` draws the
 * page itself (`SourcesItemPage`): what it shows before the page machine reads anything.
 */
async function page(concept: string, id: string, view = "business", links?: unknown, access?: unknown, cached?: (client: QueryClient) => void, workspace: Workspace | null = RECORDED, whole = false) {
  globalThis.localStorage = { getItem: () => view, setItem: () => {} } as unknown as Storage
  const client = new QueryClient()
  const file = (read: string) => fixture(`objects/${concept}/${id.replaceAll("/", "~")}.${read}.json`)
  client.setQueryData(["catalog"], fixture("three-systems.definitions.json"))
  if (access !== undefined) client.setQueryData(["live-access"], access)
  cached?.(client)
  const data = { object: file("object"), links: links === undefined ? file("links") : readDocument(JSON.stringify(links), "recorded"), source: file("source") } as ObjectReads
  const route = { name: concept, id }
  const Shown = () => <><RecordHeader route={route} data={data} refreshing={false} readAgain={() => {}} /><RecordBody route={route} data={data} readAgain={() => {}} /></>
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/sources/$concept/$", component: whole ? SourcesItemPage : Shown })]),
    history: createMemoryHistory({ initialEntries: [`/sources/${concept}/${id}`] }),
  })
  await router.load()
  return renderToStaticMarkup(<QueryClientProvider client={client}><ViewProvider><WorkspaceProvider {...workspace === null ? {} : { workspace }}><RouterProvider router={router} /></WorkspaceProvider></ViewProvider></QueryClientProvider>)
}

test("a record page names its type and copyable id, and offers every tab", async () => {
  const html = await page("canon.Item", "demo/item/gold")
  expect(html).toContain("Gold scrap")
  expect(html).toContain("Copy demo/item/gold")
  for (const tab of ["Overview", "Lineage", "Rules"]) expect(html).toContain(`>${tab}</button>`)
  expect(html).not.toContain(">Guarantees</button>")
  expect(html).toContain('href="/rules"')
  // Who may act is the cell's decision: the rail is there, and the recorded demo says no role was decided.
  expect(html).not.toContain(">Access</button>")
  expect(html).toContain("Who can see or act")
  expect(html).toContain("The recorded demo has no sign-in, so the cell decided no role&#x27;s access.")
  expect(html).toContain("Rules it keeps")
})

test("each fact carries the system its field provenance names", async () => {
  const html = await page("canon.Item", "demo/item/gold")
  expect(html).toMatch(/data-fact="label"[\s\S]*?data-source="ERPNext"/)
})

test("a value the server recorded a source for opens its provenance", async () => {
  const html = await page("canon.Item", "demo/item/gold")
  expect(html).toMatch(/aria-haspopup="dialog"[^>]*>GOLD</)
})

test("the technical view shows only Norn artefacts: the definition, the mapping pin and ids, and what the read lacks", async () => {
  const html = await page("canon.Item", "demo/item/gold", "technical")
  expect(html).toContain("Record document")
  expect(html).toContain("Copy erpnext.three.mapping@0.2.0")
  // The pin as the recorded source document carries it, so a regenerated fixture moves the test with it (#1347).
  const pin = (JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer/objects/canon.Item/demo~item~gold.source.json"), "utf8")) as { mappingPin: { sha256: string } }).mappingPin.sha256
  expect(html).toContain(`Copy ${pin}`)
  expect(html).toContain("This page shows a recorded snapshot. Once live reads are available, they will return the access decision and receipt for your account.")
  expect(html).toContain("Viewing as")
  expect(html).not.toContain("no signed-in principal")
  expect(html).toContain("&quot;objectType&quot;: &quot;canon.Item&quot;")
})

test("a record id with slashes is the rest of the path, so a reload finds it without an encoded %2F", async () => {
  const html = await page("canon.Item", "demo/item/gold")
  expect(html).toContain("Gold scrap")
  expect(html).not.toContain("Not Found")
})

test("the object type's own URL still opens the type, not an empty record", async () => {
  const client = new QueryClient()
  client.setQueryData(["catalog"], fixture("three-systems.definitions.json"))
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([
      createRoute({ getParentRoute: () => root, path: "/sources/$concept", component: ObjectTypePage }),
      createRoute({ getParentRoute: () => root, path: "/sources/$concept/$", component: SourcesItemPage }),
    ]),
    history: createMemoryHistory({ initialEntries: ["/sources/canon.Holding"] }),
  })
  await router.load()
  const html = renderToStaticMarkup(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>)
  expect(html).not.toContain("This record is not available yet")
  expect(html).toContain("Owner and location partitioned holding")
  expect(html).toContain("Facts recorded")
})

test("the overview lists who points at the record; the one-sentence summary of what it is shows in the technical view only", async () => {
  expect(await page("canon.Item", "demo/item/gold")).not.toContain("Gold scrap is an item record")
  const html = await page("canon.Item", "demo/item/gold", "technical")
  // main's recorded snapshot stores two holdings of the gold item; the empty case is summaryOf's test below.
  expect(html).toContain("Gold scrap is an item record, read from ERPNext by mapping erpnext.three.mapping. 2 stored links connect it to other records.")
  expect(html).toContain("Holding records (item)")
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/owner/north/gold"')
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/owner/south/gold"')
})

test("stored links show on the overview in both directions; a link that is not stored shows nothing", async () => {
  // The shape of the recording once mapped link claims resolve (#877): one page of live links touching the object.
  const links = {
    objectType: "canon.Item", id: "demo/item/gold", hasMore: false,
    items: [
      { sourceType: "canon.Holding", link: "item", source: "demo/holding/vault-1", targetType: "canon.Item", target: "demo/item/gold", version: 1 },
      { sourceType: "canon.Holding", link: "item", source: "demo/holding/vault-2", targetType: "canon.Item", target: "demo/item/gold", version: 1 },
      // Not this object: never shown against it.
      { sourceType: "canon.Holding", link: "item", source: "demo/holding/other", targetType: "canon.Item", target: "demo/item/silver", version: 1 },
      // The same id under another type is another record: never shown against this one.
      { sourceType: "canon.Holding", link: "item", source: "demo/holding/elsewhere", targetType: "canon.Party", target: "demo/item/gold", version: 1 },
    ],
  }
  const html = await page("canon.Item", "demo/item/gold", "business", links)
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/vault-1"')
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/vault-2"')
  expect(html).not.toContain("demo/holding/other")
  expect(html).not.toContain("demo/holding/elsewhere")
  expect(await page("canon.Item", "demo/item/gold", "technical", links)).toContain("2 stored links connect it to other records.")
})

test("when the server returned only the first page of links, the summary does not claim the snapshot has none", () => {
  const concept = { name: "canon.Item", title: "Item", relationships: [] } as unknown as Parameters<typeof summaryOf>[0]
  const holding = { name: "canon.Holding", title: "Holding", relationships: [{ id: "item", target: "canon.Item" }] } as unknown as Parameters<typeof summaryOf>[0]
  const reads = (hasMore: boolean, items: unknown[]) => ({
    object: { document: { id: "demo/item/gold", objectType: "canon.Item", values: { label: "Gold scrap" } } },
    links: { document: { objectType: "canon.Item", id: "demo/item/gold", items, hasMore } },
    source: null,
  }) as unknown as Parameters<typeof summaryOf>[1]
  const edge = { sourceType: "canon.Holding", link: "item", source: "h1", targetType: "canon.Item", target: "demo/item/gold", version: 1 }
  expect(summaryOf(concept, reads(true, []), [holding])).toContain("No stored link appears on the first page the server returned; more may exist.")
  expect(summaryOf(concept, reads(true, [edge]), [holding])).toContain("At least 1 stored link connects it to other records.")
  expect(summaryOf(concept, reads(false, []), [holding])).toContain("Its one declared relation has no stored link in this snapshot.")
})

test("Business shows a type's plain summary only; Technical shows the registry's description and the type's id", async () => {
  const { catalogOf } = await import("../src/lib/catalog.ts")
  const catalog = catalogOf((fixture("three-systems.definitions.json") as { document: Parameters<typeof catalogOf>[0] }).document)
  expect(catalog.byName.get("canon.Holding")).toMatchObject({ summary: "Owner and location partitioned holding", description: undefined })
  expect(catalog.byName.get("core.Resource")?.summary).toBeUndefined()
  expect(catalog.byName.get("core.Resource")?.description).toContain("valuation_method")
  const render = async (view: string) => {
    globalThis.localStorage = { getItem: () => view, setItem: () => {} } as unknown as Storage
    const client = new QueryClient()
    client.setQueryData(["catalog"], fixture("three-systems.definitions.json"))
    const root = createRootRoute()
    const router = createRouter({ routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/sources/$concept", component: ObjectTypePage })]), history: createMemoryHistory({ initialEntries: ["/sources/core.Resource"] }) })
    await router.load()
    return renderToStaticMarkup(<QueryClientProvider client={client}><ViewProvider><RouterProvider router={router} /></ViewProvider></QueryClientProvider>)
  }
  const business = await render("business")
  expect(business).not.toContain("valuation_method")
  expect(business).not.toContain(">core.Resource<")
  const technical = await render("technical")
  expect(technical).toContain("valuation_method")
  expect(technical).toContain(">core.Resource<")
})

test("the technical view shows the host's live access decision as the server wrote it, or its refusal, or the snapshot", async () => {
  // The host's access document (#1022) for a viewer: Population only.
  const document = JSON.stringify({ principal: { subjectId: "user:u1", actorKind: "user", roles: ["viewer"], roleTable: "studio-roles/1" }, policy: "demo.host.read", grantTable: "norn-cell-do-grants/1",
    capabilities: { population: { decision: "permit", rule: "norn-cell-do-grants/1:viewer" }, values: { decision: "deny", rule: null }, sources: { decision: "deny", rule: null } } })
  const live = await page("canon.Item", "demo/item/gold", "technical", undefined, { kind: "read", read: readDocument(document, "live") })
  expect(live).toContain("read live from the cell for your account")
  expect(live).toContain("Copy user:u1")
  expect(live).toContain("norn-cell-do-grants/1:viewer")
  expect(live.match(/>Permit</g)?.length).toBe(1)
  expect(live.match(/>Deny</g)?.length).toBe(2)
  expect(live).toContain("no rule grants it")
  const refused = await page("canon.Item", "demo/item/gold", "technical", undefined, { kind: "refused", refusal: { code: "authz.denied", message: "the read request is not authorized" } })
  expect(refused).toContain("the read request is not authorized")
  expect(refused).not.toContain(">Permit<")
  const unavailable = await page("canon.Item", "demo/item/gold", "technical", undefined, { kind: "unavailable", status: 503 })
  expect(unavailable).toContain("This page shows a recorded snapshot.")
})


const lineage = (digests: Partial<Pick<SourceLineage, "bundleDigest" | "documentDigest">>): SourceLineage => ({
  mappingPin: { artifactId: "demo.mapping", version: "0.1.0", sha256: "m".repeat(64) },
  sourceRecordRef: { system: "synthetic-erpnext", record: "Rows/1" },
  documentRef: { system: "synthetic-erpnext", record: "DOC-1" },
  fieldProvenance: {
    recovered: { source: { system: "synthetic-erpnext", record: "Rows/1" }, mappingVersion: "0.1.0", transform: "rule.recovered", recordedAt: "2026-01-01T00:00:00Z", sourceFields: ["recovered"] },
  },
  ...digests,
}) as SourceLineage

const html = (value: SourceLineage) => renderToStaticMarkup(<ProvenanceChain lineage={value} fact="recovered" />)

test("digests the server withheld read withheld, absent or null, and never leak an old value", () => {
  for (const withheld of [{}, { bundleDigest: null, documentDigest: null }]) {
    const out = html(lineage(withheld))
    expect(out.match(/withheld/g)?.length).toBe(2)
    expect(out).not.toContain("b".repeat(12))
    expect(out).not.toContain("d".repeat(12))
  }
})

test("disclosed digests render, and one withheld digest does not hide the other", () => {
  const both = html(lineage({ bundleDigest: "b".repeat(64), documentDigest: "d".repeat(64) }))
  expect(both).toContain("b".repeat(12))
  expect(both).toContain("d".repeat(12))
  expect(both).not.toContain("withheld")
  const one = html(lineage({ bundleDigest: "b".repeat(64) }))
  expect(one).toContain("b".repeat(12))
  expect(one.match(/withheld/g)?.length).toBe(1)
})

test("rendering again with a different lineage shows only that lineage's digests", () => {
  const first = html(lineage({ bundleDigest: "b".repeat(64), documentDigest: "d".repeat(64) }))
  const second = html(lineage({}))
  expect(first).toContain("b".repeat(12))
  expect(second).not.toContain("b".repeat(12))
})

test("LABEL-LOCALE-1: a partially labelled enum shows every declared value, labelled or as stored and marked English", async () => {
  const { catalogOf } = await import("../src/lib/catalog.ts")
  const { FactsTable } = await import("../src/sources.tsx")
  const { LocaleProvider } = await import("../src/lib/locale.tsx")
  const envelope = (artifactId: string, kind: string, payload: unknown) => ({
    irVersion: "0.1", artifactId, kind, namespace: "sales", name: "Invoice", version: "1.0.0", lifecycle: "active",
    scope: {}, dependencies: [], provenance: { source: "system" }, payload,
  })
  const values = ["draft", "posted", "on_hold", "void"]
  const status = { kind: "enumValue", objectType: "sales.Invoice", property: "status" }
  const artifacts = [
    envelope("sales.Invoice.entity", "entity", { entityVersion: "2.0", objectType: { name: "sales.Invoice", properties: [{ id: "status", type: { kind: "enum", values: values.map((value) => ({ value })) } }] } }),
    envelope("sales.labels", "labels", { labelsVersion: "0.1", entries: [
      { target: { ...status, value: "posted" }, label: { ar: "مُرحّلة" } },
      { target: { ...status, value: "void" }, label: { ar: "ملغاة" } },
    ] }),
  ] as unknown as Parameters<typeof catalogOf>[0]
  const concept = catalogOf(artifacts, "ar").byName.get("sales.Invoice")!
  for (const technical of [false, true]) {
    const html = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><LocaleProvider initial="ar"><FactsTable concept={concept} technical={technical} /></LocaleProvider></QueryClientProvider>)
    // Every declared value is on the page, in declaration order: nothing is hidden for lack of a label.
    const shown = ["draft", "مُرحّلة", technical ? "on_hold" : "on hold", "ملغاة"]
    const at = shown.map((text) => html.indexOf(text))
    expect(at.every((index) => index >= 0)).toBe(true)
    expect([...at].sort((a, b) => a - b)).toEqual(at)
    expect(html).toContain(`data-values="${values.length}"`)
    // An unlabelled value is marked as English; a labelled one is not.
    expect(html).toMatch(/<bdi[^>]*lang="en"[^>]*>draft<\/bdi>/)
    expect(html).not.toMatch(/lang="en"[^>]*>مُرحّلة/)
  }
  // With no label at all in the reader's language, the type's own description lists every value.
  const english = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><FactsTable concept={catalogOf(artifacts, "en").byName.get("sales.Invoice")!} technical={false} /></QueryClientProvider>)
  expect(english).toContain("One of: draft, posted, on hold, void")
})

test("an English summary under an Arabic title is marked lang=en; an Arabic one carries its own tag", async () => {
  const { catalogOf } = await import("../src/lib/catalog.ts")
  const document = (fixture("three-systems.definitions.json") as { document: Parameters<typeof catalogOf>[0] }).document
  const arabic = catalogOf(document, "ar")
  // The locale pack describes canon.Holding in Arabic; core.Agent has an Arabic label and no Arabic description.
  expect(arabic.byName.get("canon.Holding")).toMatchObject({ titleLang: "ar", summary: "رصيد مقسَّم حسب المالك والموقع", summaryLang: "ar" })
  expect(arabic.byName.get("core.Agent")).toMatchObject({ titleLang: "ar", summaryLang: "en" })
  expect(catalogOf(document, "en").byName.get("canon.Holding")).toMatchObject({ titleLang: "en", summary: "Owner and location partitioned holding", summaryLang: "en" })
})

test("a refusal with no detail reads nothing but the refusal: a warm cache's findings, record and history name no rule and no value", async () => {
  const { RedactedLawRefusal } = await import("../src/actions/law-refusal.tsx")
  const artifact = (file: string) => JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/packs/scans/artifacts", file), "utf8")) as unknown
  const client = new QueryClient()
  client.setQueryData(["catalog"], readDocument(JSON.stringify([artifact("docs.ScannedDocument.entity.json"), JSON.parse(readFileSync(join(import.meta.dir, "../../../registry/packs/canon/artifacts/canon.ItemizedDocument.total_matches_lines.law.json"), "utf8"))]), "live"))
  const id = "SCAN-INV-1042"
  // What this page read earlier, under a grant since revoked: the finding's law and the values it compared.
  client.setQueryData(["findings", "docs.ScannedDocument", id], readDocument(JSON.stringify({ objectType: "docs.ScannedDocument", id, quarantined: true, evidence: [],
    findings: [{ id: "f-1", ruleRef: { kind: "law", artifactId: "canon.ItemizedDocument.total_matches_lines.law", lawId: "canon.ItemizedDocument.total_matches_lines", version: "0.1" }, severity: "error", message: "total 720 vs lines 72", status: "open", firstSeen: "", lastSeen: "", values: { total: 720, "lines.amount": [30, 40, 2] } }] }), "live"))
  client.setQueryData(["object", "docs.ScannedDocument", id], null)
  client.setQueryData(["history", "docs.ScannedDocument", id], readDocument(JSON.stringify({ objectType: "docs.ScannedDocument", id, truncated: false,
    versions: [{ version: 1, commit: 2975, logicalTime: 0, values: { total: 720 }, writer: { kind: "other" } }] }), "live"))
  // Handed the record it refused (as the form once did), the panel still reads none of it.
  const Panel = RedactedLawRefusal as unknown as (props: Readonly<Record<string, unknown>>) => ReactNode
  const html = renderToStaticMarkup(<QueryClientProvider client={client}><ViewProvider><Panel objectType="docs.ScannedDocument" id={id} code="core.laws.violated" message="a declared precondition does not hold" technical={false} /></ViewProvider></QueryClientProvider>)
  expect(html).toContain("Refused by a law. Nothing was written.")
  expect(html).toContain("It does not say which law, or which values, to your role.")
  for (const disclosed of ["Open findings", "sum of its lines", "total 720", "720", "72.00", "30.00", "sum of lines"]) expect(html).not.toContain(disclosed)
})

test("a held record's finding shows the values its law compared, a summed list as its exact sum and parts", async () => {
  const { Findings } = await import("../src/evidence/findings.tsx")
  const read = (path: string) => JSON.parse(readFileSync(join(import.meta.dir, "../../../registry/packs", path), "utf8")) as unknown
  const client = new QueryClient()
  client.setQueryData(["catalog"], readDocument(JSON.stringify([read("../../fixtures/packs/scans/artifacts/docs.ScannedDocument.entity.json"), read("canon/artifacts/canon.ItemizedDocument.total_matches_lines.law.json")]), "live"))
  const id = "SCAN-INV-1042"
  const scope = { orgId: "org_a", environmentId: "prod" }
  const { reader } = liveAt(scope)
  client.setQueryData(reader.findings("docs.ScannedDocument", id).queryKey, readDocument<ObjectFindings>(JSON.stringify({ objectType: "docs.ScannedDocument", id, quarantined: true, evidence: [],
    findings: [{ id: "f-1", ruleRef: { kind: "law", artifactId: "canon.ItemizedDocument.total_matches_lines.law", lawId: "canon.ItemizedDocument.total_matches_lines", version: "0.1" }, severity: "error", message: "m", status: "open", firstSeen: "", lastSeen: "", values: { total: 720, "lines.amount": [30, 40, 2] } }] }), "live"))
  const html = renderToStaticMarkup(<QueryClientProvider client={client}><Findings objectType="docs.ScannedDocument" id={id} reader={reader} /></QueryClientProvider>)
  // The total the law read, then what it holds it against: the line objects it summed.
  expect(html).toContain("720.00")
  expect(html).toContain("72.00 = 30.00 + 40.00 + 2.00")
  expect(html).toContain("sum of lines")
  expect(html.indexOf("720.00")).toBeLessThan(html.indexOf("72.00 ="))
})

// #1700 R3: a record page's read (and so its reveals) is bound to the workspace it was read from.
test("a record's read is keyed by and sent to its source workspace: the same id in two workspaces is two records", async () => {
  const A = { orgId: "org_a", environmentId: "prod" }
  const B = { orgId: "org_b", environmentId: "prod" }
  let active = A
  const sent: string[] = []
  const fetcher = async (input: string) => {
    const url = new URL(input, "https://studio.test")
    if (url.pathname === "/api/me") return Response.json({ user: { id: "u" }, organization: { id: active.orgId, role: "admin" }, environmentId: active.environmentId })
    sent.push(`${url.searchParams.get("orgId")}/${url.searchParams.get("environmentId")}`)
    if (url.pathname.endsWith("/links")) return Response.json({ objectType: "canon.Party", id: "p/1", items: [], next: null })
    if (url.pathname.endsWith("/lineage")) return Response.json({ code: "object.not_found" }, { status: 404 })
    // Each workspace serves its own record under the same id.
    return Response.json({ objectType: "canon.Party", id: "p/1", scope: active.orgId, version: 1, values: { display_name: `of ${active.orgId}` }, withheld: false, withheldProperties: [] })
  }
  const signal = new AbortController().signal
  const a = liveAt(A, fetcher).reader.object("canon.Party", "p/1")
  const b = liveAt(B, fetcher).reader.object("canon.Party", "p/1")
  expect(a.queryKey).toEqual(["object", "canon.Party", "p/1", "org_a", "prod"])
  expect(b.queryKey).not.toEqual(a.queryKey)
  expect(RECORDED.reader.object("canon.Party", "p/1").queryKey).toEqual(["object", "recorded", "canon.Party", "p/1"])
  expect((await a.queryFn({ signal }))?.object.document.values["display_name"]).toBe("of org_a")
  expect(sent).toEqual(["org_a/prod", "org_a/prod", "org_a/prod"])
  // The session moves to B: A's read is refused before dispatch, so B's record never lands under A's key.
  active = B
  sent.length = 0
  await expect(a.queryFn({ signal })).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(sent).toEqual([])
  expect((await b.queryFn({ signal }))?.object.document.values["display_name"]).toBe("of org_b")
  expect(sent).toEqual(["org_b/prod", "org_b/prod", "org_b/prod"])
})

// #1740: the grid drawer once read a record through the mode-deciding transport and cached the live
// answer under the key the record page read without a decided mode, so the page could show it with no
// workspace. Only the recording reaches that key now, and the drawer reads only at a decided scope.
test("a record page with no decided mode never shows a cached live answer: the recording's read has a key of its own", async () => {
  const concept = "canon.Item"
  const id = "demo/item/gold"
  const live = (read: string) => readDocument(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer", `objects/${concept}/${id.replaceAll("/", "~")}.${read}.json`), "utf8"), "live")
  const answer = { object: live("object"), links: live("links"), source: live("source") }
  const scope = { orgId: "org_demo", environmentId: "analytics" }
  const html = await page(concept, id, "business", undefined, undefined, (client) => {
    // A live answer under the key the drawer used without a scope, and one at a workspace.
    client.setQueryData(["object", concept, id], answer)
    client.setQueryData(liveAt(scope).reader.object(concept, id).queryKey, answer)
  }, null, true)
  expect(html).not.toContain("Gold scrap")
  expect(html).not.toContain("Read again")
  expect(html).not.toContain('data-fact="')
  // The recording's key is none of the live ones, whatever the type or id.
  expect(RECORDED.reader.object(concept, id).queryKey).not.toEqual(["object", concept, id])
  expect(liveAt(scope).reader.object("recorded", concept).queryKey).not.toEqual(RECORDED.reader.object(concept, id).queryKey)
})

test("a grid's drawer reads through the grid's reader: a live grid's at its workspace, a recorded grid's from the recording", () => {
  const scope = { orgId: "org_a", environmentId: "prod" }
  // At a workspace: the record page's own key and read for that workspace.
  expect(liveAt(scope).reader.object("canon.Party", "p/1").queryKey).toEqual(["object", "canon.Party", "p/1", "org_a", "prod"])
  // A recorded grid: the recording, under the recording's key, which no live answer reaches.
  expect(RECORDED.reader.object("canon.Party", "p/1").queryKey).toEqual(["object", "recorded", "canon.Party", "p/1"])
})

// #1745: a record's findings were read and cached under one key for every workspace, so the record
// page's "Held back" and "N rules broken" could show findings from another workspace, or from before
// its mode failed, beside Workspace unavailable.
test("a record's findings are keyed by and read at its source workspace, and nothing is read without one", async () => {
  const A = { orgId: "org_a", environmentId: "prod" }
  const B = { orgId: "org_b", environmentId: "prod" }
  let active = A
  const sent: string[] = []
  const fetcher = async (input: string) => {
    const url = new URL(input, "https://studio.test")
    if (url.pathname === "/api/me") return Response.json({ user: { id: "u" }, organization: { id: active.orgId, role: "admin" }, environmentId: active.environmentId })
    sent.push(`${url.pathname.endsWith("/findings") ? "findings" : url.pathname} ${url.searchParams.get("orgId")}/${url.searchParams.get("environmentId")}`)
    return Response.json({ objectType: "canon.Party", id: "p/1", quarantined: active.orgId === "org_b", findings: [], evidence: [] })
  }
  const signal = new AbortController().signal
  const read = (scope: StudioScope) => liveAt(scope, fetcher).reader.findings("canon.Party", "p/1").queryFn
  expect([...liveAt(A).reader.findings("canon.Party", "p/1").queryKey]).toEqual(["findings", "canon.Party", "p/1", "org_a", "prod"])
  expect(liveAt(B).reader.findings("canon.Party", "p/1").queryKey).not.toEqual(liveAt(A).reader.findings("canon.Party", "p/1").queryKey)
  // No decided workspace: nothing is read, under a key no read answers and not the one every workspace once shared.
  const undecided = untilDecided(undefined, ["findings", "canon.Party", "p/1"])
  expect(undecided.enabled).toBe(false)
  expect([...undecided.queryKey]).not.toEqual(["findings", "canon.Party", "p/1"])
  await expect(undecided.queryFn({ signal })).rejects.toBeInstanceOf(StudioScopeError)
  expect((await read(A)({ signal })).document.quarantined).toBe(false)
  expect(sent).toEqual(["findings org_a/prod"])
  // The session moves to B: A's read is refused before dispatch, so B's findings never land under A's key.
  active = B
  sent.length = 0
  await expect(read(A)({ signal })).rejects.toThrow(WORKSPACE_CHANGED_MESSAGE)
  expect(sent).toEqual([])
  expect((await read(B)({ signal })).document.quarantined).toBe(true)
  expect(sent).toEqual(["findings org_b/prod"])
})

test("the record page's Quarantined and rules-broken chips show only under a decided mode, from findings read at its workspace", async () => {
  const concept = "canon.Item"
  const id = "demo/item/gold"
  const scope = { orgId: "org_demo", environmentId: "analytics" }
  const live = (read: string) => readDocument(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer", `objects/${concept}/${id.replaceAll("/", "~")}.${read}.json`), "utf8"), "live")
  const answer = { object: live("object"), links: live("links"), source: live("source") }
  const held = readDocument<ObjectFindings>(JSON.stringify({ objectType: concept, id, quarantined: true, evidence: [],
    findings: [{ id: "f-1", ruleRef: { kind: "builtin", id: "core.required", version: "1" }, severity: "error", message: "A required value is missing.", status: "open", firstSeen: "", lastSeen: "" }] }), "live")
  const mode = { kind: "live", scope, catalog: fixture("three-systems.definitions.json"), counts: {}, publishedQuestions: new Set<string>() }
  const modeKey = ["studio-mode", studioScopeKey(scope)]
  const decided = (client: QueryClient) => {
    client.setQueryData(["studio-scope"], scope)
    client.setQueryData(modeKey, mode)
    client.setQueryData(liveAt(scope).reader.object(concept, id).queryKey, answer)
    client.setQueryData(liveAt(scope).reader.findings(concept, id).queryKey, held)
  }
  const shown = await page(concept, id, "business", undefined, undefined, decided, null)
  // One statement of the state: the dot-first Quarantined status, not a second "Held back" chip beside it.
  expect(shown).toContain(">Quarantined<")
  expect(shown).not.toContain(">Held back<")
  expect(shown).toContain("1 rule broken")
  // The mode is read again and the read fails: TanStack keeps its last answer, but nothing read under it
  // shows as current: no chip, no findings panel, no record; the page's Workspace unavailable instead.
  const failed = await page(concept, id, "business", undefined, undefined, (client) => {
    decided(client)
    const read = client.getQueryCache().find({ queryKey: modeKey, exact: true })
    read?.setState({ ...read.state, status: "error", error: new CellUnavailable(503, "The workspace cell could not be opened."), errorUpdatedAt: Date.now() })
  }, null, true)
  for (const absent of [">Quarantined<", "rule broken", "Held out of authoritative reads", "Gold scrap"]) expect(failed).not.toContain(absent)
  expect(failed).toContain("Workspace unavailable")
  // No mode decided at all: findings cached under the key every workspace once shared show nothing.
  const undecided = await page(concept, id, "business", undefined, undefined, (client) => {
    client.setQueryData(["findings", concept, id], held)
    client.setQueryData(liveAt(scope).reader.findings(concept, id).queryKey, held)
  }, null, true)
  for (const absent of [">Quarantined<", "rule broken"]) expect(undecided).not.toContain(absent)
})


test("a type page's grid waits for a decided mode: its skeleton while the mode is read, Workspace unavailable with Retry after a failed read", async () => {
  const { DecidedRecordsGrid } = await import("../src/grid/records-grid.tsx")
  const { catalogOf } = await import("../src/lib/catalog.ts")
  const concept = catalogOf((fixture("three-systems.definitions.json") as { document: Parameters<typeof catalogOf>[0] }).document, "en").byName.get("canon.Item")!
  const render = (workspace: Workspace) => renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><ViewProvider><WorkspaceProvider workspace={workspace}><DecidedRecordsGrid concept={concept} /></WorkspaceProvider></ViewProvider></QueryClientProvider>)
  const pending = render(workspaceOf({ status: "pending" }))
  expect(pending).toContain('data-bone="records-table"')
  expect(pending).toContain('aria-label="Reading the records"')
  expect(pending).not.toContain('role="grid"')
  // A refetch that failed after an earlier live answer: the answer is kept, but no grid reads under it.
  const failed = render(workspaceOf({ status: "error", error: new CellUnavailable(503, "The workspace cell could not be opened.") }))
  expect(failed).toContain("Workspace unavailable")
  expect(failed).toContain(">Retry<")
  expect(failed).toContain('aria-label="Records"')
  expect(failed).not.toContain('role="grid"')
})

// #1763 r1: the header's one withheld chip counts the values the server projected, by how it projected them.
test("the withheld chip counts the masked values in words: one, several, none, and mixed modes", () => {
  expect(maskedWords({ display_name: { mode: "pseudonym" } })).toBe("1 value masked")
  expect(maskedWords({ display_name: { mode: "redacted" }, email: { mode: "pseudonym" }, phone: { mode: "redacted" } })).toBe("3 values masked")
  expect(maskedWords({ display_name: { mode: "refuse" }, email: { mode: "refuse" } })).toBe("2 values withheld")
  expect(maskedWords({ display_name: { mode: "redacted" }, email: { mode: "refuse" } })).toBe("2 values masked or withheld")
  expect(maskedWords({})).toBe("0 values masked")
  // The count is in the reader's number format.
  const many = Object.fromEntries(Array.from({ length: 1200 }, (_, at) => [`p${at}`, { mode: "redacted" }]))
  expect(maskedWords(many, "en")).toBe("1,200 values masked")
  expect(maskedWords(many, "de")).toBe("1.200 values masked")
})

// #1762 C2: the record page is the widget the cell serves; a cell that serves none says so, and Studio
// holds no second record page. (The renderer drawing it in the shell is the browser's to prove:
// e2e/record-states.e2e.ts. Under bun test the renderer's React is its package's own copy.)
test("the record page is the cell's: read from the definitions, or said to be not served", async () => {
  const widget = JSON.parse(readFileSync(join(import.meta.dir, "../../../registry/studio/artifacts/norn.studio.record.widget.json"), "utf8")) as { artifactId: string }
  const definitions = (fixture("three-systems.definitions.json") as { document: { artifactId: string }[] }).document
  const others = definitions.filter((artifact) => artifact.artifactId !== widget.artifactId)
  expect(recordPageOf([...others, widget])?.pages[0]?.pageId).toBe("record")
  expect(recordPageOf(others)).toBeUndefined()
  const render = async (artifacts: unknown[]) => {
    const client = new QueryClient()
    client.setQueryData(["catalog"], readDocument(JSON.stringify(artifacts), "recorded"))
    const root = createRootRoute()
    const router = createRouter({
      routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/sources/$concept/$", component: SourcesItemPage })]),
      history: createMemoryHistory({ initialEntries: ["/sources/canon.Item/demo/item/gold"] }),
    })
    await router.load()
    return renderToStaticMarkup(<QueryClientProvider client={client}><ViewProvider><WorkspaceProvider workspace={RECORDED}><RouterProvider router={router} /></WorkspaceProvider></ViewProvider></QueryClientProvider>)
  }
  const none = await render(others)
  expect(none).toContain('data-record-page="not-served"')
  expect(none).toContain("serves no record page")
})

test("Studio's translations resolve the record page's messages by id, and fall back to the source text", async () => {
  const { message, translate } = await import("../src/lib/translations.ts")
  const { messageId } = await import("../../../tools/norn-ts/src/builder/message.ts")
  expect(translate(messageId("Record"), "ar")).toBe("السجل")
  expect(translate(messageId("Record"), "en")).toBeUndefined()
  expect(translate(messageId("Record", "another use"), "ar")).toBeUndefined()
  expect(message("Lifecycle", "ar")).toBe("دورة الحياة")
  expect(message("Not translated", "ar")).toBe("Not translated")
})

test("two translations of one message id are refused, not silently overwritten", async () => {
  const { byId } = await import("../src/lib/translations.ts")
  expect(byId([{ source: "Record", ar: "السجل" }, { source: "Record", context: "a heading", ar: "سجل" }]).size).toBe(2)
  expect(() => byId([{ source: "Record", ar: "السجل" }, { source: "Record", ar: "سجل" }])).toThrow("share message id")
})
