import { useState } from 'react'
import { DagNode } from '@/components'
import { GraphCanvas } from '@/components/graph/GraphCanvas'
import type { GraphEntry } from '@/lib/graph'
import { groupHealth, withPorts } from '@/lib/groups'
import { graphState, isLive, liveGraphState, type GraphState } from '@/lib/graphState'
import { isFailed } from '@/lib/health'
import { runningGraphState } from '@/lib/running'
import { NODE_KINDS, STATUS_KINDS, failedNode, failedTask, neverSucceededNode, nodeByState, runningByStep, runningPartitioned, variants } from '../fixtures'
import { Frame, Spec, Variant } from '../Kit'

function StateNode({ name, state, kind = 'asset' }: { name: string; state: GraphState; kind?: 'asset' | 'task' | 'sensor' }) {
  return <DagNode name={name} kind={kind} kindLabel={kind === 'asset' ? undefined : kind} tone={state.tone} glyph={state.glyph} label={state.label} lastFailed={isFailed(state)} live={isLive(state)} stateHint={state.hint} />
}

// A small pipeline with a node in each kind of state, a group, and a live edge (a cached
// node feeding a running one).
const PIPELINE: GraphEntry[] = [
  { id: 'p:checkout_events', kind: 'sensor', inputs: [] },
  { id: 'p:product_catalog', kind: 'asset', inputs: [] },
  { id: 'p:orders_clean', kind: 'asset', inputs: ['p:checkout_events', 'p:product_catalog'] },
  { id: 'p:orders_enriched', kind: 'asset', inputs: ['p:orders_clean'] },
  { id: 'group:p:modeling', kind: 'group', inputs: ['p:orders_clean'], name: 'modeling', summary: '12 assets · 12 tasks', outputName: 'model' },
  { id: 'p:revenue_daily', kind: 'asset', inputs: ['p:orders_enriched'] },
  { id: 'p:daily_report', kind: 'task', inputs: ['p:revenue_daily', 'group:p:modeling'] },
]

const PIPELINE_STATES: Record<string, GraphState> = {
  'p:checkout_events': graphState(nodeByState.unknown),
  'p:product_catalog': graphState(nodeByState.cached),
  'p:orders_clean': graphState(nodeByState.cached),
  'p:orders_enriched': runningGraphState(runningPartitioned),
  'group:p:modeling': groupHealth(['m:a', 'm:b', 'm:c', 'm:d'], new Map([
    ['m:a', failedNode], ['m:b', { ...nodeByState.cached, id: 'm:b' }], ['m:c', { ...nodeByState.stale, id: 'm:c' }], ['m:d', { ...nodeByState.cached, id: 'm:d', running: runningPartitioned }],
  ])),
  'p:revenue_daily': graphState(nodeByState.stale),
  'p:daily_report': runningGraphState(runningByStep.queued),
}

// A group's view: members in a grid where nothing connects them, and an input from outside.
const MEMBERS: GraphEntry[] = withPorts(
  Array.from({ length: 8 }, (_, i) => ({ id: `p:invoices_0${i}`, kind: 'asset', inputs: [] })),
  new Map([['q:accounts', ['p:invoices_00']]]),
  () => 'q',
)

// A graph of groups: each feeds every group after it, so most edges are ones another path
// already shows; those are drawn for the selected card only.
const GROUP_GRAPH: GraphEntry[] = Array.from({ length: 6 }, (_, g) => ({
  id: `group:p:stage_${g}`,
  kind: 'group',
  inputs: Array.from({ length: g }, (_, up) => `group:p:stage_${up}`),
  name: `Stage ${g}`,
  summary: '4 assets',
  inputWeights: Object.fromEntries(Array.from({ length: g }, (_, up) => [`group:p:stage_${up}`, g - up])),
}))
const GROUP_GRAPH_STATES: Record<string, GraphState> = Object.fromEntries(
  GROUP_GRAPH.map((e, i) => [e.id, i % 3 ? { kind: 'group', tone: 'success', glyph: 'fresh', label: '4 fresh', hint: '' } : { kind: 'group', tone: 'failed', glyph: 'failed', label: '2 failing', hint: '' }]),
)

/** A group card in each case its health line has: from `groupHealth` over real member states. */
const GROUP_CARDS: Record<string, GraphState> = {
  'failing while running': groupHealth(['a', 'b', 'c'], new Map([['a', { ...failedNode, id: 'a' }], ['b', { ...nodeByState.cached, id: 'b', running: runningPartitioned }], ['c', { ...nodeByState.stale, id: 'c' }]])),
  'running, nothing failed': groupHealth(['a', 'b'], new Map([['a', { ...nodeByState.cached, id: 'a', running: runningPartitioned }], ['b', { ...nodeByState.cached, id: 'b' }]])),
  'stale and more': groupHealth(['a', 'b', 'c', 'd', 'e'], new Map([['a', { ...nodeByState.stale, id: 'a' }], ['b', { ...nodeByState.partial, id: 'b' }], ['c', { ...nodeByState.never_run, id: 'c' }], ['d', { ...nodeByState.always_runs, id: 'd' }], ['e', { ...nodeByState.cached, id: 'e' }]])),
  'all fresh': groupHealth(['a'], new Map([['a', { ...nodeByState.cached, id: 'a' }]])),
}
const GROUP_CARD_ENTRIES: GraphEntry[] = Object.keys(GROUP_CARDS).map((name) => ({ id: name, kind: 'group', inputs: [], name, summary: '12 assets · 12 tasks', outputName: 'model' }))

