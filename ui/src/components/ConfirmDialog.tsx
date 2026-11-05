import { Button } from './Button'

export interface ConfirmDialogProps {
  title: string
  body: string
  /** The confirm button's words: what it does, not "OK". */
  action: string
  onConfirm: () => void
  onCancel: () => void
}


/** barca · ConfirmDialog: one question before an action too big to start by accident. */
export function ConfirmDialog({ title, body, action, onConfirm, onCancel }: ConfirmDialogProps) {
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
