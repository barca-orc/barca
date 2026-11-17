import { useCallback, useState } from 'react'

function read(key: string, fallback: boolean): boolean {
  try {
    const v = window.localStorage.getItem(key)
    return v === null ? fallback : v === '1'
  } catch {
    return fallback
  }
}

/** A per-browser on/off preference (a collapsed section), remembered across reloads when storage allows. */
export function useStoredFlag(key: string, fallback: boolean): [boolean, (value: boolean) => void] {
  const [value, setValue] = useState(() => read(key, fallback))
  const set = useCallback(
    (next: boolean) => {
      setValue(next)
      try {
        window.localStorage.setItem(key, next ? '1' : '0')
      } catch {
        // Private windows and blocked storage: the preference lasts this page only.
      }
    },
    [key],
  )
  return [value, set]
}
