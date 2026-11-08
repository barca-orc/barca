import { useMemo } from 'react'
import { Absolute, KeyValue, Section, Tag } from '@/components'
import { useSchedule } from '@/hooks/useSchedule'
import { usePinNotes } from '@/hooks/usePins'
import { lastAttemptFailed, nodeStateDisplay } from '@/lib/cacheStateDisplay'
import { blockedBy } from '@/lib/graphView'
import { shortName } from '@/lib/graph'
import { blastRadius, type ScheduleRelation } from '@/lib/pauses'
import type { ListNode, NodeState } from '@/lib/types'

const RELATION: Record<ScheduleRelation, string> = {
  self: 'this asset',
  downstream: 'reads it',
  upstream: 'reconciles it',
}

export interface BlastRadiusProps {
  id: string
  /** The whole graph (`GET /assets`). */
  graph: ListNode[]
  /** The states loaded with the panel, for the state of each downstream node when known. */
  states: ReadonlyMap<string, NodeState>
  paused: ReadonlySet<string>
  onSelect: (id: string) => void
}

/**
 * What this asset reaches: every node downstream of it (what a failure here leaves stale or
 * blocked), and the schedules whose ticks compute it or what reads it, with their next run.
 * From `GET /assets` and `GET /schedule`.
 */
export function BlastRadius({ id, graph, states, paused, onSelect }: BlastRadiusProps) {
  const { data: jobs } = useSchedule()
  const radius = useMemo(() => blastRadius(id, graph, jobs ?? []), [id, graph, jobs])
  const pins = usePinNotes()
  const blockers = useMemo(() => blockedBy([...states.values()], graph, pins), [states, graph, pins])
  return (
    <Section title="Blast radius">
      <KeyValue label="downstream">
        {radius.downstream.length === 0 ? '–' : `${radius.downstream.length} ${radius.downstream.length === 1 ? 'node' : 'nodes'}`}
      </KeyValue>
      {radius.downstream.length > 0 && (
        <ul className="barca-blast-list" aria-label="Downstream nodes">
          {radius.downstream.map((d) => {
            const node = states.get(d)
            const shown = node && nodeStateDisplay({ state: node.cache.state, lastFailed: lastAttemptFailed(node), blockedBy: blockers.get(d) ?? null, pinned: pins.get(d) ?? null })
            return (
              <li key={d}>
                <button type="button" className="barca-link" onClick={() => onSelect(d)}>{shortName(d)}</button>
                {shown && <span className="barca-cell-sub">{shown.label}</span>}
                {paused.has(d) && <Tag size="sm" tone="warning">paused</Tag>}
              </li>
            )
          })}
        </ul>
      )}
      <KeyValue label="schedules">
        {radius.schedules.length === 0 ? '–' : `${radius.schedules.length}`}
      </KeyValue>
      {radius.schedules.length > 0 && (
        <ul className="barca-blast-list" aria-label="Affected schedules">
          {radius.schedules.map(({ job, relation }) => (
            <li key={job.id}>
              <span>{shortName(job.id)}</span>
              <span className="barca-cell-sub">{job.kind} · {RELATION[relation]}</span>
              {job.paused ? <Tag size="sm">schedule paused</Tag> : (
                <span className="barca-cell-sub">next <Absolute at={job.next_fire === null ? null : job.next_fire * 1000} /></span>
              )}
            </li>
          ))}
        </ul>
      )}
    </Section>
  )
}
