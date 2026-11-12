import { expect, test } from "bun:test"
import { Bridge, documentText } from "../src/widget/bridge.ts"
import { answerBuckets, bucketTitle, exactDocument } from "../src/widget/views.tsx"
import type { Answer } from "../src/lib/transport.ts"

/** A stand-in host frame: what the widget posts, and a way to answer it. */
function host() {
  const sent: { id?: number; method?: string; params?: unknown }[] = []
  const frame = { postMessage: (message: { id?: number; method?: string; params?: unknown }) => { sent.push(message) } } as unknown as Window
  let deliver: (event: MessageEvent) => void = () => undefined
  const bridge = new Bridge(frame, (handler) => { deliver = handler })
  const from = (source: unknown, data: unknown) => deliver({ source, data } as MessageEvent)
  return { bridge, sent, frame, from }
}

test("a request resolves with the host's answer to its id, and only the host frame is heard", async () => {
  const { bridge, sent, frame, from } = host()
  const answer = bridge.callTool("norn_trace_answer", { question: "q", lineageRef: "l", bucket: "b" })
  expect(sent[0]).toMatchObject({ method: "tools/call", params: { name: "norn_trace_answer", arguments: { question: "q" } } })
  const id = sent[0]?.id
  from({}, { jsonrpc: "2.0", id, result: { content: [{ type: "text", text: "{}" }], isError: true } })
  from(frame, { jsonrpc: "2.0", id, result: { content: [{ type: "text", text: '{"a":1}' }] } })
  expect(documentText(await answer)).toBe('{"a":1}')
})

test("notifications reach their listeners and host requests are acknowledged", () => {
  const { bridge, sent, frame, from } = host()
  const seen: unknown[] = []
  bridge.on("ui/notifications/tool-result", (params) => seen.push(params))
  from(frame, { jsonrpc: "2.0", method: "ui/notifications/tool-result", params: { isError: false } })
  from(frame, { jsonrpc: "2.0", id: "teardown-1", method: "ui/resource-teardown", params: {} })
  expect(seen).toEqual([{ isError: false }])
  expect(sent.at(-1)).toEqual({ jsonrpc: "2.0", id: "teardown-1", result: {} })
})

test("a bucket key reads as its grouping value", () => {
  expect(bucketTitle('{"week":"2026-10-12"}')).toBe("2026-10-12")
  expect(bucketTitle('"2026-W41"')).toBe("2026-W41")
  expect(bucketTitle('["a","b"]')).toBe("a · b")
  expect(bucketTitle("not json")).toBe("not json")
})

test("bucket values keep the scale the server wrote", () => {
  const text = '{"lineageRef":"l","freshness":"exact","nowMs":0,"knownAsOf":3,"rows":{"{\\"week\\":\\"w1\\"}":{"bucket":{"week":"w1"},"value":100.000,"totals":[2.50,2.000],"members":{"m1":{"version":1,"amounts":[1.250,1.000]}}}}}'
  const read = exactDocument<Answer>({ content: [{ type: "text", text }] })
  expect(read).not.toBeNull()
  const [bucket] = answerBuckets(read!)
  expect(bucket).toMatchObject({ label: "w1", value: "100.000", totals: ["2.50", "2.000"], members: [{ id: "m1", amounts: ["1.250", "1.000"] }] })
})