export function Graph() {
  const [selected, setSelected] = useState<string | null>('p:orders_clean')
  const [groupSelected, setGroupSelected] = useState<string | null>(GROUP_GRAPH[3]?.id ?? null)
  return (
    <>
      <Spec id="kit-graph" title="DagNode · resting states" note="graphState() of a node in each cache state, in plain words, most urgent first; then failing, which replaces the state word when the last attempt failed.">
        {variants(nodeByState).map((state) => (
          <Variant key={state} label={state}>
            <StateNode name={nodeByState[state].name} kind={nodeByState[state].kind} state={graphState(nodeByState[state])} />
          </Variant>
        ))}
        <Variant label="stale · last attempt failed">
          <StateNode name={failedNode.name} state={graphState(failedNode)} />
        </Variant>
        <Variant label="always_runs · last attempt failed">
          <StateNode name={failedTask.name} kind="task" state={graphState(failedTask)} />
        </Variant>
        <Variant label="never_run · every attempt failed">
          <StateNode name={neverSucceededNode.name} state={graphState(neverSucceededNode)} />
        </Variant>
        <Variant label="never_run · blocked by a failure upstream">
          <DagNode name="revenue_summary" kind="asset" tone="warning" label="blocked" blockedBy="broken_report" stateHint="Blocked: waits on broken_report, whose last attempt failed." />
        </Variant>
        <Variant label="stale · pinned (barca pin)">
          <DagNode name="orders" kind="asset" tone="warning" label="pinned" pinned="override from fix.json by me@host" stateHint="Pinned: override from fix.json by me@host. Runs read this version until it is unpinned." />
        </Variant>
      </Spec>

      <Spec title="DagNode · a run working on it" note="runningGraphState(), whoever started the run; then a run streaming to this page.">
        {variants(runningByStep).map((step) => (
          <Variant key={step} label={step}>
            <StateNode name="orders_enriched" state={runningGraphState(runningByStep[step])} />
          </Variant>
        ))}
        <Variant label="running · partitioned">
          <StateNode name="sessionize" state={runningGraphState(runningPartitioned)} />
        </Variant>
        {STATUS_KINDS.map((status) => (
          <Variant key={status} label={`live ${status}`}>
            <StateNode name="orders_enriched" state={liveGraphState(status)} />
          </Variant>
        ))}
      </Spec>

      <Spec title="DagNode · kind and selection" note="A task's border is dashed: it runs every time.">
        {NODE_KINDS.map((kind) => (
          <Variant key={kind} label={kind}>
            <StateNode name={`a_${kind}`} kind={kind} state={graphState(nodeByState.cached)} />
          </Variant>
        ))}
        <Variant label="selected">
          <DagNode name="orders_clean" kind="asset" tone="success" glyph="fresh" label="fresh" selected />
        </Variant>
        <Variant label="long name">
          <DagNode name="cv__fold_03__preprocessing__standard_scaler_fit" kind="asset" tone="warning" glyph="stale" label="stale" />
        </Variant>
      </Spec>

      <Spec title="GraphCanvas" note="The real canvas: asset and group nodes, a live edge into the running node, and selection (click a node).">
        <Frame height={340} width="100%">
          <GraphCanvas assets={PIPELINE} dir="LR" selected={selected} onSelect={setSelected} states={PIPELINE_STATES} />
        </Frame>
      </Spec>

      <Spec title="GroupNode · health" note="groupHealth(): the most severe fact first, in words (failing beats running beats stale); the rest as glyph and count, every part in the tooltip and the legend.">
        <Frame height={340} width="100%">
          <GraphCanvas assets={GROUP_CARD_ENTRIES} dir="LR" selected={null} onSelect={() => undefined} states={GROUP_CARDS} />
        </Frame>
      </Spec>

      <Spec title="GraphCanvas · unconnected members" note="Packed in a grid; an input from outside the view is a faded port.">
        <Frame height={300} width="100%">
          <GraphCanvas assets={MEMBERS} dir="LR" selected={null} onSelect={() => undefined} states={{}} />
        </Frame>
      </Spec>

      <Spec title="GraphCanvas · groups" note="A level of groups, by dependency depth, edges routed between the cards; an edge another path already shows (implied) is drawn for the selected card only, with how many inputs cross it.">
        <Frame height={380} width="100%">
          <GraphCanvas assets={GROUP_GRAPH} dir="LR" selected={groupSelected} onSelect={setGroupSelected} states={GROUP_GRAPH_STATES} />
        </Frame>
      </Spec>
    </>
  )
}
