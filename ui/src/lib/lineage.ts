/**
 * A node's neighbourhood: what a graph too big to draw whole shows instead. Pure.
 */

import type { GraphLink } from './health'

/** Above this many nodes a view draws a lineage instead of the whole graph: past it the
 * layout takes seconds and the nodes are too small to read at any zoom that fits them. */
export const GRAPH_LIMIT = 1500

/**
 * `id` and the nodes within `depth` hops of it, upstream and downstream, in `graph` order.
 * Siblings (other consumers of an input) are not included: a lineage answers "where does
 * this come from, and what does it feed".
 */
export function lineage<T extends GraphLink>(id: string, graph: readonly T[], depth = 2, consumers?: ReadonlyMap<string, string[]>): T[] {
  const byId = new Map(graph.map((n) => [n.id, n]))
  if (!byId.has(id)) return []
  const down = consumers ?? (() => {
    const m = new Map<string, string[]>()
    for (const n of graph) for (const i of n.inputs) m.set(i, [...(m.get(i) ?? []), n.id])
    return m
  })()
  const keep = new Set([id])
  const walk = (next: (n: string) => readonly string[]) => {
    let frontier = [id]
    for (let d = 0; d < depth && frontier.length; d++) {
      const after: string[] = []
      for (const n of frontier) for (const m of next(n)) if (!keep.has(m) && byId.has(m)) { keep.add(m); after.push(m) }
      frontier = after
    }
  }
  walk((n) => byId.get(n)?.inputs ?? [])
  walk((n) => down.get(n) ?? [])
  return graph.filter((n) => keep.has(n.id))
}
