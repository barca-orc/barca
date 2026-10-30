/**
 * The Runs page as a program (`lib/program.ts`). The selected run lives in the URL (`?run=`),
 * so selecting one is an effect (a navigation), and the page reads it back as input.
 * Pure; `pages/RunsPage.tsx` runs it and `pages/RunsView.tsx` renders it.
 */

import { match } from 'ts-pattern'
import { only, type Next } from './program'

export const RUNS_PAGE_SIZE = 100
export const RUNS_MAX = 1000

export interface RunsModel {
  /** How many runs the list asks for. */
  limit: number
}

export type RunsMsg =
  | { type: 'run_selected'; id: string }
  | { type: 'panel_closed' }
  /** The selected run's details name its durable run id, which differs from the link's. */
  | { type: 'run_resolved'; id: string }
  | { type: 'load_more' }
  | { type: 'retry_list' }
  | { type: 'retry_detail' }

export type RunsEffect =
  /** Set `?run=` (`null` removes it); `replace` keeps the history entry. */
  | { type: 'navigate'; run: string | null; replace: boolean }
  | { type: 'refetch_list' }
  | { type: 'refetch_detail' }

export const INITIAL_RUNS: RunsModel = { limit: RUNS_PAGE_SIZE }

/** Whether "Load more" can ask for more runs. */
export function canLoadMore(model: RunsModel): boolean {
  return model.limit < RUNS_MAX
}

export function updateRuns(model: RunsModel, msg: RunsMsg): Next<RunsModel, RunsEffect> {
  return match<RunsMsg, Next<RunsModel, RunsEffect>>(msg)
    .with({ type: 'run_selected' }, ({ id }) => [model, [{ type: 'navigate', run: id, replace: false }]])
    .with({ type: 'panel_closed' }, () => [model, [{ type: 'navigate', run: null, replace: false }]])
    // Replace an older link's temporary server handle with the run id, so bookmarked
    // details keep working after the server restarts.
    .with({ type: 'run_resolved' }, ({ id }) => [model, [{ type: 'navigate', run: id, replace: true }]])
    .with({ type: 'load_more' }, () => only({ limit: Math.min(model.limit + RUNS_PAGE_SIZE, RUNS_MAX) }))
    .with({ type: 'retry_list' }, () => [model, [{ type: 'refetch_list' }]])
    .with({ type: 'retry_detail' }, () => [model, [{ type: 'refetch_detail' }]])
    .exhaustive()
}
