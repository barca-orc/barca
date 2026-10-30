import { INITIAL_RUNS, RUNS_MAX } from '@/lib/runsPage'
import { RunPanelView, RunsView } from '@/pages/RunsView'
import { TokensOffView, TokensView } from '@/pages/TokensView'
import { NOW_MS, failureLists, runDetails, runLists, tokenStates } from '../fixtures'
import { INITIAL_FAILURES, failureTable, muteKey, type FailuresModel } from '@/lib/failuresView'
import { distinctNames } from '@/lib/groups'
import { FailuresView } from '@/pages/FailuresView'
import { Frame, Spec, States } from '../Kit'

/**
 * Pages written as programs (`lib/program.ts`): each view is a function of its model and its
 * server data, so every state is a fixture. No program runs here; the messages a click sends
 * are listed under the view.
 */
export function Pages() {
  return (
    <>
      <Spec id="kit-pages" title="Runs page" note="RunsView for each state of the run list.">
        <States
          label="Run list"
          states={{
            ...Object.fromEntries(Object.entries(runLists).map(([name, runs]) => [name, { runs, model: INITIAL_RUNS }])),
            'at the load-more cap': { runs: runLists.truncated, model: { ...INITIAL_RUNS, limit: RUNS_MAX } },
            'failed only': { runs: runLists['every status'], model: { ...INITIAL_RUNS, filter: 'failed' as const } },
            'one hour': { runs: runLists['every status'], model: { ...INITIAL_RUNS, hour: Math.floor(NOW_MS / 3_600_000) * 3_600_000 } },
          }}
          render={({ runs, model }, dispatch) => (
            <Frame height={420} width="100%">
              <RunsView
                model={model}
                runs={runs}
                selected={null}
                now={NOW_MS}
                dispatch={dispatch}
              />
            </Frame>
          )}
        />
      </Spec>

      <Spec title="Run details panel" note="RunPanelView for each state of its request, then each run status.">
        <States
          label="Run"
          states={runDetails}
          render={(run, dispatch) => (
            <Frame height={560} width={420}>
              <RunPanelView id="127" run={run} dispatch={dispatch} />
            </Frame>
          )}
        />
      </Spec>

      <Spec title="Failures view" note="Grouped by cause. Actions send messages; no program runs here.">
        <States
          label="Failures"
          states={{
            ...Object.fromEntries(Object.entries(failureLists).map(([name, list]) => [name, { list, model: INITIAL_FAILURES }])),
            'muted, shown': { list: failureLists['by cause'], model: { ...INITIAL_FAILURES, showMuted: true, muted: [muteKey(failureLists['by cause'][0]!)] } },
            'action failed': { list: failureLists['by cause'], model: { ...INITIAL_FAILURES, notice: { text: 'No run is recorded for invoices_00', failed: true } } },
            'read-only': { list: failureLists['by cause'], model: INITIAL_FAILURES, readOnly: true },
          } as Record<string, { list: typeof failureLists['by cause']; model: FailuresModel; readOnly?: boolean }>}
          render={({ list, model, readOnly }, dispatch) => {
            const { rows } = failureTable(list, model)
            return (
              <Frame height={320} width="100%">
                <FailuresView
                  model={model}
                  rows={rows}
                  window={{ items: rows.map((_, index) => ({ index })), before: 0, after: 0 }}
                  labels={distinctNames(list.map((f) => f.id))}
                  placeOf={() => 'Billing › Raw'}
                  selected={null}
                  nowMs={NOW_MS}
                  readOnly={readOnly ?? false}
                  onSelect={() => undefined}
                  dispatch={dispatch}
                />
              </Frame>
            )
          }}
        />
      </Spec>

      <Spec title="Tokens page" note="TokensView for each model and token list. Typing sends messages but changes nothing: no program runs here.">
        <States
          label="Tokens"
          states={tokenStates}
          render={({ model, tokens }, dispatch) => (
            <TokensView model={model} tokens={tokens} dispatch={dispatch} now={() => NOW_MS} />
          )}
        />
        <TokensOffView />
      </Spec>
    </>
  )
}
