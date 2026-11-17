import { Link } from 'react-router'
import { ArrowRight, Copy } from 'lucide-react'
import { ErrorLine, IconButton, KeyValue } from '@/components'
import { usePartitionKeys } from '@/hooks/usePartitionKeys'
import { shortName } from '@/lib/graph'
import { barSegments, keyParts, runScopeNote } from '@/lib/nodeDetail'
import { keysHref } from '@/lib/keysPage'
import type { PartitionState } from '@/lib/types'

export function CopyButton({ label, text }: { label: string; text: string }) {
  return <IconButton label={label} size="sm" onClick={() => void navigator.clipboard?.writeText(text)}><Copy size={12} /></IconButton>
}

/** A partitioned node's keys as parts of a whole, each counted; a few failed keys of thousands still show. */
export function PartitionBar({ partitions }: { partitions: PartitionState }) {
  // Every count is the row's own (`partitions`): which keys failed, which wait on an input's
  // failed keys and which a run computes is counted in `barca-core`.
  const parts = keyParts(partitions)
  const { cached, failed, blocked, behind, total } = parts
  const widths = barSegments(parts)
  const waitsOn = partitions.blocked_on ? ` on ${shortName(partitions.blocked_on)}` : ''
  return (
    <>
      <KeyValue label="partitions">
        {cached.toLocaleString()} of {total.toLocaleString()} cached
        {failed > 0 && <> · <span className="barca-cell-failed">{failed.toLocaleString()} failed</span></>}
        {blocked > 0 && <span data-keys-blocked={blocked}> · {blocked.toLocaleString()} wait{waitsOn}</span>}
        {behind > 0 && <span data-keys-behind={behind}> · {behind.toLocaleString()} not up to date</span>}
      </KeyValue>
      <div className="barca-keys-bar" role="img" aria-label={`${cached} cached, ${failed} failed, ${blocked} waiting, ${behind} not up to date, of ${total} keys`}>
        {cached > 0 && <span className="is-cached" style={{ width: `${widths.cached}%` }} />}
        {failed > 0 && <span className="is-failed" style={{ width: `${widths.failed}%` }} />}
        {blocked > 0 && <span className="is-waiting" style={{ width: `${widths.blocked}%` }} />}
        {behind > 0 && <span className="is-stale" style={{ width: `${widths.behind}%` }} />}
      </div>
    </>
  )
}

export interface NodeKeysProps {
  id: string
  partitions: PartitionState
}

/** Error groups the panel names before pointing at the explorer. */
const PANEL_ERRORS = 3

/**
 * A partitioned node's failed keys, grouped by error, each a way into the key explorer filtered
 * to it: what its panel leads with when keys failed, above what changed.
 */
export function KeyErrors({ id, failed }: { id: string; failed: number }) {
  const { data } = usePartitionKeys(id, { status: 'failed', limit: 1 })
  const errors = failed > 0 ? data?.errors ?? [] : []
  if (errors.length === 0) return null
  return (
    <ul className="barca-keys-errors" aria-label="Errors of the failed keys">
      {errors.slice(0, PANEL_ERRORS).map((e, i) => (
        <li key={e.error}>
          <Link className="barca-keys-error-link" to={keysHref(id, { filter: { states: ['failed'], error: i } })} title="Open these keys in the key explorer">
            <b>{e.keys.toLocaleString()}</b> <ErrorLine error={e.error} />
          </Link>
          <CopyButton label="Copy error" text={e.error} />
        </li>
      ))}
      {errors.length > PANEL_ERRORS && <li className="barca-cell-sub">and {errors.length - PANEL_ERRORS} more errors</li>}
    </ul>
  )
}

/**
 * A partitioned node's keys in its panel: what a run would compute, and the key explorer for
 * the keys themselves, where 10,000 of them fit; the panel stays short. Their errors are at the
 * top of the panel (`KeyErrors`).
 */
export function NodeKeys({ id, partitions }: NodeKeysProps) {
  // What a run would compute: the keys without a cached result (`missing`), as served.
  const { missing, total } = partitions
  const note = runScopeNote(missing, total)
  return (
    <div className="barca-node-keys">
      {note && <p className="barca-note">{note}</p>}
      <Link className="barca-keys-explore" to={keysHref(id)}>
        {missing > 0 ? 'See where they are' : total === 1 ? 'Explore the key' : `Explore all ${total.toLocaleString()} keys`} <ArrowRight size={12} aria-hidden />
      </Link>
    </div>
  )
}
