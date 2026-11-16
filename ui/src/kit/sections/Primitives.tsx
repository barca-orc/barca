import { useState } from 'react'
import { Play, X } from 'lucide-react'
import {
  Age,
  Button,
  Chip,
  ChipGroup,
  ConnectionBadge,
  ErrorLine,
  HealthBar,
  IconButton,
  PageRunButton,
  KeyValue,
  LastSuccess,
  MiddleName,
  SearchInput,
  Section,
  Select,
  SidePanel,
  Skeleton,
  StatusBadge,
  StatusGlyph,
  Tag,
} from '@/components'
import { NOW_MS, STATUS_KINDS, connections, failedNode, healthStates, nodeByState, variants } from '../fixtures'
import { CACHE_STATES, cacheStateGlyph, cacheStateLabel, nodeStateDisplay } from '@/lib/cacheStateDisplay'
import { GLYPHS } from '@/lib/glyph'
import { statusMeta } from '@/lib/status'
import { Spec, Variant } from '../Kit'
import { SidebarNav, SidebarSearch, SidebarView } from '@/layouts/Sidebar'
import type { PipelineSummary } from '@/lib/pipeline'
import type { SidebarGroup } from '@/lib/sidebar'
import { CommandPaletteView, ConfirmDialog } from '@/components'
import { ReadOnlyNotice } from '@/components/ReadOnlyBanner'
import { paletteActions, paletteIndex, searchPalette } from '@/lib/palette'
import { runButton } from '@/lib/runTarget'

