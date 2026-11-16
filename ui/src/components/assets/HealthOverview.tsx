import { useEffect, useMemo, useState } from 'react'
import { CausesList } from '@/components/CausesList'
import { HealthMap } from '@/components/HealthMap'
import { useAssets } from '@/hooks/useAssets'
import { useGroups } from '@/hooks/useGroups'
import { usePartitionErrors } from '@/hooks/usePartitionErrors'
import { usePinNotes } from '@/hooks/usePins'
import { useWholeState } from '@/hooks/useAssetStates'
import { causes } from '@/lib/causes'
import { consumersOf, failedKeyNodes, failures } from '@/lib/health'
import { buildHealthTree, healthSummary, leafIds, nearestInTree, ROOT } from '@/lib/healthTree'
import { inPipeline } from '@/lib/pipeline'
import { shortName } from '@/lib/graph'
import { dataSignalSets } from '@/lib/dataSignals'

export interface HealthOverviewProps {
  /** The scope the page has open: a group id, or null for the page's top level. */
  scope: string | null
  /** The open pipeline (`?pipeline=`), when the page is one file. */
  pipeline: string | null
  /** Open a scope (a group); null for the top level. */
  onScope: (id: string | null) => void
  /** Select a node. */
  onOpen: (id: string) => void
  /** Show every failure in scope. */
  onShowFailures: () => void
}

/**
 * The top of the Assets page: the health map of the open scope beside its causes, biggest
 * first. Both read one health tree over the whole project's state, so every count the page
 * shows for a scope is the sum of its members'.
 */
export function HealthOverview({ scope, pipeline, onScope, onOpen, onShowFailures }: HealthOverviewProps) {
  const whole = useWholeState()
  const assets = useAssets()
  const groups = useGroups()
  const pins = usePinNotes()
  const [nowMs, setNow] = useState(() => Date.now())
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000)
    return () => clearInterval(t)
  }, [])

  const states = useMemo(() => (whole.data ?? []).filter((s) => inPipeline(s.id, pipeline)), [whole.data, pipeline])
  const graph = useMemo(() => (assets.data ?? []).map((a) => ({ id: a.id, inputs: a.inputs })), [assets.data])
  const groupList = useMemo(
    () => (groups.data ?? []).filter((g) => !pipeline || g.id.startsWith(`group:${pipeline}:`)),
    [groups.data, pipeline],
  )
  const signals = useMemo(() => dataSignalSets(states), [states])
  const tree = useMemo(
    () => buildHealthTree({ states, graph, groups: groupList, pinned: pins, suspect: new Set(signals.suspect.keys()), late: new Set(signals.behind.keys()) }),
    [states, graph, groupList, pins, signals],
  )
  const focus = nearestInTree(tree, scope)
  const within = useMemo(() => new Set(leafIds(tree, focus)), [tree, focus])
  const scoped = useMemo(() => states.filter((s) => within.has(s.id)), [states, within])
  const keyErrors = usePartitionErrors(useMemo(() => failedKeyNodes(scoped), [scoped]))
  const consumers = useMemo(() => (assets.data ? consumersOf(graph) : null), [assets.data, graph])
  const list = useMemo(
    () => causes({ tree, failures: failures(scoped, consumers, keyErrors), suspect: signals.suspect, behind: signals.behind, within }),
    [tree, scoped, consumers, keyErrors, signals, within],
  )

  if (!whole.data) return null
  const center = tree.nodes.get(focus)!
  if (center.leaves === 0) return null
  return (
    <section className="barca-health-overview" aria-label="Health">
      <HealthMap tree={tree} focus={focus} onFocus={(id) => onScope(id === ROOT ? null : id)} onOpen={onOpen} />
      <div className="barca-health-overview-causes">
        <h2 className="barca-health-overview-title">
          {list.length === 0 ? 'Healthy' : `${list.length.toLocaleString()} ${list.length === 1 ? 'cause' : 'causes'}`}
          <span className="barca-health-overview-sub">{healthSummary(center) || `${center.leaves.toLocaleString()} nodes, nothing needs attention`}</span>
        </h2>
        <CausesList causes={list} name={shortName} onOpen={onOpen} onShowAll={onShowFailures} nowMs={nowMs} />
      </div>
    </section>
  )
}
