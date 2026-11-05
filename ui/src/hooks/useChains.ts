import { useMemo } from 'react'
import { useAssets } from './useAssets'
import { PROTOTYPE_CHAINS, resolveChain, type ChainDef } from '@/lib/chains'

export interface ResolvedChain { def: ChainDef; members: string[] }

/** Chains that match at least one served node. PROTOTYPE: declared in the browser. */
export function useChains(): ResolvedChain[] {
  const q = useAssets()
  const nodes = q.isPlaceholderData ? undefined : q.data
  return useMemo(
    () => (nodes ? PROTOTYPE_CHAINS.map((def) => ({ def, members: resolveChain(def, nodes) })).filter((c) => c.members.length > 0) : []),
    [nodes],
  )
}
