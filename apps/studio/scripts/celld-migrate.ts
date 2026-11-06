import { execFileSync, spawnSync } from "node:child_process"

// Apply a commit's Studio D1 migrations where the stage's database lives: the celld node on the
// host, through its operator project, over the controller's existing SSH. Run it on the
// controller BEFORE the deploy (expand first), so the deploy VM needs no host access.
//
//   bun scripts/celld-migrate.ts --commit <app commit> --stage next [--host hetzner]
//
// Migration files come from the app commit (`apps/studio/worker/migrations/NNNN_*.sql`). A file
// the operator project already holds must be byte-identical: an applied migration is never
// edited. celld records applied files in d1_migrations, so a repeat run applies nothing.

/** The operator tooling on the host per stage: the D1 wrapper and the project holding migrations. */
export const STAGES: Readonly<Record<string, { readonly d1: string; readonly project: string }>> = {
  next: { d1: "/opt/norn-demo/d1-next", project: "/opt/norn-demo/ops-next" },
}

export interface Migration { readonly name: string; readonly sha256: string; readonly sql: string }

/** Runs `ssh <host> <command>` with `input` on stdin; returns stdout and stderr together, and the exit status. */
export type Remote = (command: string, input?: string) => { readonly status: number; readonly output: string }

const NAME = /^[0-9]{4}_[a-z0-9_]+\.sql$/

/** Unreachable: the host, or the node that serves the stage's D1, did not answer. */
export class D1Unreachable extends Error {}

const unreachable = (output: string) => /connection refused|could not resolve|no route to host|timed out|no node|connection reset/i.test(output)

/**
 * Copy each migration into the stage's operator project (refusing a changed one), then apply.
 * Returns the apply's report. Throws D1Unreachable when the database cannot be reached, and an
 * Error for any other failure; the caller must not deploy after either.
 */
export function migrate(remote: Remote, stage: string, migrations: readonly Migration[]): string {
  const ops = STAGES[stage]
  if (ops === undefined) throw new Error(`no operator project for stage ${JSON.stringify(stage)}; known: ${Object.keys(STAGES).join(", ")}`)
  for (const migration of migrations) {
    if (!NAME.test(migration.name)) throw new Error(`${migration.name} is not NNNN_name.sql`)
    const target = `${ops.project}/migrations/${migration.name}`
    // Install only when absent; an existing file must match byte for byte.
    const copy = remote(`set -e; if [ -e ${target} ]; then sha256sum ${target} | cut -d' ' -f1; else install -m 0644 /dev/stdin ${target} && echo installed; fi`, migration.sql)
    if (copy.status !== 0) throw (unreachable(copy.output) ? new D1Unreachable(`host unreachable: ${copy.output.trim()}`) : new Error(`copying ${migration.name} failed: ${copy.output.trim()}`))
    const answer = copy.output.trim()
    if (answer !== "installed" && answer !== migration.sha256) throw new Error(`${migration.name} on the host differs from the commit's (${answer.slice(0, 12)} vs ${migration.sha256.slice(0, 12)}): an applied migration is never edited; add a new one`)
  }
  const apply = remote(`${ops.d1} migrations apply norn-studio-auth-${stage} 2>&1`)
  if (apply.status !== 0 || unreachable(apply.output)) {
    throw (unreachable(apply.output) ? new D1Unreachable(`the ${stage} D1 is unreachable: ${apply.output.trim().split("\n").at(-1) ?? ""}`) : new Error(`migrations apply failed: ${apply.output.trim()}`))
  }
  if (!/Applied |No migrations to apply/.test(apply.output)) throw new Error(`migrations apply gave no result: ${apply.output.trim()}`)
  return apply.output.trim()
}

/** The commit's migration files, read from git without checking it out. */
export function commitMigrations(commit: string, git: (...args: string[]) => string): readonly Migration[] {
  return git("ls-tree", "--name-only", commit, "apps/studio/worker/migrations/").split("\n").filter((path) => path.endsWith(".sql")).sort().map((path) => {
    const sql = git("show", `${commit}:${path}`)
    const sha256 = new Bun.CryptoHasher("sha256").update(sql).digest("hex")
    return { name: path.split("/").at(-1) ?? path, sha256, sql }
  })
}

if (import.meta.main) {
  const argv = process.argv.slice(2)
  const value = (flag: string) => { const at = argv.indexOf(flag); return at < 0 ? undefined : argv[at + 1] }
  const commit = value("--commit"), stage = value("--stage"), host = value("--host") ?? "hetzner"
  if (commit === undefined || !/^[0-9a-f]{40}$/.test(commit) || stage === undefined) throw new Error("usage: celld-migrate.ts --commit <full sha> --stage <stage> [--host hetzner]")
  // Keep the exact bytes git holds (git show adds no newline of its own).
  const git = (...args: string[]) => execFileSync("git", args, { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 })
  const remote: Remote = (command, input) => {
    const done = spawnSync("ssh", ["-o", "IdentityAgent=none", "-o", "ConnectTimeout=15", host, command], { input: input ?? "", encoding: "utf8", timeout: 120_000 })
    return { status: done.status ?? 255, output: `${done.stdout ?? ""}${done.stderr ?? ""}${done.error === undefined ? "" : done.error.message}` }
  }
  try {
    console.log(`norn-studio-auth-${stage}: ${migrate(remote, stage, commitMigrations(commit, git)).split("\n").join("; ")}`)
  } catch (error) {
    console.error(`${error instanceof D1Unreachable ? "unreachable" : "failed"}: ${error instanceof Error ? error.message : String(error)}`)
    process.exit(error instanceof D1Unreachable ? 3 : 1)
  }
}
