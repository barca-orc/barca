import { useMemo, useRef, useState, type CSSProperties } from 'react'
import { useSearchParams } from 'react-router'
import { Copy, Maximize, Plus, X } from 'lucide-react'
import { HealthBar, IconButton, MiddleName, StatusBadge, StatusDot, Tag, ViewToggle } from '@/components'
import { GraphCanvas, type GraphCanvasHandle } from '@/components/graph/GraphCanvas'
import { useAssets } from '@/hooks/useAssets'
import { useAssetStates } from '@/hooks/useAssetStates'
import { graphState, PENDING_GRAPH_STATE, type GraphState } from '@/lib/graphState'
import { shortName } from '@/lib/graph'
import { sourceFile } from '@/lib/pipeline'
import { nodeKindLabel } from '@/lib/status'
import { byStep, chainCode, chainGraph, chainInitials, chainSteps, SCRATCH, type ChainDef } from '@/lib/chains'
import { useChains } from '@/hooks/useChains'
import { useNodeRun } from '@/hooks/useNodeRun'
import { NodePanel } from '@/components/assets/NodePanel'
import { RunNotice } from '@/components/RunNotice'
import { healthCounts } from '@/lib/health'
import './chains.css'

/** Accent per open chain, in the order they were opened. */
const CHAIN_COLORS = [1, 2, 3, 4, 5, 6].map((i) => `var(--chain-${i})`)

