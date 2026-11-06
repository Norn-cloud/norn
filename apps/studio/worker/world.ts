import process from "../../../registry/core/artifacts/core.Process.interface.json"
import units from "../../../registry/core/artifacts/core.units.json"
import jewelry from "../../../crates/norn-checker/tests/fixtures/jewelry_demo.json"
import type { Envelope } from "./proposal.ts"

/**
 * The synthetic jewelry world the agent may propose changes to, the one the
 * Jewelry workspace and the org_demo cell show: melts of gold scrap and the
 * law that fine mass is conserved (crates/norn-checker/tests/fixtures/jewelry_demo.json),
 * checked on top of the installed core layer it depends on.
 */
export const WORLD = jewelry as unknown as readonly Envelope[]

/** The checker context: the installed base layer and no registry value types. */
export const CONTEXT = JSON.stringify({ base: [process, units], registry: [] })
