import { useEffect, useMemo, useRef } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type StateScope } from '@/lib/api'
import { POLL_WHOLE_MS, statePollInterval } from '@/lib/running'
import { useSearchParams } from 'react-router'
import { useAssets } from './useAssets'
import { useRollup } from './useRollup'
import { useGroups } from './useGroups'
import { useGroupTree } from './useGroupTree'
import { inPipeline } from '@/lib/pipeline'
import { viewScope } from '@/lib/groups'
import { recovering } from './polling'
import type { NodeState } from '@/lib/types'
import { knownStates } from '@/lib/knownStates'

/** The query key of `scope`'s state; `null` is the whole project. */
function stateKey(scope: StateScope | null) {
  return ['state', scope] as const
}

/** What the URL has open: a group, else a pipeline (source file), else everything. */
export function useStateScope(): { scope: StateScope | null; waiting: boolean } {
  const [params] = useSearchParams()
  const pipeline = params.get('pipeline')
  const groups = useGroups()
  const tree = useGroupTree(pipeline)
  const group = viewScope(params, tree)
  return {
    scope: group ? { group } : pipeline ? { file: pipeline } : null,
    // A group in the URL is only known to be one once the groups have loaded.
    waiting: params.get('flat') !== '1' && params.has('group') && groups.isPending,
  }
}

/**
 * The open pipeline's or group's state from `GET /state?files=` / `?targets=<group id>`, or
 * every node's on All pipelines: the server expands a group or file to its nodes, so the
 * request stays one short URL at any size. Refreshed every 10s, and every 2s while some run
 * is working on a node (whoever started it), so every tab sees every run. Unchanged polls are
 * `304`s (ETag). No placeholder data: a status table showing invented states would be worse
 * than showing nothing.
 */
export function useAssetStates(poll = true) {
  const { scope, waiting } = useStateScope()
  const assets = useAssets()
  const client = useQueryClient()
  const rollup = useRollup(null)
  // Offscreen nodes can change which rows a filter/sort would show. Refresh the scope
  // when the rollup's per-node facts change, but not for its minute-clock alone.
  const revision = JSON.stringify([...rollup.tree.entries.values()].filter((e) => e.kind === 'node').map((e) => [e.id, e.health, e.causes, e.running]))
  const previous = useRef(revision)
  useEffect(() => {
    if (!poll && previous.current !== revision) void client.invalidateQueries({ queryKey: ['state'] })
    previous.current = revision
  }, [revision, poll, client])
  // A pipeline with no nodes is an empty result, not a request the server would refuse.
  const listed = assets.data
  const file = scope && 'file' in scope ? scope.file : null
  const empty = file !== null && listed !== undefined && !listed.some((a) => inPipeline(a.id, file))
  return useQuery({
    queryKey: stateKey(scope),
    queryFn: ({ signal }) => (empty ? Promise.resolve([]) : api.state(scope, signal)),
    enabled: !waiting && (file === null || listed !== undefined),
    refetchInterval: recovering<NodeState[]>((query) => poll ? statePollInterval(query.state.data) : false),
    retry: false,
  })
}

/**
 * The whole project's state, whatever is open: what the sidebar's dots say for the pipelines
 * outside the open scope. It is the same query as All pipelines', so that view shares it;
 * refreshed slowly, and when a run finishes (runs invalidate `state`).
 */
export function useWholeState() {
  return useQuery({
    queryKey: stateKey(null),
    queryFn: ({ signal }) => api.state(null, signal),
    refetchInterval: recovering(POLL_WHOLE_MS),
    retry: false,
  })
}

/**
 * The open scope's state rows, or while they load the newest rows already loaded for its nodes
 * (`knownStates`): a navigation keeps each card's last known state instead of "Checking…".
 * `undefined` only when no state has ever loaded.
 */
export function useKnownStates(data: readonly NodeState[] | undefined): readonly NodeState[] | undefined {
  const client = useQueryClient()
  // Read when the scope's own rows change (arrive, or a navigation drops them): the cache as it
  // is at that moment.
  return useMemo(() => {
    if (data) return data
    const loaded = client.getQueryCache().findAll({ queryKey: ['state'] }).map((q) => ({ data: q.state.data as NodeState[] | undefined, updatedAt: q.state.dataUpdatedAt }))
    return knownStates(undefined, loaded)
  }, [data, client])
}

/** Refresh just the table rows on screen, preserving the scope's other loaded rows. */
export function useVisibleAssetStates(ids: string[]) {
  const { scope } = useStateScope()
  const client = useQueryClient()
  const targets = [...new Set(ids)]
  const query = useQuery({
    queryKey: ['visible-state', targets],
    queryFn: ({ signal }) => api.state({ targets }, signal),
    enabled: targets.length > 0,
    refetchInterval: recovering<NodeState[]>((q) => statePollInterval(q.state.data)),
    retry: false,
  })
  const scopeKey = JSON.stringify(scope)
  useEffect(() => {
    if (!query.data) return
    const fresh = new Map(query.data.map((row) => [row.id, row]))
    client.setQueryData<NodeState[]>(stateKey(JSON.parse(scopeKey)), (old) => old?.map((row) => fresh.get(row.id) ?? row))
  }, [query.data, query.dataUpdatedAt, client, scopeKey])
  return query
}
