import { useEffect, useMemo, useRef, useState } from 'react'
import { useSearchParams } from 'react-router'
import { useQueryClient } from '@tanstack/react-query'
import { match } from 'ts-pattern'
import { useAssets } from '@/hooks/useAssets'
import { useHealth } from '@/hooks/useHealth'
import { useProject } from '@/hooks/useProject'
import { usePageTopbarRun } from '@/hooks/useTopbarRun'
import { useProgram } from '@/hooks/useProgram'
import { useRunDetail, useRuns } from '@/hooks/useRuns'
import { useStoredFlag } from '@/hooks/useStoredFlag'
import { remote } from '@/lib/remote'
import { HIDE_SCHEDULE_BY_DEFAULT, INITIAL_RUNS, parseRunFilters, runsQuery, updateRuns, writeRunFilters, type RunsModel, type RunsMsg } from '@/lib/runsPage'
import { PageRunButton } from '@/components'
import { QueuePanel } from './QueuePanel'
import { RunsView } from './RunsView'

/** How long the search box is left alone before the server is asked, ms. */
const SEARCH_SETTLE_MS = 250

/** `value` once it has stopped changing for `ms`. */
function useSettled<T>(value: T, ms: number): T {
  const [settled, setSettled] = useState(value)
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), ms)
    return () => clearTimeout(timer)
  }, [value, ms])
  return settled
}

/** Runs the Runs program: the run list and the selected run from the server, `?run=` from the URL. */
export function RunsPage() {
  const [params, setParams] = useSearchParams()
  const selected = params.get('run')
  const health = useHealth()
  const assets = useAssets()
  const project = useProject().data
  const root = project?.root
  const client = useQueryClient()
  // Whether the schedule's runs are hidden when the URL does not say: this browser's last
  // choice, so a project that only has scheduled runs is asked to show them once.
  const [hideByDefault, setHideByDefault] = useStoredFlag('barca:runs:hide-schedule', HIDE_SCHEDULE_BY_DEFAULT)
  // The URL owns the filters: read at each message, so back, forward and a link all apply.
  const filters = parseRunFilters(params, hideByDefault)
  const update = (m: RunsModel, msg: RunsMsg) => updateRuns({ ...m, ...filters }, msg)
  // The URL as last written. `setSearchParams(previous => ...)` does not queue: two writes
  // before a render would both start from the same parameters, and the second undo the first
  // (a chip pressed, then a key typed in the search).
  const written = useRef(params)
  useEffect(() => {
    written.current = params
  }, [params])
  const write = (change: (previous: URLSearchParams) => URLSearchParams, replace: boolean) => {
    written.current = change(written.current)
    setParams(written.current, { replace })
  }
  const [program, dispatch] = useProgram(INITIAL_RUNS, update, (effect) =>
    match(effect)
      .with({ type: 'navigate' }, ({ run, replace }) => {
        write((previous) => {
          const next = new URLSearchParams(previous)
          // Only a replacement keeps it: the same run, under its durable id.
          if (!replace) next.delete('event_gap')
          if (run) next.set('run', run)
          else next.delete('run')
          return next
        }, replace)
      })
      .with({ type: 'filters' }, ({ change }) => {
        const hide = change.hideSchedule ?? hideByDefault
        if (change.hideSchedule !== undefined) setHideByDefault(change.hideSchedule)
        write((previous) => writeRunFilters(previous, { ...parseRunFilters(previous, hide), ...change }), true)
      })
      .with({ type: 'refetch_list' }, () => void client.refetchQueries({ queryKey: ['runs', 'list'] }))
      .with({ type: 'refetch_detail' }, () => void client.refetchQueries({ queryKey: ['runs', 'detail', selected] }))
      .exhaustive(),
  )
  const model = { ...program, ...filters }
  // The search asks the server once typing pauses, not at each key.
  const search = useSettled(model.query, SEARCH_SETTLE_MS)
  const runs = useRuns(runsQuery({ ...model, query: search }))
  const detail = useRunDetail(selected)

  // A running run's start, as the run list has it: the queue counts its time from it.
  const listed = runs.data?.runs
  const startedAt = useMemo(() => {
    const at = new Map<string, string>()
    for (const run of listed ?? []) for (const id of [run.id, run.run_id, run.handle]) if (id) at.set(id, run.started_at)
    return (runId: string) => at.get(runId) ?? null
  }, [listed])

  const { scopeRun } = usePageTopbarRun((handle) => dispatch({ type: 'run_selected', id: handle }))

  const durableId = detail.data?.run_id
  useEffect(() => {
    if (selected && durableId && durableId !== selected) dispatch({ type: 'run_resolved', id: durableId })
  }, [durableId, selected, dispatch])

  return (
    <RunsView
      emptyProject={assets.data?.length === 0}
      model={model}
      runs={remote(runs)}
      selected={selected ? { id: selected, run: remote(detail), eventGap: params.get('event_gap') === '1' } : null}
      queue={<QueuePanel readOnly={health.data?.read_only} startedAt={startedAt} />}
      now={runs.dataUpdatedAt}
      root={root}
      datadogUrl={project?.telemetry?.datadog_url ?? null}
      actions={<PageRunButton run={scopeRun} />}
      dispatch={dispatch}
    />
  )
}
