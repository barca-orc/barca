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
  KeyValue,
  LastSuccess,
  MiddleName,
  SearchInput,
  Section,
  Select,
  SidePanel,
  Skeleton,
  StatusBadge,
  StatusDot,
  Tag,
} from '@/components'
import { NOW_MS, STATUS_KINDS, connections, healthStates, variants } from '../fixtures'
import { Spec, Variant } from '../Kit'
import { SidebarView } from '@/layouts/Sidebar'
import type { PipelineSummary } from '@/lib/pipeline'
import type { SidebarGroup } from '@/lib/sidebar'
import { ConfirmDialog } from '@/components'
import { runButton } from '@/lib/runTarget'

/** Every primitive in components/, in each state it has. */
export function Primitives() {
  const KIT_PIPELINES: PipelineSummary[] = [
  { file: 'billing.py', name: 'billing.py', total: 639, worst: 'failed', known: true, running: 0 },
  { file: 'catalog.py', name: 'catalog.py', total: 561, worst: 'unknown', known: false, running: 0 },
]
const KIT_GROUPS: SidebarGroup[] = [
  { id: 'group:billing.py:raw', name: 'Raw', depth: 0, status: 'failed' },
  { id: 'group:billing.py:raw:invoices', name: 'Invoices', depth: 1, status: 'success' },
  { id: 'group:billing.py:marts', name: 'Marts', depth: 0, status: null },
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

    <Spec title="StatusDot / StatusBadge">
      {STATUS_KINDS.map((s) => (
        <StatusDot key={s} status={s} size={8} />
      ))}
      {STATUS_KINDS.map((s) => (
        <StatusBadge key={s} status={s} />
      ))}
      <StatusBadge status="failed" subtle />
      <StatusBadge status="success" subtle={false} />
    </Spec>

    <Spec title="ConfirmDialog" note="Run all asks first; the labels are runButton()'s. A pipeline or a node runs at once.">
      <Variant label="run all"><div style={{ position: 'relative', transform: 'translateZ(0)', height: 200, width: 460 }}><ConfirmDialog {...runButton({ kind: 'all', nodes: 8070 }).confirm!} onConfirm={() => {}} onCancel={() => {}} /></div></Variant>
    </Spec>

    <Spec title="Topbar Run labels" note="runButton() for each target: what it runs and how much.">
      {[runButton({ kind: 'node', name: 'invoices_00' }), runButton({ kind: 'pipeline', file: 'billing.py', nodes: 506 }), runButton({ kind: 'all', nodes: 8070 }), runButton({ kind: 'none', hint: 'Select a node to run' })].map((b) => (
        <Variant key={b.label} label={b.title}><Button variant="signal" size="sm">{b.label}</Button></Variant>
      ))}
    </Spec>

    <Spec title="HealthBar" note="A scope's nodes by state; a single failure stays visible among thousands.">
      {Object.entries(healthStates).map(([label, c]) => (
        <Variant key={label} label={label}><HealthBar counts={c} /></Variant>
      ))}
      <Variant label="wide, every bucket named"><HealthBar counts={healthStates['failed and stale']!} labelLimit={6} width={200} /></Variant>
    </Spec>

    <Spec title="Age / LastSuccess" note="Relative, absolute UTC on hover; a failing row adds since when and how many runs.">
      <Variant label="minutes"><Age at={new Date(NOW_MS - 300_000).toISOString()} nowMs={NOW_MS} /></Variant>
      <Variant label="healthy"><LastSuccess at={new Date(NOW_MS - 3_600_000).toISOString()} failingSince={null} failures={0} nowMs={NOW_MS} /></Variant>
      <Variant label="failing"><LastSuccess at={new Date(NOW_MS - 172_800_000).toISOString()} failingSince={new Date(NOW_MS - 50_400_000).toISOString()} failures={3} nowMs={NOW_MS} /></Variant>
      <Variant label="never succeeded"><LastSuccess at={null} failingSince={new Date(NOW_MS - 3_600_000).toISOString()} failures={1} nowMs={NOW_MS} /></Variant>
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
      <Variant label="panel size"><div style={{ width: 300 }}><ErrorLine error="row count fell 64% day over day" large /></div></Variant>
    </Spec>
    <Spec title="MiddleName" note="Cut in the middle when it does not fit; the full id on hover.">
      <Variant label="fits"><MiddleName name="invoices_00" /></Variant>
      <Variant label="cut"><div style={{ display: 'flex', width: 150 }}><MiddleName name="check__risk_review_alerts_01" /></div></Variant>
    </Spec>
    <Spec title="Sidebar" note="SidebarView: the groups of an open file, nested under the open group; collapsed to dots.">
      {([['open', false], ['collapsed', true]] as const).map(([name, collapsed]) => (
        <Variant key={name} label={name}>
          <div style={{ display: 'flex', height: 300 }}>
            <SidebarView
              model={{ collapsed, expanded: [] }}
              total={1200}
              pipelines={KIT_PIPELINES}
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
          badge={<Tag size="sm">asset</Tag>}
          subtitle="pipeline.py"
          width={380}
          actions={
            <IconButton label="Close" size="sm">
              <X size={14} />
            </IconButton>
          }
        >
          <Section title="State">
            <StatusBadge status="success" label="cached" />
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
