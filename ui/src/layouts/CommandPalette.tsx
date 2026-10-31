import { useEffect, useMemo, useState } from 'react'
import { useLocation, useNavigate } from 'react-router'
import { CommandPaletteView } from '@/components/CommandPaletteView'
import { useAssets } from '@/hooks/useAssets'
import { useWholeState } from '@/hooks/useAssetStates'
import { useGroups } from '@/hooks/useGroups'
import { usePartitionKeySearch } from '@/hooks/usePartitionKeys'
import { useProgram } from '@/hooks/useProgram'
import { useRuns } from '@/hooks/useRuns'
import { INITIAL_PALETTE, isKeyQuery, KEY_SEARCH_NODES, keyItems, paletteActions, paletteIndex, searchPalette, updatePalette, type PaletteItem } from '@/lib/palette'
import type { TopbarRun } from './shellContext'

const RECENT_RUNS = 100
/** How long typing pauses before the key query is sent to the partitioned nodes. */
const KEY_SEARCH_DEBOUNCE_MS = 150
const NO_NODES: string[] = []

interface CommandPaletteProps {
  run: TopbarRun | null | undefined
  onRun: () => void
  onClose: () => void
}

/** `value`, once it has held still for `ms`. */
function useDebounced(value: string, ms: number): string {
  const [settled, setSettled] = useState(value)
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), ms)
    return () => clearTimeout(timer)
  }, [value, ms])
  return settled
}

/** Mounted only while open, so its data is asked for only then. */
export function CommandPalette({ run, onRun, onClose }: CommandPaletteProps) {
  const [model, dispatch] = useProgram(INITIAL_PALETTE, updatePalette, () => {})
  const navigate = useNavigate()
  const location = useLocation()
  const assetQuery = useAssets()
  const assets = assetQuery.isPlaceholderData ? undefined : assetQuery.data
  const groups = useGroups().data
  const runs = useRuns(RECENT_RUNS).data?.runs
  const index = useMemo(() => paletteIndex(assets ?? [], groups ?? [], runs ?? []), [assets, groups, runs])
  const canGraph = location.pathname === '/assets' && !new URLSearchParams(location.search).has('view')
  const actions = paletteActions(run && !run.disabled ? run.label : null, canGraph)
  // A key lives on a partitioned node, which every node's state (`GET /state`) knows, whatever is open; the first few are asked.
  const states = useWholeState().data
  const partitioned = useMemo(() => states?.filter((n) => n.partitioned).map((n) => n.id).slice(0, KEY_SEARCH_NODES) ?? NO_NODES, [states])
  const keyQuery = useDebounced(isKeyQuery(model.query) ? model.query.trim() : '', KEY_SEARCH_DEBOUNCE_MS)
  const { hits, searching, answeredAt } = usePartitionKeySearch(partitioned, keyQuery)
  const keys = useMemo(() => keyItems(hits, answeredAt), [hits, answeredAt])
  const items = useMemo(() => searchPalette([...actions, ...index], model.query, keys), [actions, index, model.query, keys])

  const pick = (item: PaletteItem) => {
    onClose()
    if (item.target.type === 'go') navigate(item.target.to)
    else if (item.target.type === 'run') onRun()
    else {
      const next = new URLSearchParams(location.search)
      next.set('view', 'graph')
      navigate(`/assets?${next}`)
    }
  }

  return (
    <CommandPaletteView
      query={model.query}
      items={items}
      active={model.active}
      searching={searching && keyQuery !== ''}
      onQuery={(value) => dispatch({ type: 'query_changed', value })}
      onMove={(by) => dispatch({ type: 'moved', by, count: items.length })}
      onPick={pick}
      onClose={onClose}
    />
  )
}
