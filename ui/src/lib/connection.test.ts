import { describe, expect, it } from 'vitest'
import { connection, connectionLabel, connectionStatus } from './connection'
import type { Health } from './types'

const health = { version: '0.18.0' } as Health

describe('connection', () => {
  it('is connecting until the first health response, not offline', () => {
    const c = connection(undefined, false)
    expect(c).toEqual({ kind: 'connecting' })
    expect(connectionLabel(c)).toBe('connecting…')
    expect(connectionStatus(c)).toBe('queued')
  })

  it('is online with the server version once health arrives', () => {
    const c = connection(health, false)
    expect(connectionLabel(c)).toBe('barca serve · v0.18.0')
    expect(connectionStatus(c)).toBe('success')
  })

  it('is offline when the request failed, with a page-specific label', () => {
    const c = connection(undefined, true)
    expect(connectionLabel(c)).toBe('offline')
    expect(connectionLabel(c, 'offline · mock data')).toBe('offline · mock data')
  })

  it('is offline when a poll fails even if an earlier response is cached', () => {
    expect(connection(health, true)).toEqual({ kind: 'offline' })
  })
})
