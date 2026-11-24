import { expect, test } from "bun:test"
import { encodeLiveMessage, LIVE_REFUSALS, parseLiveFrame, type LiveClientMessage, type LiveServerMessage } from "../src/index.ts"

// The frames below are byte for byte what `crates/norn-api/src/live.rs` serializes (its
// `frames_serialize_to_the_protocol_wire_shape` test) and `apps/norn-cell-do/subscribe.mjs` sends.
test("a snapshot frame keeps the exact text of every number in its body", () => {
  const frame = parseLiveFrame('{"type":"snapshot","id":"s1","status":200,"digest":"abc","body":{"total":12345678901234567890.123456789,"rows":[{"v":1.50}]}}')
  if (frame === null || !("numbers" in frame)) throw new Error("expected a snapshot")
  const message: LiveServerMessage = frame.message
  expect(message).toMatchObject({ type: "snapshot", id: "s1", status: 200, digest: "abc" })
  expect(frame.numbers.get("/total")).toBe("12345678901234567890.123456789")
  expect(frame.numbers.get("/rows/0/v")).toBe("1.50")
  expect([...frame.numbers.keys()]).toEqual(["/total", "/rows/0/v"])
})

test("a refused frame is typed, with or without an id; anything else is no frame", () => {
  expect(parseLiveFrame('{"type":"refused","id":null,"code":"live.invalid_id","message":"a subscription id is 1 to 64"}'))
    .toEqual({ message: { type: "refused", id: null, code: LIVE_REFUSALS.invalidId, message: "a subscription id is 1 to 64" } })
  expect(parseLiveFrame('{"type":"refused","id":"s2","code":"live.too_many_subscriptions","message":"full"}'))
    .toEqual({ message: { type: "refused", id: "s2", code: LIVE_REFUSALS.tooManySubscriptions, message: "full" } })
  expect(parseLiveFrame("not json")).toBeNull()
  expect(parseLiveFrame('{"type":"snapshot","id":"s1"}')).toBeNull()
  expect(parseLiveFrame('{"type":"hello"}')).toBeNull()
  const opaque = parseLiveFrame('{"type":"snapshot","id":"s1","status":502,"digest":"abc","body":null}')
  expect(opaque?.message).toEqual({ type: "snapshot", id: "s1", status: 502, digest: "abc", body: null })
})

test("client frames encode to the protocol's wire text", () => {
  const subscribe: LiveClientMessage = { type: "subscribe", id: "s1", path: "/v1/runs/r" }
  expect(encodeLiveMessage(subscribe)).toBe('{"type":"subscribe","id":"s1","path":"/v1/runs/r"}')
  expect(encodeLiveMessage({ type: "unsubscribe", id: "s1" })).toBe('{"type":"unsubscribe","id":"s1"}')
  // @ts-expect-error A subscription names its path.
  void encodeLiveMessage({ type: "subscribe", id: "s1" })
})

test("a repeated member keeps only the numbers JSON.parse keeps: the last", () => {
  const frame = parseLiveFrame('{"type":"snapshot","id":"s1","status":200,"digest":"abc","body":{"value":1.50,"value":null,"nested":{"a":2.0},"nested":{"b":3.00}}}')
  if (frame === null || !("numbers" in frame)) throw new Error("expected a snapshot")
  expect(frame.message.body).toEqual({ value: null, nested: { b: 3 } })
  expect([...frame.numbers]).toEqual([["/nested/b", "3.00"]])
})
