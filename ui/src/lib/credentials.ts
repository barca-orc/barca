/** One tab's credential, scoped to this origin and API prefix. Never a URL or cookie. */
import { API_BASE, apiUrl } from './apiBase'

export type AuthMode = 'off' | 'token'
const key = `barca:connection:v1:${typeof window === 'undefined' ? '' : window.location.origin}${API_BASE}`
const listeners = new Set<() => void>()
let mode: AuthMode = 'off'
function valid(value: string): boolean { return /^[\x21-\x7e]+$/.test(value) }
function stored(): string | null {
  try {
    const value = window.sessionStorage.getItem(key)
    return value && valid(value) ? value : null
  } catch { return null }
}
let credential = stored()
export function connectionToken(): string | null { return credential }
export function authMode(): AuthMode { return mode }
export function setAuthMode(value: AuthMode): void {
  mode = value
  if (value === 'off' && credential !== null) connect(null)
}
export function subscribeConnection(listener: () => void): () => void {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}
export function connect(token: string | null): void {
  if (token !== null && !valid(token)) throw new Error('Enter a valid API token or bootstrap code.')
  credential = token
  try {
    if (token === null) window.sessionStorage.removeItem(key)
    else window.sessionStorage.setItem(key, token)
  } catch { /* The connection still works in memory when tab storage is unavailable. */ }
  listeners.forEach(listener => listener())
}
export function redactCredential(message: string, token: string | null): string {
  return token ? message.replaceAll(token, '<redacted>') : message
}
/** All UI HTTP, including streams, crosses this seam. A redirect cannot carry a token away. */
export async function serverFetch(path: string, init: RequestInit = {}, token = connectionToken()): Promise<Response> {
  if (!path.startsWith('/') || path.startsWith('//')) throw new Error('Invalid server request path.')
  const target = new URL(apiUrl(API_BASE, path), typeof window === 'undefined' ? 'http://localhost' : window.location.origin)
  const origin = typeof window === 'undefined' ? 'http://localhost' : window.location.origin
  const prefix = API_BASE.replace(/\/$/, '')
  if (target.origin !== origin || (prefix && !target.pathname.startsWith(`${prefix}/`))) throw new Error('Server request left the project path.')
  if (token !== null && !valid(token)) throw new Error('Enter a valid API token or bootstrap code.')
  let decoded: string
  try { decoded = decodeURIComponent(target.pathname) } catch { throw new Error('Invalid server request path.') }
  if (decoded.split('/').some(part => part === '.' || part === '..') || (prefix && !decoded.startsWith(`${decodeURIComponent(prefix)}/`))) throw new Error('Server request left the project path.')
  const headers = new Headers(init.headers)
  if (token) headers.set('Authorization', `Bearer ${token}`)
  const response = await fetch(target.pathname + target.search, {
    ...init, headers,
    credentials: mode === 'token' ? 'omit' : 'same-origin',
    redirect: 'error',
  })
  if (response.status === 401 && token && token === connectionToken()) connect(null)
  return response
}
