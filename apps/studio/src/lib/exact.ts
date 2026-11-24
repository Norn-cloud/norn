import { numberTexts } from "../../../../packages/norn-sdk/src/exact.ts"

// The exact-number reader lives in `@norn/sdk` (#1374), shared by the typed client and Studio.
export { member, numberTexts } from "../../../../packages/norn-sdk/src/exact.ts"

/** Where a document came from: engine output replayed from a recording, or the server now. */
export type Origin = "recorded" | "live"

/** A server document beside the exact text of every number in it. */
export interface Read<T> {
  readonly document: T
  readonly numbers: ReadonlyMap<string, string>
  readonly origin: Origin
}

export const readDocument = <T,>(source: string, origin: Origin): Read<T> =>
  ({ document: JSON.parse(source) as T, numbers: numberTexts(source), origin })
