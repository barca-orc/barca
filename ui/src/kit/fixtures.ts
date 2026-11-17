/**
 * Fixtures for the kit page: one value per variant of each state a component shows.
 *
 * Each table is `satisfies Record<Union, …>`, so a variant added to a union (a generated
 * wire type included) is a compile error here until it has a fixture, and the kit renders
 * it from then on. `fixtures.test.ts` checks each fixture is the state its key names.
 */

import type { Connection } from '@/lib/connection'
import { served } from '@/lib/testdata/served'
import type { CacheState } from '@/lib/cacheStateDisplay'
import type { Failure, ScopeHealth } from '@/lib/health'
import { counts as healthCountsOf } from '@/lib/testdata/rollup'
import type { Health } from '@/lib/healthDisplay'
import type { QueueStatus } from '@/lib/queue'
import type { RunFeedback } from '@/lib/runFeedback'
import type { Remote } from '@/lib/remote'
import { ready, failed, loading } from '@/lib/remote'
import type { ResetPhase } from '@/components/ResetDatabase'
import type { TokensModel } from '@/lib/tokensPage'
import { INITIAL_TOKENS } from '@/lib/tokensPage'
import type {
  DataSignal,
  ActiveStep,
  IssuedToken,
  LastMaterialization,
  LogLine,
  NodeGroup,
  NodeKind,
  NodeRunning,
  NodeState,
  NodeStatus,
  QueueEntry,
  QueueView,
  PlanReasons,
  RunList,
  RunningStep,
  RunSummary,
  RunExplanation,
  RunView,
  ReasonRow,
  WhyRan,
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
  queued: 1, running: 1, success: 1, failed: 1, cancelled: 1, skipped: 1,
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
  rows: null,
  ...overrides,
})

function node(name: string, overrides: Partial<NodeState>): NodeState {
  return served({
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
    prev_run: null,
    late_since: null,
    signals: [],
    running: null,
    last_success_at: ago(3_600),
    failing_since: null,
    consecutive_failures: 0,
    ...overrides,
  })
}

/** A node in each cache state, in the CLI's words (`graphState` reads each back as its key). */
export const nodeByState = {
  blocked: node('promo_plan', {
    partitioned: true,
    cache: { state: 'blocked', reason: 'upstream_failed', detail: '39 keys wait on failed upstream keys' },
    partitions: { total: 3077, cached: 3038, missing: 39, missing_keys: [] },
  }),
  stale: node('revenue_daily', {
    cache: { state: 'stale', reason: 'changed', detail: 'code changed since the last run', changed: ['code'] },
  }),
  never_run: node('fx_rates', {
    cache: { state: 'never_run', reason: 'no_record', detail: 'never materialized' },
    last_materialization: null,
    durations: null,
    last_success_at: null,
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
    last_materialization: materialized({ artifact: null, format: null }),
  }),
  unsynced: node('product_catalog', {
    cache: { state: 'unsynced', reason: 'local_only', detail: 'cached here, not uploaded to the store yet' },
  }),
  cached: node('orders_clean', {}),
} satisfies Record<CacheState, NodeState>

/** A node carrying each data-health signal (`NodeState.signals`), as `GET /state` sends it. */
export const nodeBySignal = {
  empty: node('orders_by_region', {
    partitioned: true,
    partitions: { total: 1205, cached: 1205, missing: 0, missing_keys: [], empty: 3 },
    signals: [{ kind: 'empty', empty_keys: 3, keys: 1205, previous_empty_keys: 0 }],
  }),
  rows_changed: node('invoices_clean', {
    last_materialization: materialized({ rows: 300 }),
    signals: [{ kind: 'rows_changed', changed_keys: 1, key: null, rows: 300, previous_rows: 1200 }],
  }),
  behind_input: node('orders_all', {
    cache: { state: 'unknown', reason: 'cutoff_pending', detail: 'an input with cutoff=True runs first' },
    last_success_at: ago(2 * 86_400),
    signals: [{ kind: 'behind_input', input: 'pipeline.py:orders_by_region', input_success_at: sqlTime(3_600), last_success_at: sqlTime(2 * 86_400) }],
  }),
} satisfies Record<DataSignal['kind'], NodeState>

