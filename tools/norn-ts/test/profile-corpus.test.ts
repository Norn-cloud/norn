/**
 * `norn-effect/2` profile pins, lockfile license audit, contract, and runtime
 * Effect export parity (T7-EFX-1).
 *
 * Full accept/reject corpus and export-path symbol resolution parity runs
 * natively in Rust (`crates/norn-authoring/tests/corpus_parity.rs`).
 */
import { describe, expect, test } from "bun:test"
import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { TOOL_ROOT, loadProfile } from "../src/profile.ts"

const LICENSES = new Set(["MIT", "Apache-2.0"])
const profile = loadProfile()

type HostPlatform = { platform: string; arch: string }

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value)

function splitLockKey(key: string): string[] {
  const raw = key.split("/")
  const pkgs: string[] = []
  for (let i = 0; i < raw.length; i++) {
    const part = raw[i]
    if (!part) throw new Error(`malformed bun.lock package key: ${key}`)
    if (part.startsWith("@")) {
      const next = raw[++i]
      if (!next) throw new Error(`malformed scoped bun.lock package key: ${key}`)
      pkgs.push(`${part}/${next}`)
    } else {
      pkgs.push(part)
    }
  }
  return pkgs
}

function matchesPlatformConstraint(value: unknown, current: string): boolean {
  if (value === undefined) return true
  const items = typeof value === "string" ? [value] : Array.isArray(value) ? value : null
  if (!items || !items.every((item): item is string => typeof item === "string")) return true
  const positive: string[] = []
  for (const item of items) {
    if (item.startsWith("!")) {
      if (item.slice(1) === current) return false
    } else {
      positive.push(item)
    }
  }
  return positive.length === 0 || positive.includes(current)
}

