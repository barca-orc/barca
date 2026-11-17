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
  ViewToggle,
  type PageRun,
} from '@/components'
import { LAST_RUN_LABEL, PAGE_LABEL, toggle, type AssetFilter } from '@/hooks/useAssetFilter'
import { PINNED } from '@/lib/cacheStateDisplay'
import { MAP_PARTS, mapPartColor, mapPartLabel, mapPartMeaning } from '@/lib/causeDisplay'
import { pipelineName } from '@/lib/pipeline'
import { EMPTY_FILTERS, LAST_RUNS, isFiltered } from '@/lib/assetFilters'

/**
 * One toolbar for a kind's page, the same over its list and its graph: the title and count, the
 * state filter, then search, the last-run filter, the view and Run. What fails needs no banner:
 * its segment says how many, and what it blocks. A page with nothing of its kind has no Run.
 */
export function AssetsToolbar({ filter, run, extra }: { filter: AssetFilter; run: PageRun | null | undefined; extra?: ReactNode }) {
  const { kind, data, pipeline, rows, visible, filters, setFilters, counts } = filter
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
      {/* Why a node is not fresh, in the health map's words and colours, what alarms first;
          a row falls under its served cause, and an unfiltered count is the rollup's own. */}
      <ChipGroup label="State" segmented>
        {MAP_PARTS.filter((part) => counts.states[part] > 0 || filters.states.includes(part)).map((part) => (
          <Chip
            key={part}
            pressed={filters.states.includes(part)}
            title={`${counts.states[part].toLocaleString()} ${mapPartLabel(part)}: ${mapPartMeaning(part)}`}
            onClick={() => setFilters({ ...filters, states: toggle(filters.states, part) })}
          >
            <i className="barca-chip-dot" data-part={part} style={{ background: mapPartColor(part) }} aria-hidden />
            <b>{counts.states[part].toLocaleString()}</b> <span className="barca-chip-word">{mapPartLabel(part)}</span>
          </Chip>
        ))}
      </ChipGroup>
      {(counts.pinned > 0 || filters.pinned) && (
        <ChipGroup label="Pinned">
          <Chip pressed={filters.pinned} title={`${counts.pinned} ${PINNED}`} onClick={() => setFilters({ ...filters, pinned: !filters.pinned })}>
            <StatusGlyph glyph="pinned" size={12} />
            <b>{counts.pinned}</b> <span className="barca-chip-word">{PINNED}</span>
          </Chip>
        </ChipGroup>
      )}
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
          <X size={12} /> Clear
        </button>
      )}
      <div className="barca-view-actions">
        {extra}
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
