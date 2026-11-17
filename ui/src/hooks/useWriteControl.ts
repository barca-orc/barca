import { useHealth } from './useHealth'
import { writeAccess, writeControl, type WriteControl } from '@/lib/writeAccess'

/** How this server's write actions are drawn (`lib/writeAccess.ts`): every write button reads this. */
export function useWriteControl(): WriteControl {
  return writeControl(writeAccess(useHealth().data))
}
