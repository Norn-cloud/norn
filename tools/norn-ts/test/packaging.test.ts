/**
 * How a released package finds its pieces (ALPHA-ACT local, #521): the checker
 * WASM and the dev host each have three states: the environment override, the
 * packaged copy, and neither. The override always wins, even when it is wrong.
 */
import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { buildInfo, parseBuildInfo, versionLine, type BuildInfo } from "../src/build-info.ts"
import { CHECKER_PKG_ENV, checkerPackageDir } from "../src/checker-abi.ts"
import { authoringVersionWarning } from "../src/authoring-version.ts"
import { registeredCodes } from "../src/cli-diagnostic.ts"
import { download, hostBinary, type HostLookup } from "../src/dev.ts"
import { distribution, projectDependencies } from "../src/init.ts"
import { TOOL_ROOT, dependencyDir } from "../src/profile.ts"
import { ProjectError } from "../src/project.ts"

const scratch = () => mkdtempSync(join(tmpdir(), "norn-packaging-"))
const sha256 = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex")
const HOST = new TextEncoder().encode("#!/bin/sh\necho norn-dev-host\n")
const CLI_VERSION = (JSON.parse(readFileSync(join(TOOL_ROOT, "package.json"), "utf8")) as { version: string }).version

function build(hosts: BuildInfo["hosts"]): BuildInfo {
  return { version: "0.1.0-alpha.7", commit: "0123456789abcdef0123456789abcdef01234567", toolchain: "1.98.0",
    packages: { "@norn/cli": "0.1.0-alpha.7", "@norn/authoring": "0.1.0-alpha.7" }, hosts }
}

function lookup(root: string, parts: Partial<HostLookup> = {}): HostLookup & { fetched: string[] } {
  const fetched: string[] = []
  return {
    env: {}, platform: "linux-x64", cacheDir: join(root, "cache"), packageDir: () => undefined,
    build: build({ "linux-x64": { url: "https://example.invalid/norn-dev-host-linux-x64", sha256: sha256(HOST), size: HOST.length } }),
    download: async (url) => { fetched.push(url); return HOST },
    fetched, ...parts,
  }
}

async function refusal(run: Promise<unknown>): Promise<ProjectError> {
  try { await run } catch (error) { if (error instanceof ProjectError) return error; throw error }
  throw new Error("expected a refusal")
}

test("checker package: the override wins, then the bundled copy, then none", () => {
  const root = scratch()
  try {
    const bundled = join(root, "checker")
    const override = join(root, "override")
    // Neither.
    expect(checkerPackageDir({}, bundled)).toBeUndefined()
    expect(checkerPackageDir({ [CHECKER_PKG_ENV]: "" }, bundled)).toBeUndefined()
    // Bundled.
    mkdirSync(bundled)
    writeFileSync(join(bundled, "norn_checker_wasm.js"), "")
    expect(checkerPackageDir({}, bundled)).toBeUndefined() // half a package is not one
    writeFileSync(join(bundled, "norn_checker_wasm_bg.wasm"), "")
    expect(checkerPackageDir({}, bundled)).toBe(bundled)
    // Override, with a bundled copy present: the override, even one that holds no package.
    expect(checkerPackageDir({ [CHECKER_PKG_ENV]: override }, bundled)).toBe(override)
  } finally { rmSync(root, { recursive: true }) }
})

test("check without a usable checker still says NORN-CHECKER-WASM-SETUP", () => {
  // The repository checkout carries no bundled checker, so with the variable unset this is the "neither" state.
  expect(existsSync(join(TOOL_ROOT, "checker"))).toBe(false)
  const run = spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "check", join(TOOL_ROOT, "fixtures/quickstart"), "--org", "example", "--env", "local"],
    { env: { ...process.env, [CHECKER_PKG_ENV]: "" }, encoding: "utf8" })
  expect(run.status).toBe(2)
  expect(run.stderr).toContain("NORN-CHECKER-WASM-SETUP:")
})

