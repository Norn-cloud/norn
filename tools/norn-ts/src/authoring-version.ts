/**
 * The installed `@norn/authoring` against this CLI. The checker never reads
 * the installed copy (the intrinsics are pinned to the CLI's own declaration),
 * so a different version cannot change admission. It can make the editor show
 * types the checker does not use, so `norn check` and `norn dev` say so, as a
 * warning, never a refusal.
 */
import { existsSync, readFileSync, statSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { TOOL_ROOT } from "./profile.ts"
import { ProjectError } from "./project.ts"

function versionAt(packageJson: string): string | undefined {
  try { return (JSON.parse(readFileSync(packageJson, "utf8")) as { version?: unknown }).version as string | undefined }
  catch { return undefined }
}

/**
 * The version of the `@norn/authoring` a project at `start` resolves to, if any:
 * the nearest `node_modules/@norn/authoring/package.json` walking up, as Node's
 * and Bun's resolution find the package. The file is read directly, not
 * through `require.resolve`, which the package's `exports` (no
 * `./package.json` entry) refuses under Node.
 */
export function installedAuthoringVersion(start: string): string | undefined {
  let dir = resolve(start)
  if (existsSync(dir) && statSync(dir).isFile()) dir = dirname(dir)
  for (;;) {
    const candidate = join(dir, "node_modules", "@norn", "authoring", "package.json")
    if (existsSync(candidate)) return versionAt(candidate)
    const parent = dirname(dir)
    if (parent === dir) return undefined
    dir = parent
  }
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
