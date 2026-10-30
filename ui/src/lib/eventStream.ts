/** Fetch SSE so the same scoped bearer credentials cover streams and ordinary requests. */
import { serverFetch } from './credentials'

export interface Frame { id: string; data: string }
export class StreamProtocolError extends Error {}
const MAX_FRAME = 16 * 1024 * 1024 + 1024

export async function consumeEvents(body: ReadableStream<Uint8Array>, onFrame: (frame: Frame) => boolean, signal: AbortSignal): Promise<boolean> {
  const reader = body.getReader()
  const decoder = new TextDecoder('utf-8', { fatal: true })
  let pending = '', id = '', data: string[] = [], size = 0, cr = false
  const cancel = () => { void reader.cancel().catch(() => {}) }
  signal.addEventListener('abort', cancel, { once: true })
  try {
    while (!signal.aborted) {
      const chunk = await reader.read()
      if (chunk.done || signal.aborted) return false
      try { pending += decoder.decode(chunk.value, { stream: true }) }
      catch { throw new StreamProtocolError('Event stream is not UTF-8.') }
      if (pending.length && cr) { if (pending.startsWith('\n')) pending = pending.slice(1); cr = false }
      if (pending.length + size > MAX_FRAME) throw new StreamProtocolError('Event stream frame exceeds its size limit.')
      let end: number
      while ((end = pending.search(/[\r\n]/)) >= 0) {
        const line = pending.slice(0, end)
        const delimiter = pending[end]
        pending = pending.slice(end + 1)
        if (delimiter === '\r') { if (pending.startsWith('\n')) pending = pending.slice(1); else if (!pending.length) cr = true }
        if (!line) {
          if (data.length && !onFrame({ id, data: data.join('\n') })) return true
          data = []; size = 0
        } else if (line.startsWith('data:')) {
          const value = line.slice(5).replace(/^ /, '')
          data.push(value); size += value.length
        } else if (line.startsWith('id:')) {
          const value = line.slice(3).replace(/^ /, '')
          if (value.length > 128) throw new StreamProtocolError('Event cursor exceeds its size limit.')
          if (!value.includes('\0')) id = value
        }
      }
    }
    return false
  } finally {
    signal.removeEventListener('abort', cancel)
    await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
}
function pause(signal: AbortSignal, ms = 1000): Promise<void> {
  return new Promise(resolve => {
    if (signal.aborted) { resolve(); return }
    const done = () => { clearTimeout(timer); signal.removeEventListener('abort', done); resolve() }
    const timer = setTimeout(done, ms)
    signal.addEventListener('abort', done, { once: true })
  })
}
export interface CanonicalRun { status: string; outcome?: Record<string, unknown> }
interface FollowOptions {
  onGap?: () => void
  onRestart?: () => void
  onOutcome?: (run: CanonicalRun) => void
}
function cursorParts(cursor: string): [string, bigint] | null {
  const match = /^([0-9a-f]{32}):([1-9][0-9]*)$/.exec(cursor)
  if (!match || cursor.length > 128) return null
  const sequence = BigInt(match[2]!)
  return sequence <= 18446744073709551615n ? [match[1]!, sequence] : null
}
/** Reconnect one run, acknowledging only successfully processed frames; never POST work. */
export async function followEvents(path: string, onFrame: (frame: Frame) => boolean, onStop: (reason?: string) => void, signal: AbortSignal, options: FollowOptions = {}): Promise<void> {
  let cursor = '', capable = false, instance = '', gap = false
  const runPath = path.replace(/\/events$/, '')
  const canonical = async (): Promise<CanonicalRun | null> => {
    const response = await serverFetch(runPath, { signal })
    if (response.status === 401 || response.status === 403 || response.status === 404) { onStop('The run is unavailable. Open its recorded details.'); return null }
    if (!response.ok) throw new Error('Run view unavailable')
    const run = await response.json() as CanonicalRun
    if (typeof run.status !== 'string') throw new StreamProtocolError('Invalid run view')
    return run
  }
  const ended = (run: CanonicalRun) => {
    if (run.status === 'pending' || run.status === 'running') return false
    if (options.onOutcome) options.onOutcome(run)
    else onStop()
    return true
  }
  while (!signal.aborted) {
    try {
      if (gap) {
        const run = await canonical()
        if (!run || ended(run)) return
        await pause(signal, 2000)
        continue
      }
      // Probe each reconnect so an older server retains its original numeric/count behavior.
      try {
        const health = await serverFetch('/health', { signal })
        if (health.status === 401 || health.status === 403) { onStop(); return }
        if (health.ok) {
          const body = await health.json() as { started_at?: string; capabilities?: { event_resume?: number } }
          capable = body.capabilities?.event_resume === 1
          if (!capable && instance && body.started_at && instance !== body.started_at) { cursor = ''; options.onRestart?.() }
          if (capable && !cursorParts(cursor)) cursor = ''
          instance = body.started_at ?? ''
        }
      } catch { if (signal.aborted) return }
      const headers = new Headers({ Accept: 'text/event-stream' })
      if (capable && cursor) headers.set('Last-Event-ID', cursor)
      const response = await serverFetch(path, { signal, headers })
      if (response.status === 401 || response.status === 403) { onStop(); return }
      if (capable && response.status === 410) { gap = true; options.onGap?.(); continue }
      if (capable && response.status === 409) {
        if (!cursor) throw new StreamProtocolError('The server rejected a fresh attempt stream')
        const run = await canonical()
        if (!run) return
        cursor = ''; options.onRestart?.()
        if (ended(run)) return
        continue
      }
      if (response.status === 404 || response.status === 204) {
        const run = await canonical()
        if (!run || ended(run)) return
        await pause(signal)
        continue
      }
      if (response.status >= 500) { await pause(signal); continue }
      if (!response.ok || !response.body || !response.headers.get('content-type')?.toLowerCase().startsWith('text/event-stream')) { onStop('Unable to follow the event stream. Open the recorded run.'); return }
      const complete = await consumeEvents(response.body, frame => {
        if (capable) {
          const next = cursorParts(frame.id), previous = cursorParts(cursor)
          if (!next) throw new StreamProtocolError('Invalid attempt cursor')
          if (previous && next[0] === previous[0] && next[1] <= previous[1]) return true
          if ((previous && (next[0] !== previous[0] || next[1] !== previous[1] + 1n)) || (!previous && next[1] !== 1n)) throw new StreamProtocolError('Event replay has a gap')
        }
        let keepGoing: boolean
        try { keepGoing = onFrame(frame) } catch { throw new StreamProtocolError('Unable to process an event') }
        cursor = frame.id
        return keepGoing
      }, signal)
      if (complete) return
    } catch (error) {
      if (signal.aborted) return
      if (error instanceof StreamProtocolError) { if (!gap) options.onGap?.(); gap = true }
    }
    await pause(signal)
  }
}