test("dev host: the override wins, and a wrong override refuses instead of falling back", async () => {
  const root = scratch()
  try {
    const mine = join(root, "my-host")
    writeFileSync(mine, HOST)
    const packaged = join(root, "pkg")
    mkdirSync(join(packaged, "bin"), { recursive: true })
    writeFileSync(join(packaged, "bin/norn-dev-host"), HOST)
    const both = lookup(root, { env: { NORN_DEV_HOST_BIN: mine }, packageDir: () => packaged })
    expect(await hostBinary(both)).toBe(mine)
    const wrong = await refusal(hostBinary(lookup(root, { env: { NORN_DEV_HOST_BIN: join(root, "absent") }, packageDir: () => packaged })))
    expect(wrong).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(wrong.message).toContain("does not exist")
    expect(both.fetched).toEqual([])
  } finally { rmSync(root, { recursive: true }) }
})

test("dev host: the installed platform package, then the release's prebuilt host, fetched once and verified", async () => {
  const root = scratch()
  try {
    const packaged = join(root, "pkg")
    mkdirSync(join(packaged, "bin"), { recursive: true })
    writeFileSync(join(packaged, "bin/norn-dev-host"), HOST)
    const asked: string[] = []
    const installed = lookup(root, { packageDir: (name) => { asked.push(name); return packaged } })
    expect(await hostBinary(installed)).toBe(join(packaged, "bin/norn-dev-host"))
    expect(asked).toEqual(["@norn/dev-host-linux-x64"])
    expect(installed.fetched).toEqual([])

    // No platform package: fetched from the address the release records, kept only if the sha256 matches.
    const fresh = lookup(root)
    const cached = await hostBinary(fresh)
    expect(cached).toBe(join(root, "cache/dev-host/0.1.0-alpha.7/linux-x64/norn-dev-host"))
    expect(sha256(readFileSync(cached))).toBe(sha256(HOST))
    expect(statSync(cached).mode & 0o777).toBe(0o755)
    expect(fresh.fetched).toEqual(["https://example.invalid/norn-dev-host-linux-x64"])
    // The next run uses the cached file and fetches nothing.
    const again = lookup(root)
    expect(await hostBinary(again)).toBe(cached)
    expect(again.fetched).toEqual([])
    // A cached file that no longer hashes to the recorded value is fetched again, not run.
    writeFileSync(cached, "tampered")
    chmodSync(cached, 0o755)
    const repaired = lookup(root)
    expect(await hostBinary(repaired)).toBe(cached)
    expect(repaired.fetched).toHaveLength(1)
    expect(sha256(readFileSync(cached))).toBe(sha256(HOST))
  } finally { rmSync(root, { recursive: true }) }
})

test("dev host: a fetched file that is not the recorded one is never kept", async () => {
  const root = scratch()
  try {
    const altered = await refusal(hostBinary(lookup(root, { download: async () => new TextEncoder().encode("something else") })))
    expect(altered).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(altered.message).toContain("it was not kept")
    expect(existsSync(join(root, "cache"))).toBe(false)
    const offline = await refusal(hostBinary(lookup(root, { download: async () => { throw new Error("no network") } })))
    expect(offline.message).toContain("no network")
    // An address that is not https is not fetched at all, except from this machine.
    const plain = lookup(root, { build: build({ "linux-x64": { url: "http://example.invalid/host", sha256: sha256(HOST), size: 1 } }) })
    expect((await refusal(hostBinary(plain))).message).toContain("not an https address")
    expect(plain.fetched).toEqual([])
    const local = lookup(root, { build: build({ "linux-x64": { url: "http://127.0.0.1:8123/host", sha256: sha256(HOST), size: 1 } }) })
    expect(await hostBinary(local)).toContain("norn-dev-host")
  } finally { rmSync(root, { recursive: true }) }
})

test("dev host: neither an override nor a prebuilt host says how to build it from source", async () => {
  const root = scratch()
  try {
    // A checkout has no build manifest.
    const checkout = await refusal(hostBinary(lookup(root, { build: undefined })))
    expect(checkout).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(checkout.message).toContain("cargo build --release -p norn-cell-do --bin norn-dev-host")
    expect(checkout.message).toContain("NORN_DEV_HOST_BIN")
    // A release without this platform (macOS in the alpha) names what it has and the toolchain to build with.
    const mac = await refusal(hostBinary(lookup(root, { platform: "darwin-arm64" })))
    expect(mac.message).toContain("no prebuilt host for darwin-arm64 (it has linux-x64)")
    expect(mac.message).toContain("(Rust 1.98.0)")
  } finally { rmSync(root, { recursive: true }) }
})

