/**
 * A Widget UX page's action calls in a decided workspace (#1762 milestone C): what
 * `<NornAppRenderer>` calls an action binding through. A live workspace sends the call to its own
 * scope (`POST /v1/actions/{actionId}`, `ActionRequest { input, idempotencyKey }`) and classifies
 * the cell's answer as the page machine's outcome; the recording serves no cell, so nothing is sent.
 * The cell decides every call; nothing here judges it.
 */
import { parseExactJson } from "../../../../packages/norn-sdk/src/index.ts"
import { classifyText } from "../../../../packages/norn-react/src/machine/outcome.ts"
import type { WidgetCalls } from "../../../../packages/norn-react/src/reads.ts"
import { defaultFetch, scopedHostFetch } from "./definitions.ts"
import type { Fetch } from "./live-transport.ts"
import type { DecidedWorkspace } from "./workspace.tsx"

const NOT_SERVED: WidgetCalls = Object.freeze({ call: async () => ({ outcome: "not_served", payload: {} }) as const })

export function widgetCalls(workspace: DecidedWorkspace, fetcher: Fetch = defaultFetch): WidgetCalls {
  if (workspace.kind === "recorded") return NOT_SERVED
  const call = scopedHostFetch(fetcher, "/api/host", async () => workspace.scope)
  return {
    async call(actionId, input, { signal, idempotencyKey }) {
      let response: Response
      try {
        response = await call(`/v1/actions/${encodeURIComponent(actionId)}`, {
          method: "POST",
          headers: { "content-type": "application/json", accept: "application/json" },
          body: JSON.stringify({ input, idempotencyKey }),
          signal,
        })
      } catch (error) {
        if (signal.aborted) throw error
        return { outcome: "network_failure", payload: {} }
      }
      return classifyText(response.status, await response.text(), parseExactJson)
    },
  }
}
