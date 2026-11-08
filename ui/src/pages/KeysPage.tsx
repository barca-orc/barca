import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Play, X } from 'lucide-react'
import { Button, Chip, ChipGroup, SearchInput, Select, Skeleton, StatusGlyph } from '@/components'
import { KeyDetail } from '@/components/partitions/KeyDetail'
import { KeyGrid } from '@/components/partitions/KeyGrid'
import { KeyTable } from '@/components/partitions/KeyTable'
import { usePartitionGrid, usePartitionKey } from '@/hooks/usePartitionGrid'
import { api } from '@/lib/api'
import { shortName } from '@/lib/graph'
import { keysParams, keysSearch, toggleState, withValues, type KeysParams } from '@/lib/keysPage'
import {
  cellOf, countStates, filterCells, filterIsSet, gridOf, keyOf, KEY_STATE_GLYPH, KEY_STATE_WORD, KEY_STATES,
  filterSelection, layoutBlocks, needsRun, NO_FILTER, rangeCells, rangeSelection, stateOf, type KeyRange, type KeyState,
} from '@/lib/partitionGrid'
import { plural } from '@/lib/plural'
import { partitionSelection } from '@/lib/nodeDetail'

/** Keys a run computes: every state but fresh. */
function countNeeding(counts: Record<KeyState, number>): number {
  return KEY_STATES.filter((s) => s !== 'fresh').reduce((n, s) => n + counts[s], 0)
}

/** A dimension with at most this many values gets a select; a longer one is searched. */
const SELECT_MAX = 60

/**
 * The key explorer: every key of a partitioned asset at once (`#/keys?node=...`). A grid of all
 * of them by state, the filtered keys as a table, and one key's detail beside them; the chain's
 * other assets one click away, at the same key.
 */
