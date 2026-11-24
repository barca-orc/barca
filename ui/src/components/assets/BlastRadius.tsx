import { useMemo } from 'react'
import { Age, KeyValue, Section, StatusBadge, Tag } from '@/components'
import { useSchedule } from '@/hooks/useSchedule'
import { usePinNotes } from '@/hooks/usePins'
import { lastAttemptFailed, nodeStateDisplay } from '@/lib/cacheStateDisplay'
import { usePauseCover } from '@/hooks/usePauses'
import { blockerOf } from '@/lib/healthDisplay'
import { shortName } from '@/lib/graph'
import { affectedSchedules, type PauseRelation } from '@/lib/pauses'
import type { NodeState } from '@/lib/types'

/** How a schedule's ticks touch the asset, in the server's word (`covers.schedules[].relation`). */
const RELATION: Record<PauseRelation, string> = {
  self: 'this asset',
  target: 'reconciles it',
  input: 'reads it',
  downstream: 'runs what is below it',
}

export interface BlastRadiusProps {
  id: string
  /** The states loaded with the panel, for the state of each downstream node when known. */
  states: ReadonlyMap<string, NodeState>
  paused: ReadonlySet<string>
  onSelect: (id: string) => void
}

/**
 * What this asset reaches: every node downstream of it (what a failure here leaves stale or
 * blocked), and the schedules whose ticks reach it or what is below it, with their next run.
 * Which nodes and which schedules is the server's answer (`covers` of `GET /pauses?target=`,
 * the scheduler's own walk); `GET /schedule` adds each schedule's next run.
 */
export function BlastRadius({ id, states, paused, onSelect }: BlastRadiusProps) {
  const { data: jobs } = useSchedule()
  const { data: cover } = usePauseCover(id)
  const radius = useMemo(() => ({ downstream: cover?.downstream ?? [], schedules: affectedSchedules(cover, jobs ?? []) }), [cover, jobs])
  const pins = usePinNotes()
  // Until the server has answered there is nothing to say, and "nothing downstream" would be a claim.
  if (!cover) return <Section title="Blast radius">{null}</Section>
  return (
    <Section title="Blast radius">
      {/* Nothing it reaches: one line, not two rows of dashes. */}
      {radius.downstream.length === 0 && radius.schedules.length === 0 && <p className="barca-note barca-muted">Nothing downstream, no related schedules.</p>}
      {radius.downstream.length > 0 && <KeyValue label="downstream">
        {`${radius.downstream.length} ${radius.downstream.length === 1 ? 'node' : 'nodes'}`}
      </KeyValue>}
      {radius.downstream.length > 0 && (
        <ul className="barca-blast-list" aria-label="Downstream nodes">
          {radius.downstream.map((d) => {
            const node = states.get(d)
            const shown = node && nodeStateDisplay({ state: node.cache.state, lastAttempt: node.last_attempt, lastFailed: lastAttemptFailed(node), blockedBy: blockerOf(node), pinned: pins.get(d) ?? null })
            return (
              <li key={d}>
                <button type="button" className="barca-link" onClick={() => onSelect(d)}>{shortName(d)}</button>
                {shown && <StatusBadge glyph={shown.glyph} label={shown.label} size="sm" />}
                {paused.has(d) && <StatusBadge glyph="paused" label="paused" size="sm" />}
              </li>
            )
          })}
        </ul>
      )}
      {radius.schedules.length > 0 && <KeyValue label="schedules">{`${radius.schedules.length}`}</KeyValue>}
      {radius.schedules.length > 0 && (
        <ul className="barca-blast-list" aria-label="Affected schedules">
          {radius.schedules.map(({ job, relation }) => (
            <li key={job.id}>
              <span>{shortName(job.id)}</span>
              <span className="barca-cell-sub">{job.kind} · {RELATION[relation]}</span>
              {job.paused ? <Tag size="sm">schedule paused</Tag> : (
                <span className="barca-cell-sub">next <Age at={job.next_fire} /></span>
              )}
            </li>
          ))}
        </ul>
      )}
    </Section>
  )
}
