/* ============================================================
   barca · API types
   The wire types are GENERATED from the Rust source of truth via ts-rs
   (`pnpm gen:types`), so they cannot drift. This module re-exports them and
   adds the few frontend-only / ad-hoc-JSON types the generator can't cover.
   Do not hand-edit anything under ./generated.
   ============================================================ */

export type { NodeGroup } from './generated/NodeGroup'

// ── Generated from Rust (single source of truth) ──────────────────────────────
export type { NodeKind } from './generated/NodeKind'
export type { CronExpr } from './generated/CronExpr'
export type { ListNode } from './generated/ListNode'
export type { ListFreshness } from './generated/ListFreshness'
export type { PlanResult } from './generated/PlanResult'
export type { PlanPhase } from './generated/PlanPhase'
export type { PlanStream } from './generated/PlanStream'
export type { GetResult } from './generated/GetResult'
export type { OutputRef } from './generated/OutputRef'
export type { RunEvent } from './generated/RunEvent'
export type { LineWhen } from './generated/LineWhen'
export type { FirstFailure } from './generated/FirstFailure'
export type { RunSummary } from './generated/RunSummary'
export type { RunList } from './generated/RunList'
export type { RunView } from './generated/RunView'
export type { RunRequest } from './generated/RunRequest'
export type { RunCommand } from './generated/RunCommand'
export type { TriggeredBy } from './generated/TriggeredBy'
export type { OutcomeStatus } from './generated/OutcomeStatus'
export type { RunStep } from './generated/RunStep'
export type { AssetStats } from './generated/AssetStats'
export type { AssetRunEntry } from './generated/AssetRunEntry'
export type { LogEntry } from './generated/LogEntry'
export type { NodeState } from './generated/NodeState'
export type { NodeStatus } from './generated/NodeStatus'
export type { CacheStatus } from './generated/CacheStatus'
export type { PartitionState } from './generated/PartitionState'
export type { LastMaterialization } from './generated/LastMaterialization'
export type { Durations } from './generated/Durations'
export type { ResetResult } from './generated/ResetResult'
export type { NodeRunning } from './generated/NodeRunning'
export type { RunningStep } from './generated/RunningStep'
export type { PartitionProgress } from './generated/PartitionProgress'
export type { ActiveStep } from './generated/ActiveStep'
export type { QueueView } from './generated/QueueView'
export type { QueueEntry } from './generated/QueueEntry'
export type { CapacitySnapshot } from './generated/CapacitySnapshot'
export type { WaitingStep } from './generated/WaitingStep'

import type { LoadError } from './generated/LoadError'

import type { ListNode } from './generated/ListNode'
import type { AssetStats } from './generated/AssetStats'
import type { NodeKind } from './generated/NodeKind'

// ── Frontend-only / ad-hoc-JSON types (not 1:1 Rust structs) ──────────────────

/** GET /health — ad-hoc JSON, not a Rust struct. */
export interface Health {
  capabilities?: { event_resume?: number }
  status: string
  version: string
  /** `barca serve --read-only`: run and cancel endpoints are refused. */
  read_only: boolean
  /** Whether this server fires `Schedule(...)` nodes. */
  scheduler: boolean
  /** `scheduler`, and false once the scheduler stopped on an error. */
  scheduler_enabled: boolean
  /** Why the metadata DB cannot be used, while it cannot; null otherwise. */
  metadata_error: string | null
  /** Unloaded sources and graph-invalid definitions; healthy node routes remain available. */
  load_errors: LoadError[]
  /** Major version of the HTTP API; a client refuses another. */
  api_version: number
  /** The environment this server runs (`barca serve --env`). */
  env: string
  /** The artifact store this server writes to; null when artifacts stay on its machine. */
  store_uri: string | null
  /** When this server started (RFC 3339). */
  started_at: string
  /** The run queue in brief; `GET /queue` has the runs. */
  queue: HealthQueue
}

