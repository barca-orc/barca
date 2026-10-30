import { match } from 'ts-pattern'
import { Button, Section } from '@/components'
import type { Dispatch } from '@/lib/program'
import type { Remote } from '@/lib/remote'
import type { TokensModel, TokensMsg } from '@/lib/tokensPage'
import type { TokenMetadata } from '@/lib/types'

export interface TokensViewProps {
  model: TokensModel
  tokens: Remote<TokenMetadata[]>
  dispatch: Dispatch<TokensMsg>
  /** For the create message's expiry check; the kit and tests pass a fixed clock. */
  now?: () => number
}

const when = (iso: string) => new Date(iso).toLocaleString()

/** The page while token authentication is off on the server. */
export function TokensOffView() {
  return <Section title="API tokens"><p>Token authentication is off on this server.</p></Section>
}

/** The Tokens page, for any model and token list: renders and dispatches, nothing else. */
export function TokensView({ model, tokens, dispatch, now = Date.now }: TokensViewProps) {
  const { issued, request } = model
  const busy = request.kind !== 'idle'
  return <main className="barca-tokens-page">
    <h1>API tokens</h1>
    <Section title="Project access">
      <p>Every token grants full project access, including creating and revoking tokens. There are no separate users or roles.</p>
      <p className="barca-note">Disconnect clears this tab’s connection; it does not revoke the token.</p>
      <Button onClick={() => dispatch({ type: 'disconnect_clicked' })}>Disconnect</Button>
    </Section>
    {issued && <Section title="Copy your token">
      <p>This secret is shown once. Keep it somewhere safe before dismissing it.</p>
      <code className="barca-token-secret">{issued.token.token}</code>
      <p>{issued.token.expires_at ? `Expires ${when(issued.token.expires_at)}` : 'This token has no expiry.'}</p>
      <Button onClick={() => dispatch({ type: 'copy_clicked' })}>{issued.copied ? 'Copied' : 'Copy token'}</Button>
      {' '}<Button onClick={() => dispatch({ type: 'dismissed' })}>Done</Button>
    </Section>}
    <Section title="Create a token">
      <form onSubmit={event => { event.preventDefault(); dispatch({ type: 'create_submitted', now: now() }) }} className="barca-auth-form">
        <label>Name<input value={model.name} onChange={event => dispatch({ type: 'name_changed', value: event.target.value })} required autoComplete="off" /></label>
        <label>Expiry (optional, your local time)<input type="datetime-local" value={model.expiry} onChange={event => dispatch({ type: 'expiry_changed', value: event.target.value })} /></label>
        {!model.expiry && <p className="barca-note">No expiry selected: this token will stay valid until revoked.</p>}
        <Button type="submit" variant="signal" loading={request.kind === 'creating'} disabled={!!issued || busy}>Create token</Button>
      </form>
      {issued && <p className="barca-note">Save and dismiss the token above before creating another.</p>}
      {model.error && <p role="alert">{model.error}</p>}
    </Section>
    <Section title="Active tokens">
      {match(tokens)
        .with({ kind: 'loading' }, () => <p>Loading tokens…</p>)
        .with({ kind: 'failed' }, ({ error }) => <p role="alert">{error} <Button onClick={() => dispatch({ type: 'retry_list' })}>Retry</Button></p>)
        .with({ kind: 'ready' }, ({ data, refreshError }) => <>
          {refreshError && <p role="alert">{refreshError} <Button onClick={() => dispatch({ type: 'retry_list' })}>Retry</Button></p>}
          {data.length === 0 && <p className="barca-note">No active tokens.</p>}
          {data.map(token => <div key={token.id} className="barca-token-row">
            <div><strong>{token.name}</strong><p className="barca-note">Created {when(token.created_at)} · {token.expires_at ? `expires ${when(token.expires_at)}` : 'no expiry'}</p></div>
            <Button variant="danger" disabled={busy} loading={request.kind === 'revoking' && request.id === token.id} onClick={() => dispatch({ type: 'revoke_clicked', id: token.id })} aria-label={`Revoke ${token.name}`}>Revoke</Button>
          </div>)}
        </>)
        .exhaustive()}
    </Section>
  </main>
}
