import { LosslessNumber, isLosslessNumber, isSafeNumber, parse, stringify } from "lossless-json"

export { isLosslessNumber, type LosslessNumber }

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

export function asString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined
}

export function asArray(value: unknown): readonly unknown[] {
  return Array.isArray(value) ? value : []
}

function parseNumber(text: string): number | LosslessNumber {
  return isSafeNumber(text) ? Number(text) : new LosslessNumber(text)
}

// Keep lossless-json's token-aware parser/stringifier at the package boundary.
export function parseExact(text: string): unknown {
  return parse(text, null, parseNumber)
}

export function stringifyExact(value: unknown, indent?: number): string {
  return stringify(value, undefined, indent) ?? "null"
}

export function exactText(value: unknown): string | undefined {
  return isLosslessNumber(value) ? value.value : undefined
}