/** Every primitive in components/, in each state it has. */
export function Primitives() {
  const KIT_PIPELINES: PipelineSummary[] = [
  { file: 'billing.py', name: 'billing.py', total: 639, shown: { glyph: 'failed', label: 'failing', live: false }, running: 0 },
  { file: 'catalog.py', name: 'catalog.py', total: 561, shown: null, running: 0 },
  { file: 'ledger.py', name: 'ledger.py', total: 12, shown: { glyph: 'running', label: '2 running', live: true }, running: 2 },
]
const KIT_GROUPS: SidebarGroup[] = [
  { id: 'group:billing.py:raw', name: 'Raw', depth: 0, tone: 'failed', glyph: 'failed' },
  { id: 'group:billing.py:raw:invoices', name: 'Invoices', depth: 1, tone: 'success', glyph: 'fresh' },
  { id: 'group:billing.py:marts', name: 'Marts', depth: 0, tone: null, glyph: null },
]

const [query, setQuery] = useState('')
  const [on, setOn] = useState(['tasks'])
  const [when, setWhen] = useState<'any' | 'day'>('any')
  const toggle = (k: string) => setOn((cur) => (cur.includes(k) ? cur.filter((x) => x !== k) : [...cur, k]))

  return (
    <>
    <Spec title="Button">
      <Button variant="signal" size="sm" iconLeft={<Play size={12} />}>
        signal
      </Button>
      <Button variant="neutral" size="sm">
        neutral
      </Button>
      <Button variant="ghost" size="sm">
        ghost
      </Button>
      <Button variant="danger" size="sm">
        danger
      </Button>
      <Button variant="signal" size="sm" loading>
        loading
      </Button>
      <Button variant="signal" size="sm" disabled>
        disabled
      </Button>
      <Button variant="signal" size="md">
        medium
      </Button>
    </Spec>

    <Spec title="IconButton">
      <IconButton label="Close" size="sm">
        <X size={14} />
      </IconButton>
      <IconButton label="Close">
        <X size={15} />
      </IconButton>
    </Spec>

    <Spec title="Tag">
      <Tag>default</Tag>
      <Tag tone="signal" dot>
        signal
      </Tag>
      <Tag tone="bare">bare</Tag>
      <Tag size="md">medium</Tag>
    </Spec>

    <Spec title="StatusGlyph" note="The one way a state is drawn: a 14px shape in the state's hue, each state its own shape and hue, in both themes. Nothing around a glyph is coloured.">
      {(['dark', 'light'] as const).map((theme) => (
        <div key={theme} className="barca-kit-theme" data-theme={theme}>
          <span className="barca-kit-theme-name">{theme}</span>
          <div className="barca-kit-glyphs">
            {GLYPHS.map((g) => (
              <span key={g} className="barca-kit-glyph"><StatusGlyph glyph={g} label={g} />{g}</span>
            ))}
            <span className="barca-kit-glyph"><StatusGlyph glyph="running" live label="running, live" />running · live</span>
          </div>
          <div className="barca-kit-glyphs">
            <StatusBadge {...nodeStateDisplay({ state: 'stale', lastFailed: true })} />
            <StatusBadge glyph="running" label="running · 3/10 keys" live />
            <StatusBadge {...nodeStateDisplay({ state: 'never_run', lastFailed: false, blockedBy: 'p:broken_report' })} />
            <StatusBadge {...nodeStateDisplay({ state: 'cached', lastFailed: false })} />
            <StatusBadge glyph="paused" label="paused" size="sm" />
          </div>
        </div>
      ))}
    </Spec>

    <Spec title="StatusBadge" note="A glyph and its word in the text colour: one state per node in plain words (failing, then pinned, then blocked by a failure upstream, outrank the cache state); then the run lifecycle's statuses.">
      <StatusBadge {...nodeStateDisplay({ state: 'stale', lastFailed: true })} />
      <StatusBadge {...nodeStateDisplay({ state: 'stale', lastFailed: false, pinned: 'override from fix.json' })} />
      <StatusBadge {...nodeStateDisplay({ state: 'never_run', lastFailed: false, blockedBy: 'p:broken_report' })} />
      {CACHE_STATES.map((s) => (
        <StatusBadge key={s} glyph={cacheStateGlyph(s)} label={cacheStateLabel(s)} />
      ))}
      <StatusBadge glyph="paused" label="paused" />
      {STATUS_KINDS.map((s) => (
        <StatusBadge key={s} glyph={statusMeta(s).glyph} label={statusMeta(s).label} live={statusMeta(s).live} />
      ))}
      {STATUS_KINDS.map((s) => (
        <StatusBadge key={s} glyph={statusMeta(s).glyph} label={statusMeta(s).label} live={statusMeta(s).live} size="sm" />
      ))}
    </Spec>

    <Spec title="CommandPaletteView" note="⌘K: results with their state's glyph, failing first among equal matches; before states load; nothing found.">
      {[
        { label: 'results', query: 'orders', items: searchPalette([...paletteActions('Run billing.py (505 assets, 1 sensor)', true), ...paletteIndex([{ id: nodeByState.cached.id, kind: 'asset' }, { id: failedNode.id, kind: 'asset' }], [], [{ id: 'orders-run', target: failedNode.id, status: 'failed' }], { byId: new Map([[nodeByState.cached.id, nodeByState.cached], [failedNode.id, failedNode]]), leavesOf: new Map() })], 'orders') },
        { label: 'states not loaded', query: 'inv', items: searchPalette([...paletteActions('Run billing.py (505 assets, 1 sensor)', true), ...paletteIndex([{ id: 'billing.py:invoices_00', kind: 'asset' }, { id: 'billing.py:check__invoices_00', kind: 'task' }], [], [])], 'inv') },
        { label: 'nothing found', query: 'zzz', items: [] },
      ].map((v) => (
        <Variant key={v.label} label={v.label}><div style={{ position: 'relative', transform: 'translateZ(0)', height: 200, width: 460 }}><CommandPaletteView query={v.query} items={v.items} active={0} onQuery={() => {}} onMove={() => {}} onPick={() => {}} onClose={() => {}} /></div></Variant>
      ))}
    </Spec>

    <Spec title="ConfirmDialog" note="Run all asks first; the labels are runButton()'s. A pipeline or a node runs at once.">
      <Variant label="run all"><div style={{ position: 'relative', transform: 'translateZ(0)', height: 200, width: 460 }}><ConfirmDialog {...runButton({ kind: 'all', assets: 8069, sensors: 1 }).confirm!} onConfirm={() => {}} onCancel={() => {}} /></div></Variant>
    </Spec>

    <Spec title="PageRunButton" note="The Run at the end of a page's toolbar: what is open, in runButton()'s short words; the size is in its name and tooltip. Disabled until the server is known to take runs; not there at all on a read-only one (ReadOnlyNotice says so once).">
      {[runButton({ kind: 'pipeline', file: 'billing.py', assets: 505, sensors: 1 }), runButton({ kind: 'all', assets: 8069, sensors: 1 })].map((b) => (
        <Variant key={b.label} label={b.title}><PageRunButton run={{ ...b, onRun: () => {}, disabled: false, loading: false }} /></Variant>
      ))}
      <Variant label="checking the server"><PageRunButton run={{ ...runButton({ kind: 'all', assets: 9, sensors: 0 }), onRun: () => {}, disabled: true, loading: false, title: 'Checking the server…' }} /></Variant>
    </Spec>

    <Spec title="ReadOnlyNotice" note="The one read-only indicator, across the top of every page of a barca serve --read-only; no write action is drawn there.">
      <Variant label="read-only server"><div style={{ width: 720 }}><ReadOnlyNotice /></div></Variant>
    </Spec>

    <Spec title="HealthBar" note="A scope's nodes by state; a single failure stays visible among thousands.">
      {Object.entries(healthStates).map(([label, c]) => (
        <Variant key={label} label={label}><HealthBar counts={c} /></Variant>
      ))}
      <Variant label="wide, every part named"><HealthBar counts={healthStates['every state']!} labelLimit={10} width={200} /></Variant>
    </Spec>

    <Spec title="Age / LastSuccess" note="Relative, absolute in the chosen zone on hover; whose success it is when not the row's own; a failing row adds since when and how many runs in a row, a group how many members fail.">
      <Variant label="minutes"><Age at={new Date(NOW_MS - 300_000).toISOString()} nowMs={NOW_MS} /></Variant>
      <Variant label="healthy"><LastSuccess cell={{ success: { at: new Date(NOW_MS - 3_600_000).toISOString(), of: 'node' }, failing: null }} nowMs={NOW_MS} /></Variant>
      <Variant label="failing"><LastSuccess cell={{ success: { at: new Date(NOW_MS - 172_800_000).toISOString(), of: 'node' }, failing: { lead: 'failing', since: new Date(NOW_MS - 50_400_000).toISOString(), tail: ' · 3 failed runs in a row' } }} nowMs={NOW_MS} /></Variant>
      <Variant label="partitioned"><LastSuccess cell={{ success: { at: new Date(NOW_MS - 600_000).toISOString(), of: 'any key' }, failing: { lead: 'failing', since: new Date(NOW_MS - 10_800_000).toISOString(), tail: ' · 130 failed runs in a row' } }} nowMs={NOW_MS} /></Variant>
      <Variant label="group"><LastSuccess cell={{ success: { at: new Date(NOW_MS - 432_000_000).toISOString(), of: 'oldest member' }, failing: { lead: '3 failing · oldest', since: new Date(NOW_MS - 10_800_000).toISOString(), tail: '' } }} nowMs={NOW_MS} /></Variant>
      <Variant label="never succeeded"><LastSuccess cell={{ success: null, failing: { lead: 'failing', since: new Date(NOW_MS - 3_600_000).toISOString(), tail: ' · 1 failed run' } }} nowMs={NOW_MS} /></Variant>
    </Spec>
    <Spec title="ConnectionBadge">
      {variants(connections).map((kind) => (
        <ConnectionBadge key={kind} connection={connections[kind]} />
      ))}
    </Spec>

    <Spec title="ErrorLine" note="The headline, and the traceback on expand when there is one.">
      <Variant label="one line"><ErrorLine error="row count fell 64% day over day" /></Variant>
      <Variant label="traceback"><ErrorLine error={'ValueError: bad shape\nTraceback (most recent call last):\n  File "a.py", line 3, in f'} /></Variant>
      <Variant label="bare traceback"><ErrorLine error={'Traceback (most recent call last):\n  File "a.py", line 3, in f\nKeyError: \'x\''} /></Variant>
      <Variant label="no error"><ErrorLine error={null} /></Variant>
      <Variant label="inline (a table cell: never expands)"><ErrorLine error={'ValueError: bad shape\nTraceback (most recent call last):\n  File "a.py", line 3, in f'} inline /></Variant>
      <Variant label="traceback, root taken off its paths"><div style={{ width: 300 }}><ErrorLine error={'KeyError: \'currency\'\nTraceback (most recent call last):\n  File "/home/me/project/pipelines/orders/enrich_with_exchange_rates.py", line 41, in orders_enriched'} root="/home/me/project" large /></div></Variant>
      <Variant label="panel size"><div style={{ width: 300 }}><ErrorLine error="row count fell 64% day over day" large /></div></Variant>
    </Spec>
    <Spec title="MiddleName" note="Cut in the middle when it does not fit; the full id on hover.">
      <Variant label="fits"><MiddleName name="invoices_00" /></Variant>
      <Variant label="cut"><div style={{ display: 'flex', width: 150 }}><MiddleName name="check__risk_review_alerts_01" /></div></Variant>
    </Spec>
    <Spec title="Sidebar" note="SidebarView: the sections, then the pipelines with the groups of an open file nested under the open group; its right edge resizes it (drag, arrow keys; double-click resets). Collapsed: the section icons, and the pipelines as one line with the project's state.">
      {([['open', false, 232], ['wide', false, 340], ['collapsed', true, 232]] as const).map(([name, collapsed, width]) => (
        <Variant key={name} label={name}>
          <div style={{ display: 'flex', height: 520 }}>
            <SidebarView
              search={<SidebarSearch onOpen={() => undefined} />}
              nav={<SidebarNav counts={{ assets: 1200, tasks: 14, sensors: 3, schedules: 6 }} search="" />}
              model={{ collapsed, width, expanded: [] }}
              total={1200}
              pipelines={KIT_PIPELINES}
              overall={{ glyph: 'failed', label: 'failing', live: false }}
              groupsOf={(file) => (file === 'billing.py' ? KIT_GROUPS : [])}
              selected="billing.py"
              scope="group:billing.py:raw"
              onSelect={() => undefined}
              onOpenGroup={() => undefined}
              dispatch={() => undefined}
            />
          </div>
        </Variant>
      ))}
    </Spec>

    <Spec title="SearchInput / Select / Chip">
      <SearchInput placeholder="Search by name or file" value={query} onChange={setQuery} />
      <Select
        label="Last run"
        value={when}
        options={[
          { value: 'any', label: 'any time' },
          { value: 'day', label: 'last 24h (3)' },
        ]}
        onChange={setWhen}
      />
      <ChipGroup label="Kind">
        {['assets', 'tasks', 'sensors'].map((k) => (
          <Chip key={k} pressed={on.includes(k)} onClick={() => toggle(k)}>
            4 {k}
          </Chip>
        ))}
      </ChipGroup>
    </Spec>

    <Spec title="Skeleton">
      <Skeleton width={160} height={14} />
      <Skeleton width={220} height={34} />
    </Spec>

    <Spec title="SidePanel / Section / KeyValue">
      <div className="barca-kit-panel">
        <SidePanel
          label="example details"
          title="example"
          badge={<span className="barca-sidepanel-kind">asset</span>}
          subtitle="pipeline.py:example"
          width={380}
          actions={
            <IconButton label="Close" size="sm">
              <X size={14} />
            </IconButton>
          }
        >
          <Section title="State">
            <StatusBadge glyph="fresh" label="fresh" />
            <p className="barca-note">cached result for this code and these inputs</p>
            <KeyValue label="cache key">
              <code>9f2c0a41be77</code>
            </KeyValue>
          </Section>
          <Section title="Metadata">
            <KeyValue label="partitioned">no</KeyValue>
            <KeyValue label="id">
              <code className="barca-wrap">pipeline.py:example</code>
            </KeyValue>
          </Section>
        </SidePanel>
      </div>
    </Spec>
    </>
  )
}