test("a released package knows its version, and a new project depends on that release", () => {
  const root = scratch()
  try {
    // A checkout: no build manifest, the version from package.json.
    expect(buildInfo()).toBeUndefined()
    expect(distribution()).toBe("repository")
    const version = (JSON.parse(readFileSync(join(TOOL_ROOT, "package.json"), "utf8")) as { version: string }).version
    expect(version).toMatch(/^0\.1\.0-alpha\.\d+$/)
    expect(versionLine()).toBe(`norn ${version} (checkout)`)
    const run = spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "--version"], { encoding: "utf8" })
    expect({ status: run.status, out: run.stdout }).toEqual({ status: 0, out: `norn ${version} (checkout)\n` })

    // A released package: the manifest the assembly writes.
    const manifest = build({})
    mkdirSync(join(root, "generated"))
    writeFileSync(join(root, "generated/build.json"), JSON.stringify(manifest))
    writeFileSync(join(root, "package.json"), JSON.stringify({ version: "0.1.0-alpha.7" }))
    expect(buildInfo(root)).toEqual(manifest)
    expect(distribution(root)).toBe("published")
    expect(versionLine(root)).toBe("norn 0.1.0-alpha.7 (0123456789ab)")
    expect(projectDependencies("published", manifest)).toEqual({ "@norn/cli": "0.1.0-alpha.7", "@norn/authoring": "0.1.0-alpha.7", effect: "4.0.0-rc.117", typescript: "6.0.3" })
    // Without a manifest a published tool refuses; a malformed manifest is an error, not a checkout.
    expect(() => projectDependencies("published", undefined)).toThrow("Published distribution is unavailable")
    for (const bad of [{}, { ...manifest, hosts: { "linux-x64": { url: "https://x/y", sha256: "abc", size: 1 } } }, { ...manifest, packages: { "@norn/cli": "" } }]) {
      expect(() => parseBuildInfo(bad)).toThrow("not a build manifest")
    }
  } finally { rmSync(root, { recursive: true }) }
})

test("an installed @norn/authoring is never what the checker reads: the intrinsics stay pinned", () => {
  // A project with its own installed `@norn/authoring` (what a package manager
  // puts there for the editor), whose declaration declares nothing: if the
  // checker read it, the starter could not compile.
  const root = scratch()
  try {
    const dir = join(root, "shop")
    const init = spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "init", dir, "--no-install"], { encoding: "utf8" })
    expect(init.status).toBe(0)
    const installed = join(dir, "node_modules/@norn/authoring")
    mkdirSync(join(installed, "profile"), { recursive: true })
    writeFileSync(join(installed, "package.json"), JSON.stringify({ name: "@norn/authoring", version: "0.0.0", types: "./profile/norn-authoring.d.ts" }))
    writeFileSync(join(installed, "profile/norn-authoring.d.ts"), 'declare module "@norn/authoring" {}\n')
    const compiled = spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "compile", join(dir, "src"), "--org", "local", "--env", "dev"], { cwd: dir, encoding: "utf8" })
    expect({ status: compiled.status, err: compiled.stderr }).toEqual({ status: 0, err: "" })
    expect(compiled.stdout).not.toContain("NORN-FLOW-MODULE")
    expect(JSON.parse(compiled.stdout)).toHaveLength(2)
    // The version differs from the CLI's: the editor's types are not the checker's, and that is said, as a warning.
    const warning = authoringVersionWarning(dir)
    expect(warning).toMatchObject({ code: "cli.setup.authoring_version_mismatch" })
    expect(warning!.message).toContain("installed @norn/authoring is 0.0.0")
    writeFileSync(join(installed, "package.json"), JSON.stringify({ name: "@norn/authoring", version: CLI_VERSION }))
    expect(authoringVersionWarning(dir)).toBeUndefined()
  } finally { rmSync(root, { recursive: true }) }
}, 60_000)

test.skipIf(!process.env[CHECKER_PKG_ENV])("check prints the authoring version warning on stderr and still admits", () => {
  const root = scratch()
  try {
    const dir = join(root, "shop")
    expect(spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "init", dir, "--no-install"], { encoding: "utf8" }).status).toBe(0)
    const installed = join(dir, "node_modules/@norn/authoring")
    mkdirSync(installed, { recursive: true })
    writeFileSync(join(installed, "package.json"), JSON.stringify({ name: "@norn/authoring", version: "0.0.0" }))
    const checked = spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "check", join(dir, "src"), "--org", "local", "--env", "dev"], { cwd: dir, encoding: "utf8" })
    expect(checked.status).toBe(0)
    expect(JSON.parse(checked.stdout).admitted).toBe(true)
    expect(checked.stderr).toStartWith("warning[cli.setup.authoring_version_mismatch] ")
  } finally { rmSync(root, { recursive: true }) }
}, 60_000)

