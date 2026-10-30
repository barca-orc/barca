import { LogViewer, RunningBadge } from '@/components'
import { ResetDatabaseDialog } from '@/components/ResetDatabase'
import { ArtifactSchema } from '@/components/assets/ArtifactSchema'
import { NodeInspector } from '@/components/graph/NodeInspector'
import type { ListNode, LogLine, StatusKind } from '@/lib/types'
import {
  NOW_MS,
  feedbackInputs,
  logLines,
  resetPhases,
  runningByStep,
  runningPartitioned,
  schemaNodes,
  variants,
} from '../fixtures'
import { Frame, Spec, States, Variant } from '../Kit'

const inspected: ListNode = {
  id: 'pipeline.py:orders_enriched',
  kind: 'asset',
  freshness: 'always',
  env: [], file: 'p.py', line: 1,
  inputs: ['pipeline.py:orders_clean', 'pipeline.py:fx_rates'],
}

interface InspectorState { status: StatusKind; logs: LogLine[]; error: string | null; readOnly: boolean; triggerError: Error | null }

/** What the inspector shows for each run feedback, then the trigger's own states. */
const inspectorStates: Record<string, InspectorState> = {
  ...Object.fromEntries(variants(feedbackInputs).map((kind) => [kind, { ...feedbackInputs[kind], readOnly: false, triggerError: null }])),
  'read-only server': { ...feedbackInputs.idle, readOnly: true, triggerError: null },
  'could not start': { ...feedbackInputs.idle, readOnly: false, triggerError: new Error('404 no node named orders_enriched') },
}

const noop = () => {}

export function Runs() {
  return (
    <>
      <Spec id="kit-runs" title="LogViewer">
        <Variant label="empty"><LogViewer lines={[]} height={120} style={{ width: 360 }} /></Variant>
        <Variant label="stdout + stderr"><LogViewer lines={logLines} height={160} style={{ width: 460 }} /></Variant>
        <Variant label="live"><LogViewer lines={logLines.slice(0, 3)} live height={120} style={{ width: 360 }} /></Variant>
      </Spec>

      <Spec title="RunningBadge" note="A node a run works on now, with elapsed time; links to the run.">
        {variants(runningByStep).map((step) => (
          <Variant key={step} label={step}><RunningBadge running={runningByStep[step]} nowMs={NOW_MS} /></Variant>
        ))}
        <Variant label="partitioned"><RunningBadge running={runningPartitioned} nowMs={NOW_MS} /></Variant>
        <Variant label="md"><RunningBadge running={runningByStep.running} nowMs={NOW_MS} size="md" /></Variant>
      </Spec>

      <Spec title="NodeInspector" note="The graph's side panel: runFeedback() decides the panel for each status, logs and error.">
        <States
          label="Inspector state"
          states={inspectorStates}
          render={(s, dispatch) => (
            <div className="barca-kit-panel">
              <NodeInspector
                asset={inspected}
                status={s.status}
                logs={s.logs}
                running={s.status === 'running'}
                error={s.error}
                readOnly={s.readOnly}
                triggering={false}
                triggerError={s.triggerError}
                onFire={() => dispatch({ type: 'fire' })}
                onClose={() => dispatch({ type: 'close' })}
              />
            </div>
          )}
        />
      </Spec>

      <Spec title="ArtifactSchema" note="The node panel's output schema, for each thing it can say.">
        {Object.entries(schemaNodes).map(([label, node]) => (
          <Variant key={label} label={label}>
            <div className="barca-kit-box"><ArtifactSchema node={node} /></div>
          </Variant>
        ))}
      </Spec>

      <Spec title="ResetDatabaseDialog" note="Each phase of a reset; the dialog is modal, so each is framed.">
        {variants(resetPhases).map((kind) => (
          <Variant key={kind} label={kind}>
            <Frame height={360} width={440}>
              <ResetDatabaseDialog phase={resetPhases[kind]} onConfirm={noop} onCancel={noop} onReload={noop} />
            </Frame>
          </Variant>
        ))}
      </Spec>
    </>
  )
}
