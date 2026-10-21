import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Button } from './Button'
import { api } from '@/lib/api'
import { injectStyles } from '@/lib/injectStyles'
import type { ResetResult } from '@/lib/types'

/**
 * barca · Reset database
 * `POST /state/reset` (`barca state reset`) behind a confirmation dialog. The metadata
 * database is moved aside to `metadata.db.reset-<UTC>` and an empty one takes its place:
 * nothing is deleted, and the dialog says how to put it back. After a reset the UI reloads.
 */

export type ResetPhase =
  | { kind: 'confirm' }
  | { kind: 'pending' }
  | { kind: 'done'; result: ResetResult }
  | { kind: 'failed'; error: string; remediation: string | null }

export interface ResetDatabaseDialogProps {
  phase: ResetPhase
  onConfirm: () => void
  onCancel: () => void
  onReload: () => void
}

const STYLE_ID = 'barca-reset-styles'
const CSS = `
  .barca-reset-backdrop { position: fixed; inset: 0; z-index: 50; display: flex; align-items: center; justify-content: center; padding: 16px; background: color-mix(in srgb, var(--bg-canvas) 70%, transparent); }
  .barca-reset-dialog { width: min(480px, 100%); background: var(--bg-raised); color: var(--text-default); border: 1px solid var(--border-default); border-radius: var(--radius-md); box-shadow: var(--elev-modal); padding: 20px; font-family: var(--font-sans); font-size: var(--text-sm); line-height: 1.5; }
  .barca-reset-dialog h2 { margin: 0 0 10px; font-size: var(--text-md); color: var(--text-strong); }
  .barca-reset-dialog p { margin: 0 0 10px; }
  .barca-reset-dialog code { font-family: var(--font-mono); font-size: 0.95em; overflow-wrap: anywhere; }
  .barca-reset-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px; }
  .barca-reset-error { color: var(--status-failed); }
`

/** The dialog alone, for each phase of a reset. */
export function ResetDatabaseDialog({ phase, onConfirm, onCancel, onReload }: ResetDatabaseDialogProps) {
  injectStyles(STYLE_ID, CSS)
  return (
    <div className="barca-reset-backdrop">
      <div className="barca-reset-dialog" role="dialog" aria-modal="true" aria-labelledby="barca-reset-title">
        {phase.kind === 'done' ? (
          <>
            <h2 id="barca-reset-title">Database reset</h2>
            {phase.result.moved_to ? (
              <>
                <p>
                  The old database is now <code>{phase.result.moved_to}</code>, and{' '}
                  <code>{phase.result.database}</code> is new and empty.
                </p>
                <p>
                  To undo, stop <code>barca serve</code> and run <code>{phase.result.undo}</code>.
                </p>
              </>
            ) : (
              <p>
                There was no database; <code>{phase.result.database}</code> is new and empty.
              </p>
            )}
            <div className="barca-reset-actions">
              <Button variant="signal" size="sm" onClick={onReload} autoFocus>
                Reload
              </Button>
            </div>
          </>
        ) : (
          <>
            <h2 id="barca-reset-title">Reset the metadata database?</h2>
            <p>
              The database (with its <code>-wal</code> and <code>-shm</code>) is moved aside to{' '}
              <code>metadata.db.reset-&lt;UTC&gt;</code> in the same directory, and an empty one takes its
              place. Run history, cache records, partition keys and schedule pauses start over; the next
              run recomputes what it needs. Artifacts are left alone.
            </p>
            <p>
              Nothing is deleted. To undo, stop <code>barca serve</code> and run{' '}
              <code>barca state restore --from &lt;the moved file&gt;</code>.
            </p>
            {phase.kind === 'failed' && (
              <p className="barca-reset-error" role="alert">
                Not reset: {phase.error}
                {phase.remediation ? ` ${phase.remediation}` : ''}
              </p>
            )}
            <div className="barca-reset-actions">
              <Button variant="ghost" size="sm" onClick={onCancel} disabled={phase.kind === 'pending'}>
                Cancel
              </Button>
              <Button variant="danger" size="sm" onClick={onConfirm} loading={phase.kind === 'pending'}>
                Reset database
              </Button>
            </div>
          </>
        )}
      </div>
    </div>
  )
}

export interface ResetDatabaseButtonProps {
  /** A `--read-only` server refuses a reset (403): the button is shown disabled. */
  readOnly?: boolean
}

/** The Reset database button and its dialog. */
export function ResetDatabaseButton({ readOnly = false }: ResetDatabaseButtonProps) {
  const [open, setOpen] = useState(false)
  const client = useQueryClient()
  const reset = useMutation({
    mutationFn: api.resetState,
    onSuccess: () => client.invalidateQueries(),
  })
  const phase: ResetPhase = reset.isPending
    ? { kind: 'pending' }
    : reset.isSuccess
      ? { kind: 'done', result: reset.data }
      : reset.isError
        ? {
            kind: 'failed',
            error: reset.error.message,
            remediation: 'remediation' in reset.error ? (reset.error.remediation as string | null) : null,
          }
        : { kind: 'confirm' }
  return (
    <>
      <Button
        variant="danger"
        size="sm"
        disabled={readOnly}
        title={readOnly ? 'This server is read-only (barca serve --read-only)' : undefined}
        onClick={() => {
          reset.reset()
          setOpen(true)
        }}
      >
        Reset database
      </Button>
      {open && (
        <ResetDatabaseDialog
          phase={phase}
          onConfirm={() => reset.mutate()}
          onCancel={() => setOpen(false)}
          onReload={() => window.location.reload()}
        />
      )}
    </>
  )
}
