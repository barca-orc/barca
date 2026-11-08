import { useMemo } from 'react'
import { useSearchParams } from 'react-router'
import { useAssetStates } from './useAssetStates'
import { useAssets } from './useAssets'
import { usePausedSet } from './usePauses'
import { usePinNotes } from './usePins'
import { blockedBy } from '@/lib/graphView'
import { inPipeline } from '@/lib/pipeline'
import {
  applyFilters,
  facetCounts,
  NEEDS_ATTENTION,
  NO_DEFAULTS,
  RECENT_MS,
  onDefaultStates,
  parseFilters,
  writeFilters,
  type Filters,
  type LastRun,
} from '@/lib/assetFilters'
import { buildRows, sortRows, type Sort } from '@/lib/assetTable'
import type { NodeKind } from '@/lib/types'

export const PAGE_LABEL: Record<NodeKind, string> = { asset: 'Assets', task: 'Tasks', sensor: 'Sensors' }

export const LAST_RUN_LABEL: Record<LastRun, string> = {
  any: 'Any time',
  hour: 'Last hour',
  day: 'Last 24h',
  week_plus: '7+ days ago',
  never: 'Not run',
}

/** Add or remove `v` from a multi-select list. */
export function toggle<T>(list: T[], v: T): T[] {
  return list.includes(v) ? list.filter((x) => x !== v) : [...list, v]
}

/**
 * A page of one kind's rows and the URL's filters over them: what the list shows as rows and
 * the graph leaves undimmed. Both views of Assets read it, so they count and filter alike.
 */
export function useAssetFilter(kind: NodeKind, sort?: Sort) {
  const states = useAssetStates()
  const { data, dataUpdatedAt } = states
  const assetQuery = useAssets()
  const graph = useMemo(() => assetQuery.data ?? [], [assetQuery.data])
  const [params, setParams] = useSearchParams()
  // The pipeline picked in the sidebar (?pipeline=<file>); null = all.
  const pipeline = params.get('pipeline')
  // `?paused=1` (the paused banner's link): only the paused assets.
  const paused = usePausedSet()
  const onlyPaused = params.get('paused') === '1'
  // Which nodes a failure upstream keeps from running: judged over the whole graph, so a
  // pipeline's row is blocked by a failure in another one too.
  // Pinned assets read `pinned` (`GET /pins`), and pass no failure above them on.
  const pins = usePinNotes()
  const blockers = useMemo(() => blockedBy(data ?? [], graph, pins), [data, graph, pins])
  const rows = useMemo(
    () => buildRows((data ?? []).filter((n) => n.kind === kind && inPipeline(n.id, pipeline) && (!onlyPaused || paused.has(n.id))), dataUpdatedAt, blockers, pins),
    [data, dataUpdatedAt, pipeline, kind, onlyPaused, paused, blockers, pins],
  )
  // Filters live in the URL with the sort and pipeline, so a filtered view can
  // be reloaded and shared.
  const defaultStates = kind === 'task' ? NEEDS_ATTENTION : NO_DEFAULTS
  const filters = { ...parseFilters(params, defaultStates), kinds: [] }
  const onDefault = onDefaultStates(filters, defaultStates)
  const setFilters = (f: Filters) => setParams(writeFilters(params, f, defaultStates), { replace: true })
  const now = dataUpdatedAt
  // The default also shows what ran recently: a task that just ran is what the page is for.
  const opts = onDefault ? { recentMs: RECENT_MS } : {}
  const counts = facetCounts(rows, filters, now, opts)
  const filtered = applyFilters(rows, filters, now, opts)
  const visible = sort ? sortRows(filtered, sort) : filtered
  // What fails, and how much of the graph waits on it: the failing segment's tooltip.
  // The blocked count is the rows that read `blocked`, so the tooltip and the chip agree.
  const blocked = rows.filter((r) => !r.lastFailed && r.pinned === null && r.blockedBy).length
  return { kind, states, data, graph, pipeline, paused, onlyPaused, pins, blockers, rows, filters, onDefault, setFilters, counts, visible, blocked, now }
}

export type AssetFilter = ReturnType<typeof useAssetFilter>
