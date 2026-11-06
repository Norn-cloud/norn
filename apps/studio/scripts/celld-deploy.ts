import { execFileSync, spawnSync } from "node:child_process"
import { cpSync, existsSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { celldConfig, celldStudio } from "../celld-config.ts"
import { awaitServed, entryChunk } from "./served-build.ts"

// Deploy the self-hosted Studio to a celld fleet from exactly one commit, apply its pending D1
// migrations, then prove the fleet serves that build. Refuses before building when the commit is not given, HEAD is
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

/** Runs a command in the app directory and returns its combined output; throws on a non-zero exit. */
export type Run = (command: string, args: readonly string[]) => string

/**
 * Apply the stage's pending D1 migrations (`worker/migrations`, the `d1_databases` entry of
 * wrangler.json) through `celld d1 migrations apply`. celld records applied files in
 * `d1_migrations`, so a second run applies nothing. Any failure fails the deploy: a Worker that
 * needs a table its database lacks must not be reported as deployed (#772, a missing
 * 0002_connector_binding.sql shipped once).
 */
export function applyMigrations(run: Run, database: string, bucket: string | undefined): string {
  if (bucket === undefined || bucket === "") throw new Error("CELLD_BUCKET is required to apply the stage's D1 migrations")
  const output = run("celld", ["d1", "migrations", "apply", database, ".", "--bucket", bucket])
  if (!/Applied |No migrations to apply/.test(output)) throw new Error(`celld d1 migrations apply ${database} gave no result:\n${output}`)
  return output
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
  writeFileSync(config, `${JSON.stringify(celldConfig(studio), null, 2)}\n`, { mode: 0o600 })
  // celld d1 prints its messages on stderr; both streams are the result.
  const run: Run = (command, args) => {
    const done = spawnSync(command, args, { cwd: app, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] })
    const output = `${done.stdout ?? ""}${done.stderr ?? ""}`
    if (done.status !== 0) throw new Error(`${command} ${args.join(" ")} failed (exit ${done.status ?? done.signal}):\n${output}`)
    return output
  }
  try {
    execFileSync("celld", ["deploy", ".", "--json"], { cwd: app, stdio: ["ignore", "inherit", "inherit"] })
    // Migrations are additive (expand first), so the schema may lead the code nodes adopt next.
    const database = `norn-studio-auth-${studio.stage}`
    console.log(`${database}: ${applyMigrations(run, database, process.env.CELLD_BUCKET).trim().split("\n").join("; ")}`)
  } finally {
    rmSync(config, { force: true })
  }
  const { served, matched } = await awaitServed(studio.authUrl, expected, 150_000)
  console.log(`norn-studio-${studio.stage} at ${commit}: ${studio.authUrl} serves ${served ?? "no entry"}, built ${expected}: ${matched ? "match" : "MISMATCH"}`)
  if (!matched) process.exit(1)
}

if (import.meta.main) await main(process.argv.slice(2))
