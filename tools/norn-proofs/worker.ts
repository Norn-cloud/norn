import { checkFinite, encodeResult } from "./core"
import { integer, object } from "./protocol"

export default {
  async fetch(request) {
    const start = performance.now()
    try {
      if (request.method !== "POST" || !request.body) throw new Error("POST body required")
      const reader = request.body.getReader()
      const chunks: Uint8Array[] = []
      let length = 0
      try {
        while (true) {
          const { done, value } = await reader.read()
          if (done) break
          length += value.byteLength
          if (length > 262144) { await reader.cancel(); throw new Error("generated request exceeds byte budget") }
          chunks.push(value)
        }
      } finally { reader.releaseLock() }
      const bytes = new Uint8Array(length)
      let offset = 0
      for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength }
      const value: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes))
      if (!object(value) || Object.keys(value).length !== 3 || typeof value.source !== "string" ||
          typeof value.expectedStates !== "number") throw new Error("malformed generated request")
      const outcome = await checkFinite({ source: value.source, expectedStates: value.expectedStates,
        seed: integer(value.seed, 0n, 18446744073709551615n), config: "INIT q_init NEXT q_step INVARIANT q_inv" })
      const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value.source))
      const modelSha256 = [...new Uint8Array(digest)].map(byte => byte.toString(16).padStart(2, "0")).join("")
      return new Response(encodeResult(outcome, { modelSha256, elapsedMs: performance.now() - start }),
        { headers: { "content-type": "application/json" } })
    } catch (error) {
      return new Response(JSON.stringify({ outcome: { status: "unknown", reason: String(error) } }),
        { status: 400, headers: { "content-type": "application/json" } })
    }
  },
} satisfies ExportedHandler
