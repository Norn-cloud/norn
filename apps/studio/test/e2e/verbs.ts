import { VERBS, type Verb, type VerbMatrix } from "../../src/lib/editor.ts"

/** The part of a Playwright page and locator the verb tests use (Playwright's own types satisfy it). */
export interface Locator {
  click(options?: { timeout?: number }): Promise<void>
  fill(value: string): Promise<void>
  press(key: string): Promise<void>
  selectOption(value: string): Promise<unknown>
  inputValue(): Promise<string>
  isDisabled(): Promise<boolean>
  textContent(): Promise<string | null>
  count(): Promise<number>
  waitFor(options?: { timeout?: number; state?: "attached" | "detached" | "visible" | "hidden" }): Promise<void>
  first(): Locator
  nth(index: number): Locator
  getByRole(role: string, options?: { name?: string | RegExp; exact?: boolean; level?: number }): Locator
  getByText(text: string | RegExp, options?: { exact?: boolean }): Locator
  getByLabel(text: string | RegExp, options?: { exact?: boolean }): Locator
}
export interface E2EPage extends Pick<Locator, "getByRole" | "getByText" | "getByLabel"> {
  goto(url: string): Promise<unknown>
  reload(): Promise<unknown>
  url(): string
  locator(selector: string): Locator
  keyboard: { press(key: string): Promise<void> }
  on(event: "dialog", listener: (dialog: { accept(): Promise<void>; dismiss(): Promise<void> }) => void): void
}
export interface E2E {
  readonly page: E2EPage
  /** The deployment under test, e.g. https://next.norn.cloud. */
  readonly base: string
}

/**
 * One end-to-end test per supported verb. The type has a key for exactly the
 * verbs the matrix marks supported, so a supported verb without its test does
 * not compile, and a test for a verb marked N/A does not either.
 */
export type VerbSpecs<M extends VerbMatrix> = { readonly [V in Verb as M[V] extends "supported" ? V : never]: (e2e: E2E) => Promise<void> }

export interface VerbSuite { readonly kind: string; readonly matrix: VerbMatrix; readonly specs: Readonly<Partial<Record<Verb, (e2e: E2E) => Promise<void>>>> }

export const defineVerbs = <const M extends VerbMatrix>(kind: string, matrix: M, specs: VerbSpecs<M>): VerbSuite => ({ kind, matrix, specs })

/** The verbs a suite must run: every supported one, each with its test. */
/** The runner's exit status: a run in which no verb ran proves nothing, so it fails like a failed verb. */
export const exitStatus = (ran: number, failed: number): 0 | 1 => ran > 0 && failed === 0 ? 0 : 1

export const supported = (suite: VerbSuite) => VERBS.filter((verb) => suite.matrix[verb] === "supported")
