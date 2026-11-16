import type { NodeState } from './types'

/** A state response already loaded: its rows and when they arrived (ms). */
export interface LoadedStates {
  data: readonly NodeState[] | undefined
  updatedAt: number
}

/**
 * What a page shows while the open scope's state loads: the scope's own rows once they have
 * arrived; until then, every node's newest row from the state responses already loaded (another
 * scope, the whole project). Those are the server's own last answers, not invented states, so a
 * card or row keeps its state across a navigation instead of flashing "Checking…"; a node no
 * response has carried stays missing, which is the one case that reads "Checking…". `undefined`
 * when nothing has ever loaded.
 */
export function knownStates(current: readonly NodeState[] | undefined, loaded: readonly LoadedStates[]): readonly NodeState[] | undefined {
  if (current) return current
  const newest = new Map<string, { state: NodeState; at: number }>()
  for (const { data, updatedAt } of loaded) {
    for (const state of data ?? []) {
      const known = newest.get(state.id)
      if (!known || known.at < updatedAt) newest.set(state.id, { state, at: updatedAt })
    }
  }
  return newest.size > 0 ? [...newest.values()].map(({ state }) => state) : undefined
}
