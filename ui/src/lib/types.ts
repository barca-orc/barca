/* ============================================================
   barca · API types
   The wire types are GENERATED from the Rust source of truth via ts-rs
   (`pnpm gen:types`), so they cannot drift. This module re-exports them and
   adds the few frontend-only / ad-hoc-JSON types the generator can't cover.
   Do not hand-edit anything under ./generated.
   ============================================================ */

export type { NodeGroup } from './generated/NodeGroup'
export type { KeysPage } from './generated/KeysPage'
export type { KeyRow } from './generated/KeyRow'
export type { KeysOverview } from './generated/KeysOverview'
export type { OverviewRow } from './generated/OverviewRow'
export type { KeyIssue } from './generated/KeyIssue'
export type { GridDimension } from './generated/GridDimension'
export type { GridCounts } from './generated/GridCounts'
export type { PartitionKeyDetail } from './generated/PartitionKeyDetail'
export type { KeyArtifact } from './generated/KeyArtifact'
export type { KeyAttempt } from './generated/KeyAttempt'
export type { ChainKeyState } from './generated/ChainKeyState'

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
export type { FailureFacts } from './generated/FailureFacts'
import type { FailureFacts } from './generated/FailureFacts'
export type { RunSummary } from './generated/RunSummary'
export type { RunList } from './generated/RunList'
export type { RunActivity } from './generated/RunActivity'
export type { ActivityHour } from './generated/ActivityHour'
export type { LeadingFailure } from './generated/LeadingFailure'
export type { RunView } from './generated/RunView'
export type { PlanReasons } from './generated/PlanReasons'
export type { PlanReason } from './generated/PlanReason'
export type { ReasonCategory } from './generated/ReasonCategory'
export type { Cancellation } from './generated/Cancellation'
export type { RunExplanation } from './generated/RunExplanation'
export type { WhyRan } from './generated/WhyRan'
export type { ReasonRow } from './generated/ReasonRow'
export type { CancelledBy } from './generated/CancelledBy'
export type { RunRequest } from './generated/RunRequest'
export type { RunCommand } from './generated/RunCommand'
export type { TriggeredBy } from './generated/TriggeredBy'
export type { OutcomeStatus } from './generated/OutcomeStatus'
export type { RunStep } from './generated/RunStep'
export type { AssetStats } from './generated/AssetStats'
export type { AssetPin } from './generated/AssetPin'
export type { AssetUpload } from './generated/AssetUpload'
export type { AssetVersion } from './generated/AssetVersion'
export type { AssetVersions } from './generated/AssetVersions'
export type { PinResult } from './generated/PinResult'
export type { UnpinResult } from './generated/UnpinResult'
export type { AssetRunEntry } from './generated/AssetRunEntry'
export type { LogEntry } from './generated/LogEntry'
export type { NodeState } from './generated/NodeState'
export type { DataSignal } from './generated/DataSignal'
export type { NodeStatus } from './generated/NodeStatus'
export type { CacheStatus } from './generated/CacheStatus'
export type { PartitionState } from './generated/PartitionState'
export type { PartitionKeys } from './generated/PartitionKeys'
export type { PartitionKeyRow } from './generated/PartitionKeyRow'
export type { RunNodeSummary } from './generated/RunNodeSummary'
export type { LastMaterialization } from './generated/LastMaterialization'
export type { Durations } from './generated/Durations'
export type { ResetResult } from './generated/ResetResult'
export type { NodeRunning } from './generated/NodeRunning'
export type { RunningStep } from './generated/RunningStep'
export type { PartitionProgress } from './generated/PartitionProgress'
export type { ActiveStep } from './generated/ActiveStep'
export type { RunNow } from './generated/RunNow'
export type { RunLine } from './generated/RunLine'
export type { QueueView } from './generated/QueueView'
export type { QueueEntry } from './generated/QueueEntry'
export type { CapacitySnapshot } from './generated/CapacitySnapshot'
export type { WaitingStep } from './generated/WaitingStep'
export type { ErrorKind } from './generated/ErrorKind'
export type { ServerHealth } from './generated/ServerHealth'
export type { HealthQueue } from './generated/HealthQueue'
export type { RunViewStatus } from './generated/RunViewStatus'
export type { RunHandle } from './generated/RunHandle'
export type { RunsQuery } from './generated/RunsQuery'
export type { ScheduleJob } from './generated/ScheduleJob'
export type { SchedulePauseAnswer } from './generated/SchedulePauseAnswer'
export type { Pauses } from './generated/Pauses'
export type { AssetPause } from './generated/AssetPause'
export type { PauseRequest } from './generated/PauseRequest'
export type { Pins } from './generated/Pins'
export type { AuthStatus } from './generated/AuthStatus'
export type { Project } from './generated/Project'
export type { RunCancelling } from './generated/RunCancelling'
export type { TokenList } from './generated/TokenList'

import type { ErrorKind } from './generated/ErrorKind'

import type { ListNode } from './generated/ListNode'
import type { AssetStats } from './generated/AssetStats'

// ── Frontend-only / ad-hoc-JSON types (not 1:1 Rust structs) ──────────────────

/**
 * Every error body: the CLI's stderr error envelope (`barca docs contract`).
 * A non-success run outcome carries the same envelope. The envelope is serialized by hand in
 * Rust (`envelope.rs`, `to_json`), so its shape is written here; its `kind` is the generated enum.
 */
export interface ErrorEnvelope {
  error: string
  code: number
  kind: ErrorKind
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
  | { status: 'success' | 'skipped'; run_id: string; final_output?: unknown; targets?: Record<string, unknown> }
  | { status: 'failed' | 'cancelled' | 'error'; envelope: ErrorEnvelope }

/** `barca stats --json`: the generated `AssetStats` with `id` in place of `node_id`. */
type NodeStats = Omit<AssetStats, 'node_id'> & { id: string }

/** GET /assets/{name} — ad-hoc wrapper around the generated pieces. */
export interface AssetDetail {
  asset: ListNode
  stats: NodeStats
  /** Present when the node's latest execution failed (absent from an older server). */
  failure?: FailureFacts | null
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
 * A run's lifecycle, and a step's within it, as the UI colours and names it (`lib/status.ts`).
 * One axis: a node's cache state is another (`lib/cacheStateDisplay.ts`) and never maps here.
 */
export type StatusKind =
  | 'queued'
  | 'running'
  | 'success'
  | 'failed'
  | 'cancelled'
  | 'skipped'

export type { TokenMetadata } from './generated/TokenMetadata'
export type { IssuedToken } from './generated/IssuedToken'

export type { ReplayRequest } from './generated/ReplayRequest'
