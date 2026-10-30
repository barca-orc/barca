import { useCallback, useEffect, useMemo } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useOutletContext, useSearchParams } from 'react-router'
import { useAssets } from './useAssets'
import { useHealth } from './useHealth'
import { useStateScope } from './useAssetStates'
import { api } from '@/lib/api'
import { shortName } from '@/lib/graph'
import { runButton, runTarget } from '@/lib/runTarget'
import type { AppShellContext } from '@/layouts/shellContext'
import type { ListNode } from '@/lib/types'

interface TopbarRunOptions {
  node: Pick<ListNode, 'id' | 'kind'> | null
  /** Offer the button at all. */
  show: boolean
  /** With nothing selected, offer the open pipeline or project; else only a selected node. */
  scopes: boolean
  fireNode: () => void
  busy: boolean
  onStarted: (handle: string) => void
}

/** The topbar's Run button: the selected node, else what is open. */
export function useTopbarRun({ node, show, scopes, fireNode, busy, onStarted }: TopbarRunOptions) {
  const { setTopbarRun } = useOutletContext<AppShellContext>()
  const { data: health } = useHealth()
  const readOnly = health?.read_only ?? false
  const assetQuery = useAssets()
  const assets = assetQuery.isPlaceholderData ? undefined : assetQuery.data
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
  const button = runButton(target)
  const { label, confirm } = button
  const title = readOnly ? 'This server is read-only' : button.title
  const onRun = useCallback(() => {
    if (target.kind === 'node') fireNode()
    else if (target.kind === 'pipeline') mutate(target.file)
    else if (target.kind === 'all') mutate(null)
  }, [target, fireNode, mutate])
  const loading = busy || start.isPending

  useEffect(() => {
    if (!show) return
    setTopbarRun({ onRun, label, disabled: target.kind === 'none' || readOnly, loading, title, confirm })
    return () => setTopbarRun(null)
    // `confirm` follows `label`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [show, setTopbarRun, onRun, label, target.kind, readOnly, loading, title])

  return { readOnly, title, error: start.error }
}