/**
 * A node whose last attempt failed: the fact shown beside its state. `stale` with reason `failed`
 * is what the server says of an asset whose newer attempt failed without replacing its result.
 */
export const failedNode: NodeState = node('orders_enriched', {
  cache: { state: 'stale', reason: 'failed', detail: 'the last attempt failed; a cached result from before remains' },
  last_materialization: materialized({ status: 'failed', artifact: null, format: null, error: "KeyError: 'currency'" }),
  last_success_at: ago(86_400 * 2),
  failing_since: ago(50_400),
  consecutive_failures: 3,
})

/** An asset that has failed every time it ran: `failing`, `never succeeded`. */
export const neverSucceededNode: NodeState = node('fx_rates_live', {
  cache: { state: 'never_run', reason: 'no_record', detail: 'never materialized' },
  last_materialization: materialized({ status: 'failed', artifact: null, format: null, error: 'ConnectionError: rates API unreachable' }),
  last_success_at: null,
  failing_since: ago(7_200),
  consecutive_failures: 2,
})

/** A task whose last run failed: `always_runs`, shown as `failing`. */
export const failedTask: NodeState = node('check__orders', {
  kind: 'task',
  cache: { state: 'always_runs', reason: 'task', detail: 'a task runs every time' },
  last_materialization: materialized({ status: 'failed', artifact: null, format: null, error: 'AssertionError: 3 rows with negative units' }),
  last_success_at: ago(86_400),
  failing_since: ago(3_600),
  consecutive_failures: 1,
})

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

const schemaNode = (overrides: Partial<NodeStatus>): NodeStatus => ({ ...nodeByState.cached, ...overrides })

