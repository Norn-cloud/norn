import { readFileSync } from "node:fs"

// The post-deploy check: the Studio an origin serves is the bundle this deploy built.
// Vite names the entry chunk by its content hash (`/assets/index-<hash>.js`), so the entry
// in the served index.html equals the one in the local dist/index.html exactly when the
// deployed bundle is this build. API checks cannot see a stale bundle; this can.
//
//   bun scripts/served-build.ts https://demo.norn.cloud [dist/index.html] [timeout-seconds]

const ENTRY = /<script[^>]*\ssrc="(\/assets\/index-[A-Za-z0-9_-]+\.js)"/

/** The entry chunk an index.html loads, or undefined. */
export function entryChunk(html: string): string | undefined {
  return ENTRY.exec(html)?.[1]
}

/**
 * Poll `origin` until it serves `expected`, for up to `timeoutMs` (a node adopts a new
 * deployment at its next pointer poll, 30 s by default). Returns the last entry seen.
 */
export async function awaitServed(origin: string, expected: string, timeoutMs: number, fetcher: typeof fetch = fetch): Promise<{ readonly served: string | undefined; readonly matched: boolean }> {
  const deadline = Date.now() + timeoutMs
  let served: string | undefined
  for (;;) {
    try {
      const response = await fetcher(`${origin}/?served-build=${Date.now()}`, { headers: { "cache-control": "no-cache" } })
      served = response.ok ? entryChunk(await response.text()) : undefined
    } catch {
      served = undefined
    }
    if (served === expected) return { served, matched: true }
    if (Date.now() >= deadline) return { served, matched: false }
    await new Promise((resolve) => setTimeout(resolve, 3000))
  }
}

if (import.meta.main) {
  const [origin, index = "dist/index.html", seconds = "120"] = process.argv.slice(2)
  if (origin === undefined || !/^https?:\/\/[^/]+$/.test(origin)) throw new Error("usage: served-build.ts <origin> [dist/index.html] [timeout-seconds]")
  const expected = entryChunk(readFileSync(index, "utf8"))
  if (expected === undefined) throw new Error(`${index} names no /assets/index-*.js entry`)
  const { served, matched } = await awaitServed(origin, expected, Number(seconds) * 1000)
  console.log(`${origin} serves ${served ?? "no entry"}; this build is ${expected}: ${matched ? "match" : "MISMATCH"}`)
  if (!matched) process.exit(1)
}
