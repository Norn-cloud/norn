import { expect, test } from "bun:test"
import { join } from "node:path"
import { appDir, applyMigrations, pinnedCommit, type Run } from "../scripts/celld-deploy.ts"
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

test("pending D1 migrations are applied through celld, idempotently, and a failure fails the deploy", () => {
  const calls: string[] = []
  const answering = (output: string): Run => (command, args) => { calls.push([command, ...args].join(" ")); return output }
  expect(applyMigrations(answering("Applied 0002_connector_binding.sql\n"), "norn-studio-auth-next", "s3://norn-next-celld")).toContain("Applied 0002")
  expect(calls[0]).toBe("celld d1 migrations apply norn-studio-auth-next . --bucket s3://norn-next-celld")
  // A second deploy applies nothing and still passes.
  expect(applyMigrations(answering("No migrations to apply.\n"), "norn-studio-auth-next", "s3://norn-next-celld")).toContain("No migrations")
  const failing: Run = () => { throw new Error("Command failed: celld d1 migrations apply (exit 1): near \"ALTR\": syntax error") }
  expect(() => applyMigrations(failing, "norn-studio-auth-next", "s3://norn-next-celld")).toThrow("syntax error")
  expect(() => applyMigrations(answering(""), "norn-studio-auth-next", "s3://norn-next-celld")).toThrow("gave no result")
  expect(() => applyMigrations(answering("Applied x"), "norn-studio-auth-next", undefined)).toThrow("CELLD_BUCKET")
})
