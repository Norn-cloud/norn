import { expect, test } from "bun:test"
import { join } from "node:path"
import { appDir, deployThenCheck, pinnedCommit } from "../scripts/celld-deploy.ts"
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

test("--app names another Studio checkout to build; anything else refuses", () => {
  const own = join(import.meta.dir, "..")
  expect(appDir([], own)).toBe(own)
  expect(appDir(["--commit", HEAD], own)).toBe(own)
  expect(() => appDir(["--app"], own)).toThrow("--app needs a directory")
  expect(() => appDir(["--app", "--commit"], own)).toThrow("--app needs a directory")
  expect(() => appDir(["--app", import.meta.dir], own)).toThrow("not a Studio app directory")
})


test("the served-bundle check always runs and is reported, and any failed step fails the deploy", async () => {
  const seen: (string | undefined)[] = []
  const served = (answer: string | undefined) => async (expected: string | undefined) => { seen.push(expected); return { served: answer, matched: answer !== undefined && answer === expected } }
  const ok = await deployThenCheck({ build: () => "/assets/index-A.js", deploy: () => {}, served: served("/assets/index-A.js") })
  expect(ok).toEqual({ ok: true, lines: ["serves /assets/index-A.js, built /assets/index-A.js: match"] })
  // The upload lands but celld reports an error: the check still runs and is reported; the deploy fails.
  const deployFailed = await deployThenCheck({ build: () => "/assets/index-A.js", deploy: () => { throw new Error("migrate: connection refused") }, served: served("/assets/index-A.js") })
  expect(deployFailed).toEqual({ ok: false, lines: ["celld deploy failed: migrate: connection refused", "serves /assets/index-A.js, built /assets/index-A.js: match"] })
  const buildFailed = await deployThenCheck({ build: () => { throw new Error("vite failed") }, deploy: () => { throw new Error("never deployed") }, served: served("/assets/index-old.js") })
  expect(buildFailed).toEqual({ ok: false, lines: ["build failed: vite failed", "serves /assets/index-old.js, built nothing: MISMATCH"] })
  expect(seen).toEqual(["/assets/index-A.js", "/assets/index-A.js", undefined])
  const stale = await deployThenCheck({ build: () => "/assets/index-B.js", deploy: () => {}, served: served("/assets/index-A.js") })
  expect(stale.ok).toBe(false)
})
