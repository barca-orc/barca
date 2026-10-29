import { useEffect, useState, useSyncExternalStore, type FormEvent, type ReactNode } from 'react'
import { Button } from '@/components'
import { api } from '@/lib/api'
import { connect, connectionToken, setAuthMode, subscribeConnection, type AuthMode } from '@/lib/credentials'
import './auth.css'

function useConnectionToken() { return useSyncExternalStore(subscribeConnection, connectionToken, () => null) }

export function AuthGate({ children }: { children: ReactNode }) {
  const token = useConnectionToken()
  const [mode, setMode] = useState<AuthMode | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [attempt, setAttempt] = useState(0)
  useEffect(() => {
    const controller = new AbortController()
    api.authStatus(controller.signal).then(({ mode }) => {
      if (mode !== 'off' && mode !== 'token') throw new Error('The server returned an invalid authentication mode.')
      setAuthMode(mode)
      setMode(mode)
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return
      if (error instanceof Error && 'status' in error && error.status === 404) {
        setAuthMode('off'); setMode('off') // Older servers have no auth discovery route.
      } else setError(error instanceof Error ? error.message : 'Unable to reach the server.')
    })
    return () => controller.abort()
  }, [attempt])
  if (mode === null) return <div className="barca-auth-screen"><main className="barca-connect"><h1>Connect to barca</h1>{error ? <><p role="alert">{error}</p><Button onClick={() => { setError(null); setAttempt(value => value + 1) }}>Retry</Button></> : <p>Checking the server…</p>}</main></div>
  if (mode === 'token' && !token) return <Connect />
  return children
}

function Connect() {
  const [value, setValue] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setBusy(true); setError(null)
    const secret = value.trim()
    try {
      if (secret.startsWith('barca_bootstrap_v1_')) {
        const issued = await api.bootstrap(secret)
        connect(issued.token)
      } else { await api.checkToken(secret); connect(secret) }
      setValue('')
    } catch (error) { setError(error instanceof Error ? error.message : 'Unable to connect.') }
    finally { setBusy(false) }
  }
  return <div className="barca-auth-screen"><main className="barca-connect">
    <h1>Connect to barca</h1>
    <p>Enter an API token or a one-use bootstrap code from the deployed host.</p>
    <form onSubmit={event => void submit(event)} className="barca-auth-form">
      <label>API token or bootstrap code<input type="password" autoComplete="off" spellCheck={false} value={value} onChange={event => setValue(event.target.value)} required /></label>
      {error && <p role="alert">{error}</p>}
      <Button type="submit" variant="signal" loading={busy} disabled={!value.trim()}>Connect</Button>
    </form>
    <p className="barca-note">Need access? Run <code>barca tokens bootstrap</code> on the deployed host, inside the project. The code expires after 15 minutes.</p>
    <p className="barca-note">This connection is kept in this tab. A bootstrap connection lasts 12 hours. Tokens grant full project access, including token management.</p>
  </main></div>
}
