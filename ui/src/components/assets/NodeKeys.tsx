import { useState } from 'react'
import { Link } from 'react-router'
import { Age, Button, Chip, ChipGroup, ErrorLine, KeyValue, SearchInput, Skeleton } from '@/components'
import { usePartitionKeys } from '@/hooks/usePartitionKeys'
import { formatSeconds, keyBreakdown } from '@/lib/assetTable'
import { formatBytes } from '@/lib/nodeDetail'
import type { PartitionState } from '@/lib/types'

const PAGE = 100

/**
 * A partitioned node's keys: how many are good, failed or stale, what the failures have in common,
 * and each key's latest attempt. The list covers keys with a recorded attempt; one that never ran
 * is in `missing_keys`.
 */
export function NodeKeys({ id, partitions, nowMs }: { id: string; partitions: PartitionState; nowMs: number }) {
  const { cached, failed, stale, total } = keyBreakdown(partitions)
  const [onlyFailed, setOnlyFailed] = useState(failed > 0)
  const [query, setQuery] = useState('')
  const [limit, setLimit] = useState(PAGE)
  const { data, isLoading, isError } = usePartitionKeys(id, { status: onlyFailed ? 'failed' : undefined, q: query.trim() || undefined, limit })
  const percent = (n: number) => `${total ? (n / total) * 100 : 0}%`
  return (
    <div className="barca-node-keys">
      <KeyValue label="partitions">
        {cached.toLocaleString()} of {total.toLocaleString()} cached
        {failed > 0 && ` · ${failed.toLocaleString()} failed`}
        {stale > 0 && ` · ${stale.toLocaleString()} stale`}
      </KeyValue>
      <div className="barca-keys-bar" role="img" aria-label={`${cached} cached, ${failed} failed, ${stale} stale, of ${total} keys`}>
        <span className="is-cached" style={{ width: percent(cached) }} />
        <span className="is-failed" style={{ width: percent(failed) }} />
        <span className="is-stale" style={{ width: percent(stale) }} />
      </div>
      {(failed > 0 || stale > 0) && <p className="barca-note">Run computes only these {(failed + stale).toLocaleString()} keys; the others stay cached.</p>}
      {data && data.errors.length > 0 && failed > 0 && (
        <ul className="barca-keys-errors" aria-label="Errors of the failed keys">
          {data.errors.map((e) => <li key={e.error}><b>{e.keys.toLocaleString()}</b> <ErrorLine error={e.error} /></li>)}
        </ul>
      )}
      <details className="barca-keys" open={failed > 0}>
        <summary>Keys</summary>
        <div className="barca-keys-tools">
          <SearchInput value={query} onChange={(value) => { setQuery(value); setLimit(PAGE) }} placeholder="Search keys" />
          <ChipGroup label="Key state">
            <Chip pressed={onlyFailed} onClick={() => { setOnlyFailed(!onlyFailed); setLimit(PAGE) }}>failed only</Chip>
          </ChipGroup>
        </div>
        {isLoading && <Skeleton height={120} />}
        {isError && <p className="barca-note">Couldn't load keys.</p>}
        {data && (
          <>
            {data.keys.length === 0 && <p className="barca-note">No key matches.</p>}
            <ul className="barca-keys-list">
              {data.keys.map((k) => (
                <li key={k.key} className={k.status === 'failed' ? 'is-failed' : undefined}>
                  <code>{k.key}</code>
                  <span className="barca-cell-sub">
                    {k.status} · <Age at={k.created_at} nowMs={nowMs} />
                    {k.elapsed_seconds !== null && ` · ${formatSeconds(k.elapsed_seconds)}`}
                    {k.size_bytes !== null && ` · ${formatBytes(k.size_bytes)}`}
                    {k.run_id && <> · <Link to={`/runs?run=${encodeURIComponent(k.run_id)}`}>run</Link></>}
                  </span>
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