export function KeysPage() {
  const [search, setSearch] = useSearchParams()
  const params = keysParams(search)
  const { node, filter } = params
  const navigate = useNavigate()
  const client = useQueryClient()
  const gridQuery = usePartitionGrid(node)
  const keyQuery = usePartitionKey(node, params.key)
  const grid = useMemo(() => (gridQuery.data ? gridOf(gridQuery.data) : null), [gridQuery.data])
  // Ages are read against when the grid arrived, as the asset tables read theirs.
  const nowMs = gridQuery.dataUpdatedAt
  const layout = useMemo(() => (grid ? layoutBlocks(grid) : null), [grid])
  const filterKey = keysSearch({ node: null, key: null, filter })
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const cells = useMemo(() => (grid ? filterCells(grid, filter) : []), [grid, filterKey])
  const shown = useMemo(() => {
    if (!grid || !filterIsSet(filter)) return null
    const mask = new Uint8Array(grid.size)
    for (const c of cells) mask[c] = 1
    return mask
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [grid, cells, filterKey])
  // The chips count what the other filters keep: with region=apac, apac's failed keys.
  const totals = useMemo(() => {
    if (!grid) return null
    const rest = { ...filter, states: [] }
    return countStates(grid, filterIsSet(rest) ? filterCells(grid, rest) : null)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [grid, filterKey])
  // What Run computes without a selection: the keys of the dimension filters, or every key.
  const scope = useMemo(() => {
    if (!grid) return null
    const byValues = { ...NO_FILTER, values: filter.values }
    if (!filterIsSet(byValues)) return { selection: null, needing: countNeeding(countStates(grid, null)) }
    return { selection: filterSelection(grid, byValues), needing: countNeeding(countStates(grid, filterCells(grid, byValues))) }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [grid, filterKey])
  const selected = grid && params.key ? cellOf(grid, params.key) : null
  // A selection belongs to the node it was made on: another node's explorer starts without one.
  const [picked, setPicked] = useState<{ node: string | null; range: KeyRange } | null>(null)
  const range = picked?.node === node ? picked.range : null
  const setRange = (next: KeyRange | null) => setPicked(next ? { node, range: next } : null)

  // Each change is read against the URL as it is now, not as it was when this page last drew:
  // two changes in a row (a chip, then a select) both land.
  const go = (change: (p: KeysParams) => Partial<KeysParams>) => {
    // The live address: the router's copy of it waits for the next render.
    const hash = window.location.hash
    const now = keysParams(new URLSearchParams(hash.includes('?') ? hash.slice(hash.indexOf('?') + 1) : ''))
    setSearch(keysSearch({ ...now, ...change(now) }), { replace: true })
  }
  const setFilter = (change: (f: KeysParams['filter']) => KeysParams['filter']) => go((p) => ({ filter: change(p.filter) }))
  // Another node of the chain, at the same key: its own states and errors, so only the key, the
  // dimension values and the search carry over.
  const chainSearch = (id: string) =>
    keysSearch({ node: id, key: params.key, filter: { ...NO_FILTER, values: filter.values, q: filter.q } })
  const select = (cell: number | null) => go(() => ({ key: cell === null || !grid ? null : keyOf(grid, cell) }))

  const start = useMutation({
    mutationFn: (partition: string | null) =>
      api.startRun({ command: 'get', targets: [node!], ...(partition ? { partition } : {}) }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ['runs'] })
      setTimeout(() => void client.invalidateQueries({ queryKey: ['partition-grid'] }), 1500)
    },
  })

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && (e.target as HTMLElement | null)?.tagName !== 'INPUT') {
        if (range) setRange(null)
        else if (params.key) go(() => ({ key: null }))
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  })

  if (!node) {
    return <div className="barca-view"><p className="barca-note">Pick a partitioned asset to explore its keys.</p></div>
  }

  const raw = gridQuery.data
  const inRange = grid && layout && range ? rangeCells(layout, range).filter((c) => stateOf(grid, c) !== null) : []
  const needing = scope?.needing ?? 0
  const valueOptions = grid?.dims.filter((d) => d.values.length > 1 && d.values.length <= SELECT_MAX) ?? []

  return (
    <div className="barca-view barca-keys">
      <div className="barca-view-head">
      <div className="barca-view-bar">
        <div className="barca-view-title">
          <Link className="barca-keys-back" to={`/assets?node=${encodeURIComponent(node)}`} title="Back to the asset">
            <ArrowLeft size={14} />
          </Link>
          <h1>{shortName(node)}</h1>
          {grid ? (
            <span className="barca-count">
              {cells.length === grid.raw.states.replace(/-/g, '').length
                ? `${cells.length.toLocaleString()} keys`
                : `${cells.length.toLocaleString()} of ${grid.raw.states.replace(/-/g, '').length.toLocaleString()} keys`}
            </span>
          ) : <Skeleton width={60} height={14} />}
        </div>
        {totals && (
          <ChipGroup label="Key state" segmented>
            {KEY_STATES.filter((s) => totals[s] > 0 || filter.states.includes(s)).map((s) => (
              <Chip key={s} pressed={filter.states.includes(s)} onClick={() => setFilter((f) => toggleState(f, s))}>
                <StatusGlyph glyph={KEY_STATE_GLYPH[s]} size={12} />
                <b>{totals[s].toLocaleString()}</b> <span className="barca-chip-word">{KEY_STATE_WORD[s]}</span>
              </Chip>
            ))}
          </ChipGroup>
        )}
        <div className="barca-view-actions">
          {valueOptions.map((d) => (
            <Select
              key={d.name}
              label={d.name}
              value={filter.values[d.name]?.[0] ?? ''}
              options={[{ value: '', label: `Any ${d.name}` }, ...d.values.map((v) => ({ value: v, label: v }))]}
              onChange={(v) => setFilter((f) => withValues(f, d.name, v ? [v] : []))}
            />
          ))}
          {raw && raw.errors.length > 0 && (
            <Select
              label="Error"
              value={filter.error === null ? '' : String(filter.error)}
              options={[
                { value: '', label: 'Any error' },
                ...raw.errors.map((e, i) => ({ value: String(i), label: `${e.keys.toLocaleString()} × ${e.error.length > 60 ? `${e.error.slice(0, 59)}…` : e.error}` })),
              ]}
              onChange={(v) => setFilter((f) => ({ ...f, error: v === '' ? null : Number(v) }))}
            />
          )}
          <SearchInput value={filter.q} onChange={(q) => setFilter((f) => ({ ...f, q }))} placeholder="Search keys" />
          {filterIsSet(filter) && (
            <Button variant="ghost" size="sm" iconLeft={<X size={12} />} onClick={() => setFilter(() => NO_FILTER)}>Clear</Button>
          )}
          {range && inRange.length > 0 ? (
            <Button
              size="sm"
              iconLeft={<Play size={11} />}
              loading={start.isPending}
              title="Compute the keys of the selection that are not fresh"
              onClick={() => start.mutate(rangeSelection(grid!, layout!, range))}
            >
              Run {plural(inRange.length, 'selected key')}
            </Button>
          ) : (
            <Button
              size="sm"
              iconLeft={<Play size={11} />}
              disabled={needing === 0}
              loading={start.isPending}
              title={needing === 0 ? 'Every key here is fresh' : scope?.selection ? 'Compute the keys of these values that are not fresh; the rest stay cached' : 'Compute every key that is not fresh; the rest stay cached'}
              onClick={() => start.mutate(scope?.selection ?? null)}
            >
              {needing === 0 ? 'All fresh' : `Run ${plural(needing, 'key')}`}
            </Button>
          )}
        </div>
      </div>
      {raw && raw.chain.length > 1 && (
        <nav className="barca-keys-chain" aria-label="Assets sharing these keys">
          <span className="barca-cell-sub">Same keys</span>
          {raw.chain.map((id, i) => (
            <span key={id} className="barca-keys-chain-step">
              {i > 0 && <span aria-hidden className="barca-keys-chain-arrow">→</span>}
              {id === node
                ? <b aria-current="page">{shortName(id)}</b>
                : <Link to={`/keys?${chainSearch(id)}`}>{shortName(id)}</Link>}
            </span>
          ))}
        </nav>
      )}
      {(start.data || start.error) && (
        <p className="barca-keys-notice" role="status">
          {start.error
            ? `Couldn't start the run: ${(start.error as Error).message}`
            : <>Started run <Link to={`/runs?run=${encodeURIComponent(start.data!.run_id)}`}>{start.data!.run_id}</Link>; keys update as it goes.</>}
        </p>
      )}
      </div>
      <div className="barca-keys-body">
        <div className="barca-keys-main">
          {gridQuery.isError && <p className="barca-note">Couldn't load the keys: {(gridQuery.error as Error).message}</p>}
          {gridQuery.isLoading && <Skeleton height={240} />}
          {raw && !raw.known && (
            <p className="barca-note">The keys are not known yet: they come from an upstream result that has not been computed. Run the asset to compute it.</p>
          )}
          {grid && layout && layout.blocks.length > 0 && (
            <section className="barca-keys-grid" aria-label="Every key by state">
              <KeyGrid
                grid={grid}
                layout={layout}
                shown={shown}
                selected={selected}
                range={range}
                onPick={(at, extend) => {
                  const cell = layout.blocks[at.block]!.cells[at.index]!
                  const anchor = selected === null ? null : layout.blocks[at.block]!.cells.indexOf(selected)
                  if (extend && anchor !== null && anchor >= 0) setRange({ block: at.block, from: anchor, to: at.index })
                  else {
                    setRange(null)
                    select(cell)
                  }
                }}
              />
              <p className="barca-keys-hint barca-cell-sub">
                {range
                  ? <>{plural(inRange.length, 'key')} selected, {inRange.filter((c) => needsRun(stateOf(grid, c))).length.toLocaleString()} not fresh · Esc clears</>
                  : 'Click a key to open it · Shift-click another in the same row to select the keys between'}
              </p>
            </section>
          )}
          {grid && (
            <KeyTable grid={grid} cells={cells} selected={selected} onSelect={select} nowMs={nowMs} />
          )}
        </div>
        {params.key && (
          <KeyDetail
            node={node}
            keyName={params.key}
            detail={keyQuery.data}
            loading={keyQuery.isLoading}
            error={keyQuery.error as Error | null}
            hrefFor={(id) => `/keys?${chainSearch(id)}`}
            running={start.isPending}
            onRun={() => start.mutate(partitionSelection(params.key!))}
            onClose={() => go(() => ({ key: null }))}
          />
        )}
      </div>
      {gridQuery.isError && (
        <p className="barca-note"><button type="button" className="barca-link" onClick={() => navigate(-1)}>Go back</button></p>
      )}
    </div>
  )
}
