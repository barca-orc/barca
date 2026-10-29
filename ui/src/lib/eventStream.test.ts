import { afterEach, expect, it, vi } from 'vitest'
import { consumeEvents, followEvents } from './eventStream'
import { connect, setAuthMode } from './credentials'

const encoder = new TextEncoder()
afterEach(() => { connect(null); vi.unstubAllGlobals() })
it('decodes split UTF-8, CRLF, comments and multiline frames, and cancels after a terminal frame', async () => {
  const bytes = encoder.encode(': ping\r\nid: 1\r\ndata: café\r\ndata: second\r\n\r\nid: 2\ndata: done\n\n')
  const cancel = vi.fn()
  const body = new ReadableStream<Uint8Array>({ start(controller) { for (const byte of bytes) controller.enqueue(new Uint8Array([byte])) }, cancel })
  const frames: unknown[] = []
  const done = await consumeEvents(body, frame => { frames.push(frame); return frame.data !== 'done' }, new AbortController().signal)
  expect(done).toBe(true)
  expect(frames).toEqual([{ id: '1', data: 'café\nsecond' }, { id: '2', data: 'done' }])
  expect(cancel).toHaveBeenCalledOnce()
})
it('aborts an idle reader', async () => {
  const cancel = vi.fn()
  const body = new ReadableStream<Uint8Array>({ cancel })
  const controller = new AbortController()
  const reading = consumeEvents(body, () => true, controller.signal)
  controller.abort()
  expect(await reading).toBe(false)
  expect(cancel).toHaveBeenCalledOnce()
})
it('sends bearer headers and treats auth failure as terminal without retrying', async () => {
  setAuthMode('token'); connect('test-stream-token')
  const fetch = vi.fn().mockResolvedValue(new Response('{}', { status: 401 }))
  vi.stubGlobal('fetch', fetch)
  const stopped = vi.fn()
  await followEvents('/runs/one/events', () => true, stopped, new AbortController().signal)
  expect(fetch).toHaveBeenCalledOnce()
  expect(fetch.mock.calls[0]![1].headers.get('Authorization')).toBe('Bearer test-stream-token')
  expect(stopped).toHaveBeenCalledOnce()
})

it('clears a pending reconnect timer when the view disconnects', async () => {
  vi.useFakeTimers()
  try {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')))
    const controller = new AbortController()
    const following = followEvents('/runs/one/events', () => true, vi.fn(), controller.signal)
    await vi.advanceTimersByTimeAsync(0)
    expect(vi.getTimerCount()).toBe(1)
    controller.abort()
    await following
    expect(vi.getTimerCount()).toBe(0)
  } finally { vi.useRealTimers() }
})
