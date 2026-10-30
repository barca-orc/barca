import { Button } from './Button'
import { injectStyles } from '@/lib/injectStyles'

export interface ConfirmDialogProps {
  title: string
  body: string
  /** The confirm button's words: what it does, not "OK". */
  action: string
  onConfirm: () => void
  onCancel: () => void
}

const STYLE_ID = 'barca-confirm-styles'
const CSS = `
  .barca-confirm-backdrop { position: fixed; inset: 0; z-index: 50; display: flex; align-items: center; justify-content: center; padding: 16px; background: color-mix(in srgb, var(--bg-canvas) 70%, transparent); }
  .barca-confirm { width: min(440px, 100%); background: var(--bg-raised); color: var(--text-default); border: 1px solid var(--border-default); border-radius: var(--radius-md); box-shadow: var(--elev-modal); padding: 20px; font-family: var(--font-sans); font-size: var(--text-sm); line-height: 1.5; }
  .barca-confirm h2 { margin: 0 0 10px; font-size: var(--text-md); color: var(--text-strong); }
  .barca-confirm p { margin: 0; }
  .barca-confirm-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px; }
`

/** barca · ConfirmDialog: one question before an action too big to start by accident. */
export function ConfirmDialog({ title, body, action, onConfirm, onCancel }: ConfirmDialogProps) {
  injectStyles(STYLE_ID, CSS)
  return (
    <div className="barca-confirm-backdrop">
      <div className="barca-confirm" role="dialog" aria-modal="true" aria-labelledby="barca-confirm-title">
        <h2 id="barca-confirm-title">{title}</h2>
        <p>{body}</p>
        <div className="barca-confirm-actions">
          <Button variant="ghost" size="sm" onClick={onCancel}>Cancel</Button>
          <Button variant="signal" size="sm" onClick={onConfirm} autoFocus>{action}</Button>
        </div>
      </div>
    </div>
  )
}
