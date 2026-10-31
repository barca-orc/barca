import { useEffect, useState, type ReactNode } from 'react'
import { isTimeZone, type TimeZone } from '@/lib/time'
import { TimeZoneContext } from './timeZone'

export const TIME_ZONE_STORAGE_KEY = 'barca-time-zone'

function initialZone(): TimeZone {
  if (typeof localStorage !== 'undefined') {
    const saved = localStorage.getItem(TIME_ZONE_STORAGE_KEY)
    if (isTimeZone(saved)) return saved
  }
  return 'local'
}

/** Keeps the time zone setting and persists it next to the theme. */
export function TimeZoneProvider({ children }: { children: ReactNode }) {
  const [zone, setZone] = useState<TimeZone>(initialZone)

  useEffect(() => {
    localStorage.setItem(TIME_ZONE_STORAGE_KEY, zone)
  }, [zone])

  const toggleZone = () => setZone((z) => (z === 'utc' ? 'local' : 'utc'))

  return (
    <TimeZoneContext.Provider value={{ zone, toggleZone }}>
      {children}
    </TimeZoneContext.Provider>
  )
}