export function ChainPage({ chainIds }: { chainIds: string[] }) {
  const [params, setParams] = useSearchParams()
  const assetQuery = useAssets()
  const nodes = useMemo(() => (assetQuery.isPlaceholderData ? [] : assetQuery.data ?? []), [assetQuery.isPlaceholderData, assetQuery.data])
  const { data: states, dataUpdatedAt } = useAssetStates()
  const chains = useChains()
  const scratchNodes = params.get('nodes') ?? ''
  /** Every open chain with its members and its own step numbers. */
  const open = useMemo(() => chainIds.map((id, i) => {
    const scratch = id === SCRATCH
    const found = chains.find((c) => c.def.id === id)
    const def: ChainDef = scratch ? { id: SCRATCH, name: 'Scratch chain' } : found?.def ?? { id, name: id }
    const members = scratch ? scratchNodes.split(',').filter((m) => nodes.some((n) => n.id === m)) : found?.members ?? []
    return { def, members, steps: chainSteps(members, nodes), color: CHAIN_COLORS[i % CHAIN_COLORS.length]!, initials: chainInitials(def.name) }
  }), [chainIds, chains, scratchNodes, nodes])
  const multi = open.length > 1
  const scratch = chainIds.includes(SCRATCH)
  const scratchMembers = open.find((c) => c.def.id === SCRATCH)?.members ?? []
  const def: ChainDef = multi ? { id: chainIds.join(','), name: open.map((c) => c.def.name).join(' + ') } : open[0]!.def
  const members = useMemo(() => nodes.map((n) => n.id).filter((id) => open.some((c) => c.members.includes(id))), [nodes, open])
  /** Chain by chain, each in step order; a node shared by two is listed under the first. */
  const ordered = useMemo(() => {
    const seen = new Set<string>()
    return open.flatMap((c) => byStep(c.steps).filter((id) => !seen.has(id) && seen.add(id)))
  }, [open])
  const stepLabel = (id: string) => open.filter((c) => c.steps.has(id)).map((c) => c.steps.get(id)).join(' ')
  const entries = useMemo(() => chainGraph(members, nodes, {
    bridges: multi,
    steps: (id) => open.filter((c) => c.steps.has(id)).map((c) => ({ label: c.steps.get(id)!, color: multi ? c.color : undefined, title: c.def.name })),
  }), [members, nodes, open, multi])
  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes])
  const statesById = useMemo(() => new Map((states ?? []).map((s) => [s.id, s])), [states])
  const stateOf = useMemo(() => {
    const m = new Map<string, GraphState>()
    for (const s of states ?? []) m.set(s.id, graphState(s))
    return (id: string) => m.get(id) ?? PENDING_GRAPH_STATE
  }, [states])
  const graphStates = useMemo(() => Object.fromEntries(members.map((m) => [m, stateOf(m)])), [members, stateOf])
  const graph = params.get('view') === 'graph'
  const selected = params.get(graph ? 'focus' : 'node')
  const selectedNode = (!graph && states?.find((n) => n.id === selected)) || null
  const nodeRun = useNodeRun(selectedNode, { showTopbar: selectedNode !== null, scopes: false })
  const openGraph = (id: string) => {
    const next = new URLSearchParams(params)
    next.delete('node')
    next.set('view', 'graph')
    next.set('focus', id)
    setParams(next)
  }
  const canvas = useRef<GraphCanvasHandle>(null)
  const [copied, setCopied] = useState(false)

  const set = (key: string, value: string | null) => {
    const next = new URLSearchParams(params)
    if (value) next.set(key, value)
    else next.delete(key)
    setParams(next, { replace: true })
  }
  const setScratch = (ids: string[]) => set('nodes', ids.join(','))
  const closeChain = (id: string) => {
    const rest = chainIds.filter((c) => c !== id)
    const next = new URLSearchParams(params)
    next.set('chain', rest.join(','))
    setParams(next, { replace: true })
  }
  const copy = () => {
    void navigator.clipboard.writeText(open.map((c) => chainCode(c.def.name, byStep(c.steps))).join('\n'))
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  const health = healthCounts(members, statesById)

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{def.name}</h1>
            <span className="barca-count">{members.length} steps</span>
            {states && <HealthBar counts={health} labelLimit={3} width={80} />}
          </div>
          <div className="barca-view-actions">
            <button type="button" className="barca-clear" onClick={copy} title="Copy the chain() declaration">
              <Copy size={12} /> {copied ? 'Copied' : 'Copy as code'}
            </button>
            <ViewToggle />
            {graph && (
              <IconButton label="Fit to screen" onClick={() => canvas.current?.fit()}>
                <Maximize size={15} />
              </IconButton>
            )}
          </div>
        </div>
        {multi ? (
          <div className="barca-chain-legend">
            {open.map((c) => (
              <span key={c.def.id} className="barca-chain-key" style={{ '--chain': c.color } as CSSProperties}>
                <b>{c.initials}</b> {c.def.name} <span className="barca-cell-sub">{c.members.length}</span>
                <IconButton size="sm" label={`Close ${c.def.name}`} onClick={() => closeChain(c.def.id)}><X size={11} /></IconButton>
              </span>
            ))}
          </div>
        ) : def.description && <p className="barca-chain-desc">{def.description}</p>}
        {scratch && <ScratchPicker all={nodes.map((n) => n.id)} members={scratchMembers} onChange={setScratch} />}
      </div>

      <RunNotice notice={nodeRun.notice} onDismiss={nodeRun.dismiss} />
      {members.length === 0 ? (
        <p className="barca-table-empty">{scratch && !multi ? 'Add nodes above to build a chain.' : 'This chain matches no served nodes.'}</p>
      ) : graph ? (
        <div className="barca-graph-wrap">
          <div className="barca-graph-canvas">
            <GraphCanvas assets={entries} dir="LR" selected={selected} onSelect={(id) => set('focus', id)} states={graphStates} handleRef={canvas} fitWhole />
          </div>
        </div>
      ) : (
        <div className="barca-assets-split">
        <div className="barca-view-body barca-table-scroll">
          <table className="barca-table barca-chain-table">
            <thead>
              <tr><th className="num">Step</th><th>Name</th><th>State</th><th>{multi ? 'Chains' : 'Also in'}</th>{scratch && <th />}</tr>
            </thead>
            <tbody>
              {ordered.map((id) => {
                const node = byId.get(id)
                const st = stateOf(id)
                const step = stepLabel(id)
                const others = chains.filter((c) => !chainIds.includes(c.def.id) && c.members.includes(id))
                return (
                  <tr key={id} aria-selected={id === selected} onClick={() => set('node', id === selected ? null : id)}>
                    <td className="num">{multi ? (
                      <div className="barca-chain-steps">
                        {open.filter((c) => c.steps.has(id)).map((c) => (
                          <span key={c.def.id} className="barca-chain-step" data-color style={{ '--chain': c.color } as CSSProperties} title={c.def.name}>{c.steps.get(id)}</span>
                        ))}
                      </div>
                    ) : <span className="barca-chain-step" data-parallel={/[a-z]/.test(step) || undefined}>{step}</span>}</td>
                    <td>
                      <div className="barca-cell-name">
                        <span className="name"><MiddleName name={shortName(id)} title={id} /></span>
                        {node && node.kind !== 'asset' && <Tag size="sm">{nodeKindLabel(node)}</Tag>}
                      </div>
                      <div className="barca-cell-sub">{sourceFile(id)}</div>
                    </td>
                    <td title={st.hint}><StatusBadge status={st.status} label={st.label} size="sm" /></td>
                    <td>
                      <div className="barca-chain-also">
                        {multi && open.filter((c) => c.steps.has(id)).map((c) => (
                          <span key={c.def.id} className="barca-chain-pill" data-open style={{ '--chain': c.color } as CSSProperties}>{c.def.name}</span>
                        ))}
                        {others.map((c) => (
                          <button key={c.def.id} type="button" className="barca-chain-pill" title={`Open ${c.def.name}; ⌘/Shift-click to add it`} onClick={(e) => { e.stopPropagation(); setParams({ chain: e.metaKey || e.ctrlKey || e.shiftKey ? [...chainIds, c.def.id].join(',') : c.def.id }) }}>
                            {c.def.name}
                          </button>
                        ))}
                      </div>
                    </td>
                    {scratch && (
                      <td>
                        {scratchMembers.includes(id) && <IconButton size="sm" label={`Remove ${shortName(id)}`} onClick={() => setScratch(scratchMembers.filter((m) => m !== id))}>
                          <X size={12} />
                        </IconButton>}
                      </td>
                    )}
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
        {selectedNode && (
          <NodePanel
            runAction={nodeRun.action}
            node={selectedNode}
            nodes={states ?? []}
            graph={nodes}
            nowMs={dataUpdatedAt}
            onSelect={(id) => set('node', id)}
            onOpenGraph={openGraph}
            onClose={() => set('node', null)}
          />
        )}
        </div>
      )}
    </div>
  )
}

function ScratchPicker({ all, members, onChange }: { all: string[]; members: string[]; onChange: (ids: string[]) => void }) {
  const [q, setQ] = useState('')
  const matches = q ? all.filter((id) => !members.includes(id) && id.toLowerCase().includes(q.toLowerCase())).slice(0, 8) : []
  return (
    <div className="barca-chain-picker">
      <Plus size={13} />
      <input placeholder="Add a node to the scratch chain…" value={q} onChange={(e) => setQ(e.target.value)} />
      {matches.length > 0 && (
        <div className="barca-chain-matches">
          {matches.map((id) => (
            <button key={id} type="button" onClick={() => { onChange([...members, id]); setQ('') }}>
              <StatusDot status="skipped" size={5} /> {shortName(id)} <span className="barca-cell-sub">{sourceFile(id)}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
