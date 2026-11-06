import { expect, test } from "bun:test"
import { pinnedCommit } from "../scripts/celld-deploy.ts"
import { awaitServed, entryChunk } from "../scripts/served-build.ts"

const HEAD = "93dfdc450d".padEnd(40, "a")

test("a deploy names its commit, which must be HEAD of a clean tree", () => {
  expect(pinnedCommit(["--commit", HEAD], HEAD, "")).toBe(HEAD)
  expect(pinnedCommit(["--commit", "93dfdc450d"], HEAD, "")).toBe(HEAD)
  expect(() => pinnedCommit([], HEAD, "")).toThrow("--commit <sha> is required")
  expect(() => pinnedCommit(["--commit"], HEAD, "")).toThrow("--commit <sha> is required")
  expect(() => pinnedCommit(["--commit", "0acfcc5c3a"], HEAD, "")).toThrow("check out the commit")
  expect(() => pinnedCommit(["--commit", HEAD], HEAD, " M apps/studio/src/main.tsx")).toThrow("the tree has changes")
  expect(() => pinnedCommit(["--commit", HEAD], HEAD, "?? apps/studio/new.ts")).toThrow("the tree has changes")
})

const page = (entry: string) => `<!doctype html><head><script type="module" crossorigin src="/assets/${entry}"></script><script>window.x=1</script></head>`

test("the entry chunk is read from Vite's index.html", () => {
  expect(entryChunk(page("index-B4eJ2Gv7.js"))).toBe("/assets/index-B4eJ2Gv7.js")
  expect(entryChunk("<html><script>1</script></html>")).toBeUndefined()
})

test("the check passes only once the origin serves this build's entry", async () => {
  const served = ["index-Df4KuRos.js", "index-B4eJ2Gv7.js"]
  let calls = 0
  const fetcher = (async () => new Response(page(served[Math.min(calls++, 1)] ?? ""))) as unknown as typeof fetch
  expect(await awaitServed("https://demo.norn.cloud", "/assets/index-B4eJ2Gv7.js", 10_000, fetcher)).toEqual({ served: "/assets/index-B4eJ2Gv7.js", matched: true })
  const stale = (async () => new Response(page("index-Df4KuRos.js"))) as unknown as typeof fetch
  expect(await awaitServed("https://demo.norn.cloud", "/assets/index-B4eJ2Gv7.js", 0, stale)).toEqual({ served: "/assets/index-Df4KuRos.js", matched: false })
  const down = (async () => { throw new Error("offline") }) as unknown as typeof fetch
  expect(await awaitServed("https://demo.norn.cloud", "/assets/index-B4eJ2Gv7.js", 0, down)).toEqual({ served: undefined, matched: false })
}, 20_000)
