/** One spending window's counters, as a budget Durable Object stores them. */
export interface Ledger {
  readonly hourStart: number
  readonly requests: number
  readonly dayStart: number
  readonly tokens: number
}

/** What one scope (a session, a visitor or the whole Worker) may spend. */
export interface Limits {
  readonly requestsPerHour: number
  readonly tokensPerDay: number
  /** Tokens one request may still use; a request is refused unless this much remains. */
  readonly reserve: number
}

const HOUR = 3_600_000
const DAY = 24 * HOUR

/** Per browser session, per visitor address, and for the whole Worker. */
export const LIMITS = {
  session: { requestsPerHour: 30, tokensPerDay: 300_000, reserve: 40_000 },
  visitor: { requestsPerHour: 60, tokensPerDay: 600_000, reserve: 40_000 },
  global: { requestsPerHour: 600, tokensPerDay: 5_000_000, reserve: 40_000 },
} as const satisfies Record<string, Limits>

export type Scope = keyof typeof LIMITS

// A ledger that has never counted: both windows start at its first request.
export const EMPTY: Ledger = { hourStart: Number.NEGATIVE_INFINITY, requests: 0, dayStart: Number.NEGATIVE_INFINITY, tokens: 0 }

const roll = (ledger: Ledger, now: number): Ledger => ({
  hourStart: now - ledger.hourStart >= HOUR ? now : ledger.hourStart,
  requests: now - ledger.hourStart >= HOUR ? 0 : ledger.requests,
  dayStart: now - ledger.dayStart >= DAY ? now : ledger.dayStart,
  tokens: now - ledger.dayStart >= DAY ? 0 : ledger.tokens,
})

export type Admission =
  | { readonly ok: true; readonly ledger: Ledger }
  | { readonly ok: false; readonly ledger: Ledger; readonly reason: string; readonly retryAfterSeconds: number }

/** Count one request against the window, or refuse it when the window is spent. */
export function admit(previous: Ledger, limits: Limits, now: number): Admission {
  const ledger = roll(previous, now)
  if (ledger.requests >= limits.requestsPerHour) {
    return { ok: false, ledger, reason: "Too many requests this hour.", retryAfterSeconds: Math.ceil((ledger.hourStart + HOUR - now) / 1000) }
  }
  if (ledger.tokens + limits.reserve > limits.tokensPerDay) {
    return { ok: false, ledger, reason: "Today's assistant budget is spent.", retryAfterSeconds: Math.ceil((ledger.dayStart + DAY - now) / 1000) }
  }
  return { ok: true, ledger: { ...ledger, requests: ledger.requests + 1 } }
}

/** Record the tokens a finished request used. */
export const spend = (previous: Ledger, tokens: number, now: number): Ledger => {
  const ledger = roll(previous, now)
  return { ...ledger, tokens: ledger.tokens + Math.max(0, Math.ceil(tokens)) }
}
