import { useState } from 'react'
import { holdOrder, sameOrder } from '@/lib/groupedPage'

/**
 * `entries`, kept in the order they were shown while `holding` (the pointer is over the table)
 * and `key` (sort, scope, search) is unchanged: rows never move under the pointer (`holdOrder`).
 * The order shown is kept in state, updated while rendering when it changes.
 */
export function useHeldOrder<T extends { id: string }>(entries: readonly T[], key: string, holding: boolean): T[] {
  const [held, setHeld] = useState<{ key: string; ids: string[] }>(() => ({ key, ids: entries.map((e) => e.id) }))
  const shown = held.key === key && holding ? holdOrder(held.ids, entries) : [...entries]
  const ids = shown.map((e) => e.id)
  if (held.key !== key || !sameOrder(held.ids, ids)) setHeld({ key, ids })
  return shown
}
