/**
 * `norn explain <action|law|concept> <id>` (#517): compile the project's
 * sources and project one `norn/explain@2` report through the checker ABI's
 * `explainArtifactSet`, the same entry point the native `norn explain` calls.
 *
 *   norn explain <action|law|concept> <id> [--json] [--wording FILE --locale TAG]
 *
 * One diagnostic model, on every path (EXPLAIN-JSON-1). `--json` always prints
 * exactly one `norn/explain@2` response, and every entry of its `diagnostics`
 * is `norn/diagnostic@2` with a registered code and the registry's `fix` or
 * `no_fix`:
 * - a checker refusal is the checker's own findings (the ones `norn check`
 *   reports for the same sources), each with the TypeScript `source` line;
 * - a source that does not compile is `cli.compile.source_refused`, one per
 *   compiler finding, with its `source` line and, in `compiler`, the
 *   compiler's own code and span;
 * - bad usage is `cli.explain.usage`; input the checker's entry point refuses
 *   (a wording file of the wrong shape, input over its size limit) is
 *   `cli.explain.input_refused`; a missing project or checker is its `cli.*`
 *   setup code;
 * - anything else that is thrown is `cli.explain.failed`. `explain` is the one
 *   boundary: nothing thrown below it reaches the caller as a stack trace.
 * Without `--json` the same findings print as code frames.
 *
 * The compile is the one `norn dev` uses (`compileProjectPack`) and the
 * admission is the one `norn check` uses (`admit`): explain gives the checker
 * the exact input of `norn check`'s final pass, and refuses whatever that
 * pass refuses. Compiling evaluates the project's modules, as `norn dev`
 * does; nothing is written and no cell is read.
 *
 * Exit status: 0 explained, 1 refused or not compiled, 2 usage, 3 setup or failure.
 */
import { existsSync, readFileSync, statSync } from "node:fs"
import { resolve } from "node:path"
import { loadChecker, sourceMappedDiagnostics, type CheckerAbi } from "./checker-abi.ts"
import { checkerDiagnostic, cliDiagnostic, registeredCodes, type Finding } from "./cli-diagnostic.ts"
import { admit, type PackResult } from "./pack.ts"
import { TOOL_ROOT } from "./profile.ts"
import { ProjectError, readProject, type Project } from "./project.ts"
import { compileProjectPack } from "./worker.ts"

const USAGE = "Usage: norn explain <action|law|concept> <id> [--json] [--wording FILE --locale TAG]"

/** What `explain` reaches outside itself; a test replaces a part to prove the boundary. */
export interface ExplainHost {
  /** Where the project is looked for. */
  readonly cwd: string
  readonly write: (text: string) => void
  readonly load: () => Promise<CheckerAbi | undefined>
  readonly compile: (root: string, manifest: Project) => PackResult
}

const HOST: ExplainHost = {
  cwd: process.cwd(),
  write: (text) => { process.stdout.write(text) },
  load: () => loadChecker(),
  compile: compileProjectPack,
}

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
  if (!kind || !["action", "law", "concept"].includes(kind) || !id || id.startsWith("-")) throw new ProjectError("cli.explain.usage", USAGE, 2)
  const options = new Map<string, string>()
  for (let i = 0; i < flags.length; i++) {
    const flag = flags[i]
    if (flag === "--json") continue
    const value = flags[i + 1]
    if ((flag !== "--wording" && flag !== "--locale") || !value || value.startsWith("-")) throw new ProjectError("cli.explain.usage", USAGE, 2)
    options.set(flag, value)
    i++
  }
  if (options.has("--wording") !== options.has("--locale")) throw new ProjectError("cli.explain.usage", "--wording and --locale go together.", 2)
  return { kind, id, wording: options.get("--wording"), locale: options.get("--locale") }
}

