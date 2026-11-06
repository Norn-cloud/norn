import { z } from "zod"
import { CellUnavailable } from "./definitions.ts"

const scope = { orgId: "org_demo", environmentId: "analytics" }
const summary = z.object({ id: z.string(), title: z.string(), version: z.number(), lifecycle: z.string() })
const page = z.object({ items: z.array(summary), cursor: z.string().nullable().optional() })
const document = z.object({ id: z.string(), kind: z.literal("question"), version: z.number(), payload: z.unknown(), signoff: z.object({ restatementSha256: z.string() }).nullable().optional() })
const result = z.object({ installed: z.literal(true), id: z.string(), version: z.number() })
const refusal = z.object({ code: z.string(), message: z.string().optional(), detail: z.string().optional(), restatement: z.string().optional(), restatementSha256: z.string().optional() })

export class QuestionDefinitionError extends Error {
  constructor(readonly code: string, message: string, readonly status = 0, readonly restatement?: { readonly text: string; readonly sha256: string }) { super(message); this.name = "QuestionDefinitionError" }
}

/** The cell owns authorization, validation, versions, and the sign-off record. */
type Fetch = (input: string, init?: RequestInit) => Promise<Response>

export function questionDefinitions(fetcher: Fetch = (input, init) => fetch(input, init)) {
  const call = async (path: string, query: Record<string, string> = {}, body?: unknown): Promise<unknown> => {
    let response: Response
    try {
      response = await fetcher(`/api/host${path}?${new URLSearchParams({ ...scope, ...query })}`, body === undefined
        ? { headers: { accept: "application/json" } }
        : { method: "POST", headers: { "content-type": "application/json", accept: "application/json" }, body: JSON.stringify(body) })
    } catch { throw new CellUnavailable(0) }
    const raw: unknown = await response.json().catch(() => null)
    if (!response.ok) {
      const error = refusal.safeParse(raw)
      if (error.success) {
        const { code, detail, message, restatement, restatementSha256 } = error.data
        let checked: { text: string; sha256: string } | undefined
        if (code === "definition.signoff_mismatch" && restatement !== undefined && restatementSha256 !== undefined && /^[0-9a-f]{64}$/.test(restatementSha256)) {
          const bytes = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(restatement))
          const digest = Array.from(new Uint8Array(bytes), (byte) => byte.toString(16).padStart(2, "0")).join("")
          if (digest === restatementSha256) checked = { text: restatement, sha256: digest }
        }
        throw new QuestionDefinitionError(code, detail ?? message ?? code, response.status, checked)
      }
      throw new CellUnavailable(response.status)
    }
    return raw
  }
  return {
    async list(cursor?: string) {
      const parsed = page.safeParse(await call("/v1/definitions:list", { kind: "question", limit: "50", ...cursor ? { cursor } : {} }))
      if (!parsed.success) throw new QuestionDefinitionError("definition.shape", "The cell returned an invalid question list.")
      return parsed.data
    },
    async get(id: string) {
      const parsed = document.safeParse(await call(`/v1/definitions/${encodeURIComponent(id)}`))
      if (!parsed.success || parsed.data.payload === undefined) throw new QuestionDefinitionError("definition.shape", "The cell returned an invalid question.")
      return parsed.data
    },
    async install(id: string, payload: unknown, sha256: string, expectedVersion?: number) {
      const parsed = result.safeParse(await call("/v1/definitions:install", {}, { kind: "question", id, payload, signoff: { restatementSha256: sha256 }, ...expectedVersion === undefined ? {} : { expectedVersion } }))
      if (!parsed.success) throw new QuestionDefinitionError("definition.shape", "The cell did not confirm that the question was saved.")
      return parsed.data
    },
    async retire(id: string, expectedVersion: number) {
      const parsed = result.safeParse(await call(`/v1/definitions/${encodeURIComponent(id)}:retire`, {}, { expectedVersion }))
      if (!parsed.success) throw new QuestionDefinitionError("definition.shape", "The cell did not confirm that the question was removed.")
      return parsed.data
    },
  }
}
