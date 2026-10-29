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

const epoch = '0123456789abcdef0123456789abcdef'
const health = () => Response.json({ capabilities: { event_resume: 1 } })
const events = (text: string) => new Response(text, { headers: { 'Content-Type': 'text/event-stream' } })

it('resumes after the last processed frame and discards a partial UTF-8 frame', async () => {
  vi.useFakeTimers()
  try {
    const fetch = vi.fn()
      .mockResolvedValueOnce(health())
      .mockResolvedValueOnce(events(`id: ${epoch}:1\ndata: first\n\nid: ${epoch}:2\ndata: partial`))
      .mockResolvedValueOnce(health())
      .mockResolvedValueOnce(events(`id: ${epoch}:2\ndata: second\n\n`))
    vi.stubGlobal('fetch', fetch)
    const frames: string[] = []
    const following = followEvents('/runs/one/events', frame => { frames.push(frame.data); return frame.data !== 'second' }, vi.fn(), new AbortController().signal)
    await vi.advanceTimersByTimeAsync(1000)
    await following
    expect(frames).toEqual(['first', 'second'])
    expect(fetch.mock.calls[1]![1].headers.has('Last-Event-ID')).toBe(false)
    expect(fetch.mock.calls[3]![1].headers.get('Last-Event-ID')).toBe(`${epoch}:1`)
    expect(fetch.mock.calls.every(call => !call[1].method || call[1].method === 'GET')).toBe(true)
  } finally { vi.useRealTimers() }
})

it('marks expired replay and polls a healthy run through its terminal outcome', async () => {
  vi.useFakeTimers()
  try {
    const fetch = vi.fn()
      .mockResolvedValueOnce(health())
      .mockResolvedValueOnce(new Response('{}', { status: 410 }))
      .mockResolvedValueOnce(Response.json({ status: 'running' }))
      .mockResolvedValueOnce(Response.json({ status: 'success', outcome: { ok: true } }))
    vi.stubGlobal('fetch', fetch)
    const gap = vi.fn(), outcome = vi.fn(), stop = vi.fn()
    const following = followEvents('/runs/one/events', vi.fn(), stop, new AbortController().signal, { onGap: gap, onOutcome: outcome })
    await vi.advanceTimersByTimeAsync(2000)
    await following
    expect(gap).toHaveBeenCalledOnce()
    expect(outcome).toHaveBeenCalledWith({ status: 'success', outcome: { ok: true } })
    expect(stop).not.toHaveBeenCalled()
    expect(fetch.mock.calls.map(call => call[0])).toEqual(['/health', '/runs/one/events', '/runs/one', '/runs/one'])
  } finally { vi.useRealTimers() }
})

it('keeps legacy numeric streams free of cursor request headers', async () => {
  vi.useFakeTimers()
  try {
    const fetch = vi.fn()
      .mockResolvedValueOnce(Response.json({ started_at: 'old' }))
      .mockResolvedValueOnce(events('id: 0\ndata: first\n\n'))
      .mockResolvedValueOnce(Response.json({ started_at: 'old' }))
      .mockResolvedValueOnce(events('id: 0\ndata: first\n\nid: 1\ndata: done\n\n'))
    vi.stubGlobal('fetch', fetch)
    const following = followEvents('/runs/one/events', frame => frame.data !== 'done', vi.fn(), new AbortController().signal)
    await vi.advanceTimersByTimeAsync(1000)
    await following
    expect(fetch.mock.calls.every(call => !call[1].headers.has('Last-Event-ID'))).toBe(true)
  } finally { vi.useRealTimers() }
})

it('resets presentation on an epoch mismatch and requests the fresh attempt without a cursor', async () => {
  vi.useFakeTimers()
  try {
    const nextEpoch = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
    const fetch = vi.fn()
      .mockResolvedValueOnce(health())
      .mockResolvedValueOnce(events(`id: ${epoch}:1\ndata: old\n\n`))
      .mockResolvedValueOnce(health())
      .mockResolvedValueOnce(new Response('{}', { status: 409 }))
      .mockResolvedValueOnce(Response.json({ status: 'running' }))
      .mockResolvedValueOnce(health())
      .mockResolvedValueOnce(events(`id: ${nextEpoch}:1\ndata: new\n\n`))
    vi.stubGlobal('fetch', fetch)
    const restart = vi.fn(), frames: string[] = []
    const following = followEvents('/runs/one/events', frame => { frames.push(frame.data); return frame.data !== 'new' }, vi.fn(), new AbortController().signal, { onRestart: restart })
    await vi.advanceTimersByTimeAsync(1000)
    await following
    expect(restart).toHaveBeenCalledOnce()
    expect(frames).toEqual(['old', 'new'])
    expect(fetch.mock.calls[3]![1].headers.get('Last-Event-ID')).toBe(`${epoch}:1`)
    expect(fetch.mock.calls[6]![1].headers.has('Last-Event-ID')).toBe(false)
  } finally { vi.useRealTimers() }
})

it('switches to canonical outcome when processing a frame fails instead of acknowledging it', async () => {
  vi.useFakeTimers()
  try {
    const fetch = vi.fn()
      .mockResolvedValueOnce(health())
      .mockResolvedValueOnce(events(`id: ${epoch}:1\ndata: broken\n\n`))
      .mockResolvedValueOnce(Response.json({ status: 'failed' }))
    vi.stubGlobal('fetch', fetch)
    const gap = vi.fn(), outcome = vi.fn()
    const following = followEvents('/runs/one/events', () => { throw new Error('bad event') }, vi.fn(), new AbortController().signal, { onGap: gap, onOutcome: outcome })
    await vi.advanceTimersByTimeAsync(1000)
    await following
    expect(gap).toHaveBeenCalledOnce()
    expect(outcome).toHaveBeenCalledWith({ status: 'failed' })
    expect(fetch).toHaveBeenCalledTimes(3)
  } finally { vi.useRealTimers() }
})
