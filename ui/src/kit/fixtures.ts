/**
 * Fixtures for the kit page: one value per variant of each state a component shows.
 *
 * Each table is `satisfies Record<Union, …>`, so a variant added to a union (a generated
 * wire type included) is a compile error here until it has a fixture, and the kit renders
 * it from then on. `fixtures.test.ts` checks each fixture is the state its key names.
 */

import type { Connection } from '@/lib/connection'
import { NOW_MS as PROJECT_NOW_MS, RUNS, RUNS_WITHOUT_SCHEDULE, RUN_IDS, row, runDetail, scenario } from '@/lib/testdata/project'
import type { RunScenarioName } from '@/lib/testdata/project'
import { served } from '@/lib/testdata/served'
import type { CacheState } from '@/lib/cacheStateDisplay'
import type { Failure } from '@/lib/health'
import type { BarScope } from '@/components/HealthBar'
import type { CauseCounts } from '@/lib/causeDisplay'
import { causeCounts, weightsOf } from '@/lib/testdata/rollup'
import type { QueueStatus } from '@/lib/queue'
import type { RunFeedback } from '@/lib/runFeedback'
import type { Remote } from '@/lib/remote'
import { ready, failed, loading } from '@/lib/remote'
import type { ResetPhase } from '@/components/ResetDatabase'
import type { TokensModel } from '@/lib/tokensPage'
import { INITIAL_TOKENS } from '@/lib/tokensPage'
import type {
  DataSignal,
  RunNow,
  IssuedToken,
  LastMaterialization,
  LogLine,
  NodeKind,
  NodeRunning,
  NodeState,
  NodeStatus,
  QueueEntry,
  QueueView,
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

/**
 * The kit's clock: the instant the generated project (`@/lib/testdata/project`) was answered
 * at. Fixtures spelled here are dated relative to it, so elapsed times never drift.
 */
export const NOW_MS = PROJECT_NOW_MS
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

/**
 * A node in each cache state, none of which failed last (`graphState` reads each back as its
 * key). Where the generated project has a node in the state, it is that node's served row; the
 * three states it has none in (a partitioned node with nothing failed, a sensor that has not
 * reported, a result not uploaded yet) are spelled here, with the health the server gives such
 * a row on its own (`served`).
 */
export const nodeByState = {
  blocked: row('sales_report'),
  stale: row('orders_enriched'),
  never_run: row('forecast'),
  partial: node('sessionize', {
    partitioned: true,
    cache: { state: 'partial', reason: 'partitions_missing', detail: '3 of 10 keys have no cached result' },
    partitions: { total: 10, cached: 7, missing: 3, missing_keys: ['day=2026-10-27', 'day=2026-10-28', 'day=2026-10-29'], behind: 3 },
  }),
  unknown: node('checkout_events', {
    cache: { state: 'unknown', reason: 'sensor_output_unknown', detail: 'the sensor has not reported its output' },
  }),
  always_runs: row('publish_dashboard'),
  unsynced: node('product_catalog', {
    cache: { state: 'unsynced', reason: 'local_only', detail: 'cached here, not uploaded to the store yet' },
  }),
  cached: row('orders'),
} satisfies Record<CacheState, NodeState>

/** A node carrying each data-health signal (`NodeState.signals`), as `GET /state` sends it. */
export const nodeBySignal = {
  empty: node('orders_by_region', {
    partitioned: true,
    partitions: { total: 1205, cached: 1205, missing: 0, missing_keys: [], empty: 3, behind: 0 },
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
 * A node whose last attempt failed, as served: `invoices` of the generated project, `stale` with
 * reason `failed` (its newer attempts failed without replacing the result it had), failing for
 * two runs in a row.
 */
export const failedNode: NodeState = row('invoices')

/** An asset that has failed every time it ran: `failing`, `never succeeded`. */
export const neverSucceededNode: NodeState = node('fx_rates_live', {
  cache: { state: 'never_run', reason: 'no_record', detail: 'never materialized' },
  last_materialization: materialized({ status: 'failed', artifact: null, format: null, error: 'ConnectionError: rates API unreachable' }),
  last_success_at: null,
  failing_since: ago(7_200),
  consecutive_failures: 2,
})

/** A task whose last run failed, as served: `always_runs`, shown as `failing`. */
export const failedTask: NodeState = row('check__invoices')

export const runningByStep = {
  waiting: { control: { cancellable: true, owner: 'this server', reason: null }, run_id: '127', triggered_by: 'client', started_at: ago(192), step: 'waiting', label: 'in this run', partitions: null },
  queued: { control: { cancellable: true, owner: 'this server', reason: null }, run_id: '128', triggered_by: 'schedule', started_at: ago(8), step: 'queued', label: 'in a queued run', partitions: null },
  running: { control: { cancellable: true, owner: 'this server', reason: null }, run_id: '127', triggered_by: 'client', started_at: ago(192), step: 'running', label: 'running', partitions: null },
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
 * A run in the state of `name` (`run-explanations.json`): the facts the Rust test states for
 * it, the explanation `barca-service` gives for them, and the rest of a `RunView` (ids, steps,
 * logs) spelled here. No count, line or reason of the explanation is written in this file.
 */
function runView(name: RunScenarioName, overrides: Partial<RunView> = {}): RunView {
  const { run, nodes, explanation } = scenario(name)
  const { first_failure, ...facts } = run
  const live = run.status === 'pending' || run.status === 'running'
  const listed = run.status === 'pending' || run.steps_executed === 0 ? [] : [
    { ...step('pipeline.py:checkout_events', 'cached'), elapsed_seconds: null },
    step('pipeline.py:orders_clean', 'success'),
  ]
  return {
    control: { cancellable: live, owner: live ? 'this server' : 'not running', reason: live ? null : `The run already finished (${run.status}).` },
    run_id: '127',
    command: 'get',
    files: ['pipeline.py'],
    scheduled_at: null,
    schedule_id: null,
    finished_at: live ? null : ago(150),
    elapsed_seconds: live ? null : 42.3,
    client_run_id: null,
    failure: first_failure ? { node: first_failure.node_id, error: first_failure.error } : null,
    trigger_reason: null,
    waiting_for: null,
    queue: null,
    outcome: null,
    steps: listed,
    steps_page: { total: listed.length, truncated: false, ...(nodes.length > 0 ? { nodes } : {}) },
    logs: listed.length === 0 ? [] : logLines.map((l, seq) => ({
      node_id: l.nodeId, seq, line: l.text, stream: l.stream ?? 'stdout', ts: l.ts ?? null,
    })),
    ...facts,
    explanation,
    ...overrides,
  }
}

function step(node_id: string, status: string, error: string | null = null): RunView['steps'][number] {
  return {
    node_id, status, elapsed_seconds: 4.2, error, partition_key: null, attempts: 1, run_hash: null,
    artifact_path: null, artifact_format: null, artifact_size_bytes: null, error_type: null,
    error_message: null, error_traceback: null, error_phase: null, worker_exit: null,
    cpu_seconds: null, max_rss_bytes: null, rows: null, sink_seconds: null, output_tail: null, batch: null,
  }
}

const envelope = (kind: string, error: string, remediation: string | null) =>
  ({ error, code: kind === 'step_failed' ? 1 : kind === 'cancelled' ? 130 : 3, kind, remediation })

/**
 * A run's details in each status the server reports (`parseRunStatus` reads each back). The
 * finished ones are runs of the generated project as served; the rest are scenarios.
 */
export const runByStatus = {
  pending: runView('queued'),
  running: runView('running'),
  success: runDetail(RUN_IDS.first),
  skipped: runView('interrupted', { status: 'skipped', skip_reason: 'another copy holds the lock', outcome: { status: 'skipped', run_id: 'skipped', final_output: null } }),
  failed: runDetail(RUN_IDS.failed),
  cancelled: runView('cancelled by the server stopping', {
    outcome: { status: 'cancelled', envelope: envelope('cancelled', 'cancelled by DELETE /runs/127', null) },
  }),
  error: runView('error', {
    outcome: { status: 'error', envelope: envelope('infra', 'could not start a worker: python3 not found', 'Set [python] interpreter in barca.toml.') },
  }),
  interrupted: runView('interrupted'),
} satisfies Record<RunViewStatus, RunView>

const runTruncated: RunView = {
  ...runDetail(RUN_IDS.failed),
  steps: [step('pipeline.py:orders_enriched', 'failed', "KeyError: 'currency'"), ...Array.from({ length: 4 }, (_, i) => step(`pipeline.py:sessionize[day=2026-10-2${i}]`, 'success'))],
  steps_page: { total: 1440, truncated: true },
}

/**
 * A failed run of a partitioned node: 5,000 keys, 1,250 failed, 1,000 listed, and a log line per
 * key. Its node is one row with the count; the logs and the opened keys render only what is in view.
 */
const manyKeys = Array.from({ length: 1000 }, (_, i) => {
  const key = `account=acct_${String(i * 4 + 1).padStart(6, '0')}`
  const failedKey = i < 250
  return { ...step(`billing.py:invoices_by_account[${key}]`, failedKey ? 'failed' : 'success', failedKey ? 'ValueError: 3 rows with a negative total' : null), partition_key: key }
})
const runManyKeys: RunView = {
  ...runDetail(RUN_IDS.failed),
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
}

/** The list row of a scenario's run: its facts, and the `line` `barca-service` words them with. */
function summary(name: RunScenarioName, id: string, overrides: Partial<RunSummary> = {}): RunSummary {
  const view = runView(name)
  const { run, line } = scenario(name)
  return {
    control: view.control, id, handle: null, run_id: id, command: view.command, files: view.files, target: view.target,
    status: view.status, triggered_by: view.triggered_by, scheduled_at: null, schedule_id: null,
    started_at: view.started_at, finished_at: view.finished_at, elapsed_seconds: view.elapsed_seconds,
    steps_total: view.steps_total, steps_executed: view.steps_executed, steps_cached: view.steps_cached,
    error: view.error, first_failure: run.first_failure, principal: view.principal, trigger_reason: view.trigger_reason,
    cancellation: view.cancellation, line,
    ...overrides,
  }
}

/**
 * `GET /runs` as the Runs page asks for it (the schedule's runs left out), served for the
 * generated project, with a row in each status its history has none in put first.
 */
const runList: RunList = {
  ...RUNS_WITHOUT_SCHEDULE,
  runs: [
    summary('queued', '133'),
    summary('cancelled by the server stopping', '132'),
    summary('error', '131'),
    summary('interrupted', '130'),
    // Steps failed but the run itself recorded no error: the row names the first of them.
    summary('failed at a key', '125'),
    // The same with no failed step on record (a run that died before recording one).
    summary('failed with no step recorded', '124'),
    ...RUNS_WITHOUT_SCHEDULE.runs,
  ],
  total: RUNS_WITHOUT_SCHEDULE.total + 6,
}
const runActivity = runList.activity!

/** The run list in each state the Runs page shows. */
export const runLists = {
  loading,
  'failed to load': failed('connection refused'),
  empty: ready({ runs: [], total: 0, truncated: false }),
  'every status': ready(runList),
  truncated: ready({ ...runList, total: 4120, truncated: true }),
  'refresh failed': ready(runList, 'connection refused'),
} satisfies Record<string, Remote<RunList>>

/** An hour of the activity with a failed run in it, as a bar names it. */
const failedHour = runActivity.hours.find((h) => h.not_successful > 0)!
export const runHour = { since: failedHour.at, until: failedHour.until }

/** Every run, the schedule's included, as served: nothing left out of the activity. */
export const everyRunList: Remote<RunList> = ready(RUNS)

/** A filter that keeps nothing. */
export const noMatchRunList: Remote<RunList> = ready({ ...runList, runs: [], total: 0, truncated: false })

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
  control: { cancellable: true, owner: 'this server', reason: null },
  run_id: `a8ba${String(n).padStart(4, '0')}`, status: 'pending', position: n, command: 'get',
  target_line: overrides.target ?? `web.py:sessions_${String(n).padStart(2, '0')}`,
  target: `web.py:sessions_${String(n).padStart(2, '0')}`, triggered_by: n % 3 === 0 ? 'schedule' : 'api',
  schedule_id: n % 3 === 0 ? `web.py:sessions_${String(n).padStart(2, '0')}` : null, queued_at: ago(1200 - n * 30),
  started_at: null, estimated_start_at: null, wait_seconds: null, waiting_for: null, queue: null, ...overrides,
})
const runningEntry = queued(0, { status: 'running', position: null, target: 'web.py:slow_export', queued_at: ago(1500) })
const deepQueue: QueueView = {
  slots: '1 of 1 running',
  paused: false, paused_at: null, draining: false, max_runs: 1,
  running: [runningEntry], queued: Array.from({ length: 18 }, (_, i) => queued(i + 1)), capacity: null,
}

/** The Runs page's queue in each state it shows. */
export const queueStates: Record<string, { queue: QueueView | undefined; collapsed?: boolean; readOnly?: boolean; loadError?: string }> = {
  '1 running, 18 waiting': { queue: deepQueue },
  collapsed: { queue: deepQueue, collapsed: true },
  'running, nothing waiting': { queue: { ...deepQueue, queued: [] } },
  paused: { queue: { ...deepQueue, paused: true, paused_at: ago(600), running: [] } },
  draining: { queue: { ...deepQueue, draining: true, queued: deepQueue.queued.map((q) => ({ ...q, waiting_for: 'restart' as const })) } },
  'waiting for capacity': { queue: { ...deepQueue, queued: deepQueue.queued.slice(0, 2), capacity: { total: 4, source: '--capacity', admission: 'requested_first', scheduled_max_wait_seconds: 120, in_use: 3, runs: [{ run_id: runningEntry.run_id, weight: 3 }], waiting: [{ run_id: 'a8ba0001', node: 'web.py:big_join', weight: 3, waiting_seconds: 42, origin: 'requested' }, { run_id: 'a8ba0002', node: 'web.py:sync', weight: 1, waiting_seconds: 7, origin: 'scheduled' }] } } },
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

/** `now` of `GET /runs?status=running`, as the server words a rebuild in flight. */
const rebuildNow: RunNow = {
  running: [
    { node_id: 'pipeline.py:sessionize', keys: '3,120 of 5,000 keys', share: 0.624 },
    { node_id: 'pipeline.py:ltv_by_account', keys: null, share: null },
  ],
  queued: 1,
  keys: { done: 3_120, total: 5_000 },
  groups: [
    { id: 'group:finance', group: 'finance', running: 1, queued: 1 },
    { id: 'group:web', group: 'web', running: 1, queued: 0 },
  ],
  line: 'executing 2: sessionize, ltv_by_account · 1 queued · 3,120/5,000 keys',
}

/**
 * The run overview (`RunOverviewView`) in each state it says something different in: each is a
 * scenario of `run-explanations.json`, its facts and its explanation from `barca-service`.
 */
export const runOverviews: Record<string, { run: RunView; now: RunNow | null; nowMs: number }> = {
  'full rebuild in flight': {
    run: runView('full rebuild in flight', { trigger_reason: 'backfill after the bucket_ltv fix', steps_page: { total: 19_399, truncated: true, nodes: scenario('full rebuild in flight').nodes } }),
    now: rebuildNow, nowMs: NOW_MS,
  },
  'just started': { run: runView('just started'), now: null, nowMs: NOW_MS },
  'finished, served mostly from cache': { run: runView('finished, served mostly from cache'), now: null, nowMs: NOW_MS },
  'sensors and no retries': { run: runView('sensors and no retries'), now: null, nowMs: NOW_MS },
  'cancelled before it started': { run: runView('cancelled before it started'), now: null, nowMs: NOW_MS },
  'cancelled by the server stopping': { run: runView('cancelled by the server stopping'), now: null, nowMs: NOW_MS },
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

/** A scope as a bar reads it: its causes as the rollup entry states them, with the weights the server gives them. */
const barOf = (nodes: number, causes: Partial<CauseCounts>, running = 0): BarScope => {
  const counted = causeCounts(causes)
  const not_fresh = Object.values(causes).reduce((sum, n) => sum + (n ?? 0), 0)
  return { nodes, not_fresh, causes: counted, weights: weightsOf(nodes, counted), running }
}

/** A scope in each mix the bar distinguishes: one legend with the health map. */
export const barStates: Record<string, BarScope> = {
  fresh: barOf(504, {}),
  'one failure in thousands': barOf(2412, { failing: 1, catching_up: 12 }),
  'failing and catching up': barOf(562, { failing: 61, catching_up: 121 }),
  'a run working on it': barOf(504, { catching_up: 4 }, 40),
  'every cause': barOf(106, { failing: 4, overdue: 3, no_refresh_path: 7, queued: 2, undecided: 6, catching_up: 4 }),
  'nothing refreshes it': barOf(128, { no_refresh_path: 120 }),
  empty: barOf(0, {}),
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