test("dev host: an installed platform package runs only if it is the host the release records", async () => {
  const root = scratch()
  try {
    const packaged = join(root, "pkg")
    mkdirSync(join(packaged, "bin"), { recursive: true })
    writeFileSync(join(packaged, "bin/norn-dev-host"), "a stale host")
    const stale = await refusal(hostBinary(lookup(root, { packageDir: () => packaged })))
    expect(stale).toMatchObject({ code: "cli.setup.host_unavailable", exitCode: 3 })
    expect(stale.message).toContain("is not the host this release records")
    // On the registry channel a host has a digest and no address: the installed package is the only source.
    const registry = build({ "linux-x64": { sha256: sha256(HOST), size: HOST.length } })
    writeFileSync(join(packaged, "bin/norn-dev-host"), HOST)
    expect(await hostBinary(lookup(root, { packageDir: () => packaged, build: registry }))).toBe(join(packaged, "bin/norn-dev-host"))
    const missing = await refusal(hostBinary(lookup(root, { build: registry })))
    expect(missing.message).toContain("@norn/dev-host-linux-x64 is not installed")
  } finally { rmSync(root, { recursive: true }) }
})

test("dev host: the download is bounded by the recorded size and by time", async () => {
  const body = new Uint8Array(1024).fill(7)
  const server = Bun.serve({
    port: 0, hostname: "127.0.0.1",
    fetch(request) {
      const path = new URL(request.url).pathname
      if (path === "/exact") return new Response(body)
      if (path === "/declared-too-big") return new Response(body, { headers: { "content-length": String(body.length) } })
      if (path === "/streamed-too-big") {
        // No content-length: the reader has to stop at the limit on its own.
        return new Response(new ReadableStream({ start(controller) { for (let i = 0; i < 64; i++) controller.enqueue(body); controller.close() } }))
      }
      if (path === "/stalled") return new Response(new ReadableStream({ start(controller) { controller.enqueue(body.subarray(0, 10)) } }))
      return new Response("missing", { status: 404 })
    },
  })
  try {
    const base = `http://127.0.0.1:${server.port}`
    expect(await download(`${base}/exact`, body.length)).toEqual(body)
    await expect(download(`${base}/declared-too-big`, body.length - 1)).rejects.toThrow("the server offers 1024 bytes")
    await expect(download(`${base}/streamed-too-big`, 4096)).rejects.toThrow("more than the 4096 bytes")
    await expect(download(`${base}/stalled`, body.length, 300)).rejects.toThrow()
    await expect(download(`${base}/absent`, 10)).rejects.toThrow("HTTP 404")
  } finally { server.stop(true) }
})

test("dev host: the build-from-source instructions name this CLI's commit and are the registered ones", async () => {
  const root = scratch()
  try {
    const mac = await refusal(hostBinary(lookup(root, { platform: "darwin-arm64" })))
    const commit = build({}).commit
    expect(mac.message).toContain(`git checkout ${commit}`)
    expect(mac.message).toContain("cargo build --release -p norn-cell-do --bin norn-dev-host")
    expect(mac.message).toContain("NORN_DEV_HOST_BIN to target/release/norn-dev-host")
    // The registered fix gives the same command and path, not another one.
    const registered = registeredCodes().get("cli.setup.host_unavailable")!
    expect(registered.fix).toContain("cargo build --release -p norn-cell-do --bin norn-dev-host")
    expect(registered.fix).toContain("target/release/norn-dev-host")
    expect(registered.fix).toContain("`norn --version`")
  } finally { rmSync(root, { recursive: true }) }
})

test("pinned dependencies are real paths, as TypeScript resolves them", () => {
  for (const name of ["effect", "typescript"]) {
    const dir = dependencyDir(name)
    expect(realpathSync(dir)).toBe(dir)
  }
  expect(realpathSync(TOOL_ROOT)).toBe(TOOL_ROOT)
})
