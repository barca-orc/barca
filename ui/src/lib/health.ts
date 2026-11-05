/**
 * What is broken, how much it affects, and why: pure answers over `GET /state` rows and the
 * graph from `GET /assets`. The grouped page's health strip and failures list, and the node
 * panel's "Why" section, only render these.
 */

import { CACHE_STATES, FAILED_TONE, LAST_ATTEMPT_FAILED, cacheStateTone, type CacheState } from './cacheStateDisplay'
import { memberHealth, type GraphState } from './graphState'
import type { PartitionErrorGroup } from './generated/PartitionErrorGroup'
import type { Tone } from './tone'
import type { NodeState, PartitionKeys, StatusKind } from './types'

/** A node's place in the graph: what `GET /assets` says, all a lineage needs. */
export interface GraphLink {
  id: string
  inputs: string[]
}

/**
 * How many nodes of a scope are in each state, as the health bar shows them: one count per cache
 * state word, plus the nodes a run is working on and those whose state is not loaded. A failed
 * last attempt is counted beside the states, not among them: a node is in one state and may also
 * have failed last.
 */
export interface HealthCounts {
  states: Record<CacheState, number>
  running: number
  /** No state loaded for them yet. */
  pending: number
  /** Whose last attempt failed (`lastAttemptFailed`), whatever their state. */
  lastFailed: number
  total: number
}

/** A segment of the bar or a part of its label: a state, run work, or no state yet. */
type HealthPart = CacheState | 'running' | 'pending'

/** Most urgent first: the order of the bar's segments. */
export const HEALTH_PARTS: readonly HealthPart[] = ['running', ...CACHE_STATES, 'pending']

export function emptyHealth(total = 0): HealthCounts {
  return {
    states: Object.fromEntries(CACHE_STATES.map((s) => [s, 0])) as Record<CacheState, number>,
    running: 0,
    pending: 0,
    lastFailed: 0,
    total,
  }
}

/** The tone of a part of the bar. */
export function healthPartTone(part: HealthPart): Tone {
  if (part === 'running') return 'running'
  if (part === 'pending') return 'neutral'
  return cacheStateTone(part)
}

/** Counts of `ids`; a live run's status (`live`) outranks the polled state. */
export function healthCounts(
  ids: readonly string[],
  byId: ReadonlyMap<string, NodeState>,
  live: Readonly<Record<string, StatusKind>> = {},
): HealthCounts {
  const counts = emptyHealth(ids.length)
  for (const id of ids) {
    const status = live[id]
    const box = memberHealth(byId.get(id))
    if (status === 'running' || status === 'queued' || box.kind === 'run') counts.running += 1
    else if (box.kind === 'cache') {
      counts.states[box.state] += 1
      if (box.lastFailed || status === 'failed') counts.lastFailed += 1
    } else {
      counts.pending += 1
      if (status === 'failed') counts.lastFailed += 1
    }
  }
  return counts
}

const PENDING_WORD = 'checking'

/** One part of the label: `3 stale`, `2 running`, `640 checking`. */
function partText(part: HealthPart, count: number): string {
  return `${count.toLocaleString()} ${part === 'pending' ? PENDING_WORD : part}`
}

interface HealthLabelPart {
  /** What the part counts: a state, run work, no state yet, or failed last attempts. */
  key: HealthPart | 'lastFailed'
  tone: Tone
  text: string
}

/**
 * Every non-empty part, most urgent first, as the words a label shows: failed last attempts
 * first (`2 failed last attempt`), then run work, then each state present by rank, then what
 * is not loaded. The state words are the CLI's.
 */
export function healthParts(counts: HealthCounts, limit = HEALTH_PARTS.length + 1): HealthLabelPart[] {
  const parts: HealthLabelPart[] = []
  if (counts.lastFailed > 0) parts.push({ key: 'lastFailed', tone: FAILED_TONE, text: `${counts.lastFailed.toLocaleString()} ${LAST_ATTEMPT_FAILED}` })
  for (const part of HEALTH_PARTS) {
    const count = part === 'running' || part === 'pending' ? counts[part] : counts.states[part]
    if (count > 0) parts.push({ key: part, tone: healthPartTone(part), text: partText(part, count) })
  }
  return parts.slice(0, limit)
}

/** `2 last attempt failed · 121 stale · 380 cached`. */
export function healthLabel(counts: HealthCounts, limit?: number): string {
  return healthParts(counts, limit).map((p) => p.text).join(' · ')
}

/** How many nodes are in a state that needs attention (a warning tone: blocked, stale, partial, unsynced). */
export function needsAttention(counts: HealthCounts): number {
  return CACHE_STATES.filter((s) => cacheStateTone(s) === 'warning').reduce((n, s) => n + counts.states[s], 0)
}

