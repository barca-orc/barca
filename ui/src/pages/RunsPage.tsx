import { useEffect } from 'react'
import { useSearchParams } from 'react-router'
import { useQueryClient } from '@tanstack/react-query'
import { match } from 'ts-pattern'
import { useHealth } from '@/hooks/useHealth'
import { useProgram } from '@/hooks/useProgram'
import { useRunDetail, useRuns } from '@/hooks/useRuns'
import { remote } from '@/lib/remote'
import { INITIAL_RUNS, parseRunFilters, updateRuns, writeRunFilters, type RunsModel, type RunsMsg } from '@/lib/runsPage'
import { QueuePanel } from './QueuePanel'
import { RunsView } from './RunsView'

/** Runs the Runs program: the run list and the selected run from the server, `?run=` from the URL. */
export function RunsPage() {
  const [params, setParams] = useSearchParams()
  const selected = params.get('run')
  const health = useHealth()
  const client = useQueryClient()
  // The URL owns the filters: read at each message, so back, forward and a link all apply.
  const filters = parseRunFilters(params)
  const update = (m: RunsModel, msg: RunsMsg) => updateRuns({ ...m, ...filters }, msg)
  const [program, dispatch] = useProgram(INITIAL_RUNS, update, (effect) =>
    match(effect)
      .with({ type: 'navigate' }, ({ run, replace }) => {
        setParams(previous => {
          const next = new URLSearchParams(previous)
          // Only a replacement keeps it: the same run, under its durable id.
          if (!replace) next.delete('event_gap')
          if (run) next.set('run', run)
          else next.delete('run')
          return next
        }, { replace })
      })
      .with({ type: 'filters' }, (e) => setParams(previous => writeRunFilters(previous, e.filters), { replace: true }))
      .with({ type: 'refetch_list' }, () => void client.refetchQueries({ queryKey: ['runs', 'list'] }))
      .with({ type: 'refetch_detail' }, () => void client.refetchQueries({ queryKey: ['runs', 'detail', selected] }))
      .exhaustive(),
  )
  const model = { ...program, ...filters }
  const runs = useRuns(model.limit)
  const detail = useRunDetail(selected)

  const durableId = detail.data?.run_id
  useEffect(() => {
    if (selected && durableId && durableId !== selected) dispatch({ type: 'run_resolved', id: durableId })
  }, [durableId, selected, dispatch])

  return (
    <RunsView
      model={model}
      runs={remote(runs)}
      selected={selected ? { id: selected, run: remote(detail), eventGap: params.get('event_gap') === '1' } : null}
      queue={<QueuePanel readOnly={health.data?.read_only} />}
      now={runs.dataUpdatedAt}
      dispatch={dispatch}
    />
  )
}
