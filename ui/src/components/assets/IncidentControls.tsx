import { useMemo, useState } from 'react'
import { Link } from 'react-router'
import { Absolute, Button, ConfirmDialog, ErrorLine, KeyValue, Section, StatusBadge } from '@/components'
import { usePauses, useTogglePause } from '@/hooks/usePauses'
import { useQueue, useToggleQueuePause } from '@/hooks/useQueue'
import { useSchedule, useToggleSchedulePause } from '@/hooks/useSchedule'
import { shortName } from '@/lib/graph'
import { blastRadius, fileOf } from '@/lib/pauses'
import type { ListNode } from '@/lib/types'

interface Confirm {
  title: string
  body: string
  action: string
  run: () => void
}

/** A small "paused" state for a list row: the paused glyph and its word. */
export function PausedTag({ id, paused }: { id: string; paused: ReadonlySet<string> }) {
  return paused.has(id) ? <StatusBadge glyph="paused" label="paused" size="sm" /> : null
}

export interface IncidentControlsProps {
  node: Pick<ListNode, 'id'>
  graph: ListNode[]
  readOnly: boolean
}

/**
 * Everything needed to contain an incident from this asset, one click away: pause it and what is
 * below it, or its whole file (`POST /pauses`), pause the schedules that would compute it
 * (`POST /schedule/{id}/pause`) and the run queue (`POST /queue/pause`), and resume each.
 */
export function IncidentControls({ node, graph, readOnly }: IncidentControlsProps) {
  const pauses = usePauses()
  const toggle = useTogglePause()
  const { data: jobs } = useSchedule()
  const scheduleToggle = useToggleSchedulePause()
  const queue = useQueue()
  const queueToggle = useToggleQueuePause()
  const [confirm, setConfirm] = useState<Confirm | null>(null)
  const name = shortName(node.id)
  const file = fileOf(node.id)
  const pause = pauses.data?.paused.find((p) => p.id === node.id) ?? null
  const radius = useMemo(() => blastRadius(node.id, graph, jobs ?? []), [node.id, graph, jobs])
  const running = radius.schedules.filter((s) => !s.job.paused)
  const held = radius.schedules.filter((s) => s.job.paused)
  const queuePaused = queue.data?.paused ?? false
  const busy = toggle.isPending || scheduleToggle.isPending || queueToggle.isPending
  const disabled = readOnly || busy
  const error = (toggle.error ?? scheduleToggle.error ?? queueToggle.error) as Error | null
  const ask = (c: Confirm) => setConfirm(c)
  const below = radius.downstream.length

  return (
    <Section title="Incident controls">
      <KeyValue label="paused">
        {pause ? (
          <span className="barca-state-line">
            <StatusBadge glyph="paused" label="paused" size="sm" />
            <span className="barca-cell-sub">since <Absolute at={pause.paused_at * 1000} /> via {shortName(pause.via)}</span>
          </span>
        ) : 'no'}
      </KeyValue>
      <div className="barca-incident-actions">
        {pause ? (
          <>
            <Button size="sm" disabled={disabled} onClick={() => toggle.mutate({ pause: false, target: node.id })}>
              Resume this asset
            </Button>
            {below > 0 && (
              <Button size="sm" variant="ghost" disabled={disabled} onClick={() => toggle.mutate({ pause: false, target: node.id, downstream: true })}>
                Resume it and downstream
              </Button>
            )}
          </>
        ) : (
          <Button size="sm" variant="danger" disabled={disabled}
            onClick={() => ask({
              title: `Pause ${name} and downstream?`,
              body: `Scheduled and sensor-triggered runs skip ${name}${below ? ` and the ${below} ${below === 1 ? 'node' : 'nodes'} below it` : ''} until resumed. Runs by hand ask first.`,
              action: 'Pause',
              run: () => toggle.mutate({ pause: true, target: node.id, downstream: true }),
            })}>
            Pause this asset and downstream
          </Button>
        )}
        <Button size="sm" variant="ghost" disabled={disabled}
          onClick={() => ask({
            title: `Pause every asset of ${file}?`,
            body: `Scheduled runs skip every node defined in ${file} until resumed.`,
            action: 'Pause pipeline',
            run: () => toggle.mutate({ pause: true, target: file }),
          })}>
          Pause pipeline (file)
        </Button>
        {running.length > 0 && (
          <Button size="sm" variant="ghost" disabled={disabled}
            onClick={() => ask({
              title: `Pause ${running.length} ${running.length === 1 ? 'schedule' : 'schedules'}?`,
              body: `No tick of ${running.map((s) => shortName(s.job.id)).join(', ')} fires until resumed.`,
              action: 'Pause schedules',
              run: () => running.forEach((s) => scheduleToggle.mutate({ id: s.job.id, pause: true })),
            })}>
            Pause {running.length} related {running.length === 1 ? 'schedule' : 'schedules'}
          </Button>
        )}
        {held.length > 0 && (
          <Button size="sm" variant="ghost" disabled={disabled}
            onClick={() => held.forEach((s) => scheduleToggle.mutate({ id: s.job.id, pause: false }))}>
            Resume {held.length} {held.length === 1 ? 'schedule' : 'schedules'}
          </Button>
        )}
        {queuePaused ? (
          <Button size="sm" variant="ghost" disabled={disabled} onClick={() => queueToggle.mutate(false)}>
            Resume the run queue
          </Button>
        ) : (
          <Button size="sm" variant="ghost" disabled={disabled}
            onClick={() => ask({
              title: 'Pause the run queue?',
              body: 'No queued run starts until the queue is resumed, for every asset; runs in flight go on.',
              action: 'Pause queue',
              run: () => queueToggle.mutate(true),
            })}>
            Pause the run queue
          </Button>
        )}
      </div>
      <p className="barca-cell-sub">
        <Link to="/assets?paused=1">All paused assets</Link> · <Link to="/schedules">Schedules</Link> · <Link to="/runs">Runs and queue</Link>
      </p>
      {readOnly && <p className="barca-cell-sub">This server is read-only: nothing can be paused here.</p>}
      {error && <ErrorLine error={error.message} />}
      {confirm && (
        <ConfirmDialog
          title={confirm.title}
          body={confirm.body}
          action={confirm.action}
          onConfirm={() => { confirm.run(); setConfirm(null) }}
          onCancel={() => setConfirm(null)}
        />
      )}
    </Section>
  )
}
