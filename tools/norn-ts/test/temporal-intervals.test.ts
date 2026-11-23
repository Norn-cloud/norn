import { expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, relative, resolve } from "node:path"
import { compileDefinitionFiles, compileDefinitions } from "../src/compiler/definitions.ts"
import { checkPack, compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const REPO = resolve(TOOL_ROOT, "../..")
const CLI = join(TOOL_ROOT, "src/cli.ts")

function withTempDir<T>(fn: (dir: string) => T): T {
  const dir = mkdtempSync(join(tmpdir(), "norn-temporal-intervals-"))
  try {
    return fn(dir)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

test("exposes norn-zen/6 and norn-zen/7 temporal ops in @norn/authoring with clean ZEN lowering", () => {
  withTempDir((dir) => {
    const file = join(dir, "temporal.ts")
    writeFileSync(
      file,
      `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Window = Schema.Struct({
  a_start: Norn.timestamp("utc"),
  a_end: Norn.timestamp("utc"),
  b_start: Norn.timestamp("utc"),
  b_end: Norn.timestamp("utc"),
  local_wall: Schema.String,
  tz: Schema.String,
  dst_policy: Schema.String,
  day_a: Norn.date(),
  day_b: Norn.date(),
})

export const TemporalOps = Norn.decision({
  id: "demo.temporal_ops",
  title: "Temporal ops decision",
  version: "0.1.0",
  contract: "decide",
  scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({
    a_start: Schema.String,
    a_end: Schema.String,
    b_start: Schema.String,
    b_end: Schema.String,
    local_wall: Schema.String,
    tz: Schema.String,
    dst_policy: Schema.String,
    day_a: Schema.String,
    day_b: Schema.String,
  }),
  output: Schema.Struct({
    before: Schema.Boolean,
    at_or_before: Schema.Boolean,
    not_before: Schema.Boolean,
    span_seconds: Schema.Finite,
    shifted_ts: Schema.String,
    span_days: Schema.Int,
    shifted_day: Schema.String,
    overlaps: Schema.Boolean,
    gap: Schema.Finite,
    intersection: Schema.Finite,
    resolved: Schema.String,
    resolved_before_end: Schema.Boolean,
  }),
}, (input) => ({
  before: Norn.timestampBefore(input.a_start, input.a_end),
  at_or_before: Norn.timestampAtOrBefore(input.a_start, input.a_end),
  not_before: !Norn.timestampBefore(input.a_start, input.a_end) && Norn.timestampAtOrBefore(input.b_start, input.b_end),
  span_seconds: Norn.secondsBetween(input.a_start, input.a_end),
  shifted_ts: Norn.addSeconds(input.a_start, 3600),
  span_days: Norn.daysBetween(input.day_a, input.day_b),
  shifted_day: Norn.addDays(input.day_a, 7),
  overlaps: Norn.intervalsOverlap(input.a_start, input.a_end, input.b_start, input.b_end),
  gap: Norn.intervalGapSeconds(input.a_start, input.a_end, input.b_start, input.b_end),
  intersection: Norn.intervalIntersectionSeconds(input.a_start, input.a_end, input.b_start, input.b_end),
  resolved: Norn.resolveLocalTimestamp(input.local_wall, input.tz, input.dst_policy),
  resolved_before_end: Norn.timestampBefore(
    Norn.resolveLocalTimestamp(input.local_wall, input.tz, input.dst_policy),
    Norn.addSeconds(input.a_end, 60),
  ),
}))
`,
    )

    const compiled = compileDefinitions(file, { scope: { orgId: "o1", environmentId: "dev" }, sourcePath: "temporal.ts" }, {}, { forbidUnsafeZen: true })
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics, null, 2))
    expect(compiled.warnings).toEqual([])
    const rule = compiled.artifacts[0] as any
    const tableNode = rule.payload.graph.nodes.find((n: any) => n.type === "decisionTableNode")
    const row = tableNode.content.rules[0]
    const byKey = Object.fromEntries(tableNode.content.outputs.map((o: any) => [o.field, row[o.id]]))

    expect(byKey.before).toBe("timestamp(a_start) < timestamp(a_end)")
    expect(byKey.at_or_before).toBe("timestamp(a_start) <= timestamp(a_end)")
    expect(byKey.not_before).toBe("(not (timestamp(a_start) < timestamp(a_end))) and (timestamp(b_start) <= timestamp(b_end))")
    expect(byKey.span_seconds).toBe("seconds_between(timestamp(a_start), timestamp(a_end))")
    expect(byKey.shifted_ts).toBe("add_seconds(timestamp(a_start), 3600)")
    expect(byKey.span_days).toBe("days_between(date(day_a), date(day_b))")
    expect(byKey.shifted_day).toBe("add_days(date(day_a), 7)")
    expect(byKey.overlaps).toBe("intervals_overlap(timestamp(a_start), timestamp(a_end), timestamp(b_start), timestamp(b_end))")
    expect(byKey.gap).toBe("interval_gap_seconds(timestamp(a_start), timestamp(a_end), timestamp(b_start), timestamp(b_end))")
    expect(byKey.intersection).toBe("interval_intersection_seconds(timestamp(a_start), timestamp(a_end), timestamp(b_start), timestamp(b_end))")
    expect(byKey.resolved).toBe("resolve_local_timestamp(local_wall, tz, dst_policy)")
    expect(byKey.resolved_before_end).toBe("resolve_local_timestamp(local_wall, tz, dst_policy) < add_seconds(timestamp(a_end), 60)")
  })
})

test("--forbid-unsafe-zen promotes NORN-AUTHORING-UNSAFE-ZEN from warning to hard error and defaults on for registry packs", () => {
  withTempDir((dir) => {
    const file = join(dir, "escape.ts")
    writeFileSync(
      file,
      `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Shift = Norn.interfaceType({
  name: "demo.Shift",
  title: "Shift",
  version: "0.1.0",
  properties: Schema.Struct({
    id: Schema.String,
    hours: Schema.Int,
  }),
  invariants: [{
    id: "positive_hours",
    message: "hours must be positive",
    expression: Norn.unsafeZen<boolean>("self.hours > 0"),
  }],
})
`,
    )

    // Outside registry/ without flag: compiles with NORN-AUTHORING-UNSAFE-ZEN warning.
    const allowed = compilePack(dir, { scope: { orgId: "o1", environmentId: "dev" }, root: dir })
    expect(allowed.ok).toBe(true)
    if (allowed.ok) {
      expect(allowed.warnings.some((w) => w.code === "NORN-AUTHORING-UNSAFE-ZEN")).toBe(true)
    }

    // With explicit forbidUnsafeZen: hard error.
    const forbidden = compilePack(dir, { scope: { orgId: "o1", environmentId: "dev" }, root: dir, forbidUnsafeZen: true })
    expect(forbidden.ok).toBe(false)
    if (!forbidden.ok) {
      expect(forbidden.failures[0]?.diagnostics.some((d) => d.code === "NORN-AUTHORING-UNSAFE-ZEN")).toBe(true)
    }

    // With registryPackagePath (registry/ and domain packs): defaults to hard error.
    const registryPack = compileDefinitionFiles([file], { scope: {}, sourcePath: "registry/packs/demo/escape.ts", registryPackagePath: "registry/packs/demo" })[0]!
    expect(registryPack.ok).toBe(false)
    if (!registryPack.ok) {
      expect(registryPack.diagnostics.some((d) => d.code === "NORN-AUTHORING-UNSAFE-ZEN")).toBe(true)
    }

    // Under a registry/ directory path in compilePack: defaults to hard error even without registry option.
    const regSubdir = join(dir, "registry", "packs", "demo")
    mkdirSync(regSubdir, { recursive: true })
    const regFile = join(regSubdir, "escape.ts")
    writeFileSync(regFile, readFileSync(file, "utf8"))
    const regDefault = compilePack(regSubdir, { scope: { orgId: "o1", environmentId: "dev" }, root: dir })
    expect(regDefault.ok).toBe(false)
    if (!regDefault.ok) {
      expect(regDefault.failures[0]?.diagnostics.some((d) => d.code === "NORN-AUTHORING-UNSAFE-ZEN")).toBe(true)
    }

    // cwd = tools/norn-ts compiling a relative path into a registry/ directory MUST still hard-error (no cwd bypass).
    const regFromToolRoot = compilePack(regSubdir, { scope: { orgId: "o1", environmentId: "dev" }, root: TOOL_ROOT })
    expect(regFromToolRoot.ok).toBe(false)
    if (!regFromToolRoot.ok) {
      expect(regFromToolRoot.failures[0]?.diagnostics.some((d) => d.code === "NORN-AUTHORING-UNSAFE-ZEN")).toBe(true)
    }
    const relFromToolRoot = relative(TOOL_ROOT, regSubdir)
    const cliFromToolRoot = Bun.spawnSync({
      cmd: [process.execPath, CLI, "compile", relFromToolRoot, "--org", "o1", "--env", "dev"],
      cwd: TOOL_ROOT,
    })
    expect(cliFromToolRoot.exitCode).toBe(1)
    expect(new TextDecoder().decode(cliFromToolRoot.stdout)).toContain("NORN-AUTHORING-UNSAFE-ZEN")

    // Running from inside registry/packs/demo compiling `.` MUST also still hard-error (no cwd bypass).
    const cliInsideRegistry = Bun.spawnSync({
      cmd: [process.execPath, CLI, "compile", ".", "--org", "o1", "--env", "dev"],
      cwd: regSubdir,
    })
    expect(cliInsideRegistry.exitCode).toBe(1)
    expect(new TextDecoder().decode(cliInsideRegistry.stdout)).toContain("NORN-AUTHORING-UNSAFE-ZEN")

    // CLI --forbid-unsafe-zen flag returns non-zero exit code with NORN-AUTHORING-UNSAFE-ZEN.
    const cli = Bun.spawnSync({
      cmd: [process.execPath, CLI, "compile", file, "--org", "o1", "--env", "dev", "--forbid-unsafe-zen"],
      cwd: TOOL_ROOT,
    })
    expect(cli.exitCode).toBe(1)
    const stdout = new TextDecoder().decode(cli.stdout)
    expect(stdout).toContain("NORN-AUTHORING-UNSAFE-ZEN")
  })
}, 20_000)

test("two unrelated domains (HR shifts + equipment maintenance booking) share core.TimeInterval without unsafeZen", () => {
  withTempDir((dir) => {
    const hrFile = join(dir, "hr_shifts.ts")
    const maintFile = join(dir, "equipment_booking.ts")

    writeFileSync(
      hrFile,
      `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const ShiftProps = Schema.Struct({
  shift_id: Schema.String,
  worker_id: Schema.String,
  starts_at: Norn.ref("core.time.instant"),
  ends_at: Norn.ref("core.time.instant"),
  timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
})

export const ShiftAssignment = Norn.type({
  name: "workforce.ShiftAssignment",
  role: "happening",
  title: "Shift assignment",
  version: "0.1.0",
  implements: ["core.TimeInterval"],
  properties: ShiftProps,
  identity: [{ name: "primary", properties: ["shift_id"] }],
})

export const CheckShiftRest = Norn.decision({
  id: "workforce.check_shift_rest",
  title: "Check shift overlap and rest gap",
  version: "0.1.0",
  contract: "sub_decision",
  scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({
    first_start: Schema.String,
    first_end: Schema.String,
    second_start: Schema.String,
    second_end: Schema.String,
    min_rest_seconds: Schema.Finite,
  }),
  output: Schema.Struct({
    overlaps: Schema.Boolean,
    rest_gap_seconds: Schema.Finite,
    compliant: Schema.Boolean,
  }),
}, (input) => ({
  overlaps: Norn.intervalsOverlap(input.first_start, input.first_end, input.second_start, input.second_end),
  rest_gap_seconds: Norn.intervalGapSeconds(input.first_start, input.first_end, input.second_start, input.second_end),
  compliant:
    !Norn.intervalsOverlap(input.first_start, input.first_end, input.second_start, input.second_end) &&
    Norn.intervalGapSeconds(input.first_start, input.first_end, input.second_start, input.second_end) >= input.min_rest_seconds,
}))
`,
    )

    writeFileSync(
      maintFile,
      `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const BookingProps = Schema.Struct({
  booking_id: Schema.String,
  machine_id: Schema.String,
  starts_at: Norn.ref("core.time.instant"),
  ends_at: Norn.ref("core.time.instant"),
  timezone: Schema.NullOr(Norn.ref("core.time.iana_timezone")),
})

export const MaintenanceWindow = Norn.type({
  name: "plant.MaintenanceWindow",
  role: "happening",
  title: "Equipment maintenance window",
  version: "0.1.0",
  implements: ["core.TimeInterval"],
  properties: BookingProps,
  identity: [{ name: "primary", properties: ["booking_id"] }],
})

export const CheckEquipmentConflict = Norn.decision({
  id: "plant.check_equipment_conflict",
  title: "Check equipment booking conflict and downtime overlap",
  version: "0.1.0",
  contract: "sub_decision",
  scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({
    maint_local_start: Schema.String,
    maint_local_end: Schema.String,
    tz: Schema.String,
    dst_policy: Schema.String,
    job_start: Schema.String,
    job_end: Schema.String,
  }),
  output: Schema.Struct({
    conflicts: Schema.Boolean,
    downtime_overlap_seconds: Schema.Finite,
  }),
}, (input) => ({
  conflicts: Norn.intervalsOverlap(
    Norn.resolveLocalTimestamp(input.maint_local_start, input.tz, input.dst_policy),
    Norn.resolveLocalTimestamp(input.maint_local_end, input.tz, input.dst_policy),
    input.job_start,
    input.job_end,
  ),
  downtime_overlap_seconds: Norn.intervalIntersectionSeconds(
    Norn.resolveLocalTimestamp(input.maint_local_start, input.tz, input.dst_policy),
    Norn.resolveLocalTimestamp(input.maint_local_end, input.tz, input.dst_policy),
    input.job_start,
    input.job_end,
  ),
}))
`,
    )

    const compiled = compilePack(dir, { scope: { orgId: "o1", environmentId: "dev" }, root: dir, forbidUnsafeZen: true })
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.failures, null, 2))
    expect(compiled.warnings).toEqual([])
    expect(compiled.artifacts.length).toBe(4)

    if (checker) {
      const coreDir = join(REPO, "registry/core/artifacts")
      const base = ["core.time.json", "core.TimeInterval.interface.json"].map((name) =>
        JSON.parse(readFileSync(join(coreDir, name), "utf8")),
      )
      const report = checkPack(checker, JSON.parse(JSON.stringify(compiled.artifacts)), base)
      expect(report.diagnostics.diagnostics).toEqual([])
      expect(report.admitted).toBe(true)
    }
  })
})
