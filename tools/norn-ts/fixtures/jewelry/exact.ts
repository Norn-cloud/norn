export const PURITY_DENOMINATOR = 24_000_000n
export const MAX_FINE_TICKS = (1n << 96n) - 1n
type Refusal = "purity_unrepresentable" | "purity_conflict" | "fine_mass_overflow" | "invalid_gross_mass"
type Result<T> = { readonly ok: true; readonly value: T } | { readonly ok: false; readonly code: Refusal }

function scaled(input: string, places: number): bigint | undefined {
  if (input.length > 128 || !/^(0|[1-9][0-9]*)(\.[0-9]+)?$/.test(input)) return undefined
  const [whole, fraction = ""] = input.split(".")
  if (fraction.length > places) return undefined
  return BigInt(whole!) * 10n ** BigInt(places) + BigInt(fraction.padEnd(places, "0"))
}

export function purity(input: { readonly karat?: string; readonly fineness?: string }): Result<bigint> {
  const karat = input.karat === undefined ? undefined : scaled(input.karat, 6)
  const fineness = input.fineness === undefined ? undefined : scaled(input.fineness, 3)
  if ((input.karat !== undefined && karat === undefined) ||
      (input.fineness !== undefined && fineness === undefined)) return { ok: false, code: "purity_unrepresentable" }
  const fromFineness = fineness === undefined ? undefined : fineness * 24n
  if ([karat, fromFineness].some(value => value !== undefined && (value <= 0n || value > PURITY_DENOMINATOR))) {
    return { ok: false, code: "purity_unrepresentable" }
  }
  if (karat !== undefined && fromFineness !== undefined && karat !== fromFineness) return { ok: false, code: "purity_conflict" }
  const value = karat ?? fromFineness
  if (value === undefined || value <= 0n || value > PURITY_DENOMINATOR) return { ok: false, code: "purity_unrepresentable" }
  return { ok: true, value }
}

export function fineMass(input: { readonly grossMicrograms: string; readonly karat?: string; readonly fineness?: string }): Result<bigint> {
  const normalized = purity(input)
  if (!normalized.ok) return normalized
  if (!/^[1-9][0-9]*$/.test(input.grossMicrograms)) return { ok: false, code: "invalid_gross_mass" }
  if (input.grossMicrograms.length > 29) return { ok: false, code: "fine_mass_overflow" }
  const gross = BigInt(input.grossMicrograms)
  if (gross > MAX_FINE_TICKS / normalized.value) return { ok: false, code: "fine_mass_overflow" }
  return { ok: true, value: gross * normalized.value }
}
