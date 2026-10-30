import { headline } from '@/lib/health'

export interface ErrorLineProps {
  /** The whole recorded error, traceback included. */
  error: string | null | undefined
  /** The line to show, when it is not the error's own headline. */
  text?: string
  /** The panel's size: wraps, and is the largest thing on it. */
  large?: boolean
}

/** barca · ErrorLine: an error's headline, and its traceback on expand. The one way to show one. */
export function ErrorLine({ error, text, large = false }: ErrorLineProps) {
  const line = text ?? headline(error)
  const full = error?.trim() ?? ''
  if (!full.includes('\n')) {
    return <span className="barca-error-line" data-large={large || undefined} title={full || line}>{line}</span>
  }
  return (
    // Inside a table row: expanding is not selecting the row.
    <details
      className="barca-error-line"
      data-large={large || undefined}
      onClick={(event) => event.stopPropagation()}
      onKeyDown={(event) => event.stopPropagation()}
    >
      <summary title={line}>{line}</summary>
      <pre className="barca-error">{full}</pre>
    </details>
  )
}
