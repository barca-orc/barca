import { useMemo } from 'react'
import { useSearchParams } from 'react-router'
import { useAssetStates } from './useAssetStates'
import { useAssets } from './useAssets'
import { usePausedSet } from './usePauses'
import { usePinNotes } from './usePins'
import { useRollup } from './useRollup'
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
export function useAssetFilter(kind: NodeKind, sort?: Sort, poll = true) {
  const states = useAssetStates(poll)
  const { data, dataUpdatedAt } = states
  const assetQuery = useAssets()
  const graph = useMemo(() => assetQuery.data ?? [], [assetQuery.data])
  const [params, setParams] = useSearchParams()
  // The pipeline picked in the sidebar (?pipeline=<file>); null = all.
  const pipeline = params.get('pipeline')
  // `?paused=1` (the paused banner's link): only the paused assets.
  const paused = usePausedSet()
  const onlyPaused = params.get('paused') === '1'
  // Why a row is not fresh, and which node holds it, are the server's answers on the row
  // (`cause`, `root`). Pinned assets read `pinned` (`GET /pins`).
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
  // The page's kind in the rollup (`kinds`): the whole project's, or the open file's. Every
  // node of the kind is counted there by cause, so an unfiltered chip states the server's
  // number; a list cut to the paused nodes is not that population, and counts its own rows.
  const rollup = useRollup(null)
  const kinds = pipeline ? rollup.tree.files.get(pipeline)?.kinds : rollup.tree.kinds
  const counts = facetCounts(rows, filters, now, onlyPaused ? null : kinds?.[kind] ?? null)
  const filtered = applyFilters(rows, filters, now)
  const visible = sort ? sortRows(filtered, sort) : filtered
  return { kind, states, data, graph, pipeline, paused, onlyPaused, pins, rows, filters, setFilters, counts, visible, now }
}

export type AssetFilter = ReturnType<typeof useAssetFilter>
