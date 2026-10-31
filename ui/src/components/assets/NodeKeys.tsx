import { useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router'
import { Copy, Play } from 'lucide-react'
import { Age, Button, Chip, ChipGroup, ErrorLine, IconButton, KeyValue, SearchInput, Skeleton } from '@/components'
import { usePartitionKeys } from '@/hooks/usePartitionKeys'
import { formatSeconds, keyBreakdown } from '@/lib/assetTable'
import { barSegments, formatBytes } from '@/lib/nodeDetail'
import type { PartitionKeys, PartitionState } from '@/lib/types'

const PAGE = 100
/** Past this many failed keys the list starts closed: the grouped errors above it say what failed. */
export const KEYS_OPEN_MAX = 10

export function CopyButton({ label, text }: { label: string; text: string }) {
  return <IconButton label={label} size="sm" onClick={() => void navigator.clipboard?.writeText(text)}><Copy size={12} /></IconButton>
}

/** A partitioned node's keys as parts of a whole, each counted; a few failed keys of thousands still show. */
export function PartitionBar({ partitions }: { partitions: PartitionState }) {
  const parts = keyBreakdown(partitions)
  const { cached, failed, stale, total } = parts
  const widths = barSegments(parts)
  return (
    <>
      <KeyValue label="partitions">
        {cached.toLocaleString()} of {total.toLocaleString()} cached
        {failed > 0 && <> · <span className="barca-cell-failed">{failed.toLocaleString()} failed</span></>}
        {stale > 0 && ` · ${stale.toLocaleString()} stale`}
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
  nowMs: number
  /** Start a run of one key; absent when runs cannot be started from here. */
  onRunKey?: (key: string) => void
  runDisabled?: boolean
  /** The key whose run is being started. */
  startingKey?: string | null
}

/**
 * A partitioned node's keys: what the failures have in common, and each key's latest attempt
 * with its artifact and a run of that key alone. The list covers keys with a recorded attempt;
 * one that never ran is in `missing_keys`.
 */
export function NodeKeys({ id, partitions, nowMs, onRunKey, runDisabled = false, startingKey = null }: NodeKeysProps) {
  const { failed, stale, total } = keyBreakdown(partitions)
  // The key search: what is typed, and whether it covers only failed keys. (A `?key=` URL
  // param pre-fills `query`.)
  const [query, setQuery] = useState('')
  const [onlyFailed, setOnlyFailed] = useState(failed > 0)
  const [limit, setLimit] = useState(PAGE)
  const q = query.trim() || undefined
  // A search no failed key matches widens to every key, and says so, instead of hiding a healthy
  // key: once the failed-only page of this search is known empty, the list reads every key.
  const client = useQueryClient()
  const narrowFilter = { status: 'failed' as const, q, limit }
  const widened = onlyFailed && q !== undefined && client.getQueryData<PartitionKeys>(['partition-keys', id, narrowFilter])?.total === 0
  const narrow = onlyFailed && !widened
  const { data, isLoading, isError } = usePartitionKeys(id, narrow ? narrowFilter : { q, limit })
  const search = (value: string) => { setQuery(value); setLimit(PAGE) }
  const toggleFailed = () => { setOnlyFailed(!onlyFailed); setLimit(PAGE) }
  return (
    <div className="barca-node-keys">
      {(failed > 0 || stale > 0) && <p className="barca-note">Run computes only these {(failed + stale).toLocaleString()} keys; the others stay cached.</p>}
      {data && data.errors.length > 0 && failed > 0 && (
        <ul className="barca-keys-errors" aria-label="Errors of the failed keys">
          {data.errors.map((e) => (
            <li key={e.error}>
              <b>{e.keys.toLocaleString()}</b> <ErrorLine error={e.error} /> <CopyButton label="Copy error" text={e.error} />
            </li>
          ))}
        </ul>
      )}
      <details className="barca-keys" open={failed > 0 && failed <= KEYS_OPEN_MAX}>
        <summary>
          Keys · {total.toLocaleString()}
          {failed > 0 && <> · <span className="barca-cell-failed">{failed.toLocaleString()} failed</span></>}
        </summary>
        <div className="barca-keys-tools">
          <SearchInput value={query} onChange={search} placeholder="Search keys" />
          <ChipGroup label="Key state">
            <Chip pressed={narrow} onClick={toggleFailed}>failed only</Chip>
          </ChipGroup>
        </div>
        {isLoading && <Skeleton height={120} />}
        {isError && <p className="barca-note">Couldn't load keys.</p>}
        {data && (
          <>
            {widened && <p className="barca-note" role="status">No failed key matches; showing every key.</p>}
            {data.keys.length === 0 && <p className="barca-note">No key matches.</p>}
            <ul className="barca-keys-list">
              {data.keys.map((k) => (
                <li key={k.key} className={k.status === 'failed' ? 'is-failed' : undefined}>
                  <div className="barca-key-head">
                    <code>{k.key}</code>
                    {onRunKey && (
                      <Button variant="ghost" size="sm" iconLeft={<Play size={11} />} disabled={runDisabled}
                        loading={startingKey === k.key} title={`barca get ${id} --partition ${k.key}`} onClick={() => onRunKey(k.key)}>
                        Run this key
                      </Button>
                    )}
                  </div>
                  <span className="barca-cell-sub">
                    {k.status} · <Age at={k.created_at} nowMs={nowMs} />
                    {k.elapsed_seconds !== null && ` · ${formatSeconds(k.elapsed_seconds)}`}
                    {k.size_bytes !== null && ` · ${formatBytes(k.size_bytes)}`}
                    {k.run_id && <> · <Link to={`/runs?run=${encodeURIComponent(k.run_id)}`}>run</Link></>}
                  </span>
                  {k.artifact_path && (
                    <span className="barca-key-artifact">
                      <code className="barca-wrap">{k.artifact_path}</code>
                      <CopyButton label="Copy artifact path" text={k.artifact_path} />
                    </span>
                  )}
                  {k.error && <ErrorLine error={k.error} />}
                </li>
              ))}
            </ul>
            {data.total > data.keys.length && (
              <Button variant="ghost" size="sm" onClick={() => setLimit(limit + PAGE)}>
                Show more ({data.keys.length.toLocaleString()} of {data.total.toLocaleString()})
              </Button>
            )}
          </>
        )}
      </details>
      {partitions.missing_keys.length > 0 && stale > 0 && (
        <details className="barca-keys">
          <summary>
            {partitions.missing > partitions.missing_keys.length
              ? `First ${partitions.missing_keys.length} keys to compute`
              : 'Keys to compute'}
          </summary>
          <code className="barca-wrap">{partitions.missing_keys.join('\n')}</code>
        </details>
      )}
    </div>
  )
}
