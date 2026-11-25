import { readFileSync } from "node:fs"
import type { Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * A stand-in for the cell's record reads, for the record page. Records, links and lineage replay the
 * server's own documents (fixtures/demo/explorer for the three systems, fixtures/privacy for the
 * Party pages as a reader without reveal rights and one with them); `GET /v1/access?objectType=`
 * follows its contract (crates/norn-cell-do/src/server.rs `access`, pinned by tests/tenant_actions.rs):
 * the reader's own decisions and each declared role's, as the cell's PDP made them. Lineage is served
 * only to a role the access document grants Sources, as the cell does. It is not a client policy.
 */

const file = (path: string) => readFileSync(new URL(`../../../${path}`, import.meta.url), "utf8")
const json = (path: string) => JSON.parse(file(path)) as Record<string, unknown> & { artifactId?: string }
const overlay = json("registry/privacy/overlays/canon.Party.entity.json")
const ARTIFACTS = [
  ...(json("fixtures/demo/explorer/three-systems.definitions.json") as unknown as { artifactId: string }[]).map((artifact) => artifact.artifactId === overlay.artifactId ? overlay : artifact),
  json("registry/privacy/artifacts/privacy.markings.entity.json"),
  json("registry/privacy/artifacts/privacy.reveal.policy.json"),
]

/** The reader: a member (Values, no Sources, no reveal) or an admin (everything, reveal). */
export type Reader = "member" | "admin"

const GRANT_TABLE = "norn-cell-do-grants/3"
const permit = (role: string) => ({ decision: "permit", rule: `${GRANT_TABLE}:${role}` })
const deny = { decision: "deny", rule: null }
const READS: Readonly<Record<string, { population: unknown; values: unknown; sources: unknown }>> = {
  viewer: { population: permit("viewer"), values: deny, sources: deny },
  member: { population: permit("member"), values: permit("member"), sources: deny },
  admin: { population: permit("admin"), values: permit("admin"), sources: permit("admin") },
  owner: { population: permit("owner"), values: permit("owner"), sources: permit("owner") },
}

/** What the cell answers each role's "See" on the collection: Population and Values read together, as it does. */
const SEES: Readonly<Record<string, string>> = { viewer: "partial", member: "permit", admin: "permit", owner: "permit" }

/** The action the stand-in's policy governs on each type, and each role's decision on it. */
export const VERIFY = { actionId: "canon.party.verify_contact", title: "Verify contact" }
const ACTS: Readonly<Record<string, string>> = { viewer: "deny", member: "conditional", admin: "permit", owner: "permit" }

/** The access document the cell answers `reader` for `objectType`. */
export function typeAccess(reader: Reader, objectType: string) {
  return {
    principal: { subjectId: `user:${reader}`, actorKind: "user", roles: [reader], roleTable: "studio-roles/1" },
    policy: "demo.host.read",
    grantTable: GRANT_TABLE,
    capabilities: READS[reader],
    objectType,
    roles: Object.entries(READS).map(([role, read]) => ({
      role,
      scope: "collection",
      see: { decision: SEES[role], scope: "collection" },
      read,
      actions: objectType === "canon.Party" ? [{ ...VERIFY, decision: ACTS[role] }] : [],
    })),
  }
}

const recordedName = (id: string) => id.replaceAll("/", "~")
const explorer = (objectType: string, id: string, read: string): string | null => {
  try { return file(`fixtures/demo/explorer/objects/${objectType}/${recordedName(id)}.${read}.json`) } catch { return null }
}

/**
 * The record as the cell projects it for `reader`, read after the governed action in its history: the
 * recorded document at the version that action wrote (one past the recording's), so the page's
 * version and its history's newest agree, as one cell's reads do.
 */
function record(reader: Reader, objectType: string, id: string): Record<string, unknown> | null {
  const recorded = recordedRecord(reader, objectType, id)
  return recorded === null ? null : { ...recorded, version: Number(recorded.version) + 1 }
}

/** The record as recorded: a Party from the privacy pages, any other from the recording. */
function recordedRecord(reader: Reader, objectType: string, id: string): Record<string, unknown> | null {
  if (objectType === "canon.Party" && id.startsWith("demo/party/0")) {
    const page = JSON.parse(file(`fixtures/privacy/canon.Party.${reader === "admin" ? "auditor" : "viewer"}.page.json`)) as { records: { id: string }[] }
    return page.records.find((candidate) => candidate.id === id) ?? null
  }
  const text = explorer(objectType, id, "object")
  return text === null ? null : JSON.parse(text) as Record<string, unknown>
}

/** Where the record came from: the recorded customer Party's lineage stands for the privacy Parties. */
const lineage = (objectType: string, id: string): string | null =>
  objectType === "canon.Party" && id.startsWith("demo/party/0") ? explorer("canon.Party", "demo/party/customer", "source") : explorer(objectType, id, "source")

/**
 * Two versions: the import that created the record, then a governed action that changed one fact.
 * Values are the record's own, as the server projected them. As the cell does, the action names its
 * verified actor only to a role that reads call records (an admin), the import its lineage only to a
 * role with Sources, and no version carries the call's input.
 */
