import { useMemo } from 'react'
import { CausesList } from '@/components/CausesList'
import { HealthMap } from '@/components/HealthMap'
import { useAssets } from '@/hooks/useAssets'
import { usePartitionErrors } from '@/hooks/usePartitionErrors'
import { causes } from '@/lib/causes'
import { consumersOf, failedKeyNodes, failures } from '@/lib/health'
import { healthSummary, leafIds, nearestInTree, ROOT } from '@/lib/healthTree'
import { useNow } from '@/hooks/useNow'
import type { ProjectHealth } from '@/hooks/useHealthTree'
import { shortName } from '@/lib/graph'

export interface HealthOverviewProps {
  /** The project's health, as the page counts it (`useHealthTree`). */
  health: ProjectHealth
  /** The scope the page has open: a group id, or null for the page's top level. */
  scope: string | null
  /** Open a scope (a group); null for the top level. */
  onScope: (id: string | null) => void
  /** Select a node. */
  onOpen: (id: string) => void
  /** Show every failure in scope. */
  onShowFailures: () => void
}

/**
 * The top of the Assets page: the health map of the open scope beside its causes, biggest
 * first. Both read the page's health tree (`useHealthTree`), so every count the page shows for a
 * scope is the sum of its members', as the map draws it.
 */
export function HealthOverview({ health, scope, onScope, onOpen, onShowFailures }: HealthOverviewProps) {
  const assets = useAssets()
  const nowMs = useNow()
  const { tree, states, signals } = health
  const graph = useMemo(() => (assets.data ?? []).map((a) => ({ id: a.id, inputs: a.inputs })), [assets.data])
  const focus = nearestInTree(tree, scope)
  const within = useMemo(() => new Set(leafIds(tree, focus)), [tree, focus])
  const scoped = useMemo(() => states.filter((s) => within.has(s.id)), [states, within])
  const keyErrors = usePartitionErrors(useMemo(() => failedKeyNodes(scoped), [scoped]))
  const consumers = useMemo(() => (assets.data ? consumersOf(graph) : null), [assets.data, graph])
  const list = useMemo(
    () => causes({ tree, failures: failures(scoped, consumers, keyErrors), suspect: signals.suspect, behind: signals.behind, within }),
    [tree, scoped, consumers, keyErrors, signals, within],
  )

  // Its space is held while the project's state loads, so the table below never jumps.
  if (!health.loaded) return <section className="barca-health-overview" aria-label="Health" aria-busy="true" />
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
