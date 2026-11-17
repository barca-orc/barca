import { useMemo } from 'react'
import { useAssets } from './useAssets'
import { useGroups } from './useGroups'
import { useWholeState } from './useAssetStates'
import { dataSignalSets, type DataSignalSets } from '@/lib/dataSignals'
import { buildHealthTree, type HealthTree } from '@/lib/healthTree'
import { inPipeline } from '@/lib/pipeline'
import type { NodeState } from '@/lib/types'

export interface ProjectHealth {
  tree: HealthTree
  /** The rows the tree was built from: the whole project's, the page's fresher rows in place. */
  states: NodeState[]
  signals: DataSignalSets
  /** The whole project's state has loaded once. */
  loaded: boolean
}

/**
 * The project's health tree, built once per page from the whole project's state (shared with
 * the sidebar) with the page's own fresher rows in place, the graph and the groups. Every count
 * a page shows for a scope reads it (its `healthOf`), and the health map draws it, so they agree.
 */
export function useHealthTree(pipeline: string | null, fresher: readonly NodeState[] = []): ProjectHealth {
  const whole = useWholeState()
  const assets = useAssets()
  const groups = useGroups()
  const states = useMemo(() => {
    const byId = new Map((whole.data ?? []).map((s) => [s.id, s]))
    for (const s of fresher) byId.set(s.id, s)
    return [...byId.values()].filter((s) => inPipeline(s.id, pipeline))
  }, [whole.data, fresher, pipeline])
  const graph = useMemo(() => (assets.data ?? []).map((a) => ({ id: a.id, inputs: a.inputs })), [assets.data])
  const groupList = useMemo(
    () => (groups.data ?? []).filter((g) => !pipeline || g.id.startsWith(`group:${pipeline}:`)),
    [groups.data, pipeline],
  )
  const signals = useMemo(() => dataSignalSets(states), [states])
  const tree = useMemo(
    () => buildHealthTree({ states, graph, groups: groupList }),
    [states, graph, groupList],
  )
  return { tree, states, signals, loaded: !!whole.data }
}
