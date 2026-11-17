import { useMemo } from 'react'
import { CausesList, DataSignals } from '@/components/CausesList'
import { HealthMap } from '@/components/HealthMap'
import { causeParts, mapPartColor } from '@/lib/causeDisplay'
import { signals as signalsIn, todos } from '@/lib/causesView'
import { coverageLine, type DataSignalSets } from '@/lib/dataSignals'
import { freshWithin, leafIds, nearestInTree, noEstimate, ROOT } from '@/lib/rollupView'
import { useNow } from '@/hooks/useNow'
import type { ProjectRollup } from '@/hooks/useRollup'
import { shortName } from '@/lib/graph'
import { plural } from '@/lib/plural'
import { formatSpan } from '@/lib/time'
import type { NodeState } from '@/lib/types'

export interface HealthOverviewProps {
  /** The rollup the page reads (`useRollup`). */
  rollup: ProjectRollup
  /** The rows of `GET /state` the page has, by node: a failure's times. */
  byId: ReadonlyMap<string, NodeState>
  /** Each node's data signals, in words (`dataSignalSets`). */
  signals: DataSignalSets
  /** The scope the page has open: a group id, or null for the page's top level. */
  scope: string | null
  /** Open a scope (a group); null for the top level. */
  onScope: (id: string | null) => void
  /** Select a node. */
  onOpen: (id: string) => void
  /** The root whose nodes are lit, by id, and what it holds (`heldIds`). */
  root: string | null
  held: ReadonlySet<string> | null
  /** Light what a root holds on the map and in the table; null to clear. */
  onRoot: (id: string | null) => void
}

/**
 * The top of the Assets page: how far the open scope is from fresh, why, and what closes the
 * distance. The health map of the scope sits beside its to-do list. Both read the rollup
 * (`GET /state/rollup`): the map draws its entries, the list is the scope's `held_by` joined
 * with `roots`, and the line under the title is the scope's entry in the terms of the gap,
 * every count in nodes. Data signals follow in a section of their own.
 */
export function HealthOverview({ rollup, byId, signals, scope, onScope, onOpen, root, held, onRoot }: HealthOverviewProps) {
  const nowMs = useNow()
  const { tree } = rollup
  const focus = nearestInTree(tree, scope)
  const center = tree.entries.get(focus)
  const within = useMemo(() => new Set(leafIds(tree, focus)), [tree, focus])
  const list = useMemo(() => todos(center, tree.roots), [center, tree.roots])
  const data = useMemo(() => signalsIn({ suspect: signals.suspect, behind: signals.behind, within }), [signals, within])

  // Its space is held while the rollup loads, so the table below never jumps.
  if (!rollup.loaded) return <section className="barca-health-overview" aria-label="Health" aria-busy="true" />
  if (!center || center.nodes === 0) return null
  const coverage = center.data_coverage
  const parts = causeParts(center.causes)
  const estimate = [freshWithin(center, tree.asOf), noEstimate(center)].filter((part) => part !== null)
  return (
    <section className="barca-health-overview" aria-label="Health">
      <HealthMap tree={tree} focus={focus} onFocus={(id) => onScope(id === ROOT ? null : id)} onOpen={onOpen} lit={held} />
      <div className="barca-health-overview-causes">
        <h2 className="barca-health-overview-title">
          {center.not_fresh === 0 ? 'Fresh' : `${center.not_fresh.toLocaleString()} of ${plural(center.nodes, 'node')} not fresh`}
          <span className="barca-health-overview-sub" title="Every count here is in nodes: assets, tasks and sensors.">
            {plural(center.assets, 'asset')} among them
          </span>
        </h2>
        {parts.length > 0 && (
          // The scope in the terms of the gap: its nodes by cause as served, then the estimate.
          <p className="barca-gap-line" aria-label="Nodes that are not fresh, by cause">
            {parts.map((part) => (
              <span key={part.cause} className="barca-gap-part" data-alarm={part.alarm || undefined}>
                <i style={{ background: mapPartColor(part.cause) }} />
                <strong>{part.count.toLocaleString()}</strong> {part.label}
              </span>
            ))}
            {estimate.length > 0 && <span className="barca-gap-estimate">{estimate.join(' · ')}</span>}
            {center.gap_seconds > 0 && <span className="barca-gap-estimate" title="The freshness gap: how long the nodes have been out of date plus how long until they are expected fresh, added up over the nodes">{formatSpan(center.gap_seconds)} of gap</span>}
          </p>
        )}
        <CausesList
          todos={list}
          scope={center}
          name={shortName}
          onOpen={onOpen}
          selected={root}
          onSelect={onRoot}
          failingSince={(node) => byId.get(node)?.failing_since ?? null}
          nowMs={nowMs}
        />
        <DataSignals signals={data} name={shortName} onOpen={onOpen} />
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
