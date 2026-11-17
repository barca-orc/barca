/**
 * The to-do list of a scope: what holds the nodes that are not fresh, one entry per root, as
 * the rollup serves them (`GET /state/rollup`). A scope's entry lists the roots that hold its
 * nodes (`held_by`), ranked by the server with what each holds there; the root list (`roots`)
 * carries what each root is, why, and what clears it. Every not-fresh node in the scope is
 * under exactly one of them, so the list explains the whole scope and names no node twice.
 * Nothing is ranked, counted or added up here: this joins the two by root id and words them.
 * Data signals (output that looks wrong, a node older than its input) are not roots and are
 * listed apart.
 */

import { causeLine } from './causeDisplay'
import type { Glyph } from './glyph'
import { plural } from './plural'
import { formatSpan } from './time'
import type { ActionKind } from './generated/ActionKind'
import type { HeldBy } from './generated/HeldBy'
import type { RollupEntry } from './generated/RollupEntry'
import type { RollupRoot } from './generated/RollupRoot'
import type { RootKind } from './generated/RootKind'

/** A root kind's display word: the one `barca status --rollup` prints. */
export const ROOT_KIND_WORD: Record<RootKind, string> = {
  sensor_value: 'sensor value',
  code: 'code change',
  env: 'environment change',
  input: 'input change',
  never_run: 'never run',
  no_schedule: 'no schedule',
  schedule_paused: 'paused',
  scheduler_off: 'scheduler off',
  failure: 'failure',
  queue: 'run queue',
  undecided: 'undecided',
}

/** An action kind's display word: the one `barca status --rollup` prints. */
export const ACTION_WORD: Record<ActionKind, string> = {
  wait: 'wait',
  run: 'run',
  add_schedule: 'add a schedule',
  resume_schedule: 'resume',
  start_server: 'start a server',
  fix_failure: 'fix the failure',
  raise_max_runs: 'raise max_runs',
}

/** What to do where an action has no command: the hint `barca status --rollup` prints. */
const ACTION_HINT: Partial<Record<ActionKind, string>> = {
  add_schedule: 'freshness=Schedule(...) on it or on a node above it',
  raise_max_runs: 'barca serve --max-runs N or [server] max_runs in barca.toml, or space the schedules',
}

/** One root and what it holds in the scope shown. */
export interface Held {
  /** The root as served: what it is, why, what clears it, and what it holds in all. */
  root: RollupRoot
  /** What it holds underneath the scope (`held_by` of the scope's entry). */
  held: HeldBy
}

/** One entry of the list: a root, and the failures in scope that share its error. */
export interface Todo extends Held {
  /** Other failure roots with the same error (`error_group`), in served order. */
  same: Held[]
}

/**
 * The to-do list of `scope`: its `held_by`, in the order served, each joined with its root.
 * Failure roots that share an error are one entry, headed by the first of them.
 */
export function todos(scope: Pick<RollupEntry, 'held_by'> | undefined, roots: readonly RollupRoot[]): Todo[] {
  const byId = new Map(roots.map((r) => [r.id, r]))
  const groups = new Map<string, Todo>()
  const out: Todo[] = []
  for (const held of scope?.held_by ?? []) {
    const root = byId.get(held.root)
    if (!root) continue
    const first = root.error_group === null ? undefined : groups.get(root.error_group)
    if (first) {
      first.same.push({ root, held })
      continue
    }
    const todo: Todo = { root, held, same: [] }
    if (root.error_group !== null) groups.set(root.error_group, todo)
    out.push(todo)
  }
  return out
}

/** `holds 40 nodes (38 assets)`, and `here, 180 in all` in a scope that holds only some of them. */
export function heldLine({ root, held }: Held): string {
  const here = `holds ${plural(held.nodes, 'node')} (${plural(held.assets, 'asset')})`
  return held.nodes === root.nodes ? here : `${here} here, ${root.nodes.toLocaleString()} in all`
}

/** `17 catching up · 163 undecided`: what it holds in the scope, by cause, in nodes. */
export function heldCauses({ held }: Held): string {
  return causeLine(held.causes)
}

/** `2h of gap · 163 with no estimate`: the gap it holds in the scope; empty when it has neither. */
export function heldGap({ held }: Held): string {
  const parts: string[] = []
  if (held.gap_seconds > 0) parts.push(`${formatSpan(held.gap_seconds)} of gap`)
  if (held.unbounded > 0) parts.push(`${held.unbounded.toLocaleString()} with no estimate`)
  return parts.join(' · ')
}

/** What clears a root: the action's word, its command or what to do instead, and what it costs. */
export interface ActionWords {
  word: string
  /** The command to copy, when one clears the root. */
  command: string | null
  /** What to do where there is no command. */
  hint: string | null
  /** `about 7m of compute`: the typical compute time of what it holds, when sampled. */
  cost: string | null
}

export function actionWords(root: RollupRoot): ActionWords {
  const { kind, command, estimated_seconds: seconds } = root.action
  return {
    word: ACTION_WORD[kind],
    command,
    hint: command === null ? ACTION_HINT[kind] ?? null : null,
    cost: seconds === null ? null : `about ${formatSpan(Math.max(1, seconds))} of compute`,
  }
}

/** How much of the scope the head of the list explains, when there is more than a head. */
export interface Explained {
  /** How many roots the head is. */
  roots: number
  /** Nodes they hold (`cumulative_nodes` of the last of them), of the scope's `not_fresh`. */
  nodes: number
  of: number
}

/** The first `head` roots of `scope` against all its not-fresh nodes; null when they are all there is. */
export function explained(scope: Pick<RollupEntry, 'held_by' | 'not_fresh'>, head = 3): Explained | null {
  const last = scope.held_by[head - 1]
  if (!last || scope.held_by.length <= head) return null
  return { roots: head, nodes: last.cumulative_nodes, of: scope.not_fresh }
}

/** A data signal about a node: not a root, listed apart from the to-do list. */
export interface Signal {
  node: string
  glyph: Glyph
  /** What is off, in words: `suspect data: 0 rows`, `older than its input orders`. */
  text: string
}

export interface SignalsInput {
  /** Data signals and lateness, already worded, by node (`dataSignalSets`). */
  suspect?: ReadonlyMap<string, string>
  behind?: ReadonlyMap<string, string>
  /** Only signals about these nodes (a scope); every node when absent. */
  within?: ReadonlySet<string>
}

/** The data signals in scope: output that looks wrong, then what is older than its input. */
export function signals({ suspect, behind, within }: SignalsInput): Signal[] {
  const inScope = (id: string) => !within || within.has(id)
  const out: Signal[] = []
  for (const [node, detail] of suspect ?? []) if (inScope(node)) out.push({ node, glyph: 'partial', text: `suspect data: ${detail}` })
  for (const [node, detail] of behind ?? []) if (inScope(node)) out.push({ node, glyph: 'stale', text: detail })
  return out
}
