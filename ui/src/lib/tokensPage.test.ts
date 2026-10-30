import { describe, it, expect } from 'vitest'
import { replay } from './program'
import { INITIAL_TOKENS, parseExpiry, updateTokens, type TokensMsg } from './tokensPage'
import type { IssuedToken } from './types'

const NOW = Date.parse('2026-10-29T12:00:00Z')
const token: IssuedToken = { token: 'brc_secret', id: 't1', name: 'ci', created_at: '2026-10-29T12:00:00Z', expires_at: null }
const run = (...msgs: TokensMsg[]) => replay(updateTokens, INITIAL_TOKENS, msgs)

describe('tokens program', () => {
  it('creates a token, shows its secret once and refreshes the list', () => {
    const { model, effects } = run(
      { type: 'name_changed', value: 'ci' },
      { type: 'create_submitted', now: NOW },
      { type: 'created', token },
    )
    expect(effects).toEqual([{ type: 'create', name: 'ci', expiresAt: null }, { type: 'refresh_list' }])
    expect(model.issued).toEqual({ token, copied: false })
    expect(model).toMatchObject({ name: '', expiry: '', request: { kind: 'idle' }, error: null })
  })

  it('refuses an expiry in the past without a request', () => {
    const { model, effects } = run(
      { type: 'expiry_changed', value: '2020-01-01T00:00' },
      { type: 'create_submitted', now: NOW },
    )
    expect(effects).toEqual([])
    expect(model.error).toBe('Choose a future expiry.')
    expect(model.request).toEqual({ kind: 'idle' })
  })

  it('creates nothing while a secret is on screen or a request is in flight', () => {
    expect(run({ type: 'created', token }, { type: 'create_submitted', now: NOW }).effects).toEqual([{ type: 'refresh_list' }])
    const inFlight = run({ type: 'create_submitted', now: NOW }, { type: 'create_submitted', now: NOW }, { type: 'revoke_clicked', id: 't1' })
    expect(inFlight.effects).toHaveLength(1)
  })

  it('names the request that failed', () => {
    const create = run({ type: 'create_submitted', now: NOW }, { type: 'request_failed', error: null })
    expect(create.model.error).toBe('Unable to create a token.')
    const revoke = run({ type: 'revoke_clicked', id: 't1' }, { type: 'request_failed', error: null })
    expect(revoke.model.error).toBe('Unable to revoke the token.')
    expect(run({ type: 'revoke_clicked', id: 't1' }, { type: 'request_failed', error: '403 forbidden' }).model.error).toBe('403 forbidden')
  })

  it('copies the secret it shows, and says when copying failed', () => {
    const copied = run({ type: 'created', token }, { type: 'copy_clicked' }, { type: 'copied' })
    expect(copied.effects).toContainEqual({ type: 'copy', text: 'brc_secret' })
    expect(copied.model.issued?.copied).toBe(true)
    expect(run({ type: 'copy_clicked' }).effects).toEqual([])
    expect(run({ type: 'created', token }, { type: 'copy_failed' }).model.error).toMatch(/Copy failed/)
  })

  it('revokes and refreshes', () => {
    const { model, effects } = run({ type: 'revoke_clicked', id: 't1' }, { type: 'revoked' })
    expect(effects).toEqual([{ type: 'revoke', id: 't1' }, { type: 'refresh_list' }])
    expect(model.request).toEqual({ kind: 'idle' })
  })
})

describe('parseExpiry', () => {
  it('reads no expiry, a future one, and refuses the rest', () => {
    expect(parseExpiry('', NOW)).toEqual({ ok: true, iso: null })
    expect(parseExpiry('2030-01-01T00:00', NOW)).toMatchObject({ ok: true })
    expect(parseExpiry('not a date', NOW)).toEqual({ ok: false, error: 'Choose a future expiry.' })
  })
})
