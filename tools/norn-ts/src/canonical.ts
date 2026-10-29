/**
 * Canonical JSON for builder output (T7-FLOW-1).
 *
 * Builder output is plain data: `null`, booleans, finite numbers, strings,
 * arrays, and plain objects. {@link toJson} proves that (it rejects functions,
 * symbols, bigints, `undefined` array elements, non-finite numbers, class
 * instances, and cycles; an `undefined` object member is omitted, as in
 * `JSON.stringify`) and returns a copy with object keys sorted by UTF-16 code unit, so
 * the same definition always prints the same bytes. This is the closure-free
 * guarantee: nothing executable survives into an artifact.
 *
 * Hand-rolled on purpose: the tool may depend only on the pinned `effect` and
 * `typescript` packages (tools/norn-ts/AGENTS.md), and the rule is a dozen
 * lines. Rust canonicalisation (`norn-canonical`) stays the hashing authority;
 * this only fixes the golden-file bytes.
 */

export type Json = null | boolean | number | string | ReadonlyArray<Json> | { readonly [key: string]: Json }

export class NonJsonValue extends Error {
  constructor(
    readonly path: string,
    readonly reason: string,
  ) {
    super(`${path}: ${reason}`)
  }
}

function isPlainObject(value: object): boolean {
  const proto = Object.getPrototypeOf(value)
  return proto === Object.prototype || proto === null
}

/** A sorted-key copy of `value`, or {@link NonJsonValue} at the first non-JSON member. */
export function toJson(value: unknown, path = "$", seen: Set<object> = new Set()): Json {
  switch (typeof value) {
    case "string":
    case "boolean":
      return value
    case "number":
      if (!Number.isFinite(value)) throw new NonJsonValue(path, `non-finite number ${value}`)
      return Object.is(value, -0) ? 0 : value
    case "object": {
      if (value === null) return null
      if (seen.has(value)) throw new NonJsonValue(path, "cycle")
      seen.add(value)
      try {
        if (Array.isArray(value)) return value.map((item, index) => toJson(item, `${path}[${index}]`, seen))
        if (!isPlainObject(value)) {
          throw new NonJsonValue(path, `instance of ${value.constructor?.name ?? "an unknown class"} is not plain data`)
        }
        const out: Record<string, Json> = {}
        for (const key of Object.keys(value).sort()) {
          // Assigning it would write the copy's prototype and silently drop the member.
          if (key === "__proto__") throw new NonJsonValue(`${path}.${key}`, "__proto__ is not a portable JSON member name")
          const member = (value as Record<string, unknown>)[key]
          if (member === undefined) continue
          out[key] = toJson(member, `${path}.${key}`, seen)
        }
        return out
      } finally {
        seen.delete(value)
      }
    }
    default:
      throw new NonJsonValue(path, `${typeof value} is not JSON`)
  }
}

/** Canonical golden-file text: sorted keys, two-space indent, trailing newline. */
export function canonicalJson(value: unknown): string {
  return `${JSON.stringify(toJson(value), null, 2)}\n`
}