function history(reader: Reader, { objectType, id, values, version }: { objectType: string; id: string; values: Record<string, unknown>; version: number }, source: unknown) {
  // The fact the action changed, and what it read before: a Party was first imported as an organization.
  const changed = objectType === "canon.Party" ? "kind" : Object.keys(values).find((key) => typeof values[key] === "string") ?? "id"
  const earlier = objectType === "canon.Party" ? (values.kind === "organization" ? "person" : "organization") : `Earlier ${String(values[changed] ?? "value")}`
  const before = { ...values, [changed]: earlier }
  return {
    objectType, id, truncated: false,
    versions: [
      { version, commit: 412, logicalTime: Date.parse("2026-11-20T14:05:00Z"), values, writer: reader === "admin" ? { kind: "action", actionId: VERIFY.actionId, actor: "user:sara" } : { kind: "other" } },
      { version: version - 1, commit: 37, logicalTime: Date.parse("2026-11-18T09:12:00Z"), values: before, writer: source === null ? { kind: "other" } : { kind: "mapping", lineage: source } },
    ],
  }
}

export interface RecordCell {
  readonly reveals: unknown[]
  /** The workspace (`orgId/environmentId`) each record read was sent to. */
  readonly readScopes: string[]
  /** Each reveal's workspace and the workspace its expected-scope headers named. */
  readonly revealScopes: { readonly at: string; readonly expected: string }[]
  readonly asked: string[]
  /** Change who the cell answers as from the next read on (a grant revoked: the same record, projected for another role). */
  readonly become: (next: Reader) => void
  /** Hold the next reveal's answer until the returned release is called. */
  readonly holdReveal: () => () => void
  /** Refuse every history read from now on with the cell's opaque 404. */
  readonly refuseHistory: () => void
}

/** Serve the record reads for `reader`; `access` replaces the access answer (a refusal, say). */
export async function recordCell(page: Page, initial: Reader, access?: { status: number; body: unknown }): Promise<RecordCell> {
  const reveals: unknown[] = []
  const readScopes: string[] = []
  const revealScopes: { readonly at: string; readonly expected: string }[] = []
  const asked: string[] = []
  let reader = initial
  let held: Promise<void> | null = null
  let historyRefused = false
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }))
  await session(page)
  await page.route("**/api/host/v1/definitions**", (route) => route.fulfill({ json: { artifacts: ARTIFACTS } }))
  await page.route("**/api/host/v1/actions**", (route) => route.fulfill({ json: { definitionsVersion: 1, items: [] } }))
  await page.route("**/api/host/v1/access**", (route) => {
    const url = new URL(route.request().url())
    asked.push(url.search)
    const objectType = url.searchParams.get("objectType")
    if (access) return route.fulfill({ status: access.status, json: access.body })
    if (objectType === null) { const { objectType: _, roles: __, ...own } = typeAccess(reader, ""); return route.fulfill({ json: own }) }
    return route.fulfill({ json: typeAccess(reader, objectType) })
  })
  await page.route("**/api/host/v1/objects/**", async (route) => {
    const url = new URL(route.request().url())
    // The type's records page, for its grid: the Parties the privacy pages serve this reader.
    if (/\/v1\/objects\/canon\.Party$/.test(url.pathname)) return route.fulfill({ body: file(`fixtures/privacy/canon.Party.${reader === "admin" ? "auditor" : "viewer"}.page.json`), contentType: "application/json" })
    const one = /\/v1\/objects\/([^/]+)\/(.+?)(\/links|\/lineage|\/history|\/findings|\/reveal)?$/.exec(url.pathname)
    const notFound = () => route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such readable object." } })
    if (one === null) return notFound()
    const objectType = decodeURIComponent(one[1] ?? "")
    const id = decodeURIComponent(one[2] ?? "")
    const found = record(reader, objectType, id)
    if (found === null) return notFound()
    const sources = reader === "admin" ? lineage(objectType, id) : null
    const at = `${url.searchParams.get("orgId")}/${url.searchParams.get("environmentId")}`
    switch (one[3]) {
      case undefined: readScopes.push(at); return route.fulfill({ json: found })
      case "/links": return route.fulfill({ body: explorer(objectType, id, "links") ?? JSON.stringify({ objectType, id, items: [], next: null }), contentType: "application/json" })
      case "/lineage": return sources === null ? notFound() : route.fulfill({ body: sources, contentType: "application/json" })
      case "/findings": return route.fulfill({ json: { objectType, id, quarantined: false, findings: [], evidence: [] } })
      // The cell serves a record's history to a role that reads its values; its lineage only under Sources.
      case "/history": return historyRefused ? notFound() : route.fulfill({ json: history(reader, { objectType, id, values: found.values as Record<string, unknown>, version: Number(found.version) }, sources === null ? null : JSON.parse(sources)) })
      case "/reveal": {
        reveals.push(JSON.parse(route.request().postData() ?? "null"))
        const headers = route.request().headers()
        revealScopes.push({ at, expected: `${headers["x-norn-expected-org"] ?? null}/${headers["x-norn-expected-env"] ?? null}` })
        // The cell decided the reveal when it was asked, for who was asking then.
        const asking = reader
        if (held !== null) { const wait = held; held = null; await wait }
        if (asking !== "admin") return route.fulfill({ status: 403, json: { code: "authz.denied", message: "The reveal is not authorized." } })
        // The recorded reveal, of the version the cell serves now.
        const revealed = JSON.parse(file("fixtures/privacy/canon.Party.reveal.json")) as { receipt: Record<string, unknown> }
        return route.fulfill({ json: { ...revealed, receipt: { ...revealed.receipt, version: found.version } } })
      }
      default: return notFound()
    }
  })
  return {
    reveals, asked, readScopes, revealScopes,
    become: (next) => { reader = next },
    holdReveal: () => { let release = () => {}; held = new Promise<void>((resolve) => { release = resolve }); return () => release() },
    refuseHistory: () => { historyRefused = true },
  }
}
