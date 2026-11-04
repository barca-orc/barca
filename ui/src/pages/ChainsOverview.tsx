import { useMemo } from 'react'
import { useNavigate } from 'react-router'
import { HealthBar, StatusDot } from '@/components'
import { useAssetStates } from '@/hooks/useAssetStates'
import { useChains } from '@/hooks/useChains'
import { graphState } from '@/lib/graphState'
import { shortName } from '@/lib/graph'
import { healthCounts, type HealthCounts } from '@/lib/health'
import { chainSteps } from '@/lib/chains'
import { useAssets } from '@/hooks/useAssets'
import './chains.css'

/** Worst first: failed, then running, then stale, then by name. */
function rank(a: HealthCounts, b: HealthCounts) {
  return b.failed - a.failed || b.running - a.running || b.stale - a.stale
}

/** Every chain as a card: its health, and the steps that need attention. */
export function ChainsOverview() {
  const chains = useChains()
  const assetQuery = useAssets()
  const nodes = useMemo(() => (assetQuery.isPlaceholderData ? [] : assetQuery.data ?? []), [assetQuery.isPlaceholderData, assetQuery.data])
  const { data: states } = useAssetStates()
  const navigate = useNavigate()
  const byId = useMemo(() => new Map((states ?? []).map((s) => [s.id, s])), [states])
  const cards = useMemo(
    () => chains
      .map((c) => ({ ...c, health: healthCounts(c.members, byId), steps: chainSteps(c.members, nodes) }))
      .sort((a, b) => rank(a.health, b.health) || a.def.name.localeCompare(b.def.name)),
    [chains, byId, nodes],
  )
  const open = (id: string) => navigate({ pathname: '/assets', search: `?chain=${encodeURIComponent(id)}` })

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>Chains</h1>
            <span className="barca-count">{chains.length}</span>
          </div>
        </div>
      </div>
      <div className="barca-view-body">
        <div className="barca-chain-cards">
          {cards.map((c) => {
            const attention = c.members
              .map((id) => ({ id, state: byId.get(id) }))
              .filter((m) => m.state && ['failed', 'warning', 'running'].includes(graphState(m.state).status))
              .slice(0, 3)
            return (
              <button key={c.def.id} type="button" className="barca-chain-card" onClick={() => open(c.def.id)}>
                <div className="barca-chain-card-head">
                  <strong>{c.def.name}</strong>
                  <span className="barca-cell-sub">{c.members.length} steps</span>
                </div>
                {c.def.description && <p className="barca-chain-card-desc">{c.def.description}</p>}
                {states && <HealthBar counts={c.health} labelLimit={3} width={120} />}
                {attention.length > 0 && (
                  <ul className="barca-chain-card-list">
                    {attention.map(({ id, state }) => {
                      const g = graphState(state!)
                      return (
                        <li key={id}>
                          <span className="barca-chain-step">{c.steps.get(id)}</span>
                          <StatusDot status={g.status} size={6} />
                          <span className="name">{shortName(id)}</span>
                          <span className="barca-cell-sub">{g.label}</span>
                        </li>
                      )
                    })}
                  </ul>
                )}
              </button>
            )
          })}
        </div>
      </div>
    </div>
  )
}
