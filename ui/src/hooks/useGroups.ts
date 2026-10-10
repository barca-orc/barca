import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'

export function useGroups() {
  return useQuery({ queryKey: ['groups'], queryFn: api.groups, staleTime: 5_000, retry: false })
}
