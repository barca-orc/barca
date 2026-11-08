import { useCallback, useEffect, useMemo } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useOutletContext, useSearchParams } from 'react-router'
import { useAssets } from './useAssets'
import { useHealth } from './useHealth'
import { useStateScope } from './useAssetStates'
import { api } from '@/lib/api'
import { shortName } from '@/lib/graph'
import { pipelineName } from '@/lib/pipeline'
import { runButton, runTarget } from '@/lib/runTarget'
import type { AppShellContext, TopbarRun } from '@/layouts/shellContext'
import type { ListNode } from '@/lib/types'

interface TopbarRunOptions {
  node: Pick<ListNode, 'id' | 'kind'> | null
  /** Offer it to the command palette at all. */
  show: boolean
  /** With nothing selected, offer the open pipeline or project; else only a selected node. */
  scopes: boolean
  fireNode: () => void
  busy: boolean
  onStarted: (handle: string) => void
}

/**
 * A page's Run actions. `scopeRun` is its toolbar's button: what is open, the pipeline or the
 * whole project (`PageRunButton`). The command palette (⌘K) is offered the selected node, else
 * the same; the node panel has its own button for its node.
 */
const noop = () => {}

export function useTopbarRun({ node, show, scopes, fireNode, busy, onStarted }: TopbarRunOptions) {
  const { setTopbarRun } = useOutletContext<AppShellContext>()
  const { data: health } = useHealth()
  const readOnly = health?.read_only ?? false
  const assetQuery = useAssets()
  const assets = assetQuery.data
  const [params] = useSearchParams()
  const pipeline = params.get('pipeline')
  const { scope } = useStateScope()
  const group = scope && 'group' in scope ? scope.group : null
  const client = useQueryClient()
  const start = useMutation({
    mutationFn: (file: string | null) => (file ? api.getFile(file) : api.getAll()),
    onSuccess: (data) => {
      onStarted(data.run_id)
      void client.invalidateQueries({ queryKey: ['runs'] })
    },
  })
  const { mutate } = start
  const name = node ? shortName(node.id) : null
  const target = useMemo(
    () => runTarget({ node: name, group, pipeline }, scopes && assets ? assets : []),
    [name, group, pipeline, scopes, assets],
  )
  const scopeTarget = useMemo(
    () => runTarget({ node: null, group, pipeline }, scopes && assets ? assets : []),
    [group, pipeline, scopes, assets],
  )
  const button = runButton(target)
  const { label, short, confirm } = button
  const title = readOnly ? 'This server is read-only' : button.title
  const onRun = useCallback(() => {
    if (target.kind === 'node') fireNode()
    else if (target.kind === 'pipeline') mutate(target.file)
    else if (target.kind === 'all') mutate(null)
  }, [target, fireNode, mutate])
  const loading = busy || start.isPending

  useEffect(() => {
    if (!show) return
    setTopbarRun({ onRun, label, short, disabled: target.kind === 'none' || readOnly, loading, title, confirm })
    return () => setTopbarRun(null)
    // `confirm` and `short` follow `label`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [show, setTopbarRun, onRun, label, target.kind, readOnly, loading, title])

  const scopeButton = runButton(scopeTarget)
  const onRunScope = useCallback(() => {
    if (scopeTarget.kind === 'pipeline') mutate(scopeTarget.file)
    else if (scopeTarget.kind === 'all') mutate(null)
  }, [scopeTarget, mutate])
  // A group open: the toolbar has no Run (the panel runs a node). Before the nodes load, or with
  // nothing to get, the button is there, disabled, with the same words: the toolbar never moves
  // when the data arrives (its words come from the URL; the size is in its name and tooltip).
  const scopeRun: TopbarRun | null = group ? null : scopeTarget.kind === 'none' ? {
    onRun: noop,
    label: pipeline ? `Run ${pipelineName(pipeline)}` : 'Run all',
    short: pipeline ? `Run ${pipelineName(pipeline)}` : 'Run all',
    disabled: true,
    loading: false,
    title: assets ? scopeTarget.hint : 'Loading…',
    confirm: null,
  } : {
    onRun: onRunScope,
    label: scopeButton.label,
    short: scopeButton.short,
    disabled: readOnly,
    loading: start.isPending,
    title: readOnly ? 'This server is read-only' : scopeButton.title,
    confirm: scopeButton.confirm,
  }

  return { readOnly, title, error: start.error, scopeRun }
}

/**
 * The Run action of a page with nothing to select (Runs, Schedules, Tokens): the open pipeline
 * or the whole project, as on Assets, so every page's toolbar runs the same thing.
 */
export function usePageTopbarRun(onStarted: (handle: string) => void) {
  return useTopbarRun({ node: null, show: true, scopes: true, fireNode: noop, busy: false, onStarted })
}
