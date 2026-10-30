/**
 * Server data as a view sees it: one of three states, never a bag of flags.
 *
 * TanStack Query keeps fetching, caching and polling; `remote` folds a query's flags into
 * this union at the edge, so a view matches `kind` exhaustively and the kit page can render
 * each state from a fixture.
 */

export type Remote<T> =
  | { kind: 'loading' }
  /** Nothing loaded, and the last fetch failed. */
  | { kind: 'failed'; error: string }
  /** Loaded; `refreshError` when a later refetch failed and `data` is what was last loaded. */
  | { kind: 'ready'; data: T; refreshError: string | null }

/** The parts of a TanStack Query result `remote` reads. */
export interface QueryLike<T> {
  data: T | undefined
  error: Error | null
  isPlaceholderData?: boolean
}

export function remote<T>(query: QueryLike<T>): Remote<T> {
  // Placeholder data is a stand-in while nothing loaded, not the server's answer.
  const data = query.isPlaceholderData ? undefined : query.data
  if (data !== undefined) return { kind: 'ready', data, refreshError: query.error?.message ?? null }
  if (query.error) return { kind: 'failed', error: query.error.message }
  return { kind: 'loading' }
}

export const loading: Remote<never> = { kind: 'loading' }

export function failed(error: string): Remote<never> {
  return { kind: 'failed', error }
}

export function ready<T>(data: T, refreshError: string | null = null): Remote<T> {
  return { kind: 'ready', data, refreshError }
}

/** The data, when there is some. */
export function dataOf<T>(value: Remote<T>): T | undefined {
  return value.kind === 'ready' ? value.data : undefined
}
