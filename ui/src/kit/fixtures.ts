/**
 * Fixtures for the kit page: one value per variant of each state a component shows.
 *
 * Each table is `satisfies Record<Union, …>`, so a variant added to a union (a generated
 * wire type included) is a compile error here until it has a fixture, and the kit renders
 * it from then on. `fixtures.test.ts` checks each fixture is the state its key names.
 */

import type { Connection } from '@/lib/connection'
import type { HealthCounts } from '@/lib/health'
import type { RunFeedback } from '@/lib/runFeedback'
import type { Severity } from '@/lib/assetTable'
import type { Remote } from '@/lib/remote'
import { ready, failed, loading } from '@/lib/remote'
import type { ResetPhase } from '@/components/ResetDatabase'
import type { TokensModel } from '@/lib/tokensPage'
import { INITIAL_TOKENS } from '@/lib/tokensPage'
import type {
  IssuedToken,
  LastMaterialization,
  LogLine,
  NodeKind,
  NodeRunning,
  NodeState,
  NodeStatus,
  RunList,
  RunningStep,
  RunSummary,
  RunView,
  RunViewStatus,
  StatusKind,
  TokenMetadata,
} from '@/lib/types'

/** The variants of a union, from a table that must name every one. */
export function variants<K extends string>(table: Record<K, unknown>): K[] {
  return Object.keys(table) as K[]
}

/** The kit's clock: fixtures are dated relative to it, so elapsed times never drift. */
export const NOW_MS = Date.parse('2026-10-29T12:00:00Z')
const ago = (seconds: number) => new Date(NOW_MS - seconds * 1000).toISOString()
const sqlTime = (seconds: number) => ago(seconds).replace('T', ' ').slice(0, 19)

export const STATUS_KINDS = variants({
  success: 1, running: 1, failed: 1, warning: 1, queued: 1, skipped: 1, pending: 1,
} satisfies Record<StatusKind, 1>)

export const NODE_KINDS = variants({ asset: 1, task: 1, sensor: 1 } satisfies Record<NodeKind, 1>)

export const connections = {
  connecting: { kind: 'connecting' },
  online: { kind: 'online', version: '0.42.0' },
  offline: { kind: 'offline' },
} satisfies Record<Connection['kind'], Connection>

// ── Nodes ─────────────────────────────────────────────────────────────────────

const materialized = (overrides: Partial<LastMaterialization> = {}): LastMaterialization => ({
  status: 'success',
  created_at: sqlTime(3600),
  elapsed_seconds: 4.2,
  run_hash: '9f2c0a41be77',
  artifact: '.barca/artifacts/orders_clean/9f2c0a41be77.parquet',
  format: 'parquet',
  size_bytes: 482_113,
  ...overrides,
})

function node(name: string, overrides: Partial<NodeState>): NodeState {
  return {
    id: `pipeline.py:${name}`,
    name,
    kind: 'asset',
    inputs: [],
    partitioned: false,
    cache: { state: 'cached', reason: 'materialized', detail: 'cached result for this code and these inputs' },
    last_materialization: materialized(),
    shape: null,
    env: [],
    durations: { median_seconds: 4.1, p95_seconds: 6.3, samples: 12 },
    next_run: null,
    running: null,
    ...overrides,
  }
}

/** A node in each resting state, most urgent first (`severityOf` reads each back). */
export const nodeBySeverity = {
  failed: node('orders_enriched', {
    last_materialization: materialized({ status: 'failed', artifact: null, format: null, error: "KeyError: 'currency'" }),
  }),
  stale: node('revenue_daily', {
    cache: { state: 'stale', reason: 'changed', detail: 'code changed since the last run', changed: ['code'] },
  }),
  never_run: node('fx_rates', {
    cache: { state: 'never_run', reason: 'no_record', detail: 'never materialized' },
    last_materialization: null,
    durations: null,
  }),
  partial: node('sessionize', {
    partitioned: true,
    cache: { state: 'partial', reason: 'partitions_missing', detail: '3 of 10 keys have no cached result' },
    partitions: { total: 10, cached: 7, missing: 3, missing_keys: ['day=2026-10-27', 'day=2026-10-28', 'day=2026-10-29'] },
  }),
  unknown: node('checkout_events', {
    cache: { state: 'unknown', reason: 'sensor_output_unknown', detail: 'the sensor has not reported its output' },
  }),
  always_runs: node('daily_report', {
    kind: 'task',
    cache: { state: 'always_runs', reason: 'task', detail: 'a task runs every time' },
  }),
  unsynced: node('product_catalog', {
    cache: { state: 'unsynced', reason: 'local_only', detail: 'cached here, not uploaded to the store yet' },
  }),
  cached: node('orders_clean', {}),
} satisfies Record<Severity, NodeState>

