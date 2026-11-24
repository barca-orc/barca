import { createPortal } from 'react-dom'
import { useState } from 'react'
import { Button } from './Button'
import { ConfirmDialog } from './ConfirmDialog'

/** Optional scope controls. A hard refresh always includes Manual nodes. */
export function RunOptions({ includeManual, onIncludeManual, onRefresh, manualNames, disabled }: {
  includeManual: boolean
  onIncludeManual: (value: boolean) => void
  onRefresh: () => void
  manualNames: readonly string[]
  disabled: boolean
}) {
  const [asking, setAsking] = useState(false)
  const warning = `Hard refresh (--refresh-all) recomputes everything regardless of the cache, including Manual nodes${manualNames.length ? `: ${manualNames.join(', ')}` : ''}. Paused nodes and pinned assets stay held.`
  return <div className="barca-run-options">
    <label className="barca-note" title="Bring Manual assets up to date and run Manual upstream tasks too; current results still use the cache">
      <input type="checkbox" checked={includeManual} disabled={disabled} onChange={event => onIncludeManual(event.target.checked)} /> Including Manual
    </label>
    <Button size="sm" variant="ghost" disabled={disabled} title={warning} onClick={() => setAsking(true)}>Hard refresh…</Button>
    {asking && createPortal(<ConfirmDialog title="Hard refresh this scope?" body={warning} action="Hard refresh"
      onCancel={() => setAsking(false)} onConfirm={() => { setAsking(false); onRefresh() }} />, document.body)}
  </div>
}
