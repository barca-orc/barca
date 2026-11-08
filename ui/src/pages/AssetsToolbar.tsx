import type { ReactNode } from 'react'
import { CalendarClock, Clock, Layers, X } from 'lucide-react'
import {
  Chip,
  ChipGroup,
  PageRunButton,
  SearchInput,
  Select,
  Skeleton,
  StatusGlyph,
  Tag,
  ViewToggle,
  type PageRun,
} from '@/components'
import { useHealth } from '@/hooks/useHealth'
import { LAST_RUN_LABEL, PAGE_LABEL, toggle, type AssetFilter } from '@/hooks/useAssetFilter'
import { BLOCKED_UPSTREAM, PINNED, CACHE_STATES, LAST_ATTEMPT_FAILED, cacheStateGlyph, cacheStateLabel } from '@/lib/cacheStateDisplay'
import { plural } from '@/lib/plural'
import { pipelineName } from '@/lib/pipeline'
import { EMPTY_FILTERS, LAST_RUNS, isFiltered } from '@/lib/assetFilters'

/**
 * One toolbar for a kind's page, the same over its list and its graph: the title and count, the
 * state filter, then search, the last-run filter, the view and Run. What fails needs no banner:
 * its segment says how many, and what it blocks. A page with nothing of its kind has no Run.
 */
export function AssetsToolbar({ filter, run, extra }: { filter: AssetFilter; run: PageRun | null | undefined; extra?: ReactNode }) {
  const { kind, data, pipeline, rows, visible, filters, onDefault, setFilters, counts, blocked } = filter
  const { data: health } = useHealth()
  const loaded = data !== undefined
  return (
    <div className="barca-view-bar">
      <div className="barca-view-title">
        <h1>{PAGE_LABEL[kind]}{pipeline ? ` · ${pipelineName(pipeline)}` : ''}</h1>
        {loaded ? (
          <span className="barca-count" title={pipeline ?? undefined}>
            {visible.length === rows.length ? rows.length.toLocaleString() : `${visible.length.toLocaleString()} of ${rows.length.toLocaleString()}`}
            <span className="barca-sr-only"> {rows.length === 1 ? kind : `${kind}s`}{visible.length === rows.length ? '' : ' shown'}</span>
          </span>
        ) : (
          <Skeleton width={24} height={14} />
        )}
      </div>
      {/* The state words, as `barca status` prints them; a failed last attempt is a filter of its own. */}
      <ChipGroup label="State" segmented>
        {(counts.failed > 0 || (filters.failed && !onDefault)) && (
          <Chip
            pressed={filters.failed}
            title={`${counts.failed} ${LAST_ATTEMPT_FAILED}${blocked > 0 ? ` · blocks ${plural(blocked, 'downstream node')}` : ''}`}
            onClick={() => setFilters({ ...filters, failed: !filters.failed })}
          >
            <StatusGlyph glyph="failed" size={12} />
            <b>{counts.failed}</b> <span className="barca-chip-word">{LAST_ATTEMPT_FAILED}</span>
          </Chip>
        )}
        {(counts.pinned > 0 || filters.pinned) && (
          <Chip pressed={filters.pinned} title={`${counts.pinned} ${PINNED}`} onClick={() => setFilters({ ...filters, pinned: !filters.pinned })}>
            <StatusGlyph glyph="pinned" size={12} />
            <b>{counts.pinned}</b> <span className="barca-chip-word">{PINNED}</span>
          </Chip>
        )}
        {(counts.blocked > 0 || (filters.blocked && !onDefault)) && (
          <Chip pressed={filters.blocked} title={`${counts.blocked} ${BLOCKED_UPSTREAM}`} onClick={() => setFilters({ ...filters, blocked: !filters.blocked })}>
            <StatusGlyph glyph="blocked" size={12} />
            <b>{counts.blocked}</b> <span className="barca-chip-word">{BLOCKED_UPSTREAM}</span>
          </Chip>
        )}
        {CACHE_STATES.filter((state) => counts.states[state] > 0 || (filters.states.includes(state) && !onDefault)).map((state) => (
          <Chip
            key={state}
            pressed={filters.states.includes(state)}
            title={`${counts.states[state]} ${cacheStateLabel(state)}`}
            onClick={() => setFilters({ ...filters, states: toggle(filters.states, state) })}
          >
            <StatusGlyph glyph={cacheStateGlyph(state)} size={12} />
            <b>{counts.states[state]}</b> <span className="barca-chip-word">{cacheStateLabel(state)}</span>
          </Chip>
        ))}
      </ChipGroup>
      {(counts.scheduled > 0 || filters.scheduled) && (
        <ChipGroup label="Scheduled">
          <Chip pressed={filters.scheduled} title={`${counts.scheduled} scheduled`} onClick={() => setFilters({ ...filters, scheduled: !filters.scheduled })}>
            <CalendarClock size={12} /><b>{counts.scheduled}</b> <span className="barca-chip-word">scheduled</span>
          </Chip>
        </ChipGroup>
      )}
      {(counts.partitioned > 0 || filters.partitioned) && (
        <ChipGroup label="Partitioned">
          <Chip pressed={filters.partitioned} title={`${counts.partitioned} partitioned`} onClick={() => setFilters({ ...filters, partitioned: !filters.partitioned })}>
            <Layers size={12} /><b>{counts.partitioned}</b> <span className="barca-chip-word">partitioned</span>
          </Chip>
        </ChipGroup>
      )}
      {isFiltered(filters) && rows.length > 0 && (
        <button type="button" className="barca-clear" onClick={() => setFilters(EMPTY_FILTERS)}>
          <X size={12} /> {onDefault ? `Show all ${rows.length.toLocaleString()}` : 'Clear'}
        </button>
      )}
      <div className="barca-view-actions">
        {extra}
        {health?.read_only && <Tag tone="bare">read-only</Tag>}
        {(rows.length > 0 || !loaded) && <SearchInput
          placeholder={`Search ${PAGE_LABEL[kind].toLowerCase()}`}
          label="Search by name or file"
          value={filters.query}
          onChange={(query) => setFilters({ ...filters, query })}
        />}
        {(rows.length > 0 || !loaded) && <Select
          label="Last run"
          icon={<Clock size={13} />}
          value={filters.lastRun}
          options={LAST_RUNS.map((l) => ({
            value: l,
            label: LAST_RUN_LABEL[l] + (l === 'any' ? '' : ` (${counts.lastRun[l]})`),
          }))}
          onChange={(lastRun) => setFilters({ ...filters, lastRun })}
        />}
        {kind === 'asset' && <ViewToggle />}
        {/* Nothing of the page's kind: nothing here to run. */}
        {(rows.length > 0 || !loaded) && <PageRunButton run={run} />}
      </div>
    </div>
  )
}
