import { useMemo } from 'react'
import { groupTree, type GroupTree } from '@/lib/groups'
import { useGroups } from './useGroups'
import { useRollup } from './useRollup'

/**
 * The groups of `pipeline` (every pipeline for null) as a tree, each group listing the members
 * the rollup counts under it: the whole project's rollup places a node declared in several
 * groups under one of them, the same in every scope. Until it has loaded, the tree is the
 * declarations as `GET /groups` gives them.
 */
export function useGroupTree(pipeline: string | null): GroupTree {
  const groups = useGroups()
  const rollup = useRollup(null)
  const placed = rollup.loaded ? rollup.tree.entries : undefined
  return useMemo(
    () => groupTree((groups.data ?? []).filter((g) => !pipeline || g.id.startsWith(`group:${pipeline}:`)), placed),
    [groups.data, pipeline, placed],
  )
}
