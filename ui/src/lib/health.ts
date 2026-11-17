/**
 * What is broken and how much it holds, in the words a page shows: the rollup's entries and
 * roots (`GET /state/rollup`, classified and summed in `barca-core`) joined with the rows of
 * `GET /state`. Nothing here classifies, counts or attributes; it words and joins.
 */

import { healthDisplay, HEALTH_ORDER, type Health } from './healthDisplay'
import type { Glyph } from './glyph'
import { memberHealth, type GraphState } from './graphState'
import type { RollupEntry, RollupRoot } from './rollupView'
import type { PartitionErrorGroup } from './generated/PartitionErrorGroup'
import type { Tone } from './tone'
import type { NodeState, PartitionKeys } from './types'

/** A node's place in the graph: what `GET /assets` says, all a lineage needs. */
export interface GraphLink {
  id: string
  inputs: string[]
}

/**
 * A scope's health as a bar shows it: the entry of the rollup (`GET /state/rollup`) for that
 * scope. `health` counts its nodes, each once, by the server's word; `running` is how many of
 * them a run is working on, a separate fact beside their health. Nothing here counts: a bar,
 * a group's box and the map read the same entry, so they agree.
 */
export type ScopeHealth = Pick<RollupEntry, 'health' | 'running' | 'nodes'>

/** A segment of the bar or a part of its label: a health, or run work. */
type HealthPart = Health | 'running'

/** Most urgent first: the order of the bar's segments. A failure leads, then run work. */
export const HEALTH_PARTS: readonly HealthPart[] = ['failing', 'running', ...HEALTH_ORDER.filter((h) => h !== 'failing')]

/** The tone of a part of the bar. */
export function healthPartTone(part: HealthPart): Tone {
  return part === 'running' ? 'running' : healthDisplay(part).tone
}

/** The glyph of a part of the bar: the colour of its segment. */
export function healthPartGlyph(part: HealthPart): Glyph {
  return part === 'running' ? 'running' : healthDisplay(part).glyph
}

/** How many nodes a part of `scope` is. */
export function partCount(scope: ScopeHealth, part: HealthPart): number {
  return part === 'running' ? scope.running : scope.health[part]
}

/** The word of a part: `behind`, `running`. */
function partWord(part: HealthPart): string {
  return part === 'running' ? part : healthDisplay(part).label
}

export interface HealthLabelPart {
  /** What the part counts: a health, or run work. */
  key: HealthPart
  tone: Tone
  glyph: Glyph
  /** How many nodes it counts. */
  count: number
  /** Its word alone: `failing`, `behind`. */
  word: string
  /** The part in words: `3 behind`. */
  text: string
}

/** Every non-empty part, most urgent first, as the words a label shows. */
export function healthParts(scope: ScopeHealth, limit = HEALTH_PARTS.length): HealthLabelPart[] {
  const parts: HealthLabelPart[] = []
  for (const part of HEALTH_PARTS) {
    const count = partCount(scope, part)
    if (count > 0) parts.push({ key: part, tone: healthPartTone(part), glyph: healthPartGlyph(part), count, word: partWord(part), text: `${count.toLocaleString()} ${partWord(part)}` })
  }
  return parts.slice(0, limit)
}

/** `2 failing · 12 blocked · 380 ok`. */
export function healthLabel(scope: ScopeHealth, limit?: number): string {
  return healthParts(scope, limit).map((p) => p.text).join(' · ')
}

/** A sort key: how many nodes need attention short of failing (blocked, suspect or behind). */
export function needsAttention(scope: ScopeHealth): number {
  return scope.health.blocked + scope.health.suspect + scope.health.behind
}

/** Whether `state` is the box of a node whose last attempt failed. */
export function isFailed(state: GraphState): boolean {
  return (state.kind === 'cache' && state.lastFailed) || (state.kind === 'run' && state.status === 'failed')
}

/** Each node's direct consumers. */
export function consumersOf(graph: readonly GraphLink[]): Map<string, string[]> {
  const out = new Map<string, string[]>()
  for (const node of graph)
    for (const input of node.inputs) {
      const list = out.get(input)
      if (list) list.push(node.id)
      else out.set(input, [node.id])
    }
  return out
}

