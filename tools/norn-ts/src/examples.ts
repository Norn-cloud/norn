/**
 * LAW-EXAMPLE-1 (#1176 unit B): run authored `Norn.example` cases against the compiled
 * artifacts with the checker's own law semantics (`evaluateLaw`, the Rust
 * `norn_checker::law_examples::evaluate`). A case whose verdict differs from its
 * expectation is a `law.example.mismatch` diagnostic at the example's source; a law the
 * checker cannot evaluate on one value is `law.example.unsupported`. Nothing here
 * re-implements a law: the verdict is the checker's.
 */
import type { Json } from "./canonical.ts"
import type { CheckerAbi, CheckerDiagnostic, LawVerdict } from "./checker-abi.ts"
import type { SourceLocation } from "./builder/diagnostics.ts"

/** The expected verdict of an example. */
export type ExampleExpectation = "holds" | "violated" | "not_applicable" | { readonly excepted: ReadonlyArray<string> }

/** One compiled example: where the law lives, the subject value and the expectation. */
export interface CompiledExample {
  readonly name: string
  /** Artifact holding the law and the JSON pointer of its `LawIR`. */
  readonly artifactId: string
  readonly pointer: string
  readonly given: Json
  readonly expect: ExampleExpectation
  readonly at: SourceLocation
}

function resolvePointer(document: unknown, pointer: string): unknown {
  if (pointer === "") return document
  return pointer.slice(1).split("/").map((part) => part.replace(/~1/g, "/").replace(/~0/g, "~")).reduce<unknown>((node, key) => {
    if (node === null || typeof node !== "object") return undefined
    return (node as Record<string, unknown>)[key]
  }, document)
}

const describe = (verdict: LawVerdict): string =>
  !verdict.ok ? `unsupported (${verdict.error})`
  : verdict.verdict === "excepted" ? `excepted by [${verdict.applied.join(", ")}]`
  : verdict.verdict

const agrees = (verdict: LawVerdict, expect: ExampleExpectation): boolean => {
  if (!verdict.ok) return false
  if (typeof expect === "string") return verdict.verdict === expect
  return verdict.verdict === "excepted" && [...verdict.applied].sort().join("\u0000") === [...expect.excepted].sort().join("\u0000")
}

/**
 * Run every example; returns one diagnostic per failing example, in order. A checker built
 * before `evaluateLaw` existed fails every example as unsupported rather than skipping it.
 */
export function runExamples(checker: CheckerAbi, artifacts: ReadonlyArray<Json>, examples: ReadonlyArray<CompiledExample>): CheckerDiagnostic[] {
  const out: CheckerDiagnostic[] = []
  for (const example of examples) {
    const artifact = artifacts.find((a) => (a as { artifactId?: string }).artifactId === example.artifactId)
    const law = artifact === undefined ? undefined : resolvePointer(artifact, example.pointer)
    const verdict: LawVerdict = law === undefined
      ? { ok: false, code: "law.example.unsupported", error: `no law at ${example.artifactId}${example.pointer}` }
      : checker.evaluateLaw
        ? checker.evaluateLaw(law, example.given)
        : { ok: false, code: "law.example.unsupported", error: "this checker package cannot evaluate laws; rebuild it" }
    if (agrees(verdict, example.expect)) continue
    const expected = typeof example.expect === "string" ? example.expect : `excepted by [${example.expect.excepted.join(", ")}]`
    out.push({
      code: verdict.ok ? "law.example.mismatch" : "law.example.unsupported",
      severity: "error",
      message: `example ${JSON.stringify(example.name)}: expected ${expected}, the checker says ${describe(verdict)}`,
      artifactId: example.artifactId,
      span: { pointer: example.pointer, source: null },
      source: example.at,
    })
  }
  return out
}
