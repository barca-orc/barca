import { useState } from 'react'
import { Database, Radio, Zap } from 'lucide-react'
import { DagNode } from '@/components'
import { GraphCanvas } from '@/components/graph/GraphCanvas'
import type { GraphEntry } from '@/lib/graph'
import { graphState, liveGraphState, type GraphState } from '@/lib/graphState'
import { runningGraphState } from '@/lib/running'
import { NODE_KINDS, STATUS_KINDS, nodeBySeverity, runningByStep, runningPartitioned, variants } from '../fixtures'
import { Frame, Spec, Variant } from '../Kit'

const KIND_ICON = { asset: <Database size={12} />, task: <Zap size={12} />, sensor: <Radio size={12} /> }

function StateNode({ name, state, kind = 'asset' }: { name: string; state: GraphState; kind?: keyof typeof KIND_ICON }) {
  return <DagNode name={name} kind={kind} status={state.status} statusLabel={state.label} stateHint={state.hint} icon={KIND_ICON[kind]} />
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
  'p:checkout_events': graphState(nodeBySeverity.unknown),
  'p:product_catalog': graphState(nodeBySeverity.cached),
  'p:orders_clean': graphState(nodeBySeverity.cached),
  'p:orders_enriched': runningGraphState(runningPartitioned),
  'group:p:modeling': { status: 'failed', label: '1 failed', hint: 'fold 3 validation failed' },
  'p:revenue_daily': graphState(nodeBySeverity.stale),
  'p:daily_report': runningGraphState(runningByStep.queued),
}

export function Graph() {
  const [selected, setSelected] = useState<string | null>('p:orders_clean')
  return (
    <>
      <Spec id="kit-graph" title="DagNode · resting states" note="graphState() of a node in each cache severity, most urgent first.">
        {variants(nodeBySeverity).map((severity) => (
          <Variant key={severity} label={severity}>
            <StateNode name={nodeBySeverity[severity].name} kind={nodeBySeverity[severity].kind} state={graphState(nodeBySeverity[severity])} />
          </Variant>
        ))}
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
            <StateNode name={`a_${kind}`} kind={kind} state={graphState(nodeBySeverity.cached)} />
          </Variant>
        ))}
        <Variant label="selected">
          <DagNode name="orders_clean" kind="asset" status="success" statusLabel="Healthy" selected icon={KIND_ICON.asset} />
        </Variant>
        <Variant label="long name">
          <DagNode name="cv__fold_03__preprocessing__standard_scaler_fit" kind="asset" status="warning" statusLabel="Stale" icon={KIND_ICON.asset} />
        </Variant>
      </Spec>

      <Spec title="GraphCanvas" note="The real canvas: asset and group nodes, a live edge into the running node, and selection (click a node).">
        <Frame height={340} width="100%">
          <GraphCanvas assets={PIPELINE} dir="LR" selected={selected} onSelect={setSelected} states={PIPELINE_STATES} />
        </Frame>
      </Spec>
    </>
  )
}
