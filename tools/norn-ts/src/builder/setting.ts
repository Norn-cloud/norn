/**
 * Setting authoring (`SettingRoot`, PARAM-EXACT-1 / PARAM-BOUND-1 / VAL-EXACT-1):
 * effective-dated, reviewed, scoped configuration values compiled to `setting`
 * artifacts. Numeric settings require exact `DecimalLiteral` strings; binary
 * JavaScript numbers are refused.
 */
import { type Json, toJson } from "../canonical.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"

export type SettingValueType = "text" | "number" | "boolean" | "json"

export type SettingReviewState = "draft" | "proposed" | "approved" | "active" | "rejected" | "superseded"

export type SettingScopeDimension = "orgId" | "tenantId" | "workspaceId" | "environmentId" | "businessUnitId"

export interface SettingSupersedes {
  readonly artifactId: string
  readonly version: string
}

export interface SettingReview {
  readonly state: SettingReviewState
  readonly reviewedBy?: string
  readonly reviewedAt?: string
  readonly reason?: string
}

export interface SettingDef {
  readonly _tag: "Setting"
  readonly id: string
  readonly title: string
  readonly version: string
  readonly settingKey: string
  readonly valueType: SettingValueType
  readonly value: Json
  readonly effectiveFrom: string
  readonly effectiveTo?: string
  readonly supersedes?: SettingSupersedes
  readonly review: SettingReview
  readonly scopeRequired: ReadonlyArray<string>
}

export const SETTING_VALUE_TYPES: ReadonlySet<string> = new Set(["text", "number", "boolean", "json"])
export const SETTING_REVIEW_STATES: ReadonlySet<string> = new Set(["draft", "proposed", "approved", "active", "rejected", "superseded"])
export const SETTING_SCOPE_DIMENSIONS: ReadonlySet<string> = new Set(["orgId", "tenantId", "workspaceId", "environmentId", "businessUnitId"])

const DECIMAL_LITERAL_PATTERN = /^-?(0|[1-9][0-9]*)(\.[0-9]+)?$/
const MAX_DECIMAL_SCALE = 28
const MAX_DECIMAL_MANTISSA = (1n << 96n) - 1n
const RFC3339_PATTERN = /^(\d{4})-(\d{2})-(\d{2})[Tt](\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?([Zz]|[+-](\d{2}):(\d{2}))$/

/**
 * VAL-EXACT-1: strict wire decimal grammar (`-?(0|[1-9][0-9]*)(\.[0-9]+)?`)
 * within the shared `norn_ir::Decimal` precision bound (96-bit mantissa, at
 * most 28 fractional digits).
 */
export function isValidDecimalLiteral(text: string): boolean {
  if (!DECIMAL_LITERAL_PATTERN.test(text)) return false
  const unsigned = text.startsWith("-") ? text.slice(1) : text
  const dot = unsigned.indexOf(".")
  const intPart = dot === -1 ? unsigned : unsigned.slice(0, dot)
  const fracPart = dot === -1 ? "" : unsigned.slice(dot + 1)
  if (fracPart.length > MAX_DECIMAL_SCALE) return false
  const trimmedFrac = fracPart.replace(/0+$/, "")
  const digits = `${intPart}${trimmedFrac}`
  return BigInt(digits) <= MAX_DECIMAL_MANTISSA
}

function daysInMonth(year: number, month: number): number {
  switch (month) {
    case 2:
      return (year % 4 === 0 && year % 100 !== 0) || year % 400 === 0 ? 29 : 28
    case 4:
    case 6:
    case 9:
    case 11:
      return 30
    default:
      return 31
  }
}

/**
 * Parse a strict RFC 3339 timestamp to exact UTC nanoseconds since Unix epoch,
 * rejecting invalid calendar dates (such as February 30) and invalid offsets.
 */
export function parseRfc3339Instant(text: string): bigint | undefined {
  const match = RFC3339_PATTERN.exec(text)
  if (!match) return undefined
  const year = Number(match[1])
  const month = Number(match[2])
  const day = Number(match[3])
  const hour = Number(match[4])
  const minute = Number(match[5])
  const second = Number(match[6])
  if (month < 1 || month > 12) return undefined
  if (day < 1 || day > daysInMonth(year, month)) return undefined
  if (hour > 23 || minute > 59 || second > 59) return undefined
  const fracRaw = match[7] ?? ""
  const fracNanos = BigInt(fracRaw.padEnd(9, "0"))
  const tz = match[8]!
  let offsetNanos = 0n
  if (tz !== "Z" && tz !== "z") {
    const offHour = Number(match[9])
    const offMin = Number(match[10])
    if (offHour > 23 || offMin > 59) return undefined
    const sign = tz.startsWith("-") ? -1n : 1n
    offsetNanos = sign * BigInt((offHour * 60 + offMin) * 60) * 1_000_000_000n
  }
  const d = new Date(0)
  d.setUTCFullYear(year, month - 1, day)
  d.setUTCHours(hour, minute, second, 0)
  const utcMillis = BigInt(d.getTime())
  return utcMillis * 1_000_000n - offsetNanos + fracNanos
}

export function setting(meta: Omit<SettingDef, "_tag">): SettingDef {
  // Refuse function-valued or non-JSON data before any artifact is emitted.
  toJson(meta)
  return { _tag: "Setting", ...meta }
}

export function lowerSetting(def: SettingDef, options: EnvelopeOptions): { readonly ok: true; readonly artifact: Json } {
  const namespace = def.id.split(".")[0]!
  const payload = {
    settingVersion: "0.1",
    settingKey: def.settingKey,
    valueType: def.valueType,
    value: def.value,
    effectiveFrom: def.effectiveFrom,
    ...(def.effectiveTo !== undefined ? { effectiveTo: def.effectiveTo } : {}),
    ...(def.supersedes !== undefined ? { supersedes: def.supersedes } : {}),
    review: def.review,
    scopeRequired: def.scopeRequired,
  }
  return {
    ok: true,
    artifact: envelope(
      { kind: "setting", artifactId: def.id, namespace, name: def.title, version: def.version },
      payload,
      options,
    ),
  }
}
