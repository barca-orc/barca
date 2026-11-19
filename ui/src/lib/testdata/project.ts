/**
 * One project as a server answers about it: test and kit data only (the layering guard keeps
 * the app from importing it).
 *
 * Nobody typed these values. `crates/barca-core/src/ui_fixture_tests.rs` copies the fixture
 * project (`crates/barca-core/testdata/ui_fixture/`: `orders.py`, `billing.py`), records a
 * history for it, and writes what `barca-core` says of it at `NOW` to `served/*.json`; the test
 * fails when a file here differs from what the code produces. Regenerate them with
 *
 *     BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-core ui_fixture
 *
 * and review the diff: it is what the UI will be told. A test that asserts on a value the
 * server computes (health, reason, cause, root, an estimate, a weight, a count, a headline, a
 * schedule's standing, a run's explanation) reads it from here; a test of presentation alone
 * (a layout, the words for a given number, an empty state) may still spell its input.
 *
 * What the project holds, by node:
 *
 * | node | state |
 * |---|---|
 * | `orders.py:orders`, `customers`, `fx_rates` | fresh (`customers` is paused: `PAUSES`) |
 * | `orders.py:orders_feed` | a sensor on a 15-minute schedule, on time |
 * | `orders.py:orders_enriched` | stale, and a queued run covers it |
 * | `orders.py:revenue_hourly` | stale below it, on an hourly schedule whose tick is queued (group `Reporting`) |
 * | `orders.py:publish_dashboard` | a task that ran (group `Reporting`) |
 * | `orders.py:nightly_export` | a task on a schedule that is late: nothing decided today's slot |
 * | `orders.py:sales_by_day` | partitioned: 3 keys cached, 1 failed, 1 never computed |
 * | `orders.py:sales_report` | partitioned, blocked by the failed key above it |
 * | `orders.py:forecast` | never run, and nothing would run it |
 * | `billing.py:invoices`, `check__invoices` | failing (an asset whose code changed after its one success, and a task) |
 * | `billing.py:invoice_totals`, `ledger` | blocked by `invoices` (one stale, one never run) |
 *
 * `STATE_PINNED` and `PINS` are the same project once `fx_rates` is pinned. One run executes
 * (a backfill of `sales_report`, since 13:58) and two wait behind it for the server's one run
 * slot: the 14:00 tick of `revenue_hourly` and a run of `orders_enriched`.
 */

import type { Rollup, RollupEntry, RollupRoot } from '../rollupView'
import type { AssetPin, NodeState, RunExplanation, RunList, RunSummary, RunView } from '../types'
import type { RunLine } from '../generated/RunLine'
import type { RunNodeSummary } from '../generated/RunNodeSummary'
import type { PauseCover } from '../pauses'
import type { Pauses } from '../types'
import pauses from './served/pauses.json'
import pauseCovers from './served/pause-covers.json'
import pins from './served/pins.json'
import rollup from './served/rollup.json'
import runDetails from './served/run-details.json'
import runExplanations from './served/run-explanations.json'
import runsWithoutSchedule from './served/runs-without-schedule.json'
import runs from './served/runs.json'
import statePinned from './served/state-pinned.json'
import state from './served/state.json'

/** The instant every answer here was computed at (`NOW` in the Rust test). */
export const NOW = '2026-11-17T14:57:00Z'
export const NOW_MS = Date.parse(NOW)

// JSON modules type every string as `string`, so the files are cast to the generated wire
// types; the Rust test is what keeps their content the server's.
/** `GET /state`. */
export const STATE = state as unknown as NodeState[]
/** `GET /state/rollup`. */
export const ROLLUP = rollup as unknown as Rollup
/** `GET /runs`. */
export const RUNS = runs as unknown as RunList
/** `GET /runs?not_triggered_by=schedule`: what the Runs page asks for until told to show the schedule's runs. */
export const RUNS_WITHOUT_SCHEDULE = runsWithoutSchedule as unknown as RunList
/** `GET /runs/{run_id}` of the runs in `RUN_IDS`. */
export const RUN_DETAILS = runDetails as unknown as Record<string, RunView>
/** `GET /pauses`. */
export const PAUSES = pauses as unknown as Pauses

/**
 * `covers` of `GET /pauses?target=<id>&downstream=true` for the node named `name`: what a pause
 * of it and everything below it would cover, by the scheduler's own walk.
 */
export function pauseCover(name: string): PauseCover {
  const covers = pauseCovers as unknown as Record<string, PauseCover>
  const id = Object.keys(covers).find((k) => k.endsWith(`:${name}`))
  if (!id) throw new Error(`no node ${name} in served/pause-covers.json`)
  return covers[id]!
}
/** `GET /state` once `fx_rates` is pinned, and `GET /pins` then. */
export const STATE_PINNED = statePinned as unknown as NodeState[]
export const PINS = pins as unknown as { pins: AssetPin[] }

function found<T>(value: T | undefined, what: string): T {
  if (value === undefined) throw new Error(`${what} is not in the generated fixture: regenerate it (crates/barca-core/src/ui_fixture_tests.rs)`)
  return value
}

/**
 * A run no recorded history holds (queued, in flight, cancelled, of 23,278 steps): the facts a
 * server would have of it (`run`, `nodes`), stated in the Rust test, with the explanation and
 * the list-row words `barca-core` gives for exactly those facts.
 */
export interface RunScenario {
  run: Pick<RunView, 'status' | 'target' | 'job' | 'triggered_by' | 'principal' | 'attempts' | 'retries' | 'steps_total' | 'steps_executed' | 'steps_cached' | 'started_at' | 'plan_reasons' | 'cancellation' | 'error'> & Pick<RunSummary, 'first_failure'>
  nodes: RunNodeSummary[]
  explanation: RunExplanation
  line: RunLine
}
const SCENARIOS = runExplanations as unknown as Record<string, RunScenario>
export type RunScenarioName = keyof typeof runExplanations

/** The run scenario `name`. */
export function scenario(name: RunScenarioName): RunScenario {
  return found(SCENARIOS[name], `run scenario ${name}`)
}

/** The runs with a detail, by what they are. */
export const RUN_IDS = {
  /** The first run of everything in `orders.py`: 9 steps, all ok. */
  first: 'a1f0c0000001',
  /** `get invoice_totals` through the API: `invoices` failed, with two log lines. */
  failed: 'a1f0c0000005',
  /** One key of `sales_by_day`, which failed. */
  failedKey: 'a1f0c0000006',
  /** A tick of `orders_feed`. */
  scheduled: '5c4ed0001445',
} as const

/** The `GET /state` row of `name` (`orders_enriched`, or a full id). */
export function row(name: string, rows: readonly NodeState[] = STATE): NodeState {
  return found(rows.find((n) => n.id === name || n.name === name), `node ${name}`)
}

/** The rollup entry `id` (`root`, `file:orders.py`, `group:orders.py:reporting`, a node id). */
export function entry(id: string): RollupEntry {
  return found(ROLLUP.entries.find((e) => e.id === id), `rollup entry ${id}`)
}

/** The root `id` of the rollup (`failure:billing.py:invoices`, `scheduler_off`). */
export function root(id: string): RollupRoot {
  return found(ROLLUP.roots.find((r) => r.id === id), `root ${id}`)
}

/** The run list row of run `id`. */
export function runRow(id: string): RunSummary {
  return found(RUNS.runs.find((r) => r.id === id), `run ${id}`)
}

/** The detail of run `id` (one of `RUN_IDS`). */
export function runDetail(id: string): RunView {
  return found(RUN_DETAILS[id], `the detail of run ${id}`)
}
