import type { RefObject } from 'react'
import { BellOff, Bell, Copy, ExternalLink, RotateCw } from 'lucide-react'
import { Age, ErrorLine, IconButton, MiddleName, StatusDot, TableSpacer, Tag } from '@/components'
import { muteKey, type FailureRow, type FailuresModel, type FailuresMsg } from '@/lib/failuresView'
import { impactLabel, type Failure } from '@/lib/health'
import type { Dispatch } from '@/lib/program'
import { nodeKindLabel } from '@/lib/status'

const COLUMNS = 6
const runs = (n: number) => `${n} ${n === 1 ? 'run' : 'runs'}`

export interface FailuresViewProps {
  model: FailuresModel
  rows: readonly FailureRow[]
  /** The rows in view, as indexes into `rows`, and the height of those that are not. */
  window: { items: { index: number }[]; before: number; after: number; measure?: (row: HTMLTableRowElement | null) => void }
  labels: ReadonlyMap<string, string>
  /** Where a failed node sits: its group path or file. */
  placeOf: (id: string) => string
  selected: string | null
  nowMs: number
  readOnly: boolean
  bodyRef?: RefObject<HTMLTableSectionElement | null>
  onSelect: (id: string | null) => void
  dispatch: Dispatch<FailuresMsg>
}

/** Every failure in scope under its cause, the cause doing most damage first. */
export function FailuresView({ model, rows, window: win, labels, placeOf, selected, nowMs, readOnly, bodyRef, onSelect, dispatch }: FailuresViewProps) {
  return (
    <table className="barca-table barca-group-table barca-failure-table">
      {model.notice && (
        <caption className="barca-failure-notice" role={model.notice.failed ? 'alert' : 'status'}>{model.notice.text}</caption>
      )}
      <colgroup>
        <col style={{ width: '23%' }} />
        <col style={{ width: '29%' }} />
        <col style={{ width: '9%' }} />
        <col style={{ width: '14%' }} />
        <col style={{ width: '12%' }} />
        <col style={{ width: 118 }} />
      </colgroup>
      <thead>
        <tr>
          <th>Failed node</th>
          <th>Error</th>
          <th>When</th>
          <th title="The first failed attempt after the last clean run, and runs failed in a row">Failing since</th>
          <th className="num" title="Nodes downstream of it, transitively">Waiting on it</th>
          <th><span className="barca-sr-only">Actions</span></th>
        </tr>
      </thead>
      <tbody ref={bodyRef}>
        <TableSpacer height={win.before} columns={COLUMNS} />
        {win.items.map(({ index }) => {
          const row = rows[index]
          if (!row) return null
          if (row.kind === 'cause') {
            const { cause, failures, impact } = row.group
            return (
              <tr key={`cause:${cause}`} ref={win.measure} data-index={index} className="barca-failure-cause">
                <td colSpan={COLUMNS}>
                  <strong title={cause}>{cause}</strong>
                  <span>{failures.length.toLocaleString()} failed · {impactLabel(impact)} waiting</span>
                </td>
              </tr>
            )
          }
          const f = row.failure
          return (
            <tr
              key={f.id}
              ref={win.measure}
              data-index={index}
              data-muted={row.muted || undefined}
              tabIndex={0}
              aria-selected={selected === f.id}
              onClick={(event) => {
                event.currentTarget.focus({ preventScroll: true })
                onSelect(selected === f.id ? null : f.id)
              }}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && event.target === event.currentTarget) {
                  event.preventDefault()
                  onSelect(f.id)
                }
              }}
            >
              <td>
                <div className="barca-group-row-name">
                  <StatusDot status="failed" size={6} />
                  <strong><MiddleName name={labels.get(f.id) ?? f.name} title={f.id} /></strong>
                  {f.kind !== 'asset' && <Tag size="sm">{nodeKindLabel(f)}</Tag>}
                </div>
                <div className="barca-cell-sub" title={placeOf(f.id)}>{placeOf(f.id)}</div>
              </td>
              <td className="barca-failure-error">
                <ErrorLine error={f.error} text={f.headline} />
                {f.missingKeys !== null && f.missingKeys > 0 && (
                  <div className="barca-cell-sub">{f.missingKeys.toLocaleString()} keys without a result</div>
                )}
              </td>
              <td>{f.at ? <Age at={f.at} nowMs={nowMs} /> : '–'}</td>
              <td>{f.failingSince ? <><Age at={f.failingSince} nowMs={nowMs} /> · {runs(f.failures)}</> : '–'}</td>
              <td className="num">{impactLabel(f.impact)}</td>
              <td onClick={(event) => event.stopPropagation()}>
                <FailureActions failure={f} muted={row.muted} readOnly={readOnly} name={labels.get(f.id) ?? f.name} dispatch={dispatch} />
              </td>
            </tr>
          )
        })}
        <TableSpacer height={win.after} columns={COLUMNS} />
      </tbody>
    </table>
  )
}

function FailureActions({ failure: f, muted, readOnly, name, dispatch }: {
  failure: Failure
  muted: boolean
  readOnly: boolean
  name: string
  dispatch: Dispatch<FailuresMsg>
}) {
  return (
    <div className="barca-failure-actions">
      <IconButton size="sm" label={`Retry ${name}`} title={readOnly ? 'This server is read-only' : `Retry ${name}`} disabled={readOnly} onClick={() => dispatch({ type: 'retry', id: f.id, kind: f.kind, name })}>
        <RotateCw size={13} />
      </IconButton>
      <IconButton size="sm" label={`Copy error of ${name}`} title="Copy error" onClick={() => dispatch({ type: 'copy', error: f.error || f.headline })}>
        <Copy size={13} />
      </IconButton>
      <IconButton size="sm" label={`Open run of ${name}`} title="Open run" onClick={() => dispatch({ type: 'open_run', id: f.id, name })}>
        <ExternalLink size={13} />
      </IconButton>
      <IconButton size="sm" label={`${muted ? 'Unmute' : 'Mute'} ${name}`} title={muted ? 'Unmute' : 'Mute until it recovers (this browser only)'} onClick={() => dispatch({ type: 'toggle_mute', key: muteKey(f) })}>
        {muted ? <Bell size={13} /> : <BellOff size={13} />}
      </IconButton>
    </div>
  )
}
