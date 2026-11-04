import type { GraphEntry, StepChip } from './graph'
import { shortName } from './graph'

/**
 * A chain: a named, possibly overlapping set of nodes to look at together. Purely
 * organizational — no effect on execution or caching.
 *
 * PROTOTYPE: chains are declared here in the browser until `chain()` is parsed from Python.
 * References are node ids or bare function names (matched against every served file).
 */
export interface ChainDef {
  id: string
  name: string
  description?: string
  /** Explicit members. */
  members?: string[]
  /** Range form: everything upstream of `ends` (inclusive)… */
  ends?: string[]
  /** …and downstream of `starts` (inclusive), when given. */
  starts?: string[]
}

/** The scratch chain's id: its members live in the URL (`?chain=scratch&nodes=a,b`). */
export const SCRATCH = 'scratch'

export const PROTOTYPE_CHAINS: ChainDef[] = [
  {
    id: 'chain:revenue',
    name: 'Revenue reporting',
    description: 'From raw orders to the forecast and the dashboard',
    starts: ['orders'],
    ends: ['revenue_forecast', 'publish_dashboard'],
  },
  {
    id: 'chain:inventory',
    name: 'Inventory health',
    ends: ['notify_stock_team', 'inventory_quality_check'],
  },
  {
    id: 'chain:customers',
    name: 'Customer comms',
    members: ['customers', 'enriched_orders', 'customer_cohorts', 'send_weekly_digest'],
  },
  {
    id: 'chain:partner',
    name: 'Partner feed',
    members: ['partner_feed_version', 'partner_orders', 'revenue_by_product', 'export_partner_feed'],
  },
]

interface Node { id: string; inputs: string[] }

/** Resolve a reference (id or bare name) to the node ids it names. */
function lookup(ref: string, nodes: Node[]): string[] {
  if (nodes.some((n) => n.id === ref)) return [ref]
  return nodes.filter((n) => shortName(n.id) === ref).map((n) => n.id)
}

function walk(from: string[], next: (id: string) => string[]): Set<string> {
  const seen = new Set(from)
  const stack = [...from]
  while (stack.length) for (const n of next(stack.pop()!)) if (!seen.has(n)) { seen.add(n); stack.push(n) }
  return seen
}

/** The chain's members, in the order the graph lists them. */
export function resolveChain(def: ChainDef, nodes: Node[]): string[] {
  const byId = new Map(nodes.map((n) => [n.id, n]))
  const set = new Set<string>()
  for (const ref of def.members ?? []) for (const id of lookup(ref, nodes)) set.add(id)
  if (def.ends?.length) {
    const up = walk(def.ends.flatMap((r) => lookup(r, nodes)), (id) => byId.get(id)?.inputs ?? [])
    let range = up
    if (def.starts?.length) {
      const consumers = new Map<string, string[]>()
      for (const n of nodes) for (const i of n.inputs) consumers.set(i, [...(consumers.get(i) ?? []), n.id])
      const down = walk(def.starts.flatMap((r) => lookup(r, nodes)), (id) => consumers.get(id) ?? [])
      range = new Set([...up].filter((id) => down.has(id)))
    }
    for (const id of range) set.add(id)
  }
  return nodes.filter((n) => set.has(n.id)).map((n) => n.id)
}

/**
 * Each member's nearest member ancestors, looking through non-members: a chain that skips a
 * step still orders correctly.
 */
export function chainInputs(members: string[], nodes: Node[]): Map<string, string[]> {
  const byId = new Map(nodes.map((n) => [n.id, n]))
  const inChain = new Set(members)
  const out = new Map<string, string[]>()
  for (const m of members) {
    const found = new Set<string>()
    const seen = new Set<string>()
    const stack = [...(byId.get(m)?.inputs ?? [])]
    while (stack.length) {
      const id = stack.pop()!
      if (seen.has(id)) continue
      seen.add(id)
      if (inChain.has(id)) found.add(id)
      else stack.push(...(byId.get(id)?.inputs ?? []))
    }
    out.set(m, [...found])
  }
  return out
}

/**
 * Step labels: longest path from the chain's sources, 1-based; nodes sharing a step get
 * letters by name (`2a`, `2b`). Display only — never saved.
 */
export function chainSteps(members: string[], nodes: Node[]): Map<string, string> {
  const inputs = chainInputs(members, nodes)
  const depth = new Map<string, number>()
  const visit = (id: string): number => {
    const known = depth.get(id)
    if (known !== undefined) return known
    depth.set(id, 0) // cycle guard; the DAG has none
    const d = Math.max(0, ...(inputs.get(id) ?? []).map((i) => visit(i) + 1))
    depth.set(id, d)
    return d
  }
  members.forEach(visit)
  // Unconnected lines in one chain are numbered apart (A1, A2 / B1, B2): interleaving them by
  // depth would read as if they were one sequence.
  const lines = chainLines(members, inputs)
  const labels = new Map<string, string>()
  lines.forEach((line, li) => {
    const prefix = lines.length > 1 ? LINE_NAMES[li] ?? `L${li + 1}·` : ''
    const byDepth = new Map<number, string[]>()
    for (const m of line) byDepth.set(depth.get(m)!, [...(byDepth.get(depth.get(m)!) ?? []), m])
    for (const [d, ids] of byDepth) {
      ids.sort((a, b) => shortName(a).localeCompare(shortName(b)))
      ids.forEach((id, i) => labels.set(id, `${prefix}${d + 1}${ids.length === 1 ? '' : letter(i)}`))
    }
  })
  return labels
}