/** The tone of a scope's most urgent part; neutral for an empty scope. */
export function healthTone(counts: HealthCounts): Tone {
  return healthParts(counts, 1)[0]?.tone ?? 'neutral'
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

/** A failure, as the failures list shows it. */
export interface Failure {
  id: string
  name: string
  kind: NodeState['kind']
  /** The error's first line: usually `Type: message`. */
  headline: string
  /** The whole recorded error, traceback included. */
  error: string
  /** When the failed attempt was recorded (UTC, `YYYY-MM-DD HH:MM:SS`). */
  at: string | null
  /** The first failed attempt after its last clean run. */
  failingSince: string | null
  /** Runs in a row that failed it. */
  failures: number
  /** How many nodes downstream of it, transitively, are waiting on it; null until the graph is loaded. */
  impact: number | null
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
interface ScopeAge {
  /** The oldest member's last success; null when no member has succeeded. */
  lastSuccessAt: string | null
  failingSince: string | null
  /** The most runs in a row that failed one member. */
  failures: number
}

/** The ages of `ids`, for those with a loaded state. Times are compared as they are recorded. */
export function scopeAge(ids: readonly string[], byId: ReadonlyMap<string, NodeState>): ScopeAge {
  const age: ScopeAge = { lastSuccessAt: null, failingSince: null, failures: 0 }
  const older = (a: string | null, b: string | null) => (a === null || (b !== null && b < a) ? b ?? a : a)
  for (const id of ids) {
    const node = byId.get(id)
    if (!node) continue
    age.lastSuccessAt = older(age.lastSuccessAt, node.last_success_at)
    age.failingSince = older(age.failingSince, node.failing_since)
    age.failures = Math.max(age.failures, node.consecutive_failures)
  }
  return age
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
 * The failed nodes among `nodes`: the most downstream impact first (unknown last), then the most
 * recent. `consumers` is null while the graph is loading: then the impact is unknown, not 0. A
 * check that failed on a cached asset is a failure too; a node merely stale because something
 * upstream failed is not (it is what `impact` counts). A node whose keys failed is headed by
 * their most common error once `keyErrors` (by node id) has its keys page; before that by its
 * own error, which is one key's, or by how many keys failed.
 */
export function failures(
  nodes: readonly NodeState[],
  consumers: ReadonlyMap<string, string[]> | null,
  keyErrors?: ReadonlyMap<string, PartitionKeys>,
): Failure[] {
  return nodes
    .filter((n) => isFailed(memberHealth(n)))
    .map((n) => {
      const nodeError = n.last_materialization?.error ?? ''
      const page = keyErrors?.get(n.id)
      const keys = failedKeys(n, page)
      const top = keys?.errors?.[0]
      const sample = page?.keys[0]?.error ?? ''
      const fallback = keys && !nodeError ? `${keysLine({ ...keys, errors: null })} failed` : headline(nodeError)
      return {
        id: n.id,
        name: n.name,
        kind: n.kind,
        headline: top?.error ?? fallback,
        error: top ? sample || nodeError || top.error : nodeError,
        at: n.last_materialization?.created_at ?? null,
        failingSince: n.failing_since,
        failures: n.consecutive_failures,
        impact: consumers ? downstreamIds(n.id, consumers).size : null,
        missingKeys: n.partitions ? n.partitions.missing : null,
        keys,
      }
    })
    .sort((a, b) => (b.impact ?? -1) - (a.impact ?? -1) || (b.at ?? '').localeCompare(a.at ?? '') || a.name.localeCompare(b.name))
}

/** One cause: failures whose error differs at most in its numbers. */
export interface FailureGroup {
  /** The headline with its numbers normalised: what the failures share. */
  cause: string
  /** The first failure's headline as recorded: what the heading shows. */
  headline: string
  failures: Failure[]
  /** The members' impacts added up; null when any is unknown. A node downstream of two counts twice. */
  impact: number | null
}

/** `row count fell 64% day over day` and `… 12% …` are one cause. */
export function causeOf(headline: string): string {
  return headline.replace(/\d[\d,._]*\d|\d/g, '#')
}

/** `list` grouped by cause, the most impact first (unknown last), then the most failures. */
export function failureGroups(list: readonly Failure[]): FailureGroup[] {
  const groups = new Map<string, FailureGroup>()
  for (const f of list) {
    const cause = causeOf(f.headline)
    const group = groups.get(cause) ?? { cause, headline: f.headline, failures: [], impact: 0 }
    group.failures.push(f)
    group.impact = group.impact === null || f.impact === null ? null : group.impact + f.impact
    groups.set(cause, group)
  }
  return [...groups.values()].sort(
    (a, b) => (b.impact ?? -1) - (a.impact ?? -1) || b.failures.length - a.failures.length || a.cause.localeCompare(b.cause),
  )
}

/** A count for a table: `0` for none, `?` while it is not known. */
export function impactLabel(impact: number | null): string {
  return impact === null ? '?' : impact.toLocaleString()
}

/**
 * The failed nodes upstream of `id` (transitively, nearest first): why a stale or never-run
 * node has not been brought up to date. Only nodes with a loaded state can be judged.
 */
export function failedUpstream(
  id: string,
  inputs: ReadonlyMap<string, readonly string[]>,
  byId: ReadonlyMap<string, NodeState>,
): string[] {
  const out: string[] = []
  const seen = new Set<string>([id])
  let frontier = [...(inputs.get(id) ?? [])]
  while (frontier.length) {
    const next: string[] = []
    for (const up of frontier) {
      if (seen.has(up)) continue
      seen.add(up)
      const node = byId.get(up)
      if (node && isFailed(memberHealth(node))) out.push(up)
      next.push(...(inputs.get(up) ?? []))
    }
    frontier = next
  }
  return out
}
