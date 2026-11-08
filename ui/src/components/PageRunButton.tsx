import { useState } from 'react'
import { Play } from 'lucide-react'
import { Button } from './Button'
import { ConfirmDialog } from './ConfirmDialog'

/** What a page's toolbar runs (`useTopbarRun`'s `scopeRun`). */
export interface PageRun {
  onRun: () => void
  /** What it runs and how much: the accessible name, and the words when there is room. */
  label: string
  /** The words on the button: what it runs, without the size (`Run all`, `Run billing.py`). */
  short: string
  disabled: boolean
  loading: boolean
  title?: string
  /** Asked before `onRun`; null runs at once. */
  confirm: { title: string; body: string; action: string } | null
}

/**
 * barca · PageRunButton — the Run at the end of a page's toolbar: what is open, the pipeline or
 * the whole project. Quiet (the node panel's Run is the view's one filled button); the whole
 * project asks first. It says what it runs in two words; how much is in its name and tooltip,
 * so its width never changes when the nodes load.
 */
export function PageRunButton({ run }: { run: PageRun | null | undefined }) {
  const [confirming, setConfirming] = useState(false)
  if (!run) return null
  return (
    <>
      <Button
        variant="neutral"
        size="sm"
        className="barca-page-run"
        iconLeft={<Play size={12} />}
        aria-label={run.label}
        title={run.title}
        loading={run.loading}
        disabled={run.disabled}
        onClick={() => (run.confirm ? setConfirming(true) : run.onRun())}
      >
        {run.short}
      </Button>
      {run.confirm && confirming && (
        <ConfirmDialog
          {...run.confirm}
          onCancel={() => setConfirming(false)}
          onConfirm={() => {
            setConfirming(false)
            run.onRun()
          }}
        />
      )}
    </>
  )
}