export const runningByStep = {
  queued: { run_id: '128', triggered_by: 'schedule', started_at: ago(8), step: 'queued', partitions: null },
  running: { run_id: '127', triggered_by: 'client', started_at: ago(192), step: 'running', partitions: null },
} satisfies Record<RunningStep, NodeRunning>

export const runningPartitioned: NodeRunning = {
  ...runningByStep.running,
  partitions: { running: 4, done: 3, total: 10 },
}

// ── Logs and run feedback ─────────────────────────────────────────────────────

export const logLines: LogLine[] = [
  { nodeId: 'pipeline.py:orders_clean', text: 'reading 12,408 checkout events', ts: ago(14) },
  { nodeId: 'pipeline.py:orders_clean', text: 'dropped 37 rows with no order id', ts: ago(13) },
  { nodeId: 'pipeline.py:orders_clean', text: 'DeprecationWarning: pandas.Int64Index is deprecated', stream: 'stderr', ts: ago(12) },
  { nodeId: 'pipeline.py:orders_clean', text: `wrote ${'a very long line that wraps '.repeat(5)}`, ts: ago(11) },
  { nodeId: 'pipeline.py:orders_clean', text: 'done in 4.2s', ts: ago(10) },
]

/** What the inspector is given for each feedback it shows (`runFeedback` reads each back). */
export const feedbackInputs = {
  idle: { status: 'queued', logs: [], error: null },
  streaming: { status: 'running', logs: logLines.slice(0, 3), error: null },
  output: { status: 'success', logs: logLines, error: null },
  done: { status: 'success', logs: [], error: null },
  failed: { status: 'failed', logs: logLines.slice(0, 2), error: "KeyError: 'currency'\n  File \"pipeline.py\", line 41, in orders_enriched" },
} satisfies Record<RunFeedback['kind'], { status: StatusKind; logs: LogLine[]; error: string | null }>

// ── Artifact schemas ──────────────────────────────────────────────────────────

const schemaNode = (overrides: Partial<NodeStatus>): NodeStatus => ({ ...nodeBySeverity.cached, ...overrides })

/** Each thing the node panel's schema section can say. */
export const schemaNodes: Record<string, NodeStatus> = {
  'never materialized': schemaNode({ last_materialization: null }),
  'latest attempt failed': nodeBySeverity.failed,
  'no artifact recorded': schemaNode({ last_materialization: materialized({ artifact: null, format: null }) }),
  'parquet table': schemaNode({
    shape: { type: 'DataFrame', rows: 12_371, columns: [
      { name: 'order_id', type: 'int64' }, { name: 'customer_id', type: 'int64' },
      { name: 'amount', type: 'double' }, { name: 'currency', type: 'string' }, { name: 'placed_at', type: null },
    ] },
  }),
  'json dict': schemaNode({
    shape: { type: 'dict', keys: ['total', 'by_day', 'currency'] },
    last_materialization: materialized({ format: 'json' }),
  }),
  'json list': schemaNode({
    shape: { type: 'list', rows: 40, item_types: ['dict', 'null'] },
    last_materialization: materialized({ format: 'json' }),
  }),
  pickle: schemaNode({
    shape: { type: 'sklearn.pipeline.Pipeline' },
    last_materialization: materialized({ format: 'pickle' }),
  }),
  'partitioned key': schemaNode({
    partitioned: true,
    shape: { type: 'DataFrame', rows: 812, columns: [{ name: 'session_id', type: 'string' }] },
    last_materialization: materialized({ partition: 'day=2026-10-29' }),
  }),
  'no shape recorded': schemaNode({ shape: null }),
}

// ── Reset dialog ──────────────────────────────────────────────────────────────

