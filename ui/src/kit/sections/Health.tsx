import { CausesList } from '@/components/CausesList'
import { HealthMap } from '@/components/HealthMap'
import { indexRollup, ROOT } from '@/lib/rollupView'
import type { Cause } from '@/lib/causesView'
import { counts, nodeEntry, rollup, scopeEntry } from '@/lib/testdata/rollup'
import { NOW_MS } from '../fixtures'
import { Spec, Variant } from '../Kit'

// A rollup as `GET /state/rollup` answers it: a failing partitioned node with the six nodes it
// blocks, twelve sources of which three are behind, and twenty exports that are fine.
const levers = Array.from({ length: 6 }, (_, i) => nodeEntry(`drivers.py:lever_${i}`, 'group:p.py:drivers', 'blocked'))
const sources = Array.from({ length: 12 }, (_, i) => nodeEntry(`inputs.py:source_${i}`, 'group:p.py:inputs', i < 3 ? 'behind' : 'ok'))
const exports = Array.from({ length: 20 }, (_, i) => nodeEntry(`publish.py:export_${i}`, 'group:p.py:publish', 'ok'))
const cutoff = nodeEntry('drivers.py:cutoff', 'group:p.py:drivers', 'failing', { keys: { failed: 400, behind: 0, ok: 0 } })
const tree = indexRollup(rollup([
  scopeEntry(ROOT, 'root', null, ['group:p.py:drivers', 'group:p.py:inputs', 'group:p.py:publish'], { failing: 1, blocked: 6, behind: 3, ok: 29 }),
  scopeEntry('group:p.py:drivers', 'group', ROOT, [cutoff.id, ...levers.map((n) => n.id)], { failing: 1, blocked: 6 }, { label: 'Drivers' }),
  cutoff, ...levers,
  scopeEntry('group:p.py:inputs', 'group', ROOT, sources.map((n) => n.id), { behind: 3, ok: 9 }, { label: 'Inputs' }),
  ...sources,
  scopeEntry('group:p.py:publish', 'group', ROOT, exports.map((n) => n.id), { ok: 20 }, { label: 'Publish' }),
  ...exports,
]))
const healthy = indexRollup(rollup([
  scopeEntry(ROOT, 'root', null, ['group:p.py:publish'], { ok: 20 }),
  scopeEntry('group:p.py:publish', 'group', ROOT, exports.map((n) => n.id), { ok: 20 }, { label: 'Publish', asset_health: counts({ ok: 20 }) }),
  ...exports,
]))

const causes: Cause[] = [
  { kind: 'failure', headline: "TypeError: cutoff() got an unexpected keyword argument 'registry'", node: 'drivers.py:cutoff', others: 0, failedKeys: 400, held: 7, heldAssets: 7, failingSince: '2026-11-16 09:00:00' },
  { kind: 'suspect', node: 'drivers.py:lever_3', detail: '2 of 1,205 keys wrote 0 rows (previously 0)' },
  { kind: 'behind', node: 'drivers.py:lever_3_all', detail: 'Built Nov 14, before its input lever_3 (today 17:32)' },
]
const noop = () => {}

export function Health() {
  return (
    <>
      <div id="kit-health" />
      <Spec title="HealthMap" note="A scope as a sunburst over the rollup: angle is how many assets are underneath, colour the worst health inside. Click to zoom, the centre to go up.">
        <Variant label="problems"><HealthMap tree={tree} focus={ROOT} onFocus={noop} onOpen={noop} size={260} /></Variant>
        <Variant label="zoomed"><HealthMap tree={tree} focus="group:p.py:drivers" onFocus={noop} onOpen={noop} size={260} /></Variant>
        <Variant label="healthy"><HealthMap tree={healthy} focus={ROOT} onFocus={noop} onOpen={noop} size={260} /></Variant>
      </Spec>
      <Spec title="CausesList" note="One entry per root, the failure holding the most nodes first.">
        <Variant label="causes"><div style={{ width: 520 }}><CausesList causes={causes} name={(id) => id.split(':')[1]!} onOpen={noop} nowMs={NOW_MS} /></div></Variant>
        <Variant label="none"><div style={{ width: 520 }}><CausesList causes={[]} name={(id) => id} onOpen={noop} nowMs={NOW_MS} /></div></Variant>
        <Variant label="more than the limit"><div style={{ width: 520 }}><CausesList causes={causes} name={(id) => id.split(':')[1]!} onOpen={noop} limit={1} onShowAll={noop} nowMs={NOW_MS} /></div></Variant>
      </Spec>
    </>
  )
}
