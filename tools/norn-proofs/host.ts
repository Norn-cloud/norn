import { spawn } from "node:child_process"
import { createHash } from "node:crypto"
import { readFile } from "node:fs/promises"
export { readFile, writeFile, mkdir, mkdtemp, rm, readdir } from "node:fs/promises"

export const sha = (bytes: Uint8Array | string) => createHash("sha256").update(bytes).digest("hex")
export const diagnosticCommitment = (text: string) => ({ bytes: Buffer.byteLength(text, "utf8"), sha256: sha(text) })

export async function assetPins(assets: readonly (readonly [string, string])[]): Promise<Record<string, string>> {
  const pins = new Map<string, string>()
  for (const [name, path] of assets) {
    if (!name || name.startsWith("/") || name.includes("\\") || name.split("/").includes("..") || pins.has(name)) {
      throw new Error("invalid or duplicate logical tool asset")
    }
    pins.set(name, sha(await readFile(path)))
  }
  return Object.fromEntries(pins)
}
export type ProcessResult = { stdout: string; stderr: string } &
  ({ kind: "exited"; code: number } | { kind: "unknown"; reason: string; timedOut: boolean })

export function environment(toolRoot: string): NodeJS.ProcessEnv {
  const env = { ...process.env }
  for (const key of ["APALACHE_JAR", "CONFIG_FILE", "JVM_ARGS", "JVM_GC_ARGS", "JAVA_TOOL_OPTIONS",
    "JDK_JAVA_OPTIONS", "_JAVA_OPTIONS", "OUT_DIR", "RUN_DIR", "SMT_ENCODING", "WRITE_INTERMEDIATE",
    "BUN_OPTIONS", "NODE_OPTIONS", "NODE_PATH", "BASH_ENV", "ENV", "JAVA_HOME", "JRE_HOME"]) delete env[key]
  return { ...env, PATH: `${toolRoot}/bin:${env.PATH ?? ""}`, QUINT_HOME: `${toolRoot}/quint-home`,
    JVM_ARGS: "-Xms256m -Xmx4096m -XX:ActiveProcessorCount=4 -XX:+ExitOnOutOfMemoryError",
    JVM_GC_ARGS: "-XX:+UseG1GC -XX:G1PeriodicGCInterval=30000 -XX:+G1PeriodicGCInvokesConcurrent" }
}

/** PROOF-TLA-1: owned process groups, with cleanup included in the caller's deadline. */
export function runProcess(options: { command: readonly [string, ...string[]]; input: string;
  env: NodeJS.ProcessEnv; timeoutMs: number; maxBytes: number }): Promise<ProcessResult> {
  return new Promise(resolve => {
    const stdout: Buffer[] = [], stderr: Buffer[] = []
    let size = 0, finished = false, timedOut = false, failure: string | undefined
    const child = spawn(options.command[0], options.command.slice(1),
      { detached: true, env: options.env, stdio: ["pipe", "pipe", "pipe"] })
    const kill = () => {
      if (child.pid !== undefined) {
        try { process.kill(-child.pid, "SIGKILL") }
        catch (error) { if (!(error instanceof Error) || !("code" in error) || error.code !== "ESRCH") failure ??= String(error) }
      }
    }
    const finish = (code: number | null) => {
      if (finished) return
      finished = true; clearTimeout(stop); clearTimeout(deadline); kill()
      child.stdin.destroy(); child.stdout.destroy(); child.stderr.destroy(); child.unref()
      try {
        const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })
        const data = { stdout: decoder.decode(Buffer.concat(stdout)), stderr: decoder.decode(Buffer.concat(stderr)) }
        resolve(failure || code === null ? { ...data, kind: "unknown", reason: failure ?? "child terminated", timedOut }
          : { ...data, kind: "exited", code })
      } catch (error) { resolve({ kind: "unknown", reason: String(error), timedOut, stdout: "", stderr: "" }) }
    }
    const stop = setTimeout(() => { timedOut = true; failure = "process deadline exceeded"; kill() },
      Math.max(0, options.timeoutMs - Math.min(1000, options.timeoutMs / 2)))
    const deadline = setTimeout(() => { timedOut = true; failure = "process cleanup deadline exceeded"; finish(null) }, options.timeoutMs)
    const collect = (target: Buffer[]) => (chunk: Buffer) => {
      if (size > options.maxBytes - chunk.byteLength) { failure = "process output limit exceeded"; kill(); return }
      size += chunk.byteLength; target.push(chunk)
    }
    child.stdout.on("data", collect(stdout)); child.stderr.on("data", collect(stderr))
    child.once("error", error => { failure = String(error); finish(null) })
    child.once("exit", kill)
    child.once("close", (code, signal) => { if (signal) failure ??= `child signal ${signal}`; finish(code) })
    child.stdin.on("error", error => { failure ??= String(error) })
    child.stdin.end(options.input)
  })
}
