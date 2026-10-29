// The T1-FN-1 Function the vertical flow performs: fixtures/function/manifest.wasm.json
// (norn.classify.whatsapp-intent, version 1, the qualified-WASM intent classifier).
// Its I/O types are the manifest's input and output records; its typed failure is
// the flow's declared error.
import { Data } from "effect"
import { Norn } from "@norn/authoring"

export class UnsupportedLanguage extends Data.TaggedError("UnsupportedLanguage")<{ readonly locale: string }> {}

export const classifyIntent = Norn.fn<{
  readonly channel: string
  readonly locale: string
  readonly messageText: string
}, {
  readonly intent: string
  readonly confidence: number
  readonly labels: readonly string[]
}, UnsupportedLanguage>({ id: "norn.classify.whatsapp-intent", version: "1" })
