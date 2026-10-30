/**
 * The Tokens page as a program (`lib/program.ts`): what it remembers, what can happen to it,
 * and what it asks the outside world to do. Pure; `pages/TokensPage.tsx` runs it and
 * `pages/TokensView.tsx` renders it.
 */

import { match } from 'ts-pattern'
import { only, type Next } from './program'
import type { IssuedToken } from './types'

export interface TokensModel {
  name: string
  /** The `datetime-local` input's value: local time, `''` for no expiry. */
  expiry: string
  /** A request in flight; the page sends one at a time. */
  request: { kind: 'idle' } | { kind: 'creating' } | { kind: 'revoking'; id: string }
  /** A token just created: its secret is shown once, until dismissed. */
  issued: { token: IssuedToken; copied: boolean } | null
  error: string | null
}

export type TokensMsg =
  | { type: 'name_changed'; value: string }
  | { type: 'expiry_changed'; value: string }
  /** `now`: ms since epoch, to check the expiry is in the future. */
  | { type: 'create_submitted'; now: number }
  | { type: 'created'; token: IssuedToken }
  | { type: 'revoke_clicked'; id: string }
  | { type: 'revoked' }
  | { type: 'request_failed'; error: string | null }
  | { type: 'copy_clicked' }
  | { type: 'copied' }
  | { type: 'copy_failed' }
  | { type: 'dismissed' }
  | { type: 'disconnect_clicked' }
  | { type: 'retry_list' }

export type TokensEffect =
  | { type: 'create'; name: string; expiresAt: string | null }
  | { type: 'revoke'; id: string }
  | { type: 'refresh_list' }
  | { type: 'copy'; text: string }
  | { type: 'disconnect' }

export const INITIAL_TOKENS: TokensModel = {
  name: '',
  expiry: '',
  request: { kind: 'idle' },
  issued: null,
  error: null,
}

/** The expiry as an ISO time, `null` for none, or why it cannot be used. */
export function parseExpiry(expiry: string, now: number): { ok: true; iso: string | null } | { ok: false; error: string } {
  if (!expiry) return { ok: true, iso: null }
  const at = new Date(expiry).getTime()
  if (!Number.isFinite(at) || at <= now) return { ok: false, error: 'Choose a future expiry.' }
  return { ok: true, iso: new Date(at).toISOString() }
}

export function updateTokens(model: TokensModel, msg: TokensMsg): Next<TokensModel, TokensEffect> {
  const busy = model.request.kind !== 'idle'
  return match<TokensMsg, Next<TokensModel, TokensEffect>>(msg)
    .with({ type: 'name_changed' }, ({ value }) => only({ ...model, name: value }))
    .with({ type: 'expiry_changed' }, ({ value }) => only({ ...model, expiry: value }))
    .with({ type: 'create_submitted' }, ({ now }) => {
      // One secret on screen at a time: dismiss it before creating another.
      if (busy || model.issued) return only(model)
      const expiry = parseExpiry(model.expiry, now)
      if (!expiry.ok) return only({ ...model, error: expiry.error })
      return [
        { ...model, request: { kind: 'creating' }, error: null },
        [{ type: 'create', name: model.name, expiresAt: expiry.iso }],
      ]
    })
    .with({ type: 'created' }, ({ token }) => [
      { ...model, name: '', expiry: '', request: { kind: 'idle' }, issued: { token, copied: false } },
      [{ type: 'refresh_list' }],
    ])
    .with({ type: 'revoke_clicked' }, ({ id }) =>
      busy ? only(model) : [{ ...model, request: { kind: 'revoking', id }, error: null }, [{ type: 'revoke', id }]],
    )
    .with({ type: 'revoked' }, () => [{ ...model, request: { kind: 'idle' } }, [{ type: 'refresh_list' }]])
    .with({ type: 'request_failed' }, ({ error }) => {
      const fallback = model.request.kind === 'revoking' ? 'Unable to revoke the token.' : 'Unable to create a token.'
      return only({ ...model, request: { kind: 'idle' }, error: error ?? fallback })
    })
    .with({ type: 'copy_clicked' }, () => (model.issued ? [model, [{ type: 'copy', text: model.issued.token.token }]] : only(model)))
    .with({ type: 'copied' }, () => only(model.issued ? { ...model, issued: { ...model.issued, copied: true } } : model))
    .with({ type: 'copy_failed' }, () => only({ ...model, error: 'Copy failed. Select and copy the token above.' }))
    .with({ type: 'dismissed' }, () => only({ ...model, issued: null }))
    .with({ type: 'disconnect_clicked' }, () => [model, [{ type: 'disconnect' }]])
    .with({ type: 'retry_list' }, () => [model, [{ type: 'refresh_list' }]])
    .exhaustive()
}
