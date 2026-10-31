import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { recovering } from './polling'

export function useGroups() {
  return useQuery({ queryKey: ['groups'], queryFn: api.groups, staleTime: 5_000, refetchInterval: recovering(), retry: false })
}
