#!/usr/bin/env bun
/** Compile typed definitions and run the authoritative Rust checker ABI. */
import { existsSync, statSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { ProfileChecker, compileDashboards, compileQuestions } from "./native-compiler.ts"
import { loadChecker, sourceMappedDiagnostics, staleCheckerWarning } from "./checker-abi.ts"
import { admit } from "./pack.ts"

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
if (process.argv[2] === "codegen") {
  const { codegenCli } = await import("./codegen.ts")
  process.exit(await codegenCli(process.argv.slice(3)))
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
if (process.argv[2] === "seed") {
  const { seedCli } = await import("./seed.ts")
  const { ProjectError } = await import("./project.ts")
  try { process.exit(await seedCli(process.argv.slice(3))) }
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
  const value = index < 0 ? undefined : flags[index + 1]
  return value === undefined || value.startsWith("--") ? undefined : value
}
const hasFlag = (name: string): boolean => flags.includes(name)
if (!input || (command !== "check" && command !== "compile" && command !== "profile" && command !== "question" && command !== "dashboard")) {
  process.stderr.write("usage: norn <check|compile|profile> <pack-dir|file.ts> --org ID --env ID [--forbid-unsafe-zen] [--out FILE]\n       norn question <file.ts> [--out FILE]\n       norn dashboard <file.ts> [--out FILE]\n       norn codegen --ts <artifacts.json|dir|pack-dir> [--base SET]... [--out FILE.d.ts] [--check]\n       norn setup [--version TAG] [--base-url URL]\n       norn init <dir> | norn dev [--port N] [--json] | norn seed [--json] | norn fmt\n       norn plan|apply <pack-dir|file.ts> --url URL --org ID --env ID (--email EMAIL --password-env VAR | --token-env VAR)\n")
  process.exit(2)
}
if (!existsSync(input)) {
  process.stderr.write(`${command}: ${input} does not exist\n`)
  process.exit(2)
}
if (command === "profile") {
  if (!statSync(input).isFile()) process.exit(1)
  const reports = new ProfileChecker().check([resolve(input)])
  process.stdout.write(`${JSON.stringify(reports)}\n`)
  process.exit(reports.every((report) => report.diagnostics.length === 0) ? 0 : 1)
}
if (command === "question" || command === "dashboard") {
  if (!statSync(input).isFile()) { process.stderr.write(`${command} expects one TypeScript module\n`); process.exit(2) }
  const result = command === "question" ? compileQuestions(resolve(input)) : compileDashboards(resolve(input))
  if (!result.ok) { process.stdout.write(`${JSON.stringify(result)}\n`); process.exit(1) }
  const document = `${JSON.stringify("questions" in result ? result.questions : result.dashboards)}\n`
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
// A project's source questions and dashboards are `norn dev`'s to install; a
// check names each one it leaves out (DEV-READ-DEFINITIONS-1).
const { loadInputPack } = await import("./worker.ts")
const { pack, omitted } = loadInputPack(input, {
  ...(orgId && environmentId ? { scope: { orgId, environmentId } } : {}),
  ...(forbidUnsafeZen !== undefined ? { forbidUnsafeZen } : {}),
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
    const stale = staleCheckerWarning()
    if (stale) process.stderr.write(`${stale}\n`)
    checker = await loadChecker()
    if (!checker) throw new Error("NORN_CHECKER_WASM_PKG is not set")
  } catch (error) {
    const { cliDiagnostic, formatCliDiagnostic, registeredCodes } = await import("./cli-diagnostic.ts")
    const { ProjectError } = await import("./project.ts")
    const detail = (error instanceof Error ? error.message : String(error))
      .split(/\r\n|\r|\n|\u0085|\u2028|\u2029/)
      .map((line) => line.replace(/[\u0000-\u001f\u007f-\u009f]+/g, " ").trim())
      .filter((line) => line.length > 0 && !/^at\s/.test(line))
      .join(" ")
      .replace(/\s+/g, " ")
    const base = registeredCodes().get("cli.setup.checker_unavailable")?.message ?? ""
    const message = base ? (detail ? `${base} (${detail})` : base) : detail
    const refusal = new ProjectError("cli.setup.checker_unavailable", message, 2)
    process.stderr.write(`${formatCliDiagnostic(cliDiagnostic(refusal))}\n`)
    process.exit(refusal.exitCode)
  }
  const { report } = admit(checker, pack)
  // LAW-EXAMPLE-1: every authored example runs with the checker's law semantics.
  const { runExamples } = await import("./examples.ts")
  const failedExamples = report.admitted ? runExamples(checker, pack.artifacts, pack.examples) : []
  const diagnostics = [...pack.warnings, ...sourceMappedDiagnostics(report.diagnostics.diagnostics, pack.sourceMap), ...failedExamples]
  const admitted = report.admitted && failedExamples.length === 0
  const { omittedLines } = await import("./worker.ts")
  for (const line of omittedLines(omitted)) process.stderr.write(`${line}\n`)
  process.stdout.write(`${JSON.stringify({ ...report, admitted, diagnostics, ...(omitted.length > 0 ? { omittedDefinitions: omitted } : {}) })}\n`)
  process.exit(admitted ? 0 : 1)
}
