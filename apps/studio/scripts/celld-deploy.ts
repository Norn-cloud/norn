import { execFileSync } from "node:child_process"
import { cpSync, existsSync, readFileSync, rmSync } from "node:fs"
import { join, resolve } from "node:path"
import { celldConfig, celldStudio, writeSecretConfig } from "../celld-config.ts"
import { awaitServed, entryChunk } from "./served-build.ts"

// Deploy the self-hosted Studio to a celld fleet from exactly one commit, then prove the
// fleet serves that build. Refuses before building when the commit is not given, HEAD is
// not that commit, or the tree has any change; the commit is recorded as STUDIO_BUILD.
//
//   bun scripts/celld-deploy.ts --commit <sha> [--app <checkout>/apps/studio]
//
// --app builds another clean checkout (the integration commit need not contain this
// script); the checks then apply to that checkout. It defaults to this script's own app.
//
// Environment (see #1027): STUDIO_STAGE, BETTER_AUTH_URL, BETTER_AUTH_SECRET,
// STUDIO_DEFAULT_ORG, NORN_HOST_SERVICE, AI_GATEWAY_URL + AI_GATEWAY_TOKEN +
// NORN_CHECKER_WASM_PKG (the assistant), CELLD_BUCKET, S3_ENDPOINT, AWS_REGION,
// AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY. `celld` and `esbuild` on PATH.

/** The Studio app directory to build: `--app <dir>`, or this script's own. */
export function appDir(argv: readonly string[], own: string): string {
  const at = argv.indexOf("--app")
  if (at < 0) return own
  const dir = argv[at + 1]
  if (dir === undefined || dir.startsWith("--")) throw new Error("--app needs a directory")
  if (!existsSync(join(dir, "worker/index.ts")) || !existsSync(join(dir, "package.json"))) throw new Error(`${dir} is not a Studio app directory (no worker/index.ts)`)
  return resolve(dir)
}

/** The commit to deploy: given, equal to HEAD, and with a clean tree. */
export function pinnedCommit(argv: readonly string[], head: string, status: string): string {
  const at = argv.indexOf("--commit")
  const given = at < 0 ? undefined : argv[at + 1]
  if (given === undefined || !/^[0-9a-f]{7,40}$/.test(given)) throw new Error("--commit <sha> is required: the integration commit this deploy builds")
  if (!head.startsWith(given)) throw new Error(`HEAD is ${head}, not ${given}: check out the commit to deploy`)
  if (status !== "") throw new Error(`the tree has changes; deploy only a clean checkout:\n${status}`)
  return head
}

async function main(argv: readonly string[]) {
  const app = appDir(argv, join(import.meta.dir, ".."))
  const git = (...args: string[]) => execFileSync("git", args, { cwd: app, encoding: "utf8" }).trim()
  const commit = pinnedCommit(argv, git("rev-parse", "HEAD"), git("status", "--porcelain"))
  const studio = celldStudio({ ...process.env, STUDIO_BUILD: commit })
  if (studio.agent !== undefined) {
    const pkg = process.env.NORN_CHECKER_WASM_PKG
    if (pkg === undefined || !existsSync(join(pkg, "norn_checker_wasm_bg.wasm"))) throw new Error("NORN_CHECKER_WASM_PKG must name the built checker package (crates/norn-checker-wasm/scripts/build-pkg.sh)")
    cpSync(pkg, join(app, "worker/checker"), { recursive: true })
  }
  execFileSync("./node_modules/.bin/vite", ["build"], { cwd: app, stdio: "inherit", env: { ...process.env, NODE_ENV: "production" } })
  const expected = entryChunk(readFileSync(join(app, "dist/index.html"), "utf8"))
  if (expected === undefined) throw new Error("dist/index.html names no entry chunk")
  // The config carries secrets: written mode 0600, ignored by git, removed after the deploy.
  const config = join(app, "wrangler.json")
  writeSecretConfig(config, celldConfig(studio))
  try {
    execFileSync("celld", ["deploy", ".", "--json"], { cwd: app, stdio: ["ignore", "inherit", "inherit"] })
  } finally {
    rmSync(config, { force: true })
  }
  const { served, matched } = await awaitServed(studio.authUrl, expected, 150_000)
  console.log(`norn-studio-${studio.stage} at ${commit}: ${studio.authUrl} serves ${served ?? "no entry"}, built ${expected}: ${matched ? "match" : "MISMATCH"}`)
  if (!matched) process.exit(1)
}

if (import.meta.main) await main(process.argv.slice(2))
