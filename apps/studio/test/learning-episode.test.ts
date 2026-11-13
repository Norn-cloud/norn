import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { decodeEpisode, readEpisode } from "../src/lib/learning/episode.ts"
import type { ObjectRecord } from "../src/lib/transport.ts"

const fixture = JSON.parse(readFileSync(new URL("./fixtures/learning-episode.json", import.meta.url), "utf8")) as { readonly records: readonly ObjectRecord[] }
const openapi = JSON.parse(readFileSync(new URL("../../../sdks/openapi/norn-api.openapi.json", import.meta.url), "utf8")) as { components: { schemas: Record<string, { required: string[]; properties: Record<string, { type: string }>; additionalProperties: boolean }> } }

test("every fixture record is a served NornObjectRecord: the envelope is checked against the OpenAPI document, not restated", () => {
  const schema = openapi.components.schemas["NornObjectRecord"]!
  const types: Record<string, (v: unknown) => boolean> = { string: (v) => typeof v === "string", integer: (v) => Number.isSafeInteger(v), object: (v) => typeof v === "object" && v !== null && !Array.isArray(v) }
  expect(fixture.records.length).toBeGreaterThan(0)
  for (const record of fixture.records) {
    for (const key of schema.required) expect(record).toHaveProperty(key)
    if (schema.additionalProperties === false) for (const key of Object.keys(record)) expect(Object.keys(schema.properties)).toContain(key)
    // An optional member (a privacy projection's `masked`) is checked only when the record has it.
    for (const [key, property] of Object.entries(schema.properties)) {
      if (!schema.required.includes(key) && !(key in record)) continue
      expect(types[property.type]!((record as unknown as Record<string, unknown>)[key])).toBe(true)
    }
  }
})

test("each fixture decodes, and between them they cover every agreement, outcome state and standing the timeline draws", () => {
  const episodes = fixture.records.map((r) => { const d = decodeEpisode(r); if (d.kind !== "episode") throw new Error(d.problem); return d.episode })
  expect(new Set(episodes.map((e) => e.agreement))).toEqual(new Set(["overridden", "accepted", "none"]))
  expect(new Set(episodes.map((e) => e.outcome.state))).toEqual(new Set(["recorded", "open", "overdue"]))
  expect(new Set(episodes.map((e) => e.standing))).toEqual(new Set(["measured", "context"]))
})

/** Every leaf of a JSON value with its pointer. */
function leaves(value: unknown, at = ""): [string, unknown][] {
  if (Array.isArray(value)) return value.flatMap((v, i) => leaves(v, `${at}/${i}`))
  if (typeof value === "object" && value !== null) return Object.entries(value).flatMap(([k, v]) => leaves(v, `${at}/${k}`))
  return [[at, value]]
}
function replaced(record: ObjectRecord, pointer: string, value: unknown): ObjectRecord {
  const copy = structuredClone(record) as unknown as Record<string, unknown>
  const parts = pointer.split("/").slice(1)
  let node = copy as Record<string, unknown>
  for (const part of parts.slice(0, -1)) node = node[part] as Record<string, unknown>
  node[parts.at(-1)!] = value
  return copy as unknown as ObjectRecord
}

test("fail-closed: any field the timeline reads, given a value of the wrong kind, makes the record no episode", () => {
  for (const record of fixture.records) {
    for (const [pointer, value] of leaves(record)) {
      if (pointer === "/scope" || value === null) continue // the envelope's scope is not read; nulls are absent values
      expect({ pointer, kind: decodeEpisode(replaced(record, pointer, true)).kind }).toEqual({ pointer, kind: "malformed" })
    }
  }
})

test("fail-closed: an episode whose derived fields contradict its parts is not shown as if it held", () => {
  const [overridden, accepted, unsuggested] = fixture.records as [ObjectRecord, ObjectRecord, ObjectRecord]
  for (const [record, pointer, value] of [
    [overridden, "/values/agreement", "accepted"],
    [accepted, "/values/agreement", "overridden"],
    [accepted, "/values/agreement", "none"],
    [unsuggested, "/values/agreement", "accepted"],
    [overridden, "/values/standing", "context"],
    [accepted, "/values/standing", "measured"],
    [accepted, "/values/outcome/state", "recorded"],
    [overridden, "/values/decision/choice", "not_an_option"],
    [overridden, "/objectType", "canon.Item"],
  ] as const) expect({ pointer, kind: decodeEpisode(replaced(record, pointer, value)).kind }).toEqual({ pointer, kind: "malformed" })
})

const ME = { user: { id: "u" }, organization: { id: "org_demo", name: "Demo", role: "member" }, environmentId: "analytics" }
function cell(host: (url: URL) => Response, me: unknown = ME) {
  const calls: URL[] = []
  const fetcher = async (input: string) => {
    const url = new URL(input, "https://studio.test")
    if (url.pathname === "/api/me") return Response.json(me)
    calls.push(url)
    return host(url)
  }
  return { calls, fetcher }
}

test("reading an episode: scoped to the session, each answer kept apart, no cell means no call", async () => {
  const ok = cell(() => Response.json(fixture.records[0]))
  expect((await readEpisode("ep_demo_overridden", ok.fetcher)).kind).toBe("episode")
  expect(ok.calls[0]!.pathname).toBe("/api/host/v1/objects/learning.Episode/ep_demo_overridden")
  expect(Object.fromEntries(ok.calls[0]!.searchParams)).toEqual({ orgId: "org_demo", environmentId: "analytics" })
  expect(await readEpisode("x", cell(() => Response.json({ code: "object.not_found", message: "No such object" }, { status: 404 })).fetcher)).toEqual({ kind: "not-found", message: "No such object" })
  expect(await readEpisode("x", cell(() => Response.json({ code: "authz.denied", message: "denied" }, { status: 403 })).fetcher)).toEqual({ kind: "refused", status: 403, code: "authz.denied", message: "denied" })
  expect(await readEpisode("x", cell(() => new Response("not found", { status: 404 })).fetcher)).toEqual({ kind: "unavailable" })
  expect(await readEpisode("x", cell(() => Response.json({ code: "route.not_found" }, { status: 503 })).fetcher)).toEqual({ kind: "unavailable" })
  expect((await readEpisode("x", cell(() => Response.json({ ...fixture.records[0], values: {} })).fetcher)).kind).toBe("malformed")
  const none = cell(() => { throw new Error("no call") }, { ...ME, environmentId: null })
  expect(await readEpisode("x", none.fetcher)).toEqual({ kind: "no-cell" })
  expect(none.calls).toHaveLength(0)
})
