import { expect, test } from "bun:test"
import { boundedText } from "../worker/body.ts"

const streamed = (text: string) =>
  new Request("https://studio.test/api/agent/example", {
    method: "POST",
    // A stream has no content-length, so only the read itself can stop it.
    body: new ReadableStream({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(text))
        controller.close()
      },
    }),
  })

test("a body within the limit is read whole", async () => {
  expect(await boundedText(streamed('{"chip":"melt"}'), 64)).toBe('{"chip":"melt"}')
})

test("a streamed body past the limit is refused without a declared length", async () => {
  expect(await boundedText(streamed("x".repeat(65)), 64)).toBeUndefined()
})

test("a declared length past the limit is refused before reading", async () => {
  const request = new Request("https://studio.test/api/agent/example", { method: "POST", body: "{}", headers: { "content-length": "65" } })
  expect(await boundedText(request, 64)).toBeUndefined()
})
