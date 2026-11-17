import { CausesList, DataSignals } from '@/components/CausesList'
import { HealthMap } from '@/components/HealthMap'
import { indexRollup, ROOT } from '@/lib/rollupView'
import { signals, todos } from '@/lib/causesView'
import { causeCounts, counts, failureRoot, heldBy, heldRoot, nodeEntry, rollup, scopeEntry } from '@/lib/testdata/rollup'
import { NOW_MS } from '../fixtures'
import { Spec, Variant } from '../Kit'

// A rollup as `GET /state/rollup` answers it: a failing partitioned node with the six nodes it
// holds, twelve sources of which three no tick reaches, and twenty exports that are fresh.
const levers = Array.from({ length: 6 }, (_, i) => nodeEntry(`drivers.py:lever_${i}`, 'group:p.py:drivers', 'blocked', { cause: 'failing' }))
const sources = Array.from({ length: 12 }, (_, i) => nodeEntry(`inputs.py:source_${i}`, 'group:p.py:inputs', i < 3 ? 'behind' : 'ok', i < 3 ? { cause: 'no_refresh_path' } : {}))
const exports = Array.from({ length: 20 }, (_, i) => nodeEntry(`publish.py:export_${i}`, 'group:p.py:publish', 'ok'))
const cutoff = nodeEntry('drivers.py:cutoff', 'group:p.py:drivers', 'failing', { cause: 'failing', keys: { failed: 400, behind: 0, ok: 0, cause: 'failing' } })
const failure = failureRoot('drivers.py:cutoff', "TypeError: cutoff() got an unexpected keyword argument 'registry'", 7)
const unscheduled = heldRoot({ kind: 'no_schedule', node: 'inputs.py:source_0', detail: 'no tick reaches source_0', nodes: 3, causes: causeCounts({ no_refresh_path: 3 }), unbounded: 3, action: { kind: 'add_schedule', command: null, estimated_seconds: 42 } })
const held = [
  heldBy(failure.id, 7, 7, { causes: causeCounts({ failing: 7 }), unbounded: 7, gap_seconds: 93_600 }),
  heldBy(unscheduled.id, 3, 10, { causes: causeCounts({ no_refresh_path: 3 }), unbounded: 3 }),
]
const tree = indexRollup(rollup([
  scopeEntry(ROOT, 'root', null, ['group:p.py:drivers', 'group:p.py:inputs', 'group:p.py:publish'], { failing: 1, blocked: 6, behind: 3, ok: 29 }, { causes: causeCounts({ failing: 7, no_refresh_path: 3 }), not_fresh: 10, unbounded: 10, held_by: held }),
  scopeEntry('group:p.py:drivers', 'group', ROOT, [cutoff.id, ...levers.map((n) => n.id)], { failing: 1, blocked: 6 }, { label: 'Drivers', causes: causeCounts({ failing: 7 }), not_fresh: 7, unbounded: 7, held_by: [held[0]!] }),
  cutoff, ...levers,
  scopeEntry('group:p.py:inputs', 'group', ROOT, sources.map((n) => n.id), { behind: 3, ok: 9 }, { label: 'Inputs', causes: causeCounts({ no_refresh_path: 3 }), not_fresh: 3, unbounded: 3, held_by: [heldBy(unscheduled.id, 3, 3, { causes: causeCounts({ no_refresh_path: 3 }), unbounded: 3 })] }),
  ...sources,
  scopeEntry('group:p.py:publish', 'group', ROOT, exports.map((n) => n.id), { ok: 20 }, { label: 'Publish' }),
  ...exports,
], [failure, unscheduled]))
const healthy = indexRollup(rollup([
  scopeEntry(ROOT, 'root', null, ['group:p.py:publish'], { ok: 20 }),
  scopeEntry('group:p.py:publish', 'group', ROOT, exports.map((n) => n.id), { ok: 20 }, { label: 'Publish', asset_health: counts({ ok: 20 }) }),
  ...exports,
]))

const root = tree.entries.get(ROOT)!
const list = todos(root, tree.roots)
const data = signals({
  suspect: new Map([['drivers.py:lever_3', '2 of 1,205 keys wrote 0 rows (previously 0)']]),
  behind: new Map([['drivers.py:lever_3_all', 'older than its input lever_3']]),
})
const name = (id: string) => id.split(':')[1]!
const noop = () => {}

export function Health() {
  return (
    <>
      <div id="kit-health" />
      <Spec title="HealthMap" note="A scope as a sunburst over the rollup: a segment's angle is how far what is underneath is from fresh, its colours what is inside it, by cause. Click to zoom, the centre to go up.">
        <Variant label="problems"><HealthMap tree={tree} focus={ROOT} onFocus={noop} onOpen={noop} size={260} /></Variant>
        <Variant label="zoomed"><HealthMap tree={tree} focus="group:p.py:drivers" onFocus={noop} onOpen={noop} size={260} /></Variant>
        <Variant label="what a root holds"><HealthMap tree={tree} focus={ROOT} onFocus={noop} onOpen={noop} lit={new Set(['group:p.py:inputs'])} size={260} /></Variant>
        <Variant label="fresh"><HealthMap tree={healthy} focus={ROOT} onFocus={noop} onOpen={noop} size={260} /></Variant>
      </Spec>
      <Spec title="CausesList" note="The to-do list: one entry per root as the server ranks them, with what it holds, why, and what clears it.">
        <Variant label="roots"><div style={{ width: 620 }}><CausesList todos={list} scope={root} name={name} onOpen={noop} failingSince={() => '2026-11-16 09:00:00'} nowMs={NOW_MS} /></div></Variant>
        <Variant label="one picked"><div style={{ width: 620 }}><CausesList todos={list} scope={root} name={name} onOpen={noop} selected={unscheduled.id} onSelect={noop} nowMs={NOW_MS} /></div></Variant>
        <Variant label="none"><div style={{ width: 620 }}><CausesList todos={[]} scope={{ held_by: [], not_fresh: 0 }} name={name} onOpen={noop} nowMs={NOW_MS} /></div></Variant>
        <Variant label="more than the limit"><div style={{ width: 620 }}><CausesList todos={list} scope={root} name={name} onOpen={noop} limit={1} nowMs={NOW_MS} /></div></Variant>
        <Variant label="data signals"><div style={{ width: 620 }}><DataSignals signals={data} name={name} onOpen={noop} /></div></Variant>
      </Spec>
    </>
  )
}
