import { useState, type FormEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Button, Section } from '@/components'
import { api, type IssuedToken } from '@/lib/api'
import { authMode, connect } from '@/lib/credentials'

export function TokensPage() {
  const queryClient = useQueryClient()
  const tokens = useQuery({ queryKey: ['tokens'], queryFn: api.tokens, enabled: authMode() === 'token', retry: false })
  const [name, setName] = useState('')
  const [expiry, setExpiry] = useState('')
  const [issued, setIssued] = useState<IssuedToken | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [copied, setCopied] = useState(false)
  if (authMode() !== 'token') return <Section title="API tokens"><p>Token authentication is off on this server.</p></Section>
  const create = async (event: FormEvent) => {
    event.preventDefault(); setError(null); setBusy(true)
    try {
      const expires = expiry ? new Date(expiry) : null
      if (expires && (!Number.isFinite(expires.getTime()) || expires.getTime() <= Date.now())) throw new Error('Choose a future expiry.')
      const token = await api.createToken(name, expires?.toISOString() ?? null)
      setIssued(token); setCopied(false); setName(''); setExpiry('')
      void queryClient.invalidateQueries({ queryKey: ['tokens'] })
    } catch (error) { setError(error instanceof Error ? error.message : 'Unable to create a token.') }
    finally { setBusy(false) }
  }
  const revoke = async (id: string) => {
    setError(null); setBusy(true)
    try { await api.revokeToken(id); void queryClient.invalidateQueries({ queryKey: ['tokens'] }) }
    catch (error) { setError(error instanceof Error ? error.message : 'Unable to revoke the token.') }
    finally { setBusy(false) }
  }
  return <main className="barca-tokens-page">
    <h1>API tokens</h1>
    <Section title="Project access">
      <p>Every token grants full project access, including creating and revoking tokens. There are no separate users or roles.</p>
      <p className="barca-note">Disconnect clears this tab’s connection; it does not revoke the token.</p>
      <Button onClick={() => connect(null)}>Disconnect</Button>
    </Section>
    {issued && <Section title="Copy your token">
      <p>This secret is shown once. Keep it somewhere safe before dismissing it.</p>
      <code className="barca-token-secret">{issued.token}</code>
      <p>{issued.expires_at ? `Expires ${new Date(issued.expires_at).toLocaleString()}` : 'This token has no expiry.'}</p>
      <Button onClick={() => { void navigator.clipboard.writeText(issued.token).then(() => setCopied(true)).catch(() => setError('Copy failed. Select and copy the token above.')) }}>{copied ? 'Copied' : 'Copy token'}</Button>
      {' '}<Button onClick={() => setIssued(null)}>Done</Button>
    </Section>}
    <Section title="Create a token">
      <form onSubmit={event => void create(event)} className="barca-auth-form">
        <label>Name<input value={name} onChange={event => setName(event.target.value)} required autoComplete="off" /></label>
        <label>Expiry (optional, your local time)<input type="datetime-local" value={expiry} onChange={event => setExpiry(event.target.value)} /></label>
        {!expiry && <p className="barca-note">No expiry selected: this token will stay valid until revoked.</p>}
        <Button type="submit" variant="signal" loading={busy} disabled={!!issued}>Create token</Button>
      </form>
      {issued && <p className="barca-note">Save and dismiss the token above before creating another.</p>}
      {error && <p role="alert">{error}</p>}
    </Section>
    <Section title="Active tokens">
      {tokens.isPending && <p>Loading tokens…</p>}
      {tokens.error && <p role="alert">{tokens.error.message} <Button onClick={() => void tokens.refetch()}>Retry</Button></p>}
      {tokens.data?.tokens.map(token => <div key={token.id} className="barca-token-row">
        <div><strong>{token.name}</strong><p className="barca-note">Created {new Date(token.created_at).toLocaleString()} · {token.expires_at ? `expires ${new Date(token.expires_at).toLocaleString()}` : 'no expiry'}</p></div>
        <Button variant="danger" disabled={busy} onClick={() => void revoke(token.id)} aria-label={`Revoke ${token.name}`}>Revoke</Button>
      </div>)}
    </Section>
  </main>
}
