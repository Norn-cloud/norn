/**
 * `norn explain <action|law|concept> <id>` (#517): compile the project's
 * sources and project one `norn/explain@2` report through the checker ABI's
 * `explainArtifactSet`, the same entry point the native `norn explain` calls.
 * A refusal is the checker's `norn/diagnostic@2` findings, each mapped back to
 * the TypeScript line that declared it; `--json` prints the response unchanged
 * apart from that `source` field.
 *
 *   norn explain <action|law|concept> <id> [--json] [--wording FILE --locale TAG]
 *
 * Exit status: 0 explained, 1 refused or not compiled, 2 usage, 3 setup.
 */
import { existsSync, readFileSync, statSync } from "node:fs"
import { resolve } from "node:path"
import { loadChecker, sourceMappedDiagnostics, type CheckerDiagnostic } from "./checker-abi.ts"
import { compilePack } from "./pack.ts"
import { TOOL_ROOT } from "./profile.ts"
import { ProjectError, readProject } from "./project.ts"
import { projectModules } from "./worker.ts"
import type { SourceLocation } from "./builder/diagnostics.ts"

const USAGE = "Usage: norn explain <action|law|concept> <id> [--json] [--wording FILE --locale TAG]"

interface Advice { readonly fix?: string | null; readonly no_fix?: string | null }
type Finding = CheckerDiagnostic & Advice & { readonly source?: SourceLocation }
interface Unknown { readonly footprint: string; readonly reason: string; readonly fix: string; readonly writer?: unknown }
interface Law {
  readonly law: { readonly id: string }
  readonly proof: string
  readonly sourcePath?: string | null
  readonly enforcement?: unknown
  readonly whyUnknown: ReadonlyArray<Unknown>
}
interface Report {
  readonly target: { readonly artifactId: string; readonly version: string }
  readonly closure: { readonly artifacts: ReadonlyArray<unknown> }
  readonly writers: ReadonlyArray<unknown>
  readonly laws: ReadonlyArray<Law>
}

function parse(args: readonly string[]) {
  const [kind, id, ...flags] = args
  if (!kind || !["action", "law", "concept"].includes(kind) || !id || id.startsWith("-")) throw new ProjectError("cli.project.usage", USAGE, 2)
  const options = new Map<string, string>()
  for (let i = 0; i < flags.length; i++) {
    const flag = flags[i]
    if (flag === "--json") continue
    const value = flags[i + 1]
    if ((flag !== "--wording" && flag !== "--locale") || !value || value.startsWith("-")) throw new ProjectError("cli.project.usage", USAGE, 2)
    options.set(flag, value)
    i++
  }
  if (options.has("--wording") !== options.has("--locale")) throw new ProjectError("cli.project.usage", "--wording and --locale go together.", 2)
  return { kind, id, json: flags.includes("--json"), wording: options.get("--wording"), locale: options.get("--locale") }
}

/** One frame per finding: where, what, and the registry's fix or why there is none. */
function frame(root: string, finding: Finding): string {
  const at = finding.source
  const lines = [`${at ? `${at.file}:${at.line}${at.column ? `:${at.column}` : ""} ` : ""}${finding.code}: ${finding.message}`]
  const file = at && [resolve(root, at.file), resolve(TOOL_ROOT, at.file)].find((path) => existsSync(path) && statSync(path).isFile())
  const text = file ? readFileSync(file, "utf8").split("\n")[at.line - 1] : undefined
  if (at && text !== undefined) {
    lines.push(`  ${at.line} | ${text}`)
    if (at.column) lines.push(`  ${" ".repeat(String(at.line).length)} | ${" ".repeat(at.column - 1)}^`)
  }
  if (finding.fix) lines.push(`  fix: ${finding.fix}`)
  else if (finding.no_fix) lines.push(`  no automatic fix: ${finding.no_fix}`)
  return lines.join("\n")
}

function render(report: Report): string {
  const lines = [`${report.target.artifactId}@${report.target.version}`,
    `closure: ${report.closure.artifacts.length} declarations; writers: ${report.writers.length}`]
  for (const law of report.laws) {
    lines.push(`law ${law.law.id}: proof ${law.proof}; enforcement ${law.enforcement ? "declared invariant at commit" : "not established"}`)
    if (law.sourcePath) lines.push(`  authored source: ${law.sourcePath}`)
    for (const unknown of law.whyUnknown) {
      lines.push(`  unknown ${unknown.writer ? `${JSON.stringify(unknown.writer)} ` : ""}${unknown.footprint}: ${unknown.reason}`, `    fix: ${unknown.fix}`)
    }
  }
  return lines.join("\n")
}

export async function explain(args: readonly string[]): Promise<number> {
  const { kind, id, json, wording, locale } = parse(args)
  const { root, manifest } = readProject()
  let requirements: unknown = []
  if (wording) {
    try { requirements = JSON.parse(readFileSync(resolve(wording), "utf8")) }
    catch (error) { throw new ProjectError("cli.project.usage", `Cannot read --wording ${wording} as JSON: ${String(error)}`, 2) }
  }
  // The same confined module set `norn dev` compiles: nothing outside the project.
  const pack = compilePack(root, { modules: projectModules(root, manifest), scope: manifest.scope, root })
  const print = (value: unknown, text: string) => process.stdout.write(`${json ? JSON.stringify(value) : text}\n`)
  if (!pack.ok) {
    const findings: Finding[] = pack.failures.flatMap((failure) => failure.diagnostics.map((diagnostic) => ({
      code: diagnostic.code, severity: "error", message: diagnostic.message,
      source: { file: failure.file, line: diagnostic.start.line, column: diagnostic.start.column },
    })))
    print({ format: "norn/explain@2", admitted: false, report: null, diagnostics: findings }, findings.map((finding) => frame(root, finding)).join("\n"))
    return 1
  }
  let checker: Awaited<ReturnType<typeof loadChecker>>
  try { checker = await loadChecker() }
  catch { checker = undefined }
  if (!checker?.explainArtifactSet) {
    throw new ProjectError("cli.setup.checker_unavailable",
      "The checker WASM package is unavailable. Build it with `bash crates/norn-checker-wasm/scripts/build-pkg.sh \"$PWD/target/norn-checker-wasm/pkg\"` and set NORN_CHECKER_WASM_PKG to that directory.", 3)
  }
  const response = checker.explainArtifactSet([...pack.baseArtifacts, ...pack.artifacts], kind, id, { ...(locale ? { locale } : {}), requirements })
  if (!response.admitted || !response.report) {
    const findings = sourceMappedDiagnostics(response.diagnostics, pack.sourceMap) as ReadonlyArray<Finding>
    print({ ...response, diagnostics: findings }, findings.map((finding) => frame(root, finding)).join("\n"))
    return 1
  }
  print(response, render(response.report as Report))
  return 0
}
