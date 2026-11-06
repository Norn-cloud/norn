import { expect, test } from "bun:test"
import { Refused, liveTransport, within } from "../src/lib/live.ts"
import { readDocument } from "../src/lib/exact.ts"
import type { QuestionAnswer, Transport } from "../src/lib/transport.ts"

// Recordings load through Vite's import.meta.glob, so tests use a stub fallback.
const recordedRead = <T,>(document: T) => readDocument<T>(JSON.stringify(document), "recorded")
const board: QuestionAnswer = {
  question: recordedRead({ name: "demo.yield_week" } as never),
  answer: recordedRead({ freshness: "exact" } as never),
  metrics: recordedRead([] as never),
  objectType: recordedRead({ name: "demo.Conversion" } as never),
}
const recordedTransport: Transport = {
  questionBoard: async () => [board],
  definitions: async () => [],
  restatement: async () => { throw new Error("not used") },
  catalog: async () => recordedRead([] as never),
  guarantees: async () => [{ name: "original", read: recordedRead({} as never) }],
  objectPage: async () => null,
  object: async () => null,
}

type Call = { url: string; authorization: string | null }

function host(status: number, body: string, calls: Call[]): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), authorization: new Headers(init?.headers).get("authorization") })
    return new Response(body, { status })
  }) as typeof fetch
}

test("a member read keeps the server's exact number text", () => {
  const read = within<{ quantity: number }>(readDocument('{"artifacts":[{"quantity":100.000}]}', "live"), "/artifacts/0")
  expect(read.document.quantity).toBe(100)
  expect(read.numbers.get("/quantity")).toBe("100.000")
  expect(read.origin).toBe("live")
})

test("the catalog is read live with the explicit scope and the supplied credential", async () => {
  const calls: Call[] = []
  const transport = liveTransport(
    { baseUrl: "https://cell.example", authorization: () => "Bearer fixture:viewer@demo", fetch: host(200, '{"admitted":true,"artifacts":[{"rate":0.250}]}', calls) },
    recordedTransport,
  )
  const catalog = await transport.catalog()
  expect(catalog.origin).toBe("live")
  expect(catalog.numbers.get("/0/rate")).toBe("0.250")
  expect(calls).toEqual([{ url: "https://cell.example/v1/definitions?orgId=org_demo&environmentId=dev", authorization: "Bearer fixture:viewer@demo" }])
})

test("the answer is live while reads without a live route stay recorded", async () => {
  const calls: Call[] = []
  const transport = liveTransport(
    { baseUrl: "https://cell.example", authorization: () => undefined, fetch: host(200, '{"freshness":"exact","rows":{}}', calls) },
    recordedTransport,
  )
  const [board] = await transport.questionBoard()
  expect(board?.answer.origin).toBe("live")
  expect(board?.question.origin).toBe("recorded")
  expect(calls[0]?.url).toBe("https://cell.example/v1/questions/demo.yield_week/answer?orgId=org_demo&environmentId=analytics")
  expect(calls[0]?.authorization).toBeNull()
  expect((await transport.guarantees())[0]?.read.origin).toBe("recorded")
})

test("a refusal surfaces the host's code, never a fabricated document", async () => {
  const transport = liveTransport(
    { baseUrl: "https://cell.example", authorization: () => undefined, fetch: host(403, '{"code":"authz.denied","message":"the read request is not authorized"}', []) },
    recordedTransport,
  )
  const refused = await transport.catalog().catch((error: unknown) => error)
  expect(refused).toBeInstanceOf(Refused)
  expect((refused as Refused).code).toBe("authz.denied")
  expect((refused as Refused).status).toBe(403)
})