/** Every node downstream of `id`, transitively (not `id` itself). */
export function downstreamIds(id: string, consumers: ReadonlyMap<string, string[]>): Set<string> {
  const seen = new Set<string>()
  const stack = [...(consumers.get(id) ?? [])]
  while (stack.length) {
    const next = stack.pop()!
    if (seen.has(next)) continue
    seen.add(next)
    for (const c of consumers.get(next) ?? []) if (!seen.has(c)) stack.push(c)
  }
  return seen
}

/**
 * A failure, as the failures list shows it: a failure root of the rollup (which node failed,
 * how many nodes it holds, the error it shares with others) joined with that node's own row.
 */
export interface Failure {
  id: string
  name: string
  kind: NodeState['kind']
  /** The error's one line: the root's `detail`, or the node's most common key error once loaded. */
  headline: string
  /** The whole recorded error, traceback included. */
  error: string
  /** The root's `error_group`: failures sharing one are listed under one heading. Null: on its own. */
  group: string | null
  /** When the failed attempt was recorded (UTC, `YYYY-MM-DD HH:MM:SS`). */
  at: string | null
  /** The first failed attempt after its last clean run. */
  failingSince: string | null
  /** Runs in a row that failed it (`streakLabel`). */
  failures: number
  /** Nodes the root holds (`nodes`): the failing node itself and every node it blocks. */
  held: number
  /** Of them, assets (`assets`). */
  heldAssets: number
  /** For a partitioned node: how many keys have no cached result. */
  missingKeys: number | null
  /** For a partitioned node whose keys failed: how many, and their errors once loaded. */
  keys?: FailedKeys | null
}

/** The failed keys of a partitioned node, as the failures list shows them. */
interface FailedKeys {
  failed: number
  total: number
  /**
   * Most common first, the key's own values replaced by `<key>`; null until
   * `GET /assets/{name}/partitions?status=failed` has answered.
   */
  errors: PartitionErrorGroup[] | null
  /** The run that recorded a failed key, once loaded: what "Open run" opens. */
  runId: string | null
}

/** A node's failed keys, from its state and (once loaded) its keys page; null when none failed. */
function failedKeys(node: NodeState, page: PartitionKeys | undefined): FailedKeys | null {
  const partitions = node.partitions
  const failed = partitions?.failed ?? page?.failed ?? 0
  if (!partitions || failed === 0) return null
  return { failed, total: partitions.total, errors: page ? page.errors : null, runId: page?.keys[0]?.run_id ?? null }
}

/** The failed nodes among `nodes` whose keys failed: those whose key errors are worth loading. */
export function failedKeyNodes(nodes: readonly NodeState[]): string[] {
  return nodes.filter((n) => isFailed(memberHealth(n)) && failedKeys(n, undefined)).map((n) => n.id)
}

/** `7 of 3,077 keys · +2 more errors`: the line under a partitioned failure's headline. */
export function keysLine(keys: FailedKeys): string {
  const more = (keys.errors?.length ?? 0) - 1
  const count = `${keys.failed.toLocaleString()} of ${keys.total.toLocaleString()} keys`
  return more > 0 ? `${count} · +${more} more ${more === 1 ? 'error' : 'errors'}` : count
}

/** How late a group is: its members' worst, so one old failure is not averaged away. */
export interface ScopeAge {
  /** The oldest member's last success; null when no member has succeeded. */
  lastSuccessAt: string | null
  /** The oldest of the members' failing streaks: when it started. */
  failingSince: string | null
  /** The most runs in a row that failed one member. */
  failures: number
  /** How many members are failing now (have a failing streak). */
  failing: number
}

/** The ages of `ids`, for those with a loaded state. Times are compared as they are recorded. */
export function scopeAge(ids: readonly string[], byId: ReadonlyMap<string, NodeState>): ScopeAge {
  const age: ScopeAge = { lastSuccessAt: null, failingSince: null, failures: 0, failing: 0 }
  const older = (a: string | null, b: string | null) => (a === null || (b !== null && b < a) ? b ?? a : a)
  for (const id of ids) {
    const node = byId.get(id)
    if (!node) continue
    age.lastSuccessAt = older(age.lastSuccessAt, node.last_success_at)
    age.failingSince = older(age.failingSince, node.failing_since)
    age.failures = Math.max(age.failures, node.consecutive_failures)
    if (node.failing_since) age.failing += 1
  }
  return age
}

/**
 * A failing streak (`consecutive_failures` of `GET /state`): the runs in a row, up to now, that
 * failed the node, a run counting once however many keys failed. Every surface says it in these
 * words, so it is never read as the node's total runs (`N runs recorded`, its History).
 */
