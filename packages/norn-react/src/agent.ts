/**
 * The renderer's agent capabilities (L6): `agent:redaction` and `agent:confirm`, emitted as UI SDK's
 * page-agent attributes so `createAgent` redacts what the server masked, never sees what it
 * withheld, and leaves a consequential press to a person.
 *
 * - A masked value (PRIVACY-1 `masked[property]`, any mode but `refuse`) is `data-agent-secret`:
 *   the agent reads `"[redacted]"`.
 * - A withheld value (mode `refuse`, `withheldProperties`, or a row withheld whole) is
 *   `data-agent-hidden`: the agent never sees the element.
 * - A control that starts an action the server says is irreversible, destructive, monetary
 *   (`uiEffects`) or ledgered (the admitted action), or a placement whose confirmation is not
 *   `safe`, is `data-agent-confirm` with what it does. An approval requirement is the server's
 *   answer to the call (202), which no catalog states ahead of it.
 *
 * Every mark reflects a server answer; none decides anything. Server-side redaction before
 * serialization stays the one secrecy authority: these attributes are agent and DOM defence in
 * depth only.
 */
import type { Machine } from "./machine/machine.ts"
import { text, type WidgetUx } from "./ux.ts"

/** How the server projected one marked property for this caller (`NornMaskedProperty`). */
export interface MaskedProperty {
  readonly mode: "pseudonym" | "redacted" | "null" | "last_n" | "refuse"
  readonly markings: readonly string[]
  readonly revealable: boolean
}

/**
 * What the server says a caller must handle: its action catalog's `uiEffects`
 * (`NornActionUiEffects`) and, from the admitted action definition, whether it is ledgered.
 */
export interface ActionEffects {
  readonly irreversible: boolean
  readonly destructive: boolean
  readonly money: readonly string[]
  readonly ledgered?: boolean
}

/** UI SDK's page-agent attributes, spread onto the element they mark. */
export interface AgentAttributes {
  readonly "data-agent-secret"?: string
  readonly "data-agent-hidden"?: string
  readonly "data-agent-confirm"?: string
}

const NONE: AgentAttributes = {}
const SECRET: AgentAttributes = { "data-agent-secret": "" }
const HIDDEN: AgentAttributes = { "data-agent-hidden": "" }

/** The mark of one value: hidden when withheld, secret when masked, none otherwise. */
export function valueMark(masked: MaskedProperty | undefined, withheld: boolean): AgentAttributes {
  if (withheld || masked?.mode === "refuse") return HIDDEN
  return masked === undefined ? NONE : SECRET
}

/** The mark of a row the server withheld whole (`withheld: true`). */
export const rowMark = (withheld: boolean): AgentAttributes => (withheld ? HIDDEN : NONE)

/** Why a press needs a person, from the server's own effects, or null when it does not. */
export function effectsReason(effects: ActionEffects | undefined): string | null {
  if (effects === undefined) return null
  const said = [
    ...(effects.irreversible ? ["cannot be undone"] : []),
    ...(effects.destructive ? ["may destroy data"] : []),
    ...(effects.money.length > 0 ? [`moves money (${effects.money.join(", ")})`] : []),
    ...(effects.ledgered === true ? ["is recorded in the ledger"] : []),
  ]
  return said.length === 0 ? null : said.join("; ")
}

export const confirmMark = (reason: string | null): AgentAttributes => (reason === null ? NONE : { "data-agent-confirm": reason })

/**
 * Why pressing node `nodeId` of a page needs a person: a placement whose confirmation is not
 * `safe`, or a transition its events fire that starts an action the server says is consequential.
 */
export function confirmReason(ux: WidgetUx, machine: Machine | undefined, nodeId: string, placementId: string | null, effectsOf: (actionId: string) => ActionEffects | undefined, locale: string): string | null {
  const reasons: string[] = []
  const placement = placementId === null ? undefined : ux.actionPlacements.find((candidate) => candidate.placementId === placementId)
  if (placement !== undefined && placement.confirmation.severity !== "safe") {
    reasons.push(`${text(placement.label, locale)}: ${placement.confirmation.severity}`)
  }
  const events = new Set((machine?.events ?? []).filter((event) => event.source.kind === "user" && event.source.nodeId === nodeId).map((event) => event.eventId))
  for (const transition of machine?.transitions ?? []) {
    if (!events.has(transition.on)) continue
    for (const effect of transition.effects ?? []) {
      if (effect.kind !== "start" || effect.call.kind !== "action") continue
      const bindingId = effect.call.bindingId
      const binding = ux.actionBindings.find((candidate) => candidate.bindingId === bindingId)
      const reason = binding === undefined ? null : effectsReason(effectsOf(binding.action.id))
      if (binding !== undefined && reason !== null) reasons.push(`${binding.action.id}: ${reason}`)
    }
  }
  return reasons.length === 0 ? null : [...new Set(reasons)].join(". ")
}