function classifyLockPackages(
  parsed: unknown,
  host: HostPlatform,
): {
  entries: Array<{ key: string; segments: string[] }>
  excusableIfAbsent: Set<string>
} {
  if (!isRecord(parsed) || !isRecord(parsed.workspaces) || !isRecord(parsed.packages)) {
    throw new Error("unexpected bun.lock structure: expected workspaces and packages objects")
  }
  const packages = parsed.packages
  const resolveDepKey = (parentSegments: string[], depName: string): string | null => {
    for (let depth = parentSegments.length; depth >= 0; depth--) {
      const candidate = [...parentSegments.slice(0, depth), depName].join("/")
      if (Object.hasOwn(packages, candidate)) return candidate
    }
    return null
  }
  const edgesOf = (parentSegments: string[], container: Record<string, unknown>) => {
    const optPeers = new Set<string>(
      Array.isArray(container.optionalPeers)
        ? container.optionalPeers.filter((p): p is string => typeof p === "string")
        : [],
    )
    if (isRecord(container.peerDependenciesMeta)) {
      for (const [dep, meta] of Object.entries(container.peerDependenciesMeta)) {
        if (isRecord(meta) && meta.optional === true) optPeers.add(dep)
      }
    }
    const required: string[] = []
    const optional: string[] = []
    for (const field of ["dependencies", "devDependencies"] as const) {
      if (isRecord(container[field])) {
        for (const dep of Object.keys(container[field])) {
          const target = resolveDepKey(parentSegments, dep)
          if (target !== null) required.push(target)
        }
      }
    }
    if (isRecord(container.peerDependencies)) {
      for (const dep of Object.keys(container.peerDependencies)) {
        const target = resolveDepKey(parentSegments, dep)
        if (target === null) continue
        if (optPeers.has(dep)) optional.push(target)
        else required.push(target)
      }
    }
    if (isRecord(container.optionalDependencies)) {
      for (const dep of Object.keys(container.optionalDependencies)) {
        const target = resolveDepKey(parentSegments, dep)
        if (target !== null) optional.push(target)
      }
    }
    return { required, optional }
  }

  const metaByKey = new Map<string, { segments: string[]; info: Record<string, unknown>; hostCompatible: boolean }>()
  const entries: Array<{ key: string; segments: string[] }> = []
  for (const [key, entry] of Object.entries(packages)) {
    if (!Array.isArray(entry) || entry.length < 3 || !isRecord(entry[2])) {
      throw new Error(`malformed bun.lock package tuple for ${key}`)
    }
    const segments = splitLockKey(key)
    const info = entry[2]
    const hostCompatible =
      matchesPlatformConstraint(info.os, host.platform) && matchesPlatformConstraint(info.cpu, host.arch)
    metaByKey.set(key, { segments, info, hostCompatible })
    entries.push({ key, segments })
  }

  // Active packages on this host: reachable from workspaces via required edges
  // unconditionally, and via optional edges only when the target matches the host.
  const active = new Set<string>()
  const activeQueue: string[] = []
  const excludedSeeds: string[] = []
  const enqueueActive = (key: string) => {
    if (!active.has(key)) {
      active.add(key)
      activeQueue.push(key)
    }
  }
  const visitEdges = (parentSegments: string[], container: Record<string, unknown>) => {
    const { required, optional } = edgesOf(parentSegments, container)
    for (const target of required) enqueueActive(target)
    for (const target of optional) {
      if (metaByKey.get(target)?.hostCompatible) enqueueActive(target)
      else excludedSeeds.push(target)
    }
  }
  for (const [wsPath, ws] of Object.entries(parsed.workspaces)) {
    if (!isRecord(ws)) throw new Error(`malformed bun.lock workspace entry: ${wsPath}`)
    visitEdges([], ws)
  }
  while (activeQueue.length > 0) {
    const key = activeQueue.shift()!
    const pkg = metaByKey.get(key)
    if (!pkg) continue
    visitEdges(pkg.segments, pkg.info)
  }

  // Packages reachable from an excluded optional root and not active on this host
  // may be absent from node_modules. Installed manifests are still always checked.
  const excusableIfAbsent = new Set<string>()
  const excludedQueue: string[] = []
  for (const seed of excludedSeeds) {
    if (!active.has(seed) && !excusableIfAbsent.has(seed)) {
      excusableIfAbsent.add(seed)
      excludedQueue.push(seed)
    }
  }
  while (excludedQueue.length > 0) {
    const key = excludedQueue.shift()!
    const pkg = metaByKey.get(key)
    if (!pkg) continue
    const { required, optional } = edgesOf(pkg.segments, pkg.info)
    for (const target of [...required, ...optional]) {
      if (!active.has(target) && !excusableIfAbsent.has(target)) {
        excusableIfAbsent.add(target)
        excludedQueue.push(target)
      }
    }
  }

  return { entries, excusableIfAbsent }
}

