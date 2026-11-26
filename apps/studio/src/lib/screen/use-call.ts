import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react"
import { type CallAnswer } from "../actions.ts"
import { createIntentKey } from "../../../../../packages/norn-sdk/src/index.ts"
import { init, resume, update, type Cmd, type Msg, type ParkedCall, type State, type Target } from "./call.ts"

export type Call = (actionId: string, input: string, key: string) => Promise<CallAnswer>

/**
 * React as a thin view over the call core: it holds the state, runs the two commands and feeds
 * their answers back as messages. Nothing here decides what may be called.
 */
export function useCall(target: Target, run: Call, onRefetch?: () => void, parked?: ParkedCall): readonly [State, (msg: Msg) => void] {
  // A form reopened on a call that parked starts where that call waits, with its input and key.
  const [state, setState] = useState<State>(() => parked === undefined ? init(target, createIntentKey()) : resume(target, parked))
  const current = useRef(state)
  const mounted = useRef(true)
  const effects = useRef({ run, onRefetch })
  // Only callbacks from a committed render: React may replay or discard a render.
  useLayoutEffect(() => { effects.current = { run, onRefetch } })
  useEffect(() => {
    mounted.current = true
    return () => { mounted.current = false }
  }, [])
  const dispatch = useCallback((msg: Msg) => {
    if (!mounted.current) return
    const [next, cmds] = update(current.current, msg)
    current.current = next
    setState(next)
    for (const cmd of cmds) perform(cmd, effects.current, dispatch, mounted)
  }, [])
  return [state, dispatch]
}

function perform(
  cmd: Cmd,
  effects: { readonly run: Call; readonly onRefetch?: () => void },
  dispatch: (msg: Msg) => void,
  mounted: { readonly current: boolean },
) {
  if (cmd.kind === "refetch") {
    if (mounted.current) effects.onRefetch?.()
    return
  }
  effects.run(cmd.actionId, cmd.input, cmd.key).then(
    (result) => { if (mounted.current) dispatch({ kind: "answered", result }) },
    (error: unknown) => { if (mounted.current) dispatch({ kind: "threw", message: error instanceof Error ? error.message : null }) },
  )
}