const LINE_NAMES = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'.split('')

/** The chain's connected parts, in the order their first members appear. */
function chainLines(members: string[], inputs: Map<string, string[]>): string[][] {
  const parent = new Map(members.map((m) => [m, m]))
  const find = (x: string): string => (parent.get(x) === x ? x : find(parent.get(x)!))
  for (const [m, ins] of inputs) for (const i of ins) parent.set(find(i), find(m))
  const groups = new Map<string, string[]>()
  for (const m of members) groups.set(find(m), [...(groups.get(find(m)) ?? []), m])
  return [...groups.values()]
}

function letter(i: number): string {
  return i < 26 ? String.fromCharCode(97 + i) : letter(Math.floor(i / 26) - 1) + letter(i % 26)
}

/** Members sorted by step (`1`, `2a`, `2b`, `3`; line by line: `A1`, `A2`, `B1`). */
export function byStep(steps: Map<string, string>): string[] {
  const key = (s: string) => {
    const [, line = '', n = '0', sub = ''] = /^([A-Z]*)(\d+)([a-z]*)$/.exec(s) ?? []
    return { line, n: parseInt(n, 10), sub }
  }
  return [...steps.keys()].sort((a, b) => {
    const ka = key(steps.get(a)!)
    const kb = key(steps.get(b)!)
    return ka.line.localeCompare(kb.line) || ka.n - kb.n || ka.sub.localeCompare(kb.sub)
  })
}

/**
 * The chain as graph entries: members carrying their step chips, edges between nearest
 * members, and each direct input from outside the chain as a faded port.
 *
 * With `bridges`, nodes outside the chain that sit between members (a path runs member →
 * node → member) are drawn faded in place, so chains that meet only through them connect.
 */
export function chainGraph(
  members: string[],
  nodes: (Node & { kind: GraphEntry['kind'] })[],
  opts: { steps?: (id: string) => StepChip[]; bridges?: boolean } = {},
): GraphEntry[] {
  const byId = new Map(nodes.map((n) => [n.id, n]))
  const own = opts.steps ? null : chainSteps(members, nodes)
  const steps = opts.steps ?? ((id: string) => [{ label: own!.get(id) ?? '' }])
  const inChain = new Set(members)
  const bridges = new Set<string>()
  if (opts.bridges) {
    const consumers = new Map<string, string[]>()
    for (const n of nodes) for (const i of n.inputs) consumers.set(i, [...(consumers.get(i) ?? []), n.id])
    const up = walk(members, (id) => byId.get(id)?.inputs ?? [])
    const down = walk(members, (id) => consumers.get(id) ?? [])
    for (const id of up) if (down.has(id) && !inChain.has(id)) bridges.add(id)
  }
  const shown = (id: string) => inChain.has(id) || bridges.has(id)
  const nearest = opts.bridges ? null : chainInputs(members, nodes)
  const ports = new Map<string, GraphEntry>()
  const outsideOf = (id: string) => (byId.get(id)?.inputs ?? []).filter((i) => !shown(i) && !reachesChain(i))
  const portIds = (id: string) => outsideOf(id).map((i) => {
    ports.set(`port:${i}`, { id: `port:${i}`, kind: 'port', inputs: [], name: shortName(i), summary: `From outside the chain: ${i}` })
    return `port:${i}`
  })
  const entries: GraphEntry[] = members.map((m) => ({
    id: m,
    kind: byId.get(m)!.kind,
    name: shortName(m),
    steps: steps(m),
    inputs: [...(nearest ? nearest.get(m) ?? [] : (byId.get(m)?.inputs ?? []).filter(shown)), ...portIds(m)],
  }))
  const between: GraphEntry[] = [...bridges].map((id) => ({
    id: `port:${id}`,
    kind: 'port',
    name: shortName(id),
    summary: `Not in an open chain; connects them: ${id}`,
    inputs: (byId.get(id)?.inputs ?? []).filter(shown).map((i) => (bridges.has(i) ? `port:${i}` : i)),
  }))
  // A member fed by a bridge points at the bridge's port id.
  for (const e of entries) e.inputs = e.inputs.map((i) => (bridges.has(i) ? `port:${i}` : i))
  return [...ports.values(), ...between, ...entries]

  /** An outside input whose own upstream is in the chain is already drawn as a member edge. */
  function reachesChain(id: string): boolean {
    return [...walk([id], (x) => byId.get(x)?.inputs ?? [])].some((x) => inChain.has(x))
  }
}

/** Python that declares the chain, for pasting into a pipeline file. */
export function chainCode(name: string, members: string[]): string {
  const variable = name.toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_|_$/g, '') || 'chain'
  const refs = members.map(shortName)
  return `${variable} = chain(\n    ${JSON.stringify(name)},\n    members=[${refs.join(', ')}],\n)\n`
}

/** Every chain a node is in, by name. */
export function chainsOf(id: string, resolved: { def: ChainDef; members: string[] }[]): ChainDef[] {
  return resolved.filter((c) => c.members.includes(id)).map((c) => c.def)
}

/** Short initials for a chain in a combined view ("Revenue reporting" → "RR"). */
export function chainInitials(name: string): string {
  return name.split(/\s+/).map((w) => w[0]?.toUpperCase() ?? '').join('').slice(0, 3)
}
