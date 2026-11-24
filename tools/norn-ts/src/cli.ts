#!/usr/bin/env bun
/** Compile typed definitions and run the authoritative Rust checker ABI. */
import { existsSync, statSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { ProfileChecker } from "./check.ts"
import { compileDashboards, compileQuestions } from "./compiler/definitions.ts"
import { loadChecker, sourceMappedDiagnostics } from "./checker-abi.ts"
import { admit, compilePack } from "./pack.ts"

if (process.argv[2] === "--version" || process.argv[2] === "-V") {
  const { versionLine } = await import("./build-info.ts")
  process.stdout.write(`${versionLine()}\n`)
  process.exit(0)
}
if (process.argv[2] === "fmt") {
  const { fmt } = await import("./fmt.ts")
  const { ProjectError } = await import("./project.ts")
  try { process.exit(fmt(process.argv.slice(3))) }
  catch (error) {
    if (!(error instanceof ProjectError)) throw error
    process.stdout.write(`${JSON.stringify(error.diagnostic)}\n`)
    process.exit(error.exitCode)
  }
}
if (process.argv[2] === "explain") {
  const { explain } = await import("./explain.ts")
  process.exit(await explain(process.argv.slice(3)))
}
if (process.argv[2] === "dev") {
  const { dev } = await import("./dev.ts")
  const { ProjectError } = await import("./project.ts")
  try { process.exit(await dev(process.argv.slice(3))) }
  catch (error) {
    if (!(error instanceof ProjectError)) throw error
    process.stdout.write(`${JSON.stringify(error.diagnostic)}\n`)
    process.exit(error.exitCode)
  }
}
if (process.argv[2] === "plan" || process.argv[2] === "apply") {
  const { main } = await import("./apply.ts")
  process.exit(await main(process.argv[2], process.argv.slice(3)))
}
if (process.argv[2] === "setup") {
  const { setupCli } = await import("./setup.ts")
  process.exit(await setupCli(process.argv.slice(3)))
}
if (process.argv[2] === "init") {
  const { init } = await import("./init.ts")
  const { ProjectError } = await import("./project.ts")
  try { await init(process.argv.slice(3)) }
  catch (error) {
    if (!(error instanceof ProjectError)) throw error
    process.stdout.write(`${JSON.stringify(error.diagnostic)}\n`)
    process.exit(error.exitCode)
  }
  process.exit(0)
}

const [command, input, ...flags] = process.argv.slice(2)
function option(name: string): string | undefined {
  const index = flags.indexOf(name)
  return index < 0 ? undefined : flags[index + 1]
}
const hasFlag = (name: string): boolean => flags.includes(name)
if (!input || (command !== "check" && command !== "compile" && command !== "profile" && command !== "question" && command !== "dashboard")) {
  process.stderr.write("usage: norn <check|compile|profile> <pack-dir|file.ts> --org ID --env ID [--forbid-unsafe-zen] [--out FILE]\n       norn question <file.ts> [--out FILE]\n       norn dashboard <file.ts> [--out FILE]\n       norn setup [--version TAG] [--base-url URL]\n       norn init <dir> | norn dev [--port N] [--json] | norn fmt\n       norn plan|apply <pack-dir|file.ts> --url URL --org ID --env ID (--email EMAIL --password-env VAR | --token-env VAR)\n")
  process.exit(2)
}
if (command === "profile") {
  const reports = new ProfileChecker().check([resolve(input)])
  process.stdout.write(`${JSON.stringify(reports)}\n`)
  process.exit(reports.every((report) => report.diagnostics.length === 0) ? 0 : 1)
}
if (command === "question") {
  if (!statSync(input).isFile()) { process.stderr.write("question expects one TypeScript module\n"); process.exit(2) }
  const result = compileQuestions(resolve(input))
  if (!result.ok) { process.stdout.write(`${JSON.stringify(result)}\n`); process.exit(1) }
  const document = `${JSON.stringify(result.questions)}\n`
  const output = option("--out")
  if (output) writeFileSync(output, document)
  else process.stdout.write(document)
  process.exit(0)
}
if (command === "dashboard") {
  if (!statSync(input).isFile()) { process.stderr.write("dashboard expects one TypeScript module\n"); process.exit(2) }
  const result = compileDashboards(resolve(input))
  if (!result.ok) { process.stdout.write(`${JSON.stringify(result)}\n`); process.exit(1) }
  const document = `${JSON.stringify(result.dashboards)}\n`
  const output = option("--out")
  if (output) writeFileSync(output, document)
  else process.stdout.write(document)
  process.exit(0)
}
// A project directory (one holding norn.json) is read exactly as `norn dev`
// and `norn explain` read it: its manifest's sources and scope, its JSON
// artifacts and definitions (one loader, `compileProjectPack`).
const project = statSync(input).isDirectory() && existsSync(join(input, "norn.json"))
const orgId = option("--org")
const environmentId = option("--env")
if (!project && (!orgId || !environmentId)) {
  process.stderr.write("--org and --env are required\n")
  process.exit(2)
}
if (command === "check") {
  // Before compiling: a project whose sources do not compile is told too.
  const { warnAuthoringVersion } = await import("./authoring-version.ts")
  warnAuthoringVersion(resolve(input))
}
const forbidUnsafeZen = hasFlag("--forbid-unsafe-zen") ? true : undefined
const pack = project
  ? await (async () => {
      const { readProject } = await import("./project.ts")
      const { compileProjectPack } = await import("./worker.ts")
      const { root, manifest } = readProject(resolve(input))
      return compileProjectPack(root, manifest, { ...(forbidUnsafeZen !== undefined ? { forbidUnsafeZen } : {}) })
    })()
  : compilePack(resolve(input), {
      scope: { orgId: orgId!, environmentId: environmentId! },
      root: process.cwd(),
      ...(forbidUnsafeZen !== undefined ? { forbidUnsafeZen } : {}),
      ...(statSync(input).isFile() ? { modules: [resolve(input)] } : {}),
    })
if (!pack.ok) {
  process.stdout.write(`${JSON.stringify(pack)}\n`)
  process.exit(1)
}
if (command === "compile") {
  const output = option("--out")
  const document = `${JSON.stringify(pack.artifacts)}\n`
  if (output) writeFileSync(output, document)
  else process.stdout.write(document)
} else {
  let checker: Awaited<ReturnType<typeof loadChecker>>
  try {
    checker = await loadChecker()
    if (!checker) throw new Error("NORN_CHECKER_WASM_PKG is not set")
  } catch (error) {
    const { cliDiagnostic, formatCliDiagnostic, registeredCodes } = await import("./cli-diagnostic.ts")
    const { ProjectError } = await import("./project.ts")
    const detail = error instanceof Error ? error.message : String(error)
    const base = registeredCodes().get("cli.setup.checker_unavailable")?.message ?? ""
    const refusal = new ProjectError("cli.setup.checker_unavailable", base ? `${base} (${detail})` : detail, 2)
    process.stderr.write(`${formatCliDiagnostic(cliDiagnostic(refusal))}\n`)
    process.exit(refusal.exitCode)
  }
  const { report } = admit(checker, pack)
  // LAW-EXAMPLE-1: every authored example runs with the checker's law semantics.
  const { runExamples } = await import("./examples.ts")
  const failedExamples = report.admitted ? runExamples(checker, pack.artifacts, pack.examples) : []
  const diagnostics = [...pack.warnings, ...sourceMappedDiagnostics(report.diagnostics.diagnostics, pack.sourceMap), ...failedExamples]
  const admitted = report.admitted && failedExamples.length === 0
  process.stdout.write(`${JSON.stringify({ ...report, admitted, diagnostics })}\n`)
  process.exit(admitted ? 0 : 1)
}
