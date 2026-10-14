import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import type { NodeState } from '@/lib/types'

/** Inspect on selection and when the node or an input has a new artifact. */
export function useAssetSchema(node: NodeState, inputs: NodeState[]) {
  const revisions = [node, ...inputs].map(n => [n.id, n.last_materialization])
  return useQuery({
    queryKey: ['asset-schema', node.id, revisions],
    queryFn: () => api.assetSchema(node.id),
    staleTime: Infinity,
    retry: false,
  })
}
