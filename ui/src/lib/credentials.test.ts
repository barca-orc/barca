import { afterEach, describe, expect, it, vi } from 'vitest'

async function fixture(prefix = '/project/ui/') {
  vi.resetModules()
  const data = new Map<string, string>()
  vi.stubGlobal('window', { location: { origin: 'https://barca.example', pathname: prefix }, sessionStorage: { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value), removeItem: (key: string) => data.delete(key) } })
  return { data, api: await import('./credentials') }
}
afterEach(() => vi.unstubAllGlobals())

describe('tab credential lifecycle', () => {
  it('scopes persisted credentials to the origin and API prefix and clears on disconnect', async () => {
    const { data, api } = await fixture()
    api.connect('private-test-token')
    expect([...data.keys()]).toEqual(['barca:connection:v1:https://barca.example/project'])
    vi.resetModules()
    expect((await import('./credentials')).connectionToken()).toBe('private-test-token')
    api.connect(null)
    expect(data.size).toBe(0)
    expect(api.connectionToken()).toBeNull()
  })
  it('covers ordinary and streaming requests, omits cookies in token mode and rejects redirects', async () => {
    const { api } = await fixture()
    const fetch = vi.fn().mockResolvedValue(new Response('{}'))
    vi.stubGlobal('fetch', fetch)
    api.setAuthMode('token'); api.connect('private-test-token')
    for (const path of ['/health', '/runs/one/events', '/runs/one']) await api.serverFetch(path)
    for (const [path, init] of fetch.mock.calls) {
      expect(path).toMatch(/^\/project\//)
      expect(init.headers.get('Authorization')).toBe('Bearer private-test-token')
      expect(init.credentials).toBe('omit')
      expect(init.redirect).toBe('error')
    }
    await expect(api.serverFetch('/../outside')).rejects.toThrow('project path')
    await expect(api.serverFetch('/%2f..%2foutside')).rejects.toThrow('project path')
    await expect(api.serverFetch('//other.example')).rejects.toThrow('Invalid server request')
    await expect(api.serverFetch('/health', {}, 'bad\nprivate-token')).rejects.toThrow('Enter a valid')
    expect(fetch).toHaveBeenCalledTimes(3)
  })
  it('clears a revoked connection before notifying listeners but keeps a newer credential', async () => {
    const { api, data } = await fixture()
    api.connect('old-token')
    const seen: (string | null)[] = []
    api.subscribeConnection(() => seen.push(api.connectionToken()))
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 401 })))
    await api.serverFetch('/health')
    expect(seen).toEqual([null]); expect(data.size).toBe(0)
    api.connect('new-token')
    await api.serverFetch('/health', {}, 'old-token')
    expect(api.connectionToken()).toBe('new-token')
  })
  it('clears obsolete bearer credentials when the server switches auth off', async () => {
    const { api, data } = await fixture()
    api.connect('previous-token'); api.setAuthMode('off')
    expect(api.connectionToken()).toBeNull(); expect(data.size).toBe(0)
  })
  it('keeps a memory connection if tab storage is unavailable', async () => {
    const { api } = await fixture()
    vi.stubGlobal('window', { sessionStorage: { setItem: () => { throw new Error('blocked') }, removeItem: () => { throw new Error('blocked') } } })
    api.connect('private-token'); expect(api.connectionToken()).toBe('private-token')
    api.connect(null); expect(api.connectionToken()).toBeNull()
  })
})

it('redacts credentials reflected in API errors and bootstrap errors', async () => {
  const { api: credentials } = await fixture()
  credentials.setAuthMode('token'); credentials.connect('private-token')
  const { api } = await import('./api')
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: 'rejected private-token', remediation: 'remove private-token' }), { status: 401 })))
  await expect(api.health()).rejects.toMatchObject({ message: 'rejected <redacted>', remediation: 'remove <redacted>' })
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: 'rejected private-bootstrap' }), { status: 401 })))
  await expect(api.bootstrap('private-bootstrap')).rejects.toMatchObject({ message: 'rejected <redacted>' })
})
