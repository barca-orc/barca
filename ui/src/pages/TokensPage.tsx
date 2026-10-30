import { useQuery, useQueryClient } from '@tanstack/react-query'
import { match } from 'ts-pattern'
import { useProgram } from '@/hooks/useProgram'
import { api } from '@/lib/api'
import { authMode, connect } from '@/lib/credentials'
import { remote } from '@/lib/remote'
import { INITIAL_TOKENS, updateTokens } from '@/lib/tokensPage'
import { TokensOffView, TokensView } from './TokensView'

const message = (error: unknown) => (error instanceof Error ? error.message : null)

/** Runs the Tokens program: the token list from the server, and each effect's I/O. */
export function TokensPage() {
  const queryClient = useQueryClient()
  const auth = authMode() === 'token'
  const tokens = useQuery({ queryKey: ['tokens'], queryFn: api.tokens, enabled: auth, retry: false, select: d => d.tokens })
  const [model, dispatch] = useProgram(INITIAL_TOKENS, updateTokens, (effect, dispatch) =>
    match(effect)
      .with({ type: 'create' }, ({ name, expiresAt }) => {
        api.createToken(name, expiresAt).then(
          token => dispatch({ type: 'created', token }),
          error => dispatch({ type: 'request_failed', error: message(error) }),
        )
      })
      .with({ type: 'revoke' }, ({ id }) => {
        api.revokeToken(id).then(
          () => dispatch({ type: 'revoked' }),
          error => dispatch({ type: 'request_failed', error: message(error) }),
        )
      })
      .with({ type: 'refresh_list' }, () => void queryClient.invalidateQueries({ queryKey: ['tokens'] }))
      .with({ type: 'copy' }, ({ text }) => {
        navigator.clipboard.writeText(text).then(
          () => dispatch({ type: 'copied' }),
          () => dispatch({ type: 'copy_failed' }),
        )
      })
      .with({ type: 'disconnect' }, () => connect(null))
      .exhaustive(),
  )
  if (!auth) return <TokensOffView />
  return <TokensView model={model} tokens={remote(tokens)} dispatch={dispatch} />
}
