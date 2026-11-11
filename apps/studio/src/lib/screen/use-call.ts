import { useCallback, useLayoutEffect, useRef, useState } from "react"
import { newIdempotencyKey, type CallResult } from "../actions.ts"
import { init, update, type Cmd, type Msg, type State, type Target } from "./call.ts"

export type Call = (actionId: string, input: string, key: string) => Promise<CallResult>

/**
 * React as a thin view over the call core: it holds the state, runs the two commands and feeds
 * their answers back as messages. Nothing here decides what may be called.
 */
export function useCall(target: Target, run: Call, onRefetch?: () => void): readonly [State, (msg: Msg) => void] {
  const [state, setState] = useState<State>(() => init(target, newIdempotencyKey()))
  const current = useRef(state)
  const effects = useRef({ run, onRefetch })
  // Only callbacks from a committed render: React may replay or discard a render.
  useLayoutEffect(() => { effects.current = { run, onRefetch } })
  const dispatch = useCallback((msg: Msg) => {
    const [next, cmds] = update(current.current, msg)
    current.current = next
    setState(next)
    for (const cmd of cmds) perform(cmd, effects.current, dispatch)
  }, [])
  return [state, dispatch]
}

function perform(cmd: Cmd, effects: { readonly run: Call; readonly onRefetch?: () => void }, dispatch: (msg: Msg) => void) {
  if (cmd.kind === "refetch") { effects.onRefetch?.(); return }
  effects.run(cmd.actionId, cmd.input, cmd.key).then(
    (result) => dispatch({ kind: "answered", result }),
    (error: unknown) => dispatch({ kind: "threw", message: error instanceof Error ? error.message : null }),
  )
}
