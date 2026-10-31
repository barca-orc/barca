/** How often a query that failed asks again, so a restarted server is picked up without a reload. */
export const RECOVER_MS = 5_000

/** What `recovering` reads of a TanStack query. */
export interface PolledQuery<T> {
  state: { status: 'pending' | 'error' | 'success'; data: T | undefined }
}

type Interval<T> = number | false | ((query: PolledQuery<T>) => number | false)

/**
 * A `refetchInterval` that keeps asking while the last fetch failed. Every query has
 * `retry: false` (an error is shown at once, not after a silent minute of retries), so
 * without this one failed fetch is permanent for the session: the server restarts, every
 * endpoint answers 200 again, and the page keeps saying it is not reachable.
 */
export function recovering(interval?: number | false): (query: PolledQuery<unknown>) => number | false
export function recovering<T>(interval: (query: PolledQuery<T>) => number | false): (query: PolledQuery<T>) => number | false
export function recovering<T>(interval: Interval<T> = false) {
  return (query: PolledQuery<T>): number | false => {
    if (query.state.status === 'error') return RECOVER_MS
    return typeof interval === 'function' ? interval(query) : interval
  }
}