describe("pins", () => {
  test("installed effect matches the profile pin and typescript is retired", () => {
    expect(profile.pins.effect.version).toBe("4.0.0-rc.117")
    const pkg = (name: string) =>
      JSON.parse(readFileSync(join(TOOL_ROOT, "node_modules", name, "package.json"), "utf8")) as {
        version: string
        license: string
      }
    expect(pkg("effect").version).toBe(profile.pins.effect.version)
    expect(existsSync(join(TOOL_ROOT, "node_modules", "typescript"))).toBe(false)
  })

  test("package.json pins exact versions and bun.lock records the pinned integrity (no typescript)", () => {
    const manifest = JSON.parse(readFileSync(join(TOOL_ROOT, "package.json"), "utf8")) as {
      dependencies: Record<string, string>
      devDependencies: Record<string, string>
    }
    for (const range of [...Object.values(manifest.dependencies), ...Object.values(manifest.devDependencies)]) {
      expect(range).toMatch(/^\d+\.\d+\.\d+(-[\w.]+)?$/)
    }
    expect(manifest.dependencies.effect).toBe(profile.pins.effect.version)
    expect(manifest.dependencies.typescript).toBeUndefined()
    expect(manifest.devDependencies.typescript).toBeUndefined()
    const lock = readFileSync(join(TOOL_ROOT, "bun.lock"), "utf8")
    expect(lock).toContain(`"effect@${profile.pins.effect.version}", "", {}, "${profile.pins.effect.integrity}"`)
    expect(lock).not.toContain('"typescript@')
  })

  test("every locked package carries an allowed license (MIT / Apache-2.0)", () => {
    const lock = readFileSync(join(TOOL_ROOT, "bun.lock"), "utf8")
    const { entries, excusableIfAbsent } = classifyLockPackages(Bun.JSONC.parse(lock), {
      platform: process.platform,
      arch: process.arch,
    })
    expect(entries.length).toBeGreaterThan(0)
    for (const { key, segments } of entries) {
      const manifestPath = join(TOOL_ROOT, ...segments.flatMap((pkg) => ["node_modules", pkg]), "package.json")
      if (!existsSync(manifestPath) && excusableIfAbsent.has(key)) {
        continue
      }
      const meta = JSON.parse(readFileSync(manifestPath, "utf8")) as { license?: string }
      expect({
        name: key,
        license: meta.license,
        allowed: typeof meta.license === "string" && LICENSES.has(meta.license),
      }).toEqual({
        name: key,
        license: meta.license,
        allowed: true,
      })
    }
    expect(profile.pins.effect.license).toBe("MIT")
  })

  test("lockfile platform classification determines requiredness from host-active packages", () => {
    const linux = { platform: "linux", arch: "x64" }
    const { excusableIfAbsent } = classifyLockPackages(
      {
        workspaces: {
          "": {
            dependencies: { root: "1.0.0" },
            optionalDependencies: { "ws-darwin": "1.0.0", "negated-ok": "1.0.0" },
          },
        },
        packages: {
          root: [
            "root@1.0.0",
            "",
            {
              dependencies: { shared: "1.0.0" },
              optionalDependencies: { "darwin-a": "1.0.0" },
            },
            "sha512-x",
          ],
          shared: ["shared@1.0.0", "", {}, "sha512-x"],
          "darwin-a": [
            "darwin-a@1.0.0",
            "",
            { os: "darwin", dependencies: { "darwin-b": "1.0.0", shared: "1.0.0" } },
            "sha512-x",
          ],
          "darwin-b": ["darwin-b@1.0.0", "", { os: "darwin" }, "sha512-x"],
          "ws-darwin": ["ws-darwin@1.0.0", "", { os: "darwin" }, "sha512-x"],
          "negated-ok": ["negated-ok@1.0.0", "", { os: "!win32", cpu: "!arm64" }, "sha512-x"],
        },
      },
      linux,
    )
    expect([...excusableIfAbsent].sort()).toEqual(["darwin-a", "darwin-b", "ws-darwin"])
  })
})

describe("profile contract", () => {
  test("machine-readable contract lists every symbol, module default, and code in the profile JSON", () => {
    const doc = readFileSync(join(TOOL_ROOT, "profile", "contract.json"), "utf8")
    const contract: { required: string[] } = JSON.parse(doc)
    const expected = [
      ...profile.admitted.map((a) => `\`${a.symbol}\``),
      ...profile.rejected.map((r) => `\`${r.symbol}\``),
      ...profile.rejectedModules.map((m) => `\`${m.module}\``),
      ...Object.keys(profile.diagnostics).map((c) => `\`${c}\``),
      ...profile.allowedImports.packages.map((p) => `\`${p}\``),
      ...profile.rejectedConstructs.flatMap((c) => c.fixtures.map((f) => `\`${f}\``)),
      profile.pins.effect.integrity,
    ]
    expect(contract.required).toEqual(expected)
  })
})

describe("runtime exports in the pinned effect version", () => {
  const probes = [...profile.admitted, ...profile.rejected]

  test("runtime exports agree with the declarations (no type-only ghosts)", async () => {
    const runtime = { effect: await import("effect"), ai: await import("effect/unstable/ai") }
    for (const p of probes) {
      const [root, ...path] = p.probe.split(".")
      if (root === "norn") continue
      let value: unknown = runtime[root as "effect" | "ai"]
      for (const key of path) value = (value as Record<string, unknown>)[key]
      expect({ probe: p.probe, defined: value !== undefined }).toEqual({ probe: p.probe, defined: true })
    }
  })

  test("v3 Schedule.intersect is absent in v4; Schedule.max is its admitted replacement", async () => {
    const { Schedule } = await import("effect")
    expect((Schedule as Record<string, unknown>).intersect).toBeUndefined()
    expect(typeof Schedule.max).toBe("function")
  })
})
