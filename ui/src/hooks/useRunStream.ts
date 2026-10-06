import { useEffect, useState } from 'react'
import { reduceRunEvent, EMPTY_RUN_STATE, type RunStreamState } from '@/lib/runStream'
import { API_BASE, apiUrl } from '@/lib/apiBase'
import type { RunEvent } from '@/lib/types'

/** A run we've subscribed to but haven't heard from yet. */
const STARTING: RunStreamState = { ...EMPTY_RUN_STATE, running: true }

/**
 * Thin presentation wrapper: opens the SSE connection and pipes each live
 * `RunEvent` through the pure `reduceRunEvent` reducer. All the folding logic
 * lives in `lib/runStream.ts` so it can be tested without a browser.
 *
 * Pass `null` to disconnect. The server replays the backlog on connect, so a
 * slightly-late subscribe still sees everything from the start. State is
 * tagged with the handle it belongs to, so switching runs starts fresh without
 * resetting state inside the effect.
 */
export function useRunStream(handle: string | null): RunStreamState {
  const [tagged, setTagged] = useState<{ handle: string | null; state: RunStreamState }>({
    handle: null,
    state: EMPTY_RUN_STATE,
  })

  useEffect(() => {
    if (!handle) return
    const es = new EventSource(apiUrl(API_BASE, `/events/${encodeURIComponent(handle)}`))

    es.onmessage = (e) => {
      let event: RunEvent
      try {
        event = JSON.parse(e.data) as RunEvent
      } catch {
        return
      }
      setTagged((prev) => ({
        handle,
        state: reduceRunEvent(prev.handle === handle ? prev.state : STARTING, event),
      }))
      if (event.type === 'run_finished') es.close()
    }

    es.onerror = () => {
      // The browser auto-reconnects; if the run is already done the server has
      // dropped the channel and the stream simply ends.
    }

    return () => es.close()
  }, [handle])

  if (!handle) return EMPTY_RUN_STATE
  return tagged.handle === handle ? tagged.state : STARTING
}
