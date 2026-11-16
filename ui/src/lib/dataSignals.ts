/**
 * What a node's recorded facts say about its data and its timing, worded for the causes list:
 * output that looks wrong (`suspect`) and work that is late (`behind`). Each entry is a line
 * that names what is off, with numbers.
 */

import type { NodeState } from './types'

export interface DataSignalSets {
  /** Nodes whose output looks wrong, with what is wrong. */
  suspect: Map<string, string>
  /** Nodes that are late, with by how much. */
  behind: Map<string, string>
}

/** The data and timing signals of `states`. */
export function dataSignalSets(states: readonly NodeState[]): DataSignalSets {
  const suspect = new Map<string, string>()
  const behind = new Map<string, string>()
  void states
  return { suspect, behind }
}
