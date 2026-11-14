import { execFileSync } from "node:child_process"
import { cpSync, existsSync, readFileSync, realpathSync, rmSync } from "node:fs"
import { join, resolve } from "node:path"
import { celldConfig, celldStudio, writeSecretConfig } from "../celld-config.ts"
import { awaitServed, entryChunk } from "./served-build.ts"

// Deploy the self-hosted Studio to a celld fleet from exactly one commit, then prove the fleet
// serves that build. D1 migrations are applied before this, where the database lives
// (scripts/celld-migrate.ts, on the controller), so this machine needs no host access. Refuses before building when the commit is not given, HEAD is
// not that commit, or the tree has any change; the commit is recorded as STUDIO_BUILD.
//
//   bun scripts/celld-deploy.ts --commit <sha> [--app <checkout>/apps/studio]
//
// --app builds another clean checkout (the integration commit need not contain this
// script); the checks then apply to that checkout. It defaults to this script's own app.
//
// Environment (see #1027): STUDIO_STAGE, BETTER_AUTH_URL, BETTER_AUTH_SECRET,
// STUDIO_CLIENT_IP_HEADER (required: the header the stage's edge sets to the
// client's address, `x-real-ip` behind Traefik, `cf-connecting-ip` behind
// Cloudflare; the deploy refuses without it, #1328),
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

/**
 * The app must be the checkout's own `apps/studio`, after resolving links:
 * a copy elsewhere (for example under an ignored directory) would pass the
 * clean-tree check while STUDIO_BUILD named a commit it was not built from.
 * A real worktree is its own checkout, so it qualifies.
 */
export function appInCheckout(app: string, toplevel: string): string {
  const real = realpathSync(app)
  const own = join(realpathSync(toplevel), "apps", "studio")
  if (real !== own) throw new Error(`${app} is not this checkout's apps/studio (${own}); deploy only a checkout's own app`)
  return real
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

/** The deploy's steps; `served` reads what the origin serves now, given the entry this build expects. */
export interface DeploySteps {
  /** Builds the bundle; returns its entry chunk. */
  readonly build: () => string
  readonly deploy: () => void
  readonly served: (expected: string | undefined) => Promise<{ readonly served: string | undefined; readonly matched: boolean }>
}

/**
 * Build, deploy, then check what the origin serves. The check always runs and is reported, even
 * when building or deploying failed: an upload can land before celld reports an error, and the
 * origin is what people see. The deploy succeeds only if every step did and the entry matches.
 */
export async function deployThenCheck(steps: DeploySteps): Promise<{ readonly ok: boolean; readonly lines: readonly string[] }> {
  const lines: string[] = []
  const attempt = (name: string, step: () => void) => {
    try {
      step()
      return true
    } catch (error) {
      lines.push(`${name} failed: ${error instanceof Error ? error.message.split("\n")[0] : String(error)}`)
      return false
    }
  }
  let expected: string | undefined
  const built = attempt("build", () => { expected = steps.build() })
  const deployed = built && attempt("celld deploy", steps.deploy)
  const { served, matched } = await steps.served(expected)
  lines.push(`serves ${served ?? "no entry"}, built ${expected ?? "nothing"}: ${matched ? "match" : "MISMATCH"}`)
  return { ok: built && deployed && matched, lines }
}

async function main(argv: readonly string[]) {
  const asked = appDir(argv, join(import.meta.dir, ".."))
  const app = appInCheckout(asked, execFileSync("git", ["rev-parse", "--show-toplevel"], { cwd: asked, encoding: "utf8" }).trim())
  const git = (...args: string[]) => execFileSync("git", args, { cwd: app, encoding: "utf8" }).trim()
  const commit = pinnedCommit(argv, git("rev-parse", "HEAD"), git("status", "--porcelain"))
  const studio = celldStudio({ ...process.env, STUDIO_BUILD: commit })
  if (studio.agent !== undefined) {
    const pkg = process.env.NORN_CHECKER_WASM_PKG
    if (pkg === undefined || !existsSync(join(pkg, "norn_checker_wasm_bg.wasm"))) throw new Error("NORN_CHECKER_WASM_PKG must name the built checker package (crates/norn-checker-wasm/scripts/build-pkg.sh)")
    cpSync(pkg, join(app, "worker/checker"), { recursive: true })
  }
  const { ok, lines } = await deployThenCheck({
    build: () => {
      execFileSync("./node_modules/.bin/vite", ["build"], { cwd: app, stdio: "inherit", env: { ...process.env, NODE_ENV: "production" } })
      const entry = entryChunk(readFileSync(join(app, "dist/index.html"), "utf8"))
      if (entry === undefined) throw new Error("dist/index.html names no entry chunk")
      return entry
    },
    deploy: () => {
      // The config carries secrets: written mode 0600, ignored by git, removed after the deploy.
      const config = join(app, "wrangler.json")
      writeSecretConfig(config, celldConfig(studio))
      try {
        execFileSync("celld", ["deploy", ".", "--json"], { cwd: app, stdio: ["ignore", "inherit", "inherit"] })
      } finally {
        rmSync(config, { force: true })
      }
    },
    served: (expected) => awaitServed(studio.authUrl, expected ?? "/assets/<no build>", expected === undefined ? 0 : 150_000),
  })
  for (const line of lines) console.log(`norn-studio-${studio.stage} at ${commit}: ${line}`)
  if (!ok) process.exit(1)
}

if (import.meta.main) await main(process.argv.slice(2))
