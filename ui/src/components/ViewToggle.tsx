import { GitBranch, List } from 'lucide-react'
import { useSearchParams } from 'react-router'

/** List or graph, for the view it sits in; the selection follows to the other one. */
export function ViewToggle() {
  const [params, setParams] = useSearchParams()
  const graph = params.get('view') === 'graph'
  const setView = (view: 'list' | 'graph') => {
    const next = new URLSearchParams(params)
    const selected = next.get(graph ? 'focus' : 'node')
    next.delete('focus')
    next.delete('node')
    if (selected) next.set(view === 'graph' ? 'focus' : 'node', selected)
    if (view === 'graph') next.set('view', 'graph')
    else next.delete('view')
    setParams(next, { replace: true })
  }
  // Two icons, each named (for a screen reader and the tooltip): the toolbar stays one line.
  return (
    <div className="barca-view-toggle" role="group" aria-label="Asset view">
      <button type="button" aria-pressed={!graph} title="List" onClick={() => setView('list')}><List size={14} /><span className="barca-sr-only">List</span></button>
      <button type="button" aria-pressed={graph} title="Graph" onClick={() => setView('graph')}><GitBranch size={14} /><span className="barca-sr-only">Graph</span></button>
    </div>
  )
}
