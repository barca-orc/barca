/* ============================================================
   barca · API client
   Thin fetch wrappers over barca-server. Same-origin: barca serve
   serves this UI at <prefix>/ui/ and the API at <prefix>/ (see
   apiBase.ts); in dev, Vite proxies non-/ui paths to barca serve.
   ============================================================ */

import { connectionToken, redactCredential, serverFetch } from './credentials'

import type {
  NodeGroup,
  ListNode,
  AssetDetail,
  NodeState,
  NodeStatus,
  PlanResult,
  RunHandle,
  RunRequest,
  Health,
  RunList,
  RunView,
  ScheduleJob,
  ResetResult,
  QueueView,
  TokenMetadata,
  IssuedToken,
} from './types'


class ApiError extends Error {
  status: number
  /** The error envelope's `remediation`: what to do about it, when the server says. */
  remediation: string | null
  /** The envelope's `reason`, e.g. `metadata_unusable`, when the server has one. */
  reason: string | null

  constructor(
    status: number,
    message: string,
    remediation: string | null = null,
    reason: string | null = null,
  ) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.remediation = remediation
    this.reason = reason
  }
}

async function request<T>(path: string, init?: RequestInit, auth?: { token?: string | null; secret?: string }): Promise<T> {
  const token = auth?.token === undefined ? connectionToken() : auth.token
  const res = await serverFetch(path, {
    headers: { 'content-type': 'application/json' },
    ...init,
  }, token)
  if (!res.ok) {
    let message = res.statusText
    let remediation: string | null = null
    let reason: string | null = null
    try {
      // Every error body is the CLI error envelope `{error, code, kind, remediation, ...}`.
      const body = (await res.json()) as {
        error?: string
        remediation?: string | null
        reason?: string
      }
      if (typeof body.error === 'string') message = body.error
      if (typeof body.remediation === 'string') remediation = body.remediation
      if (typeof body.reason === 'string') reason = body.reason
    } catch {
      // non-JSON error body; keep statusText
    }
    throw new ApiError(res.status, redactCredential(redactCredential(message, token), auth?.secret ?? null), redactCredential(redactCredential(remediation ?? '', token), auth?.secret ?? null) || null, reason)
  }
  if (res.status === 204) return undefined as T
  return res.json() as Promise<T>
}

/** Start a run: the flags of `barca get` / `barca run`. Answers `202 { run_id, status, position }`. */
function startRun(body: RunRequest): Promise<RunHandle> {
  return request<RunHandle>('/runs', { method: 'POST', body: JSON.stringify(body) })
}

export type { TokenMetadata, IssuedToken } from './types'

export const api = {
  authStatus: (signal?: AbortSignal) => request<{ mode: 'off' | 'token' }>('/auth/status', { signal }, { token: null }),
  bootstrap: (code: string) => request<IssuedToken>('/auth/bootstrap', { method: 'POST', body: JSON.stringify({ code }) }, { token: null, secret: code }),
  checkToken: (token: string) => request<Health>('/health', undefined, { token }),
  tokens: () => request<{ tokens: TokenMetadata[] }>('/auth/tokens'),
  createToken: (name: string, expires_at: string | null) => request<IssuedToken>('/auth/tokens', { method: 'POST', body: JSON.stringify({ name, expires_at }) }),
  revokeToken: (id: string) => request<void>(`/auth/tokens/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  health: () => request<Health>('/health'),
  groups: () => request<NodeGroup[]>('/groups'),
  assets: () => request<ListNode[]>('/assets'),
  asset: (name: string) => request<AssetDetail>(`/assets/${encodeURIComponent(name)}`),
  assetSchema: (name: string) => request<NodeStatus[]>(`/assets/${encodeURIComponent(name)}/schema`),
  plan: () => request<PlanResult>('/plan'),
  state: (targets?: string[], signal?: AbortSignal) =>
    request<NodeState[]>(
      targets ? `/state?${new URLSearchParams({ targets: targets.join(',') })}` : '/state',
      { signal },
    ),
  runs: (limit = 100) => request<RunList>(`/runs?limit=${limit}`),
  runDetail: (id: string) => request<RunView>(`/runs/${encodeURIComponent(id)}`),
  startRun,
  /** `barca get` for every asset and sensor. */
  getAll: () => startRun({ command: 'get', targets: [] }),
  /** `barca get <target>`: a cache-aware materialization of one asset or sensor. */
  getTarget: (target: string) => startRun({ command: 'get', targets: [target] }),
  /** `barca run <task> --refresh-all`: the task re-executes along with its upstream assets. */
  runTarget: (target: string) =>
    startRun({ command: 'run', targets: [target], refresh_all: true }),
  cancelRun: (runId: string) =>
    request<{ run_id: string; status: 'cancelling' }>(`/runs/${encodeURIComponent(runId)}`, { method: 'DELETE' }),
  schedule: () => request<ScheduleJob[]>('/schedule'),
  /** Pause a schedule; `until` (RFC 3339) ends the pause, otherwise it lasts until resumed. */
  pauseSchedule: (id: string, until?: string) =>
    request<unknown>(
      `/schedule/${encodeURIComponent(id)}/pause${until ? `?until=${encodeURIComponent(until)}` : ''}`,
      { method: 'POST' },
    ),
  resumeSchedule: (id: string) =>
    request<unknown>(`/schedule/${encodeURIComponent(id)}/resume`, { method: 'POST' }),
  /** The run queue: what runs, what waits (in order), and whether it is paused. */
  queue: () => request<QueueView>('/queue'),
  /** Pause the queue: runs keep being accepted, none starts until resumed. 403 on a read-only server. */
  pauseQueue: () => request<QueueView>('/queue/pause', { method: 'POST' }),
  resumeQueue: () => request<QueueView>('/queue/resume', { method: 'POST' }),
  /** `barca state reset`: move the metadata database aside and start an empty one. */
  resetState: () => request<ResetResult>('/state/reset', { method: 'POST' }),
}

export { ApiError }
