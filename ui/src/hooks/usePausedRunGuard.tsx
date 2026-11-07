import { useCallback, useState } from 'react'
import { ConfirmDialog } from '@/components'
import { shortName } from '@/lib/graph'
import { usePausedSet } from './usePauses'

/**
 * The panel's Run button for a paused node asks first: scheduled runs skip it, and a run by hand
 * is `--allow-paused`. Returns the click handler and the dialog to render.
 */
export function usePausedRunGuard(id: string, fire: (allowPaused?: boolean) => void) {
  const paused = usePausedSet().has(id)
  const [asking, setAsking] = useState(false)
  const onRun = useCallback(() => (paused ? setAsking(true) : fire()), [paused, fire])
  const dialog = asking ? (
    <ConfirmDialog
      title={`Run ${shortName(id)} while it is paused?`}
      body="It is paused: scheduled runs skip it until it is resumed. Running it by hand computes it now (barca get --allow-paused); it stays paused."
      action="Run anyway"
      onConfirm={() => { setAsking(false); fire(true) }}
      onCancel={() => setAsking(false)}
    />
  ) : null
  return { onRun, dialog, paused }
}

