import { useMemo } from 'react'
import { useSearchParams } from 'react-router'
import { useAssetStates } from './useAssetStates'
import { useAssets } from './useAssets'
import { usePausedSet } from './usePauses'
import { usePinNotes } from './usePins'
import { inPipeline } from '@/lib/pipeline'
import { applyFilters, facetCounts, parseFilters, writeFilters, type Filters, type LastRun } from '@/lib/assetFilters'
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
  // Which failing node blocks a row is the server's answer on the row (`root`), judged over
  // the whole graph. Pinned assets read `pinned` (`GET /pins`), and pass no failure above them on.
  const pins = usePinNotes()
  const rows = useMemo(
    () => buildRows((data ?? []).filter((n) => n.kind === kind && inPipeline(n.id, pipeline) && (!onlyPaused || paused.has(n.id))), dataUpdatedAt, pins),
    [data, dataUpdatedAt, pipeline, kind, onlyPaused, paused, pins],
  )
  // Filters live in the URL with the sort and pipeline, so a filtered view can be reloaded and
  // shared. Every page opens unfiltered: what it lists is what its controls say (failures sort
  // first, so what needs a look is on top without a hidden filter).
  const filters = { ...parseFilters(params), kinds: [] }
  const setFilters = (f: Filters) => setParams(writeFilters(params, f), { replace: true })
  const now = dataUpdatedAt
  const counts = facetCounts(rows, filters, now)
  const filtered = applyFilters(rows, filters, now)
  const visible = sort ? sortRows(filtered, sort) : filtered
  // What fails, and how much of the graph waits on it: the failing segment's tooltip.
  // The blocked count is the rows that read `blocked`, so the tooltip and the chip agree.
  const blocked = rows.filter((r) => !r.lastFailed && r.pinned === null && (r.blockedBy || r.state === 'blocked')).length
  return { kind, states, data, graph, pipeline, paused, onlyPaused, pins, rows, filters, setFilters, counts, visible, blocked, now }
}

export type AssetFilter = ReturnType<typeof useAssetFilter>
