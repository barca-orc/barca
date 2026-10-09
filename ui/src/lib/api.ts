/* ============================================================
   barca · API client
   Thin fetch wrappers over barca-server. Same-origin: barca serve
   serves this UI at <prefix>/ui/ and the API at <prefix>/ (see
   apiBase.ts); in dev, Vite proxies non-/ui paths to barca serve.
   ============================================================ */

import { API_BASE, apiUrl } from './apiBase'

import type {
  AssetSummary,
  AssetDetail,
  NodeState,
  NodeStatus,
  PlanResult,
  RunState,
  RunHandle,
  Health,
  RunList,
  RunDetail,
} from './types'


class ApiError extends Error {
  status: number

  constructor(status: number, message: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(apiUrl(API_BASE, path), {
    headers: { 'content-type': 'application/json' },
    ...init,
  })
  if (!res.ok) {
    let message = res.statusText
    try {
      const body = (await res.json()) as { error?: string }
      if (body.error) message = body.error
    } catch {
      // non-JSON error body; keep statusText
    }
    throw new ApiError(res.status, message)
  }
  return res.json() as Promise<T>
}

export const api = {
  health: () => request<Health>('/health'),
  assets: () => request<AssetSummary[]>('/assets'),
  asset: (name: string) => request<AssetDetail>(`/assets/${encodeURIComponent(name)}`),
  assetSchema: (name: string) => request<NodeStatus[]>(`/assets/${encodeURIComponent(name)}/schema`),
  plan: () => request<PlanResult>('/plan'),
  state: () => request<NodeState[]>('/state'),
  runs: (limit = 100) => request<RunList>(`/runs?limit=${limit}`),
  runDetail: (id: string) => request<RunDetail>(`/runs/${encodeURIComponent(id)}`),
  status: (runId: string) => request<RunState>(`/status/${encodeURIComponent(runId)}`),
  run: () => request<RunHandle>('/run', { method: 'POST' }),
  runTarget: (target: string) =>
    request<RunHandle>(`/run/${encodeURIComponent(target)}`, { method: 'POST' }),
  getTarget: (target: string) =>
    request<RunHandle>(`/get/${encodeURIComponent(target)}`, { method: 'POST' }),
}

export { ApiError }
