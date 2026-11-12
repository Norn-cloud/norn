/**
 * The installed `@norn/authoring` against this CLI. The checker never reads
 * the installed copy (the intrinsics are pinned to the CLI's own declaration),
 * so a different version cannot change admission. It can make the editor show
 * types the checker does not use, so `norn check` and `norn dev` say so, as a
 * warning, never a refusal.
 */
import { existsSync, readFileSync } from "node:fs"
import { createRequire } from "node:module"
import { dirname, join } from "node:path"
import { TOOL_ROOT } from "./profile.ts"
import { ProjectError } from "./project.ts"

function versionAt(packageJson: string): string | undefined {
  try { return (JSON.parse(readFileSync(packageJson, "utf8")) as { version?: unknown }).version as string | undefined }
  catch { return undefined }
}

/** The version of the `@norn/authoring` a project at `start` resolves to, if any. */
export function installedAuthoringVersion(start: string): string | undefined {
  const from = existsSync(start) && !start.endsWith(".ts") ? join(start, "noop.js") : join(dirname(start), "noop.js")
  try { return versionAt(createRequire(from).resolve("@norn/authoring/package.json")) }
  catch { return undefined }
}

/** The warning to print when the project's `@norn/authoring` is not this CLI's version; undefined otherwise. */
export function authoringVersionWarning(start: string, cli: string | undefined = versionAt(join(TOOL_ROOT, "package.json"))): ProjectError | undefined {
  const installed = installedAuthoringVersion(start)
  if (!installed || !cli || installed === cli) return undefined
  return new ProjectError("cli.setup.authoring_version_mismatch",
    `The project's installed @norn/authoring is ${installed}, but this CLI is ${cli}. Admission uses the CLI's own authoring types; the editor shows the installed ones. Install @norn/authoring ${cli} to make them agree.`)
}

/** Print the warning to stderr, if there is one. */
export function warnAuthoringVersion(start: string): void {
  const warning = authoringVersionWarning(start)
  if (warning) process.stderr.write(`warning[${warning.code}] ${warning.message}\n`)
}
