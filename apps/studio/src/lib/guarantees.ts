import type { Schemas } from "./transport.ts"

type Scalar = Schemas["NornGuaranteesGuaranteeScalar"]
type Verdict = Schemas["NornGuaranteesGuaranteeVerdict"]
type Level = Schemas["NornGuaranteesGuaranteeLevel"]

/** An exact scalar as text with its unit: coefficient `125`, scale 2, `g` → `1.25 g`. */
export function scalar({ coefficient, scale, unit }: Scalar): string {
  const negative = coefficient.startsWith("-")
  const digits = (negative ? coefficient.slice(1) : coefficient).padStart(scale + 1, "0")
  const whole = scale > 0 ? `${digits.slice(0, digits.length - scale)}.${digits.slice(digits.length - scale)}` : digits
  return `${negative ? "-" : ""}${whole}${unit ? ` ${unit.code}` : ""}`
}

/** The server's verdict in words; the reason for an unknown is shown as the server wrote it. */
export const verdictWords = (verdict: Verdict): string =>
  verdict.kind === "bounded-pass" ? "Holds in every checked case" : verdict.kind === "refuted" ? "Refuted" : "Unknown"

/** The server's evidence level in words. */
export const levelWords = (level: Level): string => ({
  "exhaustive-finite/bounded": "Proved on the model (every case within the bounds)",
  "runtime-enforced": "Enforced at runtime",
  "parity-tested": "Tested for parity",
  unknown: "No evidence",
})[level.kind]

/** A property name in words: `input_fine_g` → `input fine g`. */
export const field = (name: string): string => name.replaceAll("_", " ")

/** How many cases the checker's declared bounds enabled, as the server recorded them; `undefined` when the bounds name none. */
export function cases(boundsJson: string): number | undefined {
  const bounds = JSON.parse(boundsJson) as { declaredBoundsProfile?: { enabledCases?: unknown } }
  const count = bounds.declaredBoundsProfile?.enabledCases
  return typeof count === "number" ? count : undefined
}

/** A hash that names one guarantee document (`#guarantee-original`). */
export function targetsGuarantee(hash: string): boolean {
  return hash.startsWith("guarantee-")
}

/**
 * The technical details' open state once the hash moves: a guarantee link
 * opens them; any other hash (Back to the page, an unrelated anchor) keeps
 * what the reader chose.
 */
export function openAfterHash(open: boolean, hash: string): boolean {
  return open || targetsGuarantee(hash)
}
