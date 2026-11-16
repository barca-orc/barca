import { useSyncExternalStore } from 'react'

/** How often a time with no data of its own behind it (`Age` without `nowMs`) is read again. */
const TICK_MS = 15_000

const listeners = new Set<() => void>()
let timer: ReturnType<typeof setInterval> | null = null

function subscribe(listener: () => void) {
  listeners.add(listener)
  timer ??= setInterval(() => listeners.forEach((l) => l()), TICK_MS)
  return () => {
    listeners.delete(listener)
    if (listeners.size === 0 && timer !== null) {
      clearInterval(timer)
      timer = null
    }
  }
}

const noSubscription = () => () => {}

/** Now, to the tick: the same value for every reader within a tick, so a render never tears. */
const snapshot = () => Math.floor(Date.now() / TICK_MS) * TICK_MS

/**
 * The time now, refreshed every 15 s by one timer shared by every reader; `enabled: false`
 * subscribes to nothing (a reader that has its own `now`).
 */
export function useNow(enabled = true): number {
  return useSyncExternalStore(enabled ? subscribe : noSubscription, snapshot, snapshot)
}
