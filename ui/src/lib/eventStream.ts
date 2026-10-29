/** Fetch SSE so the same scoped bearer credentials cover streams and ordinary requests. */
import { serverFetch } from './credentials'

export interface Frame { id: string; data: string }
const MAX_FRAME = 16 * 1024 * 1024

export async function consumeEvents(body: ReadableStream<Uint8Array>, onFrame: (frame: Frame) => boolean, signal: AbortSignal): Promise<boolean> {
  const reader = body.getReader()
  const decoder = new TextDecoder()
  let pending = '', id = '', data: string[] = [], size = 0
  const cancel = () => { void reader.cancel().catch(() => {}) }
  signal.addEventListener('abort', cancel, { once: true })
  try {
    while (!signal.aborted) {
      const chunk = await reader.read()
      if (chunk.done || signal.aborted) return false
      pending += decoder.decode(chunk.value, { stream: true })
      if (pending.length + size > MAX_FRAME) throw new Error('Event stream frame exceeds its size limit.')
      let end: number
      while ((end = pending.indexOf('\n')) >= 0) {
        const line = pending.slice(0, end).replace(/\r$/, '')
        pending = pending.slice(end + 1)
        if (!line) {
          if (data.length && !onFrame({ id, data: data.join('\n') })) return true
          data = []; size = 0
        } else if (line.startsWith('data:')) {
          const value = line.slice(5).replace(/^ /, '')
          data.push(value); size += value.length
        } else if (line.startsWith('id:')) {
          const value = line.slice(3).replace(/^ /, '')
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
function pause(signal: AbortSignal): Promise<void> {
  return new Promise(resolve => {
    if (signal.aborted) { resolve(); return }
    const done = () => { clearTimeout(timer); signal.removeEventListener('abort', done); resolve() }
    const timer = setTimeout(done, 1000)
    signal.addEventListener('abort', done, { once: true })
  })
}
/** Current servers replay numeric frames; the presentation reducer deduplicates them. */
export async function followEvents(path: string, onFrame: (frame: Frame) => boolean, onStop: () => void, signal: AbortSignal): Promise<void> {
  while (!signal.aborted) {
    try {
      const response = await serverFetch(path, { signal, headers: { Accept: 'text/event-stream' } })
      if (!response.ok || !response.body || !response.headers.get('content-type')?.startsWith('text/event-stream')) {
        onStop(); return
      }
      if (await consumeEvents(response.body, onFrame, signal)) return
    } catch {
      if (signal.aborted) return
    }
    await pause(signal)
  }
}
