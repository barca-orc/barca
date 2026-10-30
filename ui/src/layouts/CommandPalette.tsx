import { useMemo } from 'react'
import { useLocation, useNavigate } from 'react-router'
import { CommandPaletteView } from '@/components/CommandPaletteView'
import { useAssets } from '@/hooks/useAssets'
import { useGroups } from '@/hooks/useGroups'
import { useProgram } from '@/hooks/useProgram'
import { useRuns } from '@/hooks/useRuns'
import { INITIAL_PALETTE, paletteActions, paletteIndex, searchPalette, updatePalette, type PaletteItem } from '@/lib/palette'
import type { TopbarRun } from './shellContext'

const RECENT_RUNS = 100

interface CommandPaletteProps {
  run: TopbarRun | null | undefined
  onRun: () => void
  onClose: () => void
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
  const items = useMemo(() => searchPalette([...actions, ...index], model.query), [actions, index, model.query])

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
      onQuery={(value) => dispatch({ type: 'query_changed', value })}
      onMove={(by) => dispatch({ type: 'moved', by, count: items.length })}
      onPick={pick}
      onClose={onClose}
    />
  )
}
