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
 * Pass `null` to disconnect. The server replays its retained events on connect. Reconnects acknowledge
 * successfully applied frames; an expired replay switches to the canonical
 * outcome and visibly marks the incomplete live transcript. The server closes
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
      const event = JSON.parse(frame.data) as RunEvent
      const id = Number(frame.id)
      if (frame.id !== '' && Number.isInteger(id)) lastId = id
      update(state => reduceRunEvent(state, event))
      return event.type !== 'run_finished'
    }, reason => update(state => ({ ...endRunStream(state), ...(reason ? { notice: reason } : {}) })), controller.signal, {
      onGap: () => update(state => ({ ...state, notice: 'Event replay is incomplete. Following this run’s outcome; recorded logs remain available.' })),
      onRestart: () => { lastId = null; update(() => ({ ...STARTING, notice: 'This run has a new attempt. Following its new stream.' })) },
      onOutcome: body => update(state => reduceRunEvent(state, { type: 'run_finished', run_id: handle, ok: body.status === 'success', outcome: body.outcome })),
    })

    return () => controller.abort()
  }, [handle])

  if (!handle) return EMPTY_RUN_STATE
  return tagged.handle === handle ? tagged.state : STARTING
}