/** `GET /health`'s `queue`. */
export interface HealthQueue {
  paused: boolean
  queued: number
  running: number
  max_runs: number
  /** The capacity every run shares (`--capacity`); null when the server has none. */
  capacity?: { total: number; in_use: number; waiting: number } | null
}

/**
 * Every error body: the CLI's stderr error envelope (`barca docs contract`).
 * A non-success run outcome carries the same envelope.
 */
export interface ErrorEnvelope {
  error: string
  code: number
  kind: 'step_failed' | 'usage' | 'infra' | 'cancelled'
  remediation: string | null
  node?: string | null
  traceback?: string | null
  artifact_dir?: string | null
  /** A stable name for an error a client may handle: `metadata_unusable`. Absent otherwise. */
  reason?: 'metadata_unusable' | 'authentication_required' | 'invalid_token' | 'invalid_event_cursor' | 'event_cursor_mismatch' | 'event_cursor_expired'
}

/**
 * How a run ended (`RunView.outcome`, `run_finished.outcome`), tagged by `status`.
 * The generated type is an untyped object; `parseOutcome` (lib/runs.ts) narrows it.
 */
export type RunOutcome =
  | { status: 'success'; run_id: string; final_output?: unknown; targets?: Record<string, unknown> }
  | { status: 'failed' | 'cancelled' | 'error'; envelope: ErrorEnvelope }

/** `RunView.status`: live while `pending`/`running`, terminal otherwise. */
export type RunViewStatus =
  | 'pending'
  | 'running'
  | 'success'
  | 'failed'
  | 'cancelled'
  | 'error'
  | 'interrupted'

/** One `GET /schedule` entry (ad-hoc JSON, not a ts-rs type). */
export interface ScheduleJob {
  id: string
  cron: string
  kind: NodeKind
  next_fire: number | null
  last_fired: number | null
  last_run: string | null
  last_status: string | null
  last_skipped: number | null
  skipped_for: string | null
  /** Why the last due tick was skipped. */
  skip_reason: 'in_flight' | 'paused' | null
  paused: boolean
  /** Unix seconds when a timed pause ends; null for an open-ended pause. */
  paused_until: number | null
  /** Consecutive failed ticks (RFC-0008); kept across restarts. */
  failures: number
  /** `Schedule(cron, max_failures=N)`, default 5. */
  max_failures: number
  /** `failures >= max_failures`: ticks are skipped until an explicit request or an input change. */
  failing: boolean
}

/** POST /runs — `202 { run_id, status, position }`: the database run id, and whether it started or waits. */
export interface RunHandle {
  run_id: string
  /** `running` when it started at once, `pending` while it waits in the queue. */
  status: 'pending' | 'running'
  /** Its place in the queue, 1 = next; null when it started. */
  position: number | null
}

/** `barca stats --json`: the generated `AssetStats` with `id` in place of `node_id`. */
export type NodeStats = Omit<AssetStats, 'node_id'> & { id: string }

/** GET /assets/{name} — ad-hoc wrapper around the generated pieces. */
export interface AssetDetail {
  asset: ListNode
  stats: NodeStats
}

/** A single captured log line, as rendered by the LogViewer (UI concept). */
export interface LogLine {
  nodeId: string
  text: string
  /** The step's stream; absent means stdout. */
  stream?: 'stdout' | 'stderr'
  /** When the line was written (RFC 3339, UTC); absent on older lines. */
  ts?: string | null
}

/**
 * Visual run state used across status components. A frontend superset of the
 * server's run lifecycle plus the asset-level states the design system defines
 * (queued, skipped, warning, success).
 */
export type StatusKind =
  | 'success'
  | 'running'
  | 'failed'
  | 'warning'
  | 'queued'
  | 'skipped'
  | 'pending'

export type { TokenMetadata } from './generated/TokenMetadata'
export type { IssuedToken } from './generated/IssuedToken'
