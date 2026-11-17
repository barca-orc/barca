import { CausesList } from '@/components/CausesList'
import { HealthMap } from '@/components/HealthMap'
import { buildHealthTree, ROOT } from '@/lib/healthTree'
import type { Cause } from '@/lib/causes'
import type { NodeGroup, NodeState } from '@/lib/types'
import { NOW_MS } from '../fixtures'
import { Spec, Variant } from '../Kit'
import { served } from '@/lib/testdata/served'

function node(id: string, inputs: string[], over: Partial<NodeState> = {}): NodeState {
  return served({
    id, name: id.split(':')[1]!, kind: 'asset', inputs, partitioned: false,
    cache: { state: 'cached', reason: 'materialized', detail: '' },
    last_materialization: { status: 'success', created_at: '2026-11-16 10:00:00', elapsed_seconds: 1, run_hash: null, artifact: null, format: null, size_bytes: null, rows: null },
    shape: null, env: [], durations: null, next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0, running: null, prev_run: null, late_since: null, signals: [], ...over,
  })
}
const failed = { last_materialization: { status: 'failed', created_at: '2026-11-16 11:00:00', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, rows: null, error: 'TypeError: x' } }
const stale = { cache: { state: 'stale' as const, reason: 'changed' as const, detail: '' } }

const states: NodeState[] = [
  node('drivers.py:cutoff', [], { ...failed, partitioned: true, partitions: { total: 400, cached: 0, missing: 400, missing_keys: [], failed: 400 } }),
  ...Array.from({ length: 6 }, (_, i) => node(`drivers.py:lever_${i}`, ['drivers.py:cutoff'], { cache: { state: 'unknown', reason: 'cutoff_pending', detail: '' } })),
  ...Array.from({ length: 12 }, (_, i) => node(`inputs.py:source_${i}`, [], i < 3 ? stale : {})),
  ...Array.from({ length: 20 }, (_, i) => node(`publish.py:export_${i}`, [], {})),
]
const groups: NodeGroup[] = [
  { id: 'g:drivers', name: 'Drivers', description: '', members: states.filter((s) => s.id.startsWith('drivers')).map((s) => s.id), output: 'drivers.py:lever_0' },
  { id: 'g:inputs', name: 'Inputs', description: '', members: states.filter((s) => s.id.startsWith('inputs')).map((s) => s.id), output: 'inputs.py:source_0' },
  { id: 'g:publish', name: 'Publish', description: '', members: states.filter((s) => s.id.startsWith('publish')).map((s) => s.id), output: 'publish.py:export_0' },
]
const graph = states.map((s) => ({ id: s.id, inputs: s.inputs }))
const tree = buildHealthTree({ states, graph, groups })
const healthy = buildHealthTree({ states: states.slice(7).map((s) => ({ ...s, cache: { state: 'cached', reason: 'materialized', detail: '' } })), graph, groups })

const causes: Cause[] = [
  { kind: 'failure', headline: "TypeError: cutoff() got an unexpected keyword argument 'registry'", nodes: ['drivers.py:cutoff'], failedKeys: 400, blocked: 6, at: '2026-11-16 11:00:00', failingSince: '2026-11-16 09:00:00', runs: 4 },
  { kind: 'suspect', node: 'drivers.py:lever_3', detail: '2 of 1,205 keys wrote 0 rows (previously 0)' },
  { kind: 'behind', node: 'drivers.py:lever_3_all', detail: 'Built Nov 14, before its input lever_3 (today 17:32)' },
]
const noop = () => {}

export function Health() {
  return (
    <>
      <div id="kit-health" />
      <Spec title="HealthMap" note="A scope as a sunburst: angle is how much is wrong underneath, colour the worst health inside. Click to zoom, the centre to go up.">
        <Variant label="problems"><HealthMap tree={tree} focus={ROOT} onFocus={noop} onOpen={noop} size={260} /></Variant>
        <Variant label="zoomed"><HealthMap tree={tree} focus="g:drivers" onFocus={noop} onOpen={noop} size={260} /></Variant>
        <Variant label="healthy"><HealthMap tree={healthy} focus={ROOT} onFocus={noop} onOpen={noop} size={260} /></Variant>
      </Spec>
      <Spec title="CausesList" note="One entry per root cause, the biggest first.">
        <Variant label="causes"><div style={{ width: 520 }}><CausesList causes={causes} name={(id) => id.split(':')[1]!} onOpen={noop} nowMs={NOW_MS} /></div></Variant>
        <Variant label="none"><div style={{ width: 520 }}><CausesList causes={[]} name={(id) => id} onOpen={noop} nowMs={NOW_MS} /></div></Variant>
        <Variant label="more than the limit"><div style={{ width: 520 }}><CausesList causes={causes} name={(id) => id.split(':')[1]!} onOpen={noop} limit={1} onShowAll={noop} nowMs={NOW_MS} /></div></Variant>
      </Spec>
    </>
  )
}
