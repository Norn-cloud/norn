/**
 * `norn-effect/1` conformance and rejection corpus (T7-EFX-1).
 *
 * Run: `bun --cwd tools/norn-ts test profile-corpus`.
 * Refresh expected files after an intentional change: `UPDATE_CORPUS=1 bun test profile-corpus`,
 * then review the diff. Every Effect upgrade reruns this suite and bumps the profile identity.
 */
import { describe, expect, test } from "bun:test"
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { type FileReport, ProfileChecker } from "../src/check.ts"
import { TOOL_ROOT } from "../src/profile.ts"

const CORPUS = join(TOOL_ROOT, "corpus")
const UPDATE = process.env.UPDATE_CORPUS === "1"
const LICENSES = new Set(["MIT", "Apache-2.0"])

const checker = new ProfileChecker()
const profile = checker.profile

const fixtures = (dir: "accept" | "reject") =>
  readdirSync(join(CORPUS, dir))
    .filter((f) => f.endsWith(".ts"))
    .sort()
    .map((f) => `${dir}/${f}`)

const acceptFixtures = fixtures("accept")
const rejectFixtures = fixtures("reject")
// One Program for the whole corpus: Effect's declarations load once.
const reports = new Map<string, FileReport>(
  checker
    .check([...acceptFixtures, ...rejectFixtures].map((f) => join(CORPUS, f)))
    .map((r) => [r.file.replace(/^corpus\//, ""), r]),
)

function report(fixture: string): FileReport {
  const r = reports.get(fixture)
  if (!r) throw new Error(`no report for ${fixture}`)
  return r
}

/** The stable, compared part of a report; messages may be reworded freely. */
function observed(fixture: string, withUses: boolean) {
  const r = report(fixture)
  return {
    profile: r.profile,
    diagnostics: r.diagnostics.map((d) => ({
      code: d.code,
      start: `${d.start.line}:${d.start.column}`,
      end: `${d.end.line}:${d.end.column}`,
      construct: d.construct,
    })),
    ...(withUses ? { uses: r.uses.map((u) => `${u.at} ${u.symbol}`) } : {}),
  }
}

function expectMatchesGolden(fixture: string, withUses: boolean): void {
  const golden = join(CORPUS, fixture.replace(/\.ts$/, ".expected.json"))
  const actual = observed(fixture, withUses)
  if (UPDATE) writeFileSync(golden, `${JSON.stringify(actual, null, 2)}\n`)
  expect(existsSync(golden)).toBe(true)
  expect(actual).toEqual(JSON.parse(readFileSync(golden, "utf8")))
}

describe("pins", () => {
  test("installed effect and Compiler API match the profile pins (checker refuses otherwise)", () => {
    expect(profile.pins.effect.version).toBe("4.0.0-rc.117")
    const pkg = (name: string) =>
      JSON.parse(readFileSync(join(TOOL_ROOT, "node_modules", name, "package.json"), "utf8")) as {
        version: string
        license: string
      }
    expect(pkg("effect").version).toBe(profile.pins.effect.version)
    expect(pkg("typescript").version).toBe(profile.pins.typescript.version)
  })

  test("package.json pins exact versions and bun.lock records the pinned integrity", () => {
    const manifest = JSON.parse(readFileSync(join(TOOL_ROOT, "package.json"), "utf8")) as {
      dependencies: Record<string, string>
      devDependencies: Record<string, string>
    }
    for (const range of [...Object.values(manifest.dependencies), ...Object.values(manifest.devDependencies)]) {
      expect(range).toMatch(/^\d+\.\d+\.\d+(-[\w.]+)?$/)
    }
    expect(manifest.dependencies.effect).toBe(profile.pins.effect.version)
    expect(manifest.dependencies.typescript).toBe(profile.pins.typescript.version)
    const lock = readFileSync(join(TOOL_ROOT, "bun.lock"), "utf8")
    expect(lock).toContain(`"effect@${profile.pins.effect.version}", "", {}, "${profile.pins.effect.integrity}"`)
    expect(lock).toContain(`"typescript@${profile.pins.typescript.version}"`)
    expect(lock).toContain(profile.pins.typescript.integrity)
  })

  test("every locked package carries an allowed license (MIT / Apache-2.0)", () => {
    const isRecord = (value: unknown): value is Record<string, unknown> =>
      typeof value === "object" && value !== null && !Array.isArray(value)
    const lock = readFileSync(join(TOOL_ROOT, "bun.lock"), "utf8")
    const parsed = Bun.JSONC.parse(lock)
    if (!isRecord(parsed) || !isRecord(parsed.workspaces) || !isRecord(parsed.packages)) {
      throw new Error("unexpected bun.lock structure: expected workspaces and packages objects")
    }
    const packages = parsed.packages
    const splitLockKey = (key: string): string[] => {
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
    const resolveDepKey = (parentSegments: string[], depName: string): string => {
      for (let depth = parentSegments.length; depth >= 0; depth--) {
        const candidate = [...parentSegments.slice(0, depth), depName].join("/")
        if (Object.hasOwn(packages, candidate)) return candidate
      }
      return depName
    }
    const matchesPlatformConstraint = (value: unknown, current: string): boolean => {
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

    const requiredKeys = new Set<string>()
    const optionalKeys = new Set<string>()
    const recordEdges = (parentSegments: string[], container: Record<string, unknown>) => {
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
      for (const field of ["dependencies", "devDependencies"] as const) {
        if (isRecord(container[field])) {
          for (const dep of Object.keys(container[field])) {
            requiredKeys.add(resolveDepKey(parentSegments, dep))
          }
        }
      }
      if (isRecord(container.peerDependencies)) {
        for (const dep of Object.keys(container.peerDependencies)) {
          const target = resolveDepKey(parentSegments, dep)
          if (optPeers.has(dep)) optionalKeys.add(target)
          else requiredKeys.add(target)
        }
      }
      if (isRecord(container.optionalDependencies)) {
        for (const dep of Object.keys(container.optionalDependencies)) {
          optionalKeys.add(resolveDepKey(parentSegments, dep))
        }
      }
    }

    for (const [wsPath, ws] of Object.entries(parsed.workspaces)) {
      if (!isRecord(ws)) throw new Error(`malformed bun.lock workspace entry: ${wsPath}`)
      recordEdges([], ws)
    }
    const entries = Object.entries(packages)
    expect(entries.length).toBeGreaterThan(0)
    const parsedEntries: Array<[string, string[], Record<string, unknown>]> = []
    for (const [key, entry] of entries) {
      if (!Array.isArray(entry) || entry.length < 3 || !isRecord(entry[2])) {
        throw new Error(`malformed bun.lock package tuple for ${key}`)
      }
      const segments = splitLockKey(key)
      recordEdges(segments, entry[2])
      parsedEntries.push([key, segments, entry[2]])
    }

    for (const [key, segments, info] of parsedEntries) {
      const manifestPath = join(TOOL_ROOT, ...segments.flatMap((pkg) => ["node_modules", pkg]), "package.json")
      if (!existsSync(manifestPath)) {
        const hostExcluded =
          !matchesPlatformConstraint(info.os, process.platform) ||
          !matchesPlatformConstraint(info.cpu, process.arch)
        if (optionalKeys.has(key) && !requiredKeys.has(key) && hostExcluded) {
          continue
        }
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
    expect(profile.pins.typescript.license).toBe("Apache-2.0")
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
      profile.pins.typescript.integrity,
    ]
    expect(contract.required).toEqual(expected)
  })
})

describe("export paths in the pinned version", () => {
  const probes = [...profile.admitted, ...profile.rejected]
  const dir = mkdtempSync(join(tmpdir(), "norn-effect-probe-"))
  const file = join(dir, "probe.ts")
  writeFileSync(
    file,
    [
      `import * as effect from "effect"`,
      `import * as ai from "effect/unstable/ai"`,
      `import * as norn from "@norn/authoring"`,
      ...probes.map((p, i) => `export const p${i} = ${p.probe}`),
      "",
    ].join("\n"),
  )
  const resolved = checker.resolveInitializers(file)
  rmSync(dir, { recursive: true, force: true })

  test("every admitted and rejected probe resolves to its recorded symbol identity", () => {
    expect(resolved.map((r) => r.id)).toEqual(probes.map((p) => p.symbol))
  })

  test("Tool/Toolkit come from effect/unstable/ai; Schema/Data/Effect/Schedule from effect", async () => {
    const ids = new Map(probes.map((p, i) => [p.probe, resolved[i]?.id]))
    expect(ids.get("ai.Tool.make")).toBe("effect/unstable/ai/Tool#make")
    expect(ids.get("ai.Toolkit.make")).toBe("effect/unstable/ai/Toolkit#make")
    expect(ids.get("effect.Schema.Struct")).toBe("effect/Schema#Struct")
    expect(ids.get("effect.Data.TaggedError")).toBe("effect/Data#TaggedError")
    expect(ids.get("effect.Effect.gen")).toBe("effect/Effect#gen")
    expect(ids.get("effect.Schedule.recurs")).toBe("effect/Schedule#recurs")
    // Runtime exports agree with the declarations (no type-only ghosts).
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

describe("accept corpus", () => {
  for (const fixture of acceptFixtures) {
    test(`${fixture} is admitted with its expected resolved symbols`, () => {
      expect(report(fixture).diagnostics).toEqual([])
      expectMatchesGolden(fixture, true)
    })
  }

  test("every symbol-backed §3 row is exercised by the accept corpus", () => {
    // Only accept fixtures count: a use recorded in a rejected file proves nothing about admission.
    const used = new Set(acceptFixtures.flatMap((f) => report(f).uses.map((u) => u.row)))
    const rows = new Set(profile.admitted.map((a) => a.row))
    expect([...rows].filter((row) => !used.has(row))).toEqual([])
  })

  test("aliased admitted imports resolve to the canonical symbols", () => {
    const symbols = report("accept/aliased-imports.ts").uses.map((u) => u.symbol)
    expect(symbols).toContain("effect/Effect#gen")
    expect(symbols).toContain("@norn/authoring#Norn.flow")
    expect(symbols).toContain("@norn/authoring#Norn.perform")
  })
})

describe("reject corpus", () => {
  for (const fixture of rejectFixtures) {
    test(`${fixture} fails with its expected codes and spans`, () => {
      expect(report(fixture).diagnostics.length).toBeGreaterThan(0)
      expectMatchesGolden(fixture, false)
    })
  }

  test("each ruling §4 rejected construct has a fixture that reports its code", () => {
    for (const c of profile.rejectedConstructs) {
      expect(c.fixtures.length).toBeGreaterThan(0)
      for (const fixture of c.fixtures) {
        const codes = report(fixture).diagnostics.map((d) => d.code)
        expect({ construct: c.construct, fixture, has: codes.includes(c.code) }).toEqual({
          construct: c.construct,
          fixture,
          has: true,
        })
      }
    }
  })

  test("a local binding named Effect is rejected by symbol, not accepted by spelling", () => {
    const d = report("reject/shadowed-effect.ts").diagnostics.find((x) => x.code === "NORN-FLOW-SYMBOL")
    expect(d?.construct).toBe("Effect.gen")
    const hostile = report("reject/hostile-reexport.ts").diagnostics.map((x) => x.code)
    expect(hostile).toContain("NORN-FLOW-SYMBOL")
  })

  test("reject fixtures are well-typed, so each failure is a profile decision", () => {
    for (const fixture of rejectFixtures) {
      const typeErrors = report(fixture).diagnostics.filter((d) => d.code === "NORN-FLOW-TYPE")
      expect({ fixture, typeErrors }).toEqual({ fixture, typeErrors: [] })
    }
  })
})
