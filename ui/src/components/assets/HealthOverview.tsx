import { useMemo } from 'react'
import { CausesList } from '@/components/CausesList'
import { HealthMap } from '@/components/HealthMap'
import { usePartitionErrors } from '@/hooks/usePartitionErrors'
import { causes } from '@/lib/causesView'
import { coverageLine, type DataSignalSets } from '@/lib/dataSignals'
import { failedKeyNodes, failures } from '@/lib/health'
import { healthSummary, leafIds, nearestInTree, rootsWithin, ROOT } from '@/lib/rollupView'
import { useNow } from '@/hooks/useNow'
import type { ProjectRollup } from '@/hooks/useRollup'
import { shortName } from '@/lib/graph'
import { plural } from '@/lib/plural'
import type { NodeState } from '@/lib/types'

export interface HealthOverviewProps {
  /** The rollup the page reads (`useRollup`). */
  rollup: ProjectRollup
  /** The rows of `GET /state` the page has, by node: a failure's times and error. */
  byId: ReadonlyMap<string, NodeState>
  /** Each node's data signals, in words (`dataSignalSets`). */
  signals: DataSignalSets
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
 * The top of the Assets page: the health map of the open scope beside its causes, the one
 * holding the most nodes first. Both read the rollup (`GET /state/rollup`): the map draws its
 * entries, the causes are its roots, and the counts under the title are the open scope's entry,
 * in nodes and in assets.
 */
export function HealthOverview({ rollup, byId, signals, scope, onScope, onOpen, onShowFailures }: HealthOverviewProps) {
  const nowMs = useNow()
  const { tree } = rollup
  const focus = nearestInTree(tree, scope)
  const within = useMemo(() => new Set(leafIds(tree, focus)), [tree, focus])
  const roots = useMemo(() => rootsWithin(tree, focus), [tree, focus])
  const failing = useMemo(() => roots.flatMap((r) => byId.get(r.node) ?? []), [roots, byId])
  const keyErrors = usePartitionErrors(useMemo(() => failedKeyNodes(failing), [failing]))
  const list = useMemo(
    () => causes({ failures: failures(roots, byId, keyErrors), roots, suspect: signals.suspect, behind: signals.behind, within }),
    [roots, byId, keyErrors, signals, within],
  )

  // Its space is held while the rollup loads, so the table below never jumps.
  if (!rollup.loaded) return <section className="barca-health-overview" aria-label="Health" aria-busy="true" />
  const center = tree.entries.get(focus)!
  if (center.nodes === 0) return null
  const coverage = center.data_coverage
  const size = `${plural(center.nodes, 'node')}, ${plural(center.assets, 'asset')}`
  return (
    <section className="barca-health-overview" aria-label="Health">
      <HealthMap tree={tree} focus={focus} onFocus={(id) => onScope(id === ROOT ? null : id)} onOpen={onOpen} />
      <div className="barca-health-overview-causes">
        <h2 className="barca-health-overview-title">
          {list.length === 0 ? 'Healthy' : `${list.length.toLocaleString()} ${list.length === 1 ? 'cause' : 'causes'}`}
          {/* Counts of nodes, said with both sizes of the scope: the map beside it is sized in assets. */}
          <span className="barca-health-overview-sub" title="Nodes by health. Assets, tasks and sensors are nodes; the map is sized in assets.">
            {healthSummary(center.health) ? `${size}: ${healthSummary(center.health)}` : `${size}, nothing needs attention`}
          </span>
        </h2>
        <CausesList causes={list} name={shortName} onOpen={onOpen} onShowAll={onShowFailures} nowMs={nowMs} />
        {/* Always with what it is out of: an asset no worker counted is not measured, never fine. */}
        {coverage.assets > 0 && (
          <p className="barca-note barca-muted" aria-label="Data coverage" title="Assets whose latest result has a recorded row count. Data signals (empty output, rows changed) are raised only for these.">
            Data: {coverageLine(coverage)}
          </p>
        )}
      </div>
    </section>
  )
}
