import { createContext, useContext } from 'react'
import type { TimeZone } from '@/lib/time'

export interface TimeZoneContextValue {
  zone: TimeZone
  toggleZone: () => void
}

/** Without a `TimeZoneProvider` (the kit, tests) times are local, as they are by default. */
export const TimeZoneContext = createContext<TimeZoneContextValue>({ zone: 'local', toggleZone: () => {} })

/** The zone every absolute time is shown in, and a toggle between local and UTC. */
export function useTimeZone(): TimeZoneContextValue {
  return useContext(TimeZoneContext)
}
