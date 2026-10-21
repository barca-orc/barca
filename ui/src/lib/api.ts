/* ============================================================
   barca · API client
   Thin fetch wrappers over barca-server. Same-origin: barca serve
   serves this UI at <prefix>/ui/ and the API at <prefix>/ (see
   apiBase.ts); in dev, Vite proxies non-/ui paths to barca serve.
   ============================================================ */

import { API_BASE, apiUrl } from './apiBase'

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
} from './types'


class ApiError extends Error {
  status: number
  /** The error envelope's `remediation`: what to do about it, when the server says. */
  remediation: string | null

  constructor(status: number, message: string, remediation: string | null = null) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.remediation = remediation
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(apiUrl(API_BASE, path), {
    headers: { 'content-type': 'application/json' },
    ...init,
  })
  if (!res.ok) {
    let message = res.statusText
    let remediation: string | null = null
    try {
      // Every error body is the CLI error envelope `{error, code, kind, remediation, ...}`.
      const body = (await res.json()) as { error?: string; remediation?: string | null }
      if (body.error) message = body.error
      if (body.remediation) remediation = body.remediation
    } catch {
      // non-JSON error body; keep statusText
    }
    throw new ApiError(res.status, message, remediation)
  }
  return res.json() as Promise<T>
}

/** Start a run: the flags of `barca get` / `barca run`. Answers `202 { run_id }`. */
function startRun(body: RunRequest): Promise<RunHandle> {
  return request<RunHandle>('/runs', { method: 'POST', body: JSON.stringify(body) })
}

export const api = {
  health: () => request<Health>('/health'),
  groups: () => request<NodeGroup[]>('/groups'),
  assets: () => request<ListNode[]>('/assets'),
  asset: (name: string) => request<AssetDetail>(`/assets/${encodeURIComponent(name)}`),
  assetSchema: (name: string) => request<NodeStatus[]>(`/assets/${encodeURIComponent(name)}/schema`),
  plan: () => request<PlanResult>('/plan'),
  state: () => request<NodeState[]>('/state'),
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
  /** `barca state reset`: move the metadata database aside and start an empty one. */
  resetState: () => request<ResetResult>('/state/reset', { method: 'POST' }),
}

export { ApiError }
