import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { recovering } from './polling'

/** `GET /project`: its root makes a node's file openable in an editor. */
export function useProject() {
  return useQuery({ queryKey: ['project'], queryFn: api.project, staleTime: 300_000, refetchInterval: recovering(), retry: false })
}
