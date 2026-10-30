/**
 * What the topbar's Run button acts on, and how it says so: pure, so each case is a test.
 *
 * A selected node is run by itself. With nothing selected, Assets runs what is open: a pipeline
 * (`get` of its file) or the whole project (`get` with no target, after a confirmation). Both
 * get assets and sensors: tasks run only by name, so the count leaves them out. Groups organize
 * nodes and are not run targets.
 */

import { match } from 'ts-pattern'
import { pipelineName, sourceFile } from './pipeline'
import type { ListNode } from './types'

export type RunTarget =
  | { kind: 'node'; name: string }
  | { kind: 'pipeline'; file: string; nodes: number }
  | { kind: 'all'; nodes: number }
  /** A group is open, or nothing to run has loaded yet. */
  | { kind: 'none'; hint: string }

export interface RunScope {
  /** The selected node's short name. */
  node: string | null
  /** The open group, which cannot be run. */
  group: string | null
  /** The open pipeline; null is every pipeline. */
  pipeline: string | null
}

const SELECT_A_NODE = 'Select a node to run'

export function runTarget({ node, group, pipeline }: RunScope, assets: readonly Pick<ListNode, 'id' | 'kind'>[]): RunTarget {
  if (node) return { kind: 'node', name: node }
  if (group) return { kind: 'none', hint: `${SELECT_A_NODE}: a group is not a run target` }
  const nodes = assets.filter((a) => a.kind !== 'task' && (!pipeline || sourceFile(a.id) === pipeline)).length
  if (nodes === 0) return { kind: 'none', hint: SELECT_A_NODE }
  return pipeline ? { kind: 'pipeline', file: pipeline, nodes } : { kind: 'all', nodes }
}

export interface RunButton {
  label: string
  title: string
  /** Asked before the run starts; null runs at once. */
  confirm: { title: string; body: string; action: string } | null
}

const count = (n: number) => `${n.toLocaleString()} node${n === 1 ? '' : 's'}`
const WHAT_RUNS = 'Assets and sensors whose cached result is current are skipped; tasks run only by name.'

export function runButton(target: RunTarget): RunButton {
  return match(target)
    .with({ kind: 'node' }, ({ name }) => ({ label: `Run ${name}`, title: `Run ${name}`, confirm: null }))
    .with({ kind: 'pipeline' }, ({ file, nodes }) => {
      const label = `Run ${pipelineName(file)} (${count(nodes)})`
      return { label, title: `${label}. ${WHAT_RUNS}`, confirm: null }
    })
    .with({ kind: 'all' }, ({ nodes }) => {
      const label = `Run all ${count(nodes)}`
      return {
        label,
        title: `${label}. ${WHAT_RUNS}`,
        confirm: { title: `${label}?`, body: `This starts one run over every pipeline. ${WHAT_RUNS}`, action: label },
      }
    })
    .with({ kind: 'none' }, ({ hint }) => ({ label: 'Run', title: hint, confirm: null }))
    .exhaustive()
}
