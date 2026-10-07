import { useState, type ReactNode } from 'react'
import { Play, X } from 'lucide-react'
import {
  Button,
  Chip,
  ChipGroup,
  ConnectionBadge,
  IconButton,
  KeyValue,
  SearchInput,
  Section,
  Select,
  SidePanel,
  Skeleton,
  StatusBadge,
  StatusDot,
  Tag,
} from '@/components'
import type { StatusKind } from '@/lib/types'

const STATUSES: StatusKind[] = ['success', 'running', 'queued', 'failed']

function Spec({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="barca-kit-spec">
      <h2>{title}</h2>
      <div className="barca-kit-row">{children}</div>
    </section>
  )
}

/**
 * Every primitive in components/, in each state it has. Development only (router.tsx): the
 * place to look when changing one, and where a new one is added with its states.
 */
export function KitPage() {
  const [query, setQuery] = useState('')
  const [on, setOn] = useState(['tasks'])
  const [when, setWhen] = useState<'any' | 'day'>('any')
  const toggle = (k: string) => setOn((cur) => (cur.includes(k) ? cur.filter((x) => x !== k) : [...cur, k]))

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>Kit</h1>
            <span className="barca-count">components/ · development only</span>
          </div>
        </div>
      </div>
      <div className="barca-view-body barca-kit">
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
          {STATUSES.map((s) => (
            <StatusDot key={s} status={s} size={8} />
          ))}
          {STATUSES.map((s) => (
            <StatusBadge key={s} status={s} />
          ))}
          <StatusBadge status="success" subtle />
        </Spec>

        <Spec title="ConnectionBadge">
          <ConnectionBadge connection={{ kind: 'connecting' }} />
          <ConnectionBadge connection={{ kind: 'online', version: '0.18.0' }} />
          <ConnectionBadge connection={{ kind: 'offline' }} />
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
      </div>
    </div>
  )
}
