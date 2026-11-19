import { writeFileSync } from 'node:fs'
import { it } from 'vitest'
import { NOW_MS, PINS, PAUSES, ROLLUP, RUNS, RUN_IDS, STATE, STATE_PINNED, entry, row, runDetail, runRow } from './testdata/project'
import { ROOT, indexRollup, freshWithin, noEstimate, rootsWithin, sunburst, heldIds, leafIds } from './rollupView'
import { todos, heldLine, heldCauses, heldGap, actionWords, explained } from './causesView'
import { nodeEstimate, scopeEstimate } from './estimates'
import { failures, failedKeyNodes, consumersOf, downstreamIds } from './health'
import { blockerOf } from './healthDisplay'
import { graphState } from './graphState'
import { buildRows, sortRows, DEFAULT_SORT, lastRunText, typicalText } from './assetTable'
import { dataSignalSets } from './dataSignals'
import { pinNotes } from './pins'
import { pausedIds } from './pauses'
import { runOutcomeSummary } from './runFeedback'
import { groupRunSteps, activityBarLabel } from './runsPage'
import { keyParts, whatChanged } from './nodeDetail'
import { causeLine, leadingPart } from './causeDisplay'
import { failureTable } from './failuresView'

it('explore', () => {
  const out: Record<string, unknown> = {}
  const tree = indexRollup(ROLLUP)
  const r = tree.entries.get(ROOT)!
  out.root = [r.nodes, r.assets, r.not_fresh, r.weight, r.fresh_by, r.unbounded, freshWithin(r, tree.asOf), noEstimate(r), tree.asOf]
  out.arcs = sunburst(tree, ROOT, 3).map((a) => [a.id, a.depth, a.kind, +(a.end - a.start).toFixed(4), a.bands.map((b) => b.part).join('+'), a.label])
  out.rootsWithin = [rootsWithin(tree, 'file:billing.py').map((x) => x.id), rootsWithin(tree, 'group:orders.py:reporting').map((x) => x.id)]
  out.held = [...heldIds(tree, STATE, 'failure:orders.py:sales_by_day')].sort()
  out.leaf = leafIds(tree, 'file:billing.py')
  const list = todos(r, ROLLUP.roots)
  out.todos = list.map((t) => [t.root.id, t.same.map((s) => s.root.id), heldLine(t), heldCauses(t), heldGap(t), actionWords(t.root)])
  out.todosBilling = todos(entry('file:billing.py'), ROLLUP.roots).map((t) => [t.root.id, t.same.map((s) => s.root.id), heldLine(t)])
  out.explained = [explained(r), explained(entry('file:orders.py'))]
  out.est = STATE.map((n) => [n.name, nodeEstimate(n, 'utc', NOW_MS)])
  out.scope = ['root', 'file:orders.py', 'file:billing.py', 'group:orders.py:reporting', 'orders.py:orders'].map((id) => [id, scopeEstimate(entry(id), 'utc', NOW_MS)])
  const byId = new Map(STATE.map((n) => [n.id, n]))
  out.failures = failures(ROLLUP.roots, byId)
  out.failureTable = failureTable(failures(ROLLUP.roots, byId))
  out.failedKeyNodes = failedKeyNodes(STATE)
  out.blocker = STATE.map((n) => [n.name, blockerOf(n)]).filter((x) => x[1])
  out.down = [...downstreamIds('billing.py:invoices', consumersOf(STATE))]
  out.graph = STATE.map((n) => [n.name, graphState(n)])
  const rows = sortRows(buildRows(STATE, NOW_MS, pinNotes(PINS)), DEFAULT_SORT)
  out.rows = rows.map((x) => [x.name, lastRunText(x, NOW_MS), typicalText(x), JSON.stringify(x).slice(0, 700)])
  out.signals = dataSignalSets(STATE, NOW_MS)
  out.sig2 = [...dataSignalSets(STATE, NOW_MS).behind]
  out.pins = [...pinNotes(PINS)]
  out.paused = [...pausedIds(PAUSES)]
  out.runs = Object.values(RUN_IDS).map((id) => [id, runOutcomeSummary(runDetail(id)), runRow(id).line, groupRunSteps(runDetail(id).steps, runDetail(id).steps_page.nodes ?? []).map((g) => JSON.stringify(g).slice(0, 300))])
  out.expl = Object.values(RUN_IDS).map((id) => runDetail(id).explanation)
  out.activity = [RUNS.activity!.summary, RUNS.activity!.hours.filter((h) => h.runs > 0).map((h) => activityBarLabel(h, 'utc')), RUNS.total, RUNS.runs.length]
  out.keys = [keyParts(row('sales_by_day').partitions!), keyParts(row('sales_report').partitions!), entry('orders.py:sales_by_day').keys, entry('orders.py:sales_report').keys]
  out.changed = STATE.map((n) => [n.name, n.cache.change_causes, whatChanged(n.cache, false)]).filter((x) => x[1])
  out.files = ROLLUP.files.map((f) => [f.file, f.nodes, f.worst, causeLine(f.causes), leadingPart(f.causes), f.fresh])
  out.kinds = ROLLUP.kinds
  out.pinnedDiff = STATE_PINNED.filter((n, i) => JSON.stringify(n) !== JSON.stringify(STATE[i])).map((n) => [n.name, n.cache.state, n.cache.reason, n.health, n.partitions ?? null])
  out.sched = STATE.filter((n) => n.schedule).map((n) => [n.name, n.schedule, n.next_run, n.prev_run, n.late_since])
  writeFileSync('/tmp/claude-1000/-home-mukulram-Personal-barca/4ecd3a96-7d4b-4949-a936-27d72e93bf8e/scratchpad/explore.json', JSON.stringify(out))
})
