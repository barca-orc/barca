import { Link } from 'react-router'
import { ArrowRight, Copy } from 'lucide-react'
import { ErrorLine, IconButton, KeyValue } from '@/components'
import { usePartitionKeys } from '@/hooks/usePartitionKeys'
import { keyBreakdown } from '@/lib/assetTable'
import { barSegments } from '@/lib/nodeDetail'
import { keysHref } from '@/lib/keysPage'
import type { PartitionState } from '@/lib/types'

export function CopyButton({ label, text }: { label: string; text: string }) {
  return <IconButton label={label} size="sm" onClick={() => void navigator.clipboard?.writeText(text)}><Copy size={12} /></IconButton>
}

/** A partitioned node's keys as parts of a whole, each counted; a few failed keys of thousands still show. */
export function PartitionBar({ partitions, waiting = false }: { partitions: PartitionState; waiting?: boolean }) {
  const parts = keyBreakdown(partitions)
  const { cached, failed, stale, total } = parts
  const widths = barSegments(parts)
  return (
    <>
      <KeyValue label="partitions">
        {cached.toLocaleString()} of {total.toLocaleString()} cached
        {failed > 0 && <> · <span className="barca-cell-failed">{failed.toLocaleString()} failed</span></>}
        {stale > 0 && ` · ${stale.toLocaleString()} ${waiting ? 'blocked' : 'stale'}`}
      </KeyValue>
      <div className="barca-keys-bar" role="img" aria-label={`${cached} cached, ${failed} failed, ${stale} stale, of ${total} keys`}>
        {cached > 0 && <span className="is-cached" style={{ width: `${widths.cached}%` }} />}
        {failed > 0 && <span className="is-failed" style={{ width: `${widths.failed}%` }} />}
        {stale > 0 && <span className="is-stale" style={{ width: `${widths.stale}%` }} />}
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
 * A partitioned node's keys in its panel: what failed, grouped by error, each a way into the key
 * explorer filtered to it, and the explorer itself for everything else. The keys themselves live
 * there, where 10,000 of them fit; the panel stays short.
 */
export function NodeKeys({ id, partitions }: NodeKeysProps) {
  const { failed, stale, total } = keyBreakdown(partitions)
  const { data } = usePartitionKeys(id, { status: 'failed', limit: 1 })
  const errors = failed > 0 ? data?.errors ?? [] : []
  return (
    <div className="barca-node-keys">
      {errors.length > 0 && (
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
      )}
      {(failed > 0 || stale > 0) && <p className="barca-note">Run computes only these {(failed + stale).toLocaleString()} keys; the others stay cached.</p>}
      <Link className="barca-keys-explore" to={keysHref(id)}>
        {failed + stale > 0 ? 'See where they are' : total === 1 ? 'Explore the key' : `Explore all ${total.toLocaleString()} keys`} <ArrowRight size={12} aria-hidden />
      </Link>
    </div>
  )
}
