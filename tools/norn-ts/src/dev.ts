/**
 * `norn dev` (#207): find the project, then run the native dev host with
 * this package's compile worker. The native host owns the cell, admission and
 * serving; this front door only locates the pieces.
 *
 * The host binary is `NORN_HOST_BIN`: `norn-dev-host`, built from the
 * `norn-cell-do` crate (`cargo build -p norn-cell-do --bin norn-dev-host`).
 */
import { existsSync } from "node:fs"
import { join } from "node:path"
import { TOOL_ROOT } from "./profile.ts"
import { ProjectError, readProject } from "./project.ts"

export function hostBinary(env: Readonly<Record<string, string | undefined>> = process.env): string {
  const binary = env.NORN_HOST_BIN
  if (!binary || !existsSync(binary)) {
    throw new ProjectError("cli.setup.host_unavailable",
      `No native Norn host${binary ? ` at ${binary}` : ""}. Build it with \`cargo build -p norn-cell-do --bin norn-dev-host\` and set NORN_HOST_BIN to target/debug/norn-dev-host.`, 3)
  }
  return binary
}

export async function dev(args: readonly string[]): Promise<number> {
  const { root } = readProject()
  const host = hostBinary()
  const child = Bun.spawn([host, "--project", root, ...args, "--", process.execPath, join(TOOL_ROOT, "src/worker.ts")],
    { stdin: "inherit", stdout: "inherit", stderr: "inherit" })
  const stop = () => child.kill("SIGTERM")
  process.on("SIGINT", stop)
  process.on("SIGTERM", stop)
  return await child.exited
}