export function streakLabel(failures: number): string {
  return failures === 1 ? '1 failed run' : `${failures.toLocaleString()} failed runs in a row`
}

/**
 * Whose success a Last success time is: the node's own; any key's, for a partitioned node (the
 * newest successful key, even in a run where other keys failed); a group's oldest member's.
 */
export type SuccessOf = 'node' | 'any key' | 'oldest member'

/** What a Last success cell (and the panel's line) says: the time, whose it is, and the failing streak. */
export interface AgeCell {
  /** Null: it never succeeded. */
  success: { at: string; of: SuccessOf } | null
  /** While failing: `{lead} since <at>{tail}`. */
  failing: { lead: string; since: string; tail: string } | null
}

/**
 * The Last success cell of a node or a group. A node: `failing since 3h ago · 130 failed runs in a
 * row`. A group summarizes every failing member, not its oldest alone: `3 failing · oldest since
 * 3h ago`; its time is its oldest member's last success, and says so.
 */
export function ageCell(age: ScopeAge, of: { group: boolean; partitioned: boolean }): AgeCell {
  const whose: SuccessOf = of.group ? 'oldest member' : of.partitioned ? 'any key' : 'node'
  const success = age.lastSuccessAt ? { at: age.lastSuccessAt, of: whose } : null
  if (!age.failingSince) return { success, failing: null }
  if (!of.group) return { success, failing: { lead: 'failing', since: age.failingSince, tail: ` · ${streakLabel(age.failures)}` } }
  const n = Math.max(age.failing, 1)
  return { success, failing: { lead: n === 1 ? '1 failing' : `${n.toLocaleString()} failing · oldest`, since: age.failingSince, tail: '' } }
}

/** The words for a `SuccessOf` beside the time; none for the node's own. */
export function successOfLabel(of: SuccessOf): string | null {
  return of === 'node' ? null : of
}

/** An error's one line: its first, or a bare traceback's last; no frames. */
export function headline(error: string | null | undefined): string {
  const lines = (error ?? '').split('\n').map((l) => l.trim()).filter((l) => l !== '')
  const first = lines[0]
  if (first === undefined) return 'failed, with no error recorded'
  // A bare traceback says what went wrong on its last line, not its first.
  return /^(Traceback \(most recent call last\)|File ")/.test(first) ? lines.at(-1)! : first
}

/**
 * The failures the rollup's `roots` name, in the order served (the most nodes held first), each
 * with its node's row from `GET /state`; a root whose row is not loaded is left out until it
 * is. A node whose keys failed is headed by their most common error once `keyErrors` (by node
 * id) has its keys page; before that by the root's own line.
 */
export function failures(
  roots: readonly RollupRoot[],
  byId: ReadonlyMap<string, NodeState>,
  keyErrors?: ReadonlyMap<string, PartitionKeys>,
): Failure[] {
  const out: Failure[] = []
  for (const root of roots) {
    const n = byId.get(root.node)
    if (root.kind !== 'failure' || !n) continue
    const nodeError = n.last_materialization?.error ?? ''
    const page = keyErrors?.get(n.id)
    const keys = failedKeys(n, page)
    const top = keys?.errors?.[0]
    const sample = page?.keys[0]?.error ?? ''
    out.push({
      id: n.id,
      name: n.name,
      kind: n.kind,
      // A node whose keys failed with no error of its own says how many, in the page's number format.
      headline: top?.error ?? (keys && !nodeError ? `${keysLine({ ...keys, errors: null })} failed` : root.detail),
      error: top ? sample || nodeError || top.error : nodeError,
      group: root.error_group,
      at: n.last_materialization?.created_at ?? null,
      failingSince: n.failing_since,
      failures: n.consecutive_failures,
      held: root.nodes,
      heldAssets: root.assets,
      missingKeys: n.partitions ? n.partitions.missing : null,
      keys,
    })
  }
  return out
}

/**
 * A traceback with the project root taken off its paths (`File "/home/me/proj/a.py"` reads
 * `File "a.py"`), so the frames that matter are short. Without a root it is unchanged.
 */
export function relativeTraceback(error: string, root: string | null | undefined): string {
  if (!root) return error
  const prefix = root.endsWith('/') ? root : `${root}/`
  return error.split(prefix).join('')
}

/** Text split into pieces after each `/`, so a long path can wrap at a separator, not mid-name. */
export function pathBreaks(text: string): string[] {
  return text.split(/(?<=\/)/)
}
