import { useLayoutEffect, useRef, useState } from 'react'
import type { Dispatch, Update } from '@/lib/program'

/**
 * Run a program (`lib/program.ts`) in React: the current model, and a stable `dispatch`.
 *
 * `dispatch(msg)` applies `update` at once, re-renders with the next model and hands each
 * effect to `run`, which does the I/O and dispatches the reply. Effects run exactly once per
 * message, outside render, so `update` stays pure (React may call render twice; it never sees
 * an effect). `update` and `run` are read at dispatch time, from the last render, so they
 * may close over props.
 */
export function useProgram<Model, Msg, Effect>(
  init: Model | (() => Model),
  update: Update<Model, Msg, Effect>,
  run: (effect: Effect, dispatch: Dispatch<Msg>) => void,
): [Model, Dispatch<Msg>] {
  const [model, setModel] = useState(init)
  const current = useRef(model)
  const latest = useRef({ update, run })
  useLayoutEffect(() => {
    latest.current = { update, run }
  })

  // Made once: a stable identity, safe in effect dependencies and as a prop.
  const [dispatch] = useState(() => {
    const dispatch: Dispatch<Msg> = (msg) => {
      const [next, effects] = latest.current.update(current.current, msg)
      current.current = next
      setModel(next)
      for (const effect of effects) latest.current.run(effect, dispatch)
    }
    return dispatch
  })

  return [model, dispatch]
}
