import { useEffect, useState } from 'react'
import { reduceRunEvent, endRunStream, isNewFrame, EMPTY_RUN_STATE, type RunStreamState } from '@/lib/runStream'
import { followEvents } from '@/lib/eventStream'
import type { RunEvent } from '@/lib/types'

/** A run we've subscribed to but haven't heard from yet. */
const STARTING: RunStreamState = { ...EMPTY_RUN_STATE, running: true }

/**
 * Thin presentation wrapper: opens the SSE connection
 * (`GET /runs/{run_id}/events`) and pipes each live `RunEvent` through the pure
 * `reduceRunEvent` reducer. All the folding logic lives in `lib/runStream.ts`
 * so it can be tested without a browser.
 *
 * Pass `null` to disconnect. The server replays the run from its first event
 * on connect, so a slightly-late subscribe still sees everything; frames
 * already applied (by `id:`) are skipped after a reconnect. The server closes
 * the stream after `run_finished`, so the client closes on that event and never
 * reconnects. State is tagged with the handle it belongs to, so switching runs
 * starts fresh without resetting state inside the effect.
 */
export function useRunStream(handle: string | null): RunStreamState {
  const [tagged, setTagged] = useState<{ handle: string | null; state: RunStreamState }>({
    handle: null,
    state: EMPTY_RUN_STATE,
  })

  useEffect(() => {
    if (!handle) return
    const controller = new AbortController()
    let lastId: number | null = null
    const update = (step: (state: RunStreamState) => RunStreamState) =>
      setTagged((prev) => ({
        handle,
        state: step(prev.handle === handle ? prev.state : STARTING),
      }))

    void followEvents(`/runs/${encodeURIComponent(handle)}/events`, (frame) => {
      if (!isNewFrame(lastId, frame.id)) return true
      let event: RunEvent
      try { event = JSON.parse(frame.data) as RunEvent } catch { return true }
      const id = Number(frame.id)
      if (frame.id !== '' && Number.isInteger(id)) lastId = id
      update(state => reduceRunEvent(state, event))
      return event.type !== 'run_finished'
    }, () => update(endRunStream), controller.signal)

    return () => controller.abort()
  }, [handle])

  if (!handle) return EMPTY_RUN_STATE
  return tagged.handle === handle ? tagged.state : STARTING
}
