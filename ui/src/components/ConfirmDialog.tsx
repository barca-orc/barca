import { useEffect, useId, useRef, type ReactNode } from 'react'
import { Button } from './Button'

export interface ConfirmDialogProps {
  title: string
  body: ReactNode
  /** The confirm button's words: what it does, not "OK". */
  action: string
  onConfirm: () => void
  onCancel: () => void
}


/** barca · ConfirmDialog: one question before an action too big to start by accident. */
export function ConfirmDialog({ title, body, action, onConfirm, onCancel }: ConfirmDialogProps) {
  const dialog = useRef<HTMLDivElement>(null)
  const titleId = useId()
  useEffect(() => {
    const previous = document.activeElement
    dialog.current?.querySelector<HTMLElement>('input, button')?.focus()
    return () => { if (previous instanceof HTMLElement && previous.isConnected) previous.focus() }
  }, [])
  return (
    <div className="barca-confirm-backdrop">
      <div ref={dialog} className="barca-confirm" role="dialog" aria-modal="true" aria-labelledby={titleId}
        onKeyDown={(event) => {
          if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onCancel(); return }
          if (event.key !== 'Tab') return
          const controls = dialog.current?.querySelectorAll<HTMLElement>('input:not([disabled]), button:not([disabled])')
          const first = controls?.[0]
          const last = controls?.[controls.length - 1]
          if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus() }
          else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus() }
        }}>

        <h2 id={titleId}>{title}</h2>
        <div className="barca-confirm-body">{body}</div>
        <div className="barca-confirm-actions">
          <Button variant="ghost" size="sm" onClick={onCancel}>Cancel</Button>
          <Button variant="signal" size="sm" onClick={onConfirm}>{action}</Button>
        </div>
      </div>
    </div>
  )
}
