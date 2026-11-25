import { canonicalJson } from "../canonical.ts"
import { nonNull, type ValueSchema } from "./schema.ts"

/** Undefined exactly when every value described by from is admitted by to. */
export function assignable(from: ValueSchema, to: ValueSchema, path = "$"): string | undefined {
  if (Array.isArray(to.type) && to.type.length > 2) return undefined
  const mismatch = () => `${path} is ${from.type}, expected ${to.type}`
  if (from.type === "null") return to.type === "null" || Array.isArray(to.type) ? undefined : mismatch()
  if (Array.isArray(from.type)) {
    if (from.type.length !== 2 || !Array.isArray(to.type)) return mismatch()
    return assignable(nonNull(from), nonNull(to), path)
  }
  if (Array.isArray(to.type)) return assignable(from, nonNull(to), path)
  if ("nornType" in to && to.nornType && (!("nornType" in from) || !from.nornType || canonicalJson(from.nornType) !== canonicalJson(to.nornType))) {
    return `${path} requires the declared exact decimal contract`
  }
  if (from.type === "integer" && to.type === "number") return undefined
  if (from.type !== to.type) return mismatch()
  if (from.type === "string" && to.type === "string" && to.enum) {
    const allowed = to.enum
    if (!from.enum?.every(value => allowed.includes(value))) return `${path} is outside the declared enum`
  }
  if (from.type === "array" && to.type === "array") return assignable(from.items, to.items, `${path}[]`)
  if (from.type === "object" && to.type === "object") {
    for (const key of [...(to.required ?? [])].sort()) {
      if (!from.required?.includes(key)) return `${path}.${key} is required`
    }
    for (const key of Object.keys(from.properties).sort()) {
      if (!Object.hasOwn(to.properties, key)) return `${path}.${key} is not declared`
      const problem = assignable(from.properties[key]!, to.properties[key]!, `${path}.${key}`)
      if (problem) return problem
    }
  }
  return undefined
}
