/**
 * What is broken and how much it holds, in the words a page shows: the rollup's entries and
 * roots (`GET /state/rollup`, classified and summed in `barca-core`) joined with the rows of
 * `GET /state`. Nothing here classifies, counts or attributes; it words and joins.
 */

import { memberHealth, type GraphState } from './graphState'
import type { RollupRoot } from './rollupView'
import type { PartitionErrorGroup } from './generated/PartitionErrorGroup'
import type { NodeState, PartitionKeys } from './types'

/** A node's place in the graph: what `GET /assets` says, all a lineage needs. */
export interface GraphLink {
  id: string
  inputs: string[]
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

/**
 * A failing streak (`consecutive_failures` of `GET /state`): the runs in a row, up to now, that
 * failed the node, a run counting once however many keys failed. Every surface says it in these
 * words, so it is never read as the node's total runs (`N runs recorded`, its History).
 */
export function streakLabel(failures: number): string {
  return failures === 1 ? '1 failed run' : `${failures.toLocaleString()} failed runs in a row`
}

/**
 * An error's one line: its first, or a bare traceback's last; no frames. For the summary of
 * `ErrorLine`, which shows errors no route serves a line for (a key's, a step's, a request's). A
 * failing node's line is served: `detail` of its root in the rollup (`rollup::headline`).
 */
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