export const resetPhases = {
  confirm: { kind: 'confirm' },
  pending: { kind: 'pending' },
  done: {
    kind: 'done',
    result: {
      database: '.barca/metadata.db',
      moved_to: '.barca/metadata.db.reset-20261029T120000Z',
      moved: ['.barca/metadata.db.reset-20261029T120000Z'],
      undo: 'barca state restore --from .barca/metadata.db.reset-20261029T120000Z',
    },
  },
  failed: {
    kind: 'failed',
    error: 'cannot reset the metadata database while a run is in flight (r1). Nothing was changed.',
    remediation: 'Wait for it to finish, or cancel it (DELETE /runs/{run_id}).',
  },
} satisfies Record<ResetPhase['kind'], ResetPhase>

// ── Runs ──────────────────────────────────────────────────────────────────────

function runView(status: RunViewStatus, overrides: Partial<RunView> = {}): RunView {
  const live = status === 'pending' || status === 'running'
  return {
    run_id: '127',
    status,
    command: 'get',
    target: 'revenue_daily',
    files: ['pipeline.py'],
    triggered_by: 'client',
    scheduled_at: null,
    schedule_id: null,
    started_at: ago(192),
    finished_at: live ? null : ago(150),
    elapsed_seconds: live ? null : 42.3,
    steps_total: 4,
    steps_executed: live ? 1 : 3,
    steps_cached: 1,
    job: 'pipeline.py:orders_clean,pipeline.py:revenue_daily',
    attempts: null,
    client_run_id: null,
    failure: null,
    outcome: null,
    error: null,
    steps: status === 'pending' ? [] : [
      { ...step('pipeline.py:checkout_events', 'cached'), elapsed_seconds: null },
      step('pipeline.py:orders_clean', 'success'),
    ],
    steps_page: { total: status === 'pending' ? 0 : 2, truncated: false },
    logs: status === 'pending' ? [] : logLines.map((l, seq) => ({
      node_id: l.nodeId, seq, line: l.text, stream: l.stream ?? 'stdout', ts: l.ts ?? null,
    })),
    ...overrides,
  }
}

function step(node_id: string, status: string, error: string | null = null): RunView['steps'][number] {
  return {
    node_id, status, elapsed_seconds: 4.2, error, partition_key: null, attempts: 1, run_hash: null,
    artifact_path: null, artifact_format: null, artifact_size_bytes: null, error_type: null,
    error_message: null, error_traceback: null, error_phase: null, worker_exit: null,
    cpu_seconds: null, max_rss_bytes: null, output_tail: null, batch: null,
  }
}

const envelope = (kind: string, error: string, remediation: string | null) =>
  ({ error, code: kind === 'step_failed' ? 1 : kind === 'cancelled' ? 130 : 3, kind, remediation })

/** A run's details in each status the server reports (`parseRunStatus` reads each back). */
export const runByStatus = {
  pending: runView('pending'),
  running: runView('running'),
  success: runView('success', {
    outcome: { status: 'success', run_id: '127', final_output: { rows: 12371, revenue: 48210.5 } },
  }),
  failed: runView('failed', {
    error: "orders_enriched: KeyError: 'currency'",
    steps: [step('pipeline.py:orders_clean', 'success'), step('pipeline.py:orders_enriched', 'failed', "KeyError: 'currency'")],
    outcome: { status: 'failed', envelope: envelope('step_failed', "orders_enriched: KeyError: 'currency'", 'barca logs 127 --step orders_enriched has the full output.') },
  }),
  cancelled: runView('cancelled', {
    outcome: { status: 'cancelled', envelope: envelope('cancelled', 'cancelled by DELETE /runs/127', null) },
  }),
  error: runView('error', {
    steps: [],
    error: 'could not start a worker: python3 not found',
    outcome: { status: 'error', envelope: envelope('infra', 'could not start a worker: python3 not found', 'Set [python] interpreter in barca.toml.') },
  }),
  interrupted: runView('interrupted', { error: 'the server stopped while the run was in flight' }),
} satisfies Record<RunViewStatus, RunView>

export const runTruncated = runView('failed', {
  steps: [step('pipeline.py:orders_enriched', 'failed', "KeyError: 'currency'"), ...Array.from({ length: 4 }, (_, i) => step(`pipeline.py:sessionize[day=2026-10-2${i}]`, 'success'))],
  steps_page: { total: 1440, truncated: true },
})

function summary(view: RunView, id: string, overrides: Partial<RunSummary> = {}): RunSummary {
  return {
    id, handle: null, run_id: id, command: view.command, files: view.files, target: view.target,
    status: view.status, triggered_by: view.triggered_by, scheduled_at: null, schedule_id: null,
    started_at: view.started_at, finished_at: view.finished_at, elapsed_seconds: view.elapsed_seconds,
    steps_total: view.steps_total, steps_executed: view.steps_executed, steps_cached: view.steps_cached,
    error: view.error, ...overrides,
  }
}

