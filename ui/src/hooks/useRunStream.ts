import { useEffect, useState } from 'react'
import { reduceRunEvent, endRunStream, isNewFrame, EMPTY_RUN_STATE, type RunStreamState } from '@/lib/runStream'
import { API_BASE, apiUrl } from '@/lib/apiBase'
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
    const es = new EventSource(apiUrl(API_BASE, `/runs/${encodeURIComponent(handle)}/events`))
    let lastId: number | null = null
    let finished = false
    const update = (step: (state: RunStreamState) => RunStreamState) =>
      setTagged((prev) => ({
        handle,
        state: step(prev.handle === handle ? prev.state : STARTING),
      }))

    es.onmessage = (e) => {
      if (!isNewFrame(lastId, e.lastEventId)) return
      let event: RunEvent
      try {
        event = JSON.parse(e.data) as RunEvent
      } catch {
        return
      }
      const id = Number(e.lastEventId)
      if (e.lastEventId !== '' && Number.isInteger(id)) lastId = id
      update((state) => reduceRunEvent(state, event))
      if (event.type === 'run_finished') {
        finished = true
        es.close()
      }
    }

    es.onerror = () => {
      if (finished) {
        es.close()
        return
      }
      // A transient drop: the browser reconnects and the replay is deduplicated
      // above. A refused stream (404: the run is no longer live here) closes the
      // EventSource for good; stop showing the run as running.
      if (es.readyState === EventSource.CLOSED) update(endRunStream)
    }

    return () => es.close()
  }, [handle])

  if (!handle) return EMPTY_RUN_STATE
  return tagged.handle === handle ? tagged.state : STARTING
}
