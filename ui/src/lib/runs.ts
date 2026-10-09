import type { StatusKind } from './types'

export function runStatus(status: string): StatusKind {
  switch (status) {
    case 'complete': case 'success': case 'ran': return 'success'
    case 'pending': return 'queued'
    case 'running': return 'running'
    case 'failed': return 'failed'
    default: return 'warning'
  }
}

export function runLabel(status: string): string {
  return status === 'complete' ? 'success' : status === 'pending' ? 'queued' : status
}

export function runIsLive(status: string): boolean {
  return status === 'pending' || status === 'running'
}

export function runTime(value: string | null): string {
  if (!value) return '–'
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString()
}
