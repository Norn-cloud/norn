import process from "../../../registry/core/artifacts/core.Process.interface.json"
import units from "../../../registry/core/artifacts/core.units.json"
import purchase from "../../../fixtures/stress/purchase.json"
import type { Envelope } from "./proposal.ts"

/**
 * The synthetic purchasing world the agent may propose changes to: the
 * definitions the stress corpus is built on (fixtures/stress/purchase.json),
 * checked on top of the installed core layer it depends on.
 */
export const WORLD = purchase as unknown as readonly Envelope[]

/** The checker context: the installed base layer and no registry value types. */
export const CONTEXT = JSON.stringify({ base: [process, units], registry: [] })