/** The wording file: a JSON array of objects. The checker validates each requirement's members. */
function requirements(path: string | undefined, cwd: string): ReadonlyArray<unknown> {
  if (!path) return []
  let parsed: unknown
  try { parsed = JSON.parse(readFileSync(resolve(cwd, path), "utf8")) }
  catch (error) { throw new ProjectError("cli.explain.usage", `Cannot read --wording ${path} as JSON: ${String(error)}`, 2) }
  if (!Array.isArray(parsed) || !parsed.every((entry) => typeof entry === "object" && entry !== null && !Array.isArray(entry))) {
    throw new ProjectError("cli.explain.usage", `--wording ${path} must hold a JSON array of structured requirements (objects).`, 2)
  }
  return parsed
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

/** The checker with its explain entry point, or the registered setup refusal with the reason. */
async function explainingChecker(host: ExplainHost): Promise<CheckerAbi & { readonly explainArtifactSet: NonNullable<CheckerAbi["explainArtifactSet"]> }> {
  const unavailable = (detail: string) =>
    new ProjectError("cli.setup.checker_unavailable", `${registeredCodes().get("cli.setup.checker_unavailable")?.message ?? ""} (${detail})`, 3)
  let checker: CheckerAbi | undefined
  try { checker = await host.load() }
  catch (error) { throw unavailable(String(error)) }
  if (!checker) throw unavailable("NORN_CHECKER_WASM_PKG is not set")
  const { explainArtifactSet } = checker
  if (!explainArtifactSet) throw unavailable("the package was built before explainArtifactSet existed; rebuild it")
  return { ...checker, explainArtifactSet }
}

/** Refused: the response and its frames. Every finding is `norn/diagnostic@2`. */
function refused(host: ExplainHost, json: boolean, root: string, findings: ReadonlyArray<Finding>): void {
  host.write(`${json
    ? JSON.stringify({ format: "norn/explain@2", admitted: false, report: null, diagnostics: findings })
    : findings.map((finding) => frame(root, finding)).join("\n")}\n`)
}

async function run(args: readonly string[], json: boolean, host: ExplainHost): Promise<number> {
  const { kind, id, wording, locale } = parse(args)
  const { root, manifest } = readProject(host.cwd)
  const options = { ...(locale ? { locale } : {}), requirements: requirements(wording, host.cwd) }
  // Compile before the checker is needed: a source error is a source error with or without the package.
  const pack = host.compile(root, manifest)
  if (!pack.ok) {
    refused(host, json, root, pack.failures.flatMap((failure) => failure.diagnostics.map((diagnostic) => cliDiagnostic(
      new ProjectError("cli.compile.source_refused", `The project's sources did not compile: ${diagnostic.code}: ${diagnostic.message}`),
      {
        source: { file: failure.file, line: diagnostic.start.line, column: diagnostic.start.column },
        compiler: { code: diagnostic.code, start: diagnostic.start, end: diagnostic.end },
      },
    ))))
    return 1
  }
  const checker = await explainingChecker(host)
  // The admission `norn check` runs, both passes; its final input is explain's input.
  const { report: checked, input } = admit(checker, pack)
  let response: ReturnType<typeof checker.explainArtifactSet>
  try { response = checker.explainArtifactSet(input.artifacts, input.context, kind, id, options) }
  catch (error) {
    // The entry point throws on input it refuses to read: a requirement of the wrong shape, input over its size limit.
    throw new ProjectError("cli.explain.input_refused", `The checker refused the explain request: ${error instanceof Error ? error.message : String(error)}`, 1)
  }
  if (!response.admitted || !response.report) {
    refused(host, json, root, sourceMappedDiagnostics(response.diagnostics, pack.sourceMap))
    return 1
  }
  if (!checked.admitted) {
    // What `norn check` refuses, explain refuses: its findings, projected as v2 from the registry.
    refused(host, json, root, sourceMappedDiagnostics(checked.diagnostics.diagnostics.filter((finding) => finding.severity === "error"), pack.sourceMap).map(checkerDiagnostic))
    return 1
  }
  host.write(`${json ? JSON.stringify(response) : render(response.report as Report)}\n`)
  return 0
}

/**
 * `cli.explain.failed` as the registry records it, for the one case where the
 * registry itself cannot be read. A test keeps it equal to the registry.
 */
export const FAILED: Finding = {
  code: "cli.explain.failed", tier: "T0", severity: "error", message: "norn explain could not complete",
  span: null, artifactId: null, remediation: null,
  format: "norn/diagnostic@2", phase: "T0", retryable: false, fix: null,
  no_fix: "This is a fault in norn explain, not in the project. Report it with the detail; the project's sources are not what needs to change.",
}

/** Whatever was thrown, as one registered v2 finding and an exit status. Never throws. */
function refusal(error: unknown): { readonly finding: Finding; readonly status: number } {
  const detail = error instanceof Error ? error.message : String(error)
  try {
    if (error instanceof ProjectError && registeredCodes().has(error.code)) return { finding: cliDiagnostic(error), status: error.exitCode }
    return { finding: cliDiagnostic(new ProjectError("cli.explain.failed", `norn explain could not complete: ${detail}`, 3)), status: 3 }
  } catch (registryError) {
    return { finding: { ...FAILED, message: `${FAILED.message}: ${detail} (the diagnostic registry could not be read: ${String(registryError)})` }, status: 3 }
  }
}

/**
 * The command, and its one boundary: whatever is thrown below becomes one
 * refused `norn/explain@2` response with a registered `norn/diagnostic@2`.
 */
export async function explain(args: readonly string[], host: Partial<ExplainHost> = {}): Promise<number> {
  const json = args.includes("--json")
  const within: ExplainHost = { ...HOST, ...host }
  try { return await run(args, json, within) }
  catch (error) {
    const { finding, status } = refusal(error)
    refused(within, json, within.cwd, [finding])
    return status
  }
}