/** Each thing the node panel's schema section can say. */
export const schemaNodes: Record<string, NodeStatus> = {
  'never materialized': schemaNode({ last_materialization: null }),
  'latest attempt failed': failedNode,
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

/**
 * A run's explanation as the server words it (`barca-core`'s `run_explanation`): the kit shows
 * what the page renders, so these are written out, not derived.
 */
function explanation(overrides: Partial<RunExplanation> = {}): RunExplanation {
  return {
    targets: ['pipeline.py:orders_clean', 'pipeline.py:revenue_daily'],
    requested_by: 'client',
    attempts: null,
    steps: '3 ok · 1 cached',
    target_outcomes: [],
    cancellation: null,
    why: { headline: 'Every step was served from cache.', groups: [], omitted: 0, partial: false },
    progress: null,
    slowest: [],
    ...overrides,
  }
}

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
    principal: null,
    trigger_reason: null,
    retries: null,
    plan_reasons: null,
    cancellation: null,
    explanation: explanation(live
      ? { steps: '1 ok · 1 cached', why: { headline: 'No step decided to run yet.', groups: [], omitted: 0, partial: true }, progress: { done: 2, total: 4, percent: 50, executed: 1, cached: 1, eta_seconds: null, eta: null } }
      : {}),
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
    cpu_seconds: null, max_rss_bytes: null, rows: null, output_tail: null, batch: null,
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

const runTruncated = runView('failed', {
  steps: [step('pipeline.py:orders_enriched', 'failed', "KeyError: 'currency'"), ...Array.from({ length: 4 }, (_, i) => step(`pipeline.py:sessionize[day=2026-10-2${i}]`, 'success'))],
  steps_page: { total: 1440, truncated: true },
})

/**
 * A failed run of a partitioned node: 5,000 keys, 1,250 failed, 1,000 listed, and a log line per
 * key. Its node is one row with the count; the logs and the opened keys render only what is in view.
 */
const manyKeys = Array.from({ length: 1000 }, (_, i) => {
  const key = `account=acct_${String(i * 4 + 1).padStart(6, '0')}`
  const failedKey = i < 250
  return { ...step(`billing.py:invoices_by_account[${key}]`, failedKey ? 'failed' : 'success', failedKey ? 'ValueError: 3 rows with a negative total' : null), partition_key: key }
})
const runManyKeys = runView('failed', {
  error: 'invoices_by_account: 1,250 keys failed',
  steps_total: 7644,
  steps_executed: 5021,
  steps: manyKeys,
  steps_page: {
    total: 5885,
    truncated: true,
    nodes: [
      { node_id: 'billing.py:invoices_by_account', keys: 5000, failed: 1250, elapsed_seconds: 57.9, calls: 0, status: 'ran' },
      { node_id: 'billing.py:invoice_totals', keys: 0, failed: 0, elapsed_seconds: 0, calls: 0, status: 'skipped', blocked_by: 'billing.py:invoices_by_account' },
      { node_id: 'web.py:events_by_day', keys: 0, failed: 0, elapsed_seconds: 0, calls: 0, status: 'cached' },
    ],
  },
  logs: Array.from({ length: 30_000 }, (_, seq) => ({
    node_id: `billing.py:invoices_by_account[account=acct_${String(Math.floor(seq / 5)).padStart(6, '0')}]`,
    seq, line: `exporting acct_${String(Math.floor(seq / 5)).padStart(6, '0')}: page ${seq % 5} of 5, 120 rows`, stream: 'stdout' as const, ts: ago(150),
  })),
})

function summary(view: RunView, id: string, overrides: Partial<RunSummary> = {}): RunSummary {
  return {
    id, handle: null, run_id: id, command: view.command, files: view.files, target: view.target,
    status: view.status, triggered_by: view.triggered_by, scheduled_at: null, schedule_id: null,
    started_at: view.started_at, finished_at: view.finished_at, elapsed_seconds: view.elapsed_seconds,
    steps_total: view.steps_total, steps_executed: view.steps_executed, steps_cached: view.steps_cached,
    error: view.error, first_failure: null, principal: view.principal, trigger_reason: view.trigger_reason,
    cancellation: view.cancellation, ...overrides,
  }
}

const runList: RunList = {
  runs: [
    ...Object.values(runByStatus).map((view, i) => summary(view, String(133 - i), { triggered_by: i % 3 === 0 ? 'schedule' : i % 3 === 1 ? 'client' : null })),
    // Steps failed but the run itself recorded no error: the row names the first of them.
    summary(runByStatus.failed, '125', {
      triggered_by: 'schedule', error: null, steps_total: 28_404, steps_executed: 21_950, steps_cached: 0,
      first_failure: { node_id: 'pipeline.py:sessionize[day=2026-10-27]', error: "KeyError: 'currency'" },
    }),
    // The same with no failed step on record (a run that died before recording one).
    summary(runByStatus.failed, '124', { error: null, started_at: ago(7_500) }),
  ],
  total: 9,
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

/** The latest 100 of 37,955 runs: a filter counts over those, and says so. */
export const partialRunList: Remote<RunList> = ready({ ...runList, total: 37_955, truncated: true })

/** The run panel in each state: its request's, then each run status. */
export const runDetails: Record<string, Remote<RunView>> = {
  loading,
  unavailable: failed('run 999 not found'),
  'refresh failed': ready(runByStatus.running, 'connection refused'),
  ...Object.fromEntries(Object.entries(runByStatus).map(([status, view]) => [status, ready(view)])),
  'truncated steps': ready(runTruncated),
  '5,000 keys, 30,000 log lines': ready(runManyKeys),
}

// ── Queue ─────────────────────────────────────────────────────────────────────

const queued = (n: number, overrides: Partial<QueueEntry> = {}): QueueEntry => ({
  run_id: `a8ba${String(n).padStart(4, '0')}`, status: 'pending', position: n, command: 'get',
  target: `web.py:sessions_${String(n).padStart(2, '0')}`, triggered_by: n % 3 === 0 ? 'schedule' : 'api',
  schedule_id: n % 3 === 0 ? `web.py:sessions_${String(n).padStart(2, '0')}` : null, queued_at: ago(1200 - n * 30), ...overrides,
})
const runningEntry = queued(0, { status: 'running', position: null, target: 'web.py:slow_export', queued_at: ago(1500) })
const deepQueue: QueueView = {
  paused: false, paused_at: null, draining: false, max_runs: 1,
  running: [runningEntry], queued: Array.from({ length: 18 }, (_, i) => queued(i + 1)), capacity: null,
}

/** The Runs page's queue in each state it shows. */
export const queueStates: Record<string, { queue: QueueView | undefined; collapsed?: boolean; readOnly?: boolean; loadError?: string }> = {
  '1 running, 18 waiting': { queue: deepQueue },
  collapsed: { queue: deepQueue, collapsed: true },
  'running, nothing waiting': { queue: { ...deepQueue, queued: [] } },
  paused: { queue: { ...deepQueue, paused: true, paused_at: ago(600), running: [] } },
  draining: { queue: { ...deepQueue, draining: true } },
  'waiting for capacity': { queue: { ...deepQueue, queued: deepQueue.queued.slice(0, 2), capacity: { total: 4, source: '--capacity', in_use: 3, runs: [{ run_id: runningEntry.run_id, weight: 3 }], waiting: [{ run_id: 'a8ba0001', node: 'web.py:big_join', weight: 3, waiting_seconds: 42 }] } } },
  'read-only': { queue: deepQueue, readOnly: true },
  unavailable: { queue: undefined, loadError: 'connection refused' },
}

/** The queue line every page shows, in each state. */
export const queueStatusStates: Record<string, QueueStatus | null> = {
  'no queue': null,
  'runs waiting': { running: 1, waiting: 18, oldestWaitSeconds: 1200, paused: false, draining: false },
  'waiting for hours': { running: 4, waiting: 312, oldestWaitSeconds: 3 * 3600 + 12 * 60, paused: false, draining: false },
  paused: { running: 0, waiting: 3, oldestWaitSeconds: 600, paused: true, draining: false },
  draining: { running: 1, waiting: 2, oldestWaitSeconds: 45, paused: false, draining: true },
}

// ── A run that explains itself (issue 925) ────────────────────────────────────────

const rebuildReasons: PlanReasons = {
  nodes: 469,
  steps: 23_278,
  omitted: 3,
  reasons: [
    { category: 'code_changed', subject: 'helper pipelines/common.py:bucket_ltv', nodes: 412, steps: 21_104, example: 'pipeline.py:ltv_by_account' },
    { category: 'new_keys', subject: null, nodes: 2, steps: 1_205, example: 'pipeline.py:sessionize' },
    { category: 'inputs_changed', subject: 'input pipeline.py:orders_clean', nodes: 40, steps: 957, example: 'pipeline.py:revenue_daily' },
    { category: 'never_run', subject: null, nodes: 12, steps: 12, example: 'pipeline.py:churn_model' },
  ],
}

const reasonRow = (subject: string | null, nodes: number, steps: number, example: string, of: number): ReasonRow => {
  const counts = steps === nodes ? `${nodes.toLocaleString()} node${nodes === 1 ? '' : 's'}` : `${steps.toLocaleString()} steps of ${nodes.toLocaleString()} node${nodes === 1 ? '' : 's'}`
  const name = example.slice(example.lastIndexOf(':') + 1)
  const example_text = nodes > 1 ? `e.g. ${name}` : name
  return { subject, counts, nodes, steps, share: steps / of, example, example_text, line: subject ? `${counts}: ${subject}` : `${counts}${nodes > 1 ? ',' : ':'} ${example_text}` }
}

const rebuildWhy: WhyRan = {
  headline: '23,278 steps decided to run so far: 21,104 code changed, 1,205 new partition keys, 957 input changed, and 1 more reason',
  groups: [
    { category: 'code_changed', label: 'code changed', steps: 21_104, rows: [reasonRow('helper pipelines/common.py:bucket_ltv', 412, 21_104, 'pipeline.py:ltv_by_account', 23_278)] },
    { category: 'new_keys', label: 'new partition keys', steps: 1_205, rows: [reasonRow(null, 2, 1_205, 'pipeline.py:sessionize', 23_278)] },
    { category: 'inputs_changed', label: 'input changed', steps: 957, rows: [reasonRow('pipeline.py:orders_clean', 40, 957, 'pipeline.py:revenue_daily', 23_278)] },
    { category: 'never_run', label: 'never run', steps: 12, rows: [reasonRow(null, 12, 12, 'pipeline.py:churn_model', 23_278)] },
  ],
  omitted: 3,
  partial: true,
}

/** The audit's run: 29 steps, 27 of them sensors, none retried. */
const sensorsWhy: WhyRan = {
  headline: '29 steps ran: 27 sensors (always run), 2 never run',
  groups: [
    { category: 'sensor', label: 'sensors (always run)', steps: 27, rows: [reasonRow(null, 27, 27, 'pipeline.py:orders_feed', 29)] },
    { category: 'never_run', label: 'never run', steps: 2, rows: [reasonRow(null, 2, 2, 'pipeline.py:orders_clean', 29)] },
  ],
  omitted: 0,
  partial: false,
}

const rebuildActive: ActiveStep[] = [
  { node_id: 'pipeline.py:sessionize', step: 'running', partitions: { running: 880, done: 3_120, total: 5_000 } },
  { node_id: 'pipeline.py:ltv_by_account', step: 'running', partitions: null },
  { node_id: 'pipeline.py:revenue_daily', step: 'queued', partitions: null },
]

const rebuildGroups: NodeGroup[] = [
  { id: 'group:web', name: 'web', description: '', members: ['pipeline.py:sessionize'], output: 'pipeline.py:sessionize' },
  { id: 'group:finance', name: 'finance', description: '', members: ['pipeline.py:ltv_by_account', 'pipeline.py:revenue_daily'], output: 'pipeline.py:revenue_daily' },
]

/** The run overview (`RunOverviewView`) in each state it says something different in. */
export const runOverviews: Record<string, { run: RunView; active: ActiveStep[]; groups: NodeGroup[]; nowMs: number }> = {
  'full rebuild in flight': {
    run: runView('running', {
      triggered_by: 'api', principal: 'token:ci-bot', trigger_reason: 'backfill after the bucket_ltv fix',
      started_at: ago(360), steps_total: 23_278, steps_executed: 19_399, steps_cached: 0, attempts: 19_461, retries: 62,
      plan_reasons: rebuildReasons,
      steps_page: { total: 19_399, truncated: true, nodes: [
        { node_id: 'pipeline.py:sessionize', keys: 3_120, failed: 0, elapsed_seconds: 1_840.5, calls: 0 },
        { node_id: 'pipeline.py:ltv_by_account', keys: 1, failed: 0, elapsed_seconds: 212.3, calls: 0 },
      ] },
      explanation: explanation({
        requested_by: 'api · token:ci-bot', attempts: '19,399 steps, 62 retries', steps: '19,399 ok · 0 cached', why: rebuildWhy,
        progress: { done: 19_399, total: 23_278, percent: 83, executed: 19_399, cached: 0, eta_seconds: 72, eta: 'about 1m left' },
        slowest: [{ node_id: 'pipeline.py:sessionize', seconds: 1_840.5, keys: 3_120 }, { node_id: 'pipeline.py:ltv_by_account', seconds: 212.3, keys: 1 }],
      }),
    }),
    active: rebuildActive, groups: rebuildGroups, nowMs: NOW_MS,
  },
  'just started': {
    run: runView('running', {
      triggered_by: 'schedule', started_at: ago(4), steps_total: 23_278, steps_executed: 0, steps_cached: 0, steps: [], steps_page: { total: 0, truncated: false },
      explanation: explanation({
        requested_by: 'schedule', steps: '0 ok · 0 cached', why: { headline: 'No step decided to run yet.', groups: [], omitted: 0, partial: true },
        progress: { done: 0, total: 23_278, percent: 0, executed: 0, cached: 0, eta_seconds: null, eta: null },
      }),
    }),
    active: [], groups: [], nowMs: NOW_MS,
  },
  'finished, served mostly from cache': {
    run: runView('success', {
      attempts: 3, retries: 0,
      plan_reasons: { nodes: 2, steps: 3, omitted: 0, reasons: [{ category: 'inputs_changed', subject: 'sensor pipeline.py:orders_feed', nodes: 2, steps: 3, example: 'pipeline.py:orders_clean' }] },
      explanation: explanation({
        attempts: '3 steps, no retries',
        why: {
          headline: '3 steps ran: 3 input changed',
          groups: [{ category: 'inputs_changed', label: 'input changed', steps: 3, rows: [reasonRow('sensor pipeline.py:orders_feed', 2, 3, 'pipeline.py:orders_clean', 3)] }],
          omitted: 0, partial: false,
        },
      }),
    }),
    active: [], groups: [], nowMs: NOW_MS,
  },
  'sensors and no retries': {
    run: runView('success', {
      steps_total: 29, steps_executed: 29, steps_cached: 0, attempts: 29, retries: 0,
      explanation: explanation({ attempts: '29 steps, no retries', steps: '29 ok · 0 cached', why: sensorsWhy }),
    }),
    active: [], groups: [], nowMs: NOW_MS,
  },
  'cancelled before it started': {
    run: runView('cancelled', {
      triggered_by: 'api', principal: 'token:ops', steps_total: 0, steps_executed: 0, steps_cached: 0, steps: [], steps_page: { total: 0, truncated: false },
      cancellation: { by: 'request', principal: 'token:ops', started: false, waiting_behind: ['a7e8f06a4890'], same_as: 'a7e8f06a4890' },
      explanation: explanation({
        requested_by: 'api · token:ops', steps: '0 started · 0 cached',
        cancellation: {
          headline: 'Not started: the same target was already running as run a7e8f06a4890.',
          detail: 'Cancelled by token:ops while it waited in the queue; it executed nothing.',
          run: 'a7e8f06a4890',
        },
        why: { headline: 'It was cancelled before it decided what to run.', groups: [], omitted: 0, partial: false },
      }),
    }),
    active: [], groups: [], nowMs: NOW_MS,
  },
  'cancelled by the server stopping': {
    run: runView('cancelled', {
      cancellation: { by: 'shutdown', principal: null, started: true, waiting_behind: [], same_as: null },
      explanation: explanation({
        steps: '3 started · 1 cached',
        cancellation: { headline: 'Cancelled by the server stopping while it ran.', detail: 'Steps that finished before the cancel are cached.', run: null },
      }),
    }),
    active: [], groups: [], nowMs: NOW_MS,
  },
}

// ── Tokens ────────────────────────────────────────────────────────────────────

const tokenList: TokenMetadata[] = [
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

/** A rollup entry as served: its nodes by health, how many there are, and run work beside them. */
const scopeOf = (nodes: number, states: Partial<Record<Health, number>>, running = 0): ScopeHealth => ({ health: healthCountsOf(states), nodes, running })

/** A scope in each state the health bar distinguishes, and the mixes that matter. */
export const healthStates: Record<string, ScopeHealth> = {
  ok: scopeOf(504, { ok: 504 }),
  'one failure in thousands': scopeOf(2412, { failing: 1, behind: 12, ok: 2399 }),
  'failing and blocking': scopeOf(562, { failing: 3, blocked: 58, behind: 121, ok: 380 }),
  running: scopeOf(504, { ok: 500, behind: 4 }, 40),
  'every health': scopeOf(106, { failing: 2, blocked: 2, suspect: 1, behind: 8, undecided: 6, never_run: 7, ok: 80 }),
  'not run yet': scopeOf(128, { never_run: 120, ok: 8 }),
  empty: scopeOf(0, {}),
}

// ── Failures ──────────────────────────────────────────────────────────────────

const failureOf = (id: string, headline: string, held: number, kind: Failure['kind'] = 'asset'): Failure => ({
  id: `billing.py:${id}`, name: id, kind, headline, error: `${headline}\n  File "billing.py", line 12`,
  // As the server groups them: the headline with its numbers replaced by `#`.
  group: headline.replace(/\d[\d,._]*\d|\d/g, '#'),
  at: sqlTime(3_600), failingSince: sqlTime(50_400), failures: 3, held, heldAssets: held, missingKeys: null,
})

/** The Failures view's list: errors shared by several failures, and one on its own. */
export const failureLists = {
  'by cause': [
    failureOf('invoices_00', 'row count fell 64% day over day', 41),
    failureOf('invoices_01', 'row count fell 12% day over day', 9),
    failureOf('check__ledger', "KeyError: 'currency'", 1, 'task'),
  ],
  // A long error heads its cause wrapped (its count stays in view); rows with the same words do not repeat it.
  'long error, said once': [
    failureOf('invoices_by_account', 'ValueError: invoice export for acct_000007 has 3 rows with a negative total, 2 rows with a null currency and 1 row dated in the future; the export was rejected by the ledger reconciliation step before it reached the warehouse', 59),
    failureOf('invoices_by_region', 'ValueError: invoice export for acct_000007 has 3 rows with a negative total, 2 rows with a null currency and 1 row dated in the future; the export was rejected by the ledger reconciliation step before it reached the warehouse', 4),
  ],
  nothing: [],
} satisfies Record<string, Failure[]>