const runList: RunList = {
  runs: Object.values(runByStatus).map((view, i) => summary(view, String(133 - i), { triggered_by: i % 3 === 0 ? 'schedule' : i % 3 === 1 ? 'client' : null })),
  total: 7,
  truncated: false,
}

/** The run list in each state the Runs page shows. */
export const runLists = {
  loading,
  'failed to load': failed('connection refused'),
  empty: ready({ runs: [], total: 0, truncated: false }),
  'every status': ready(runList),
  truncated: ready({ ...runList, total: 4120, truncated: true }),
  'refresh failed': ready(runList, 'connection refused'),
} satisfies Record<string, Remote<RunList>>

/** The run panel in each state: its request's, then each run status. */
export const runDetails: Record<string, Remote<RunView>> = {
  loading,
  unavailable: failed('run 999 not found'),
  'refresh failed': ready(runByStatus.running, 'connection refused'),
  ...Object.fromEntries(Object.entries(runByStatus).map(([status, view]) => [status, ready(view)])),
  'truncated steps': ready(runTruncated),
}

// ── Tokens ────────────────────────────────────────────────────────────────────

export const tokenList: TokenMetadata[] = [
  { id: 't1', name: 'ci', created_at: ago(86_400 * 30), expires_at: null },
  { id: 't2', name: 'laptop', created_at: ago(86_400 * 2), expires_at: new Date(NOW_MS + 86_400_000 * 90).toISOString() },
]

const issued: IssuedToken = { ...tokenList[1]!, id: 't3', name: 'notebook', token: 'brc_7Hq2k9vX0mPzLw4sT1eYbN8cR5uJ3aF6' }

/** The Tokens page in each state: a model and the token list it is shown with. */
export const tokenStates: Record<string, { model: TokensModel; tokens: Remote<TokenMetadata[]> }> = {
  'loading list': { model: INITIAL_TOKENS, tokens: loading },
  'list failed': { model: INITIAL_TOKENS, tokens: failed('401 unauthorized') },
  'no tokens': { model: INITIAL_TOKENS, tokens: ready([]) },
  tokens: { model: INITIAL_TOKENS, tokens: ready(tokenList) },
  'filling the form': { model: { ...INITIAL_TOKENS, name: 'notebook', expiry: '2027-01-01T09:00' }, tokens: ready(tokenList) },
  creating: { model: { ...INITIAL_TOKENS, name: 'notebook', request: { kind: 'creating' } }, tokens: ready(tokenList) },
  'secret shown': { model: { ...INITIAL_TOKENS, issued: { token: issued, copied: false } }, tokens: ready(tokenList) },
  'secret copied': { model: { ...INITIAL_TOKENS, issued: { token: issued, copied: true } }, tokens: ready(tokenList) },
  revoking: { model: { ...INITIAL_TOKENS, request: { kind: 'revoking', id: 't1' } }, tokens: ready(tokenList) },
  'past expiry': { model: { ...INITIAL_TOKENS, name: 'old', expiry: '2020-01-01T00:00', error: 'Choose a future expiry.' }, tokens: ready(tokenList) },
  'refresh failed': { model: INITIAL_TOKENS, tokens: ready(tokenList, '503 service unavailable') },
}

// ── Health ────────────────────────────────────────────────────────────────────

const counts = (c: Partial<HealthCounts>): HealthCounts => {
  const all = { failed: 0, running: 0, stale: 0, idle: 0, healthy: 0, pending: 0, ...c }
  return { ...all, total: all.failed + all.running + all.stale + all.idle + all.healthy + all.pending }
}

/** A scope in each state the health bar distinguishes, and the mixes that matter. */
export const healthStates: Record<string, HealthCounts> = {
  healthy: counts({ healthy: 504 }),
  'one failure in thousands': counts({ failed: 1, stale: 12, healthy: 2400 }),
  'failed and stale': counts({ failed: 3, stale: 121, healthy: 380 }),
  running: counts({ running: 40, healthy: 460, stale: 4 }),
  'not run yet': counts({ idle: 120, healthy: 8 }),
  checking: counts({ pending: 640 }),
  empty: counts({}),
}
