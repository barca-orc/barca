/* ============================================================
   barca · API types
   The wire types are GENERATED from the Rust source of truth via ts-rs
   (`pnpm gen:types`), so they cannot drift. This module re-exports them and
   adds the few frontend-only / ad-hoc-JSON types the generator can't cover.
   Do not hand-edit anything under ./generated.
   ============================================================ */

// ── Generated from Rust (single source of truth) ──────────────────────────────
export type { NodeKind } from './generated/NodeKind'
export type { Freshness } from './generated/Freshness'
export type { CronExpr } from './generated/CronExpr'
export type { AssetSummary } from './generated/AssetSummary'
export type { PlanResult } from './generated/PlanResult'
export type { PlanPhase } from './generated/PlanPhase'
export type { PlanStream } from './generated/PlanStream'
export type { GetResult } from './generated/GetResult'
export type { OutputRef } from './generated/OutputRef'
export type { RunEvent } from './generated/RunEvent'
export type { RunState } from './generated/RunState'
export type { RunStatus } from './generated/RunStatus'
export type { RunSummary } from './generated/RunSummary'
export type { RunList } from './generated/RunList'
export type { RunDetail } from './generated/RunDetail'
export type { AssetStats } from './generated/AssetStats'
export type { AssetRunEntry } from './generated/AssetRunEntry'
export type { LogEntry } from './generated/LogEntry'
export type { NodeState } from './generated/NodeState'
export type { NodeStatus } from './generated/NodeStatus'
export type { CacheStatus } from './generated/CacheStatus'
export type { PartitionState } from './generated/PartitionState'
export type { LastMaterialization } from './generated/LastMaterialization'
export type { Durations } from './generated/Durations'

import type { LoadError } from './generated/LoadError'

import type { AssetSummary } from './generated/AssetSummary'
import type { AssetStats } from './generated/AssetStats'

// ── Frontend-only / ad-hoc-JSON types (not 1:1 Rust structs) ──────────────────

/** GET /health — ad-hoc JSON, not a Rust struct. */
export interface Health {
  status: string
  version: string
  /** `barca serve --read-only`: run and cancel endpoints are refused. */
  read_only: boolean
  /** Whether this server fires `Schedule(...)` nodes. */
  scheduler: boolean
  /** Unloaded sources and graph-invalid definitions; healthy node routes remain available. */
  load_errors: LoadError[]
}

/** POST /run, /run/{target}, /get/{target} — ad-hoc `{ run_id }`. */
export interface RunHandle {
  run_id: string
}

/** GET /assets/{name} — ad-hoc wrapper around the generated pieces. */
export interface AssetDetail {
  asset: AssetSummary
  stats: AssetStats
}

/** A single captured log line, as rendered by the LogViewer (UI concept). */
export interface LogLine {
  nodeId: string
  text: string
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
