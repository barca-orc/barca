import { Fragment } from 'react'
import { headline, pathBreaks, relativeTraceback } from '@/lib/health'

export interface ErrorLineProps {
  /** The whole recorded error, traceback included. */
  error: string | null | undefined
  /** The line to show, when it is not the error's own headline. */
  text?: string
  /** The panel's size: wraps, and leads its section. */
  large?: boolean
  /** One line that never expands (a table cell): the row opens the panel that shows the rest. */
  inline?: boolean
  /** The project root, to show traceback paths relative to it. */
  root?: string | null
}

/** barca · ErrorLine: an error's headline, and its traceback on expand. The one way to show one. */
export function ErrorLine({ error, text, large = false, inline = false, root }: ErrorLineProps) {
  const line = text ?? headline(error)
  const full = error?.trim() ?? ''
  if (inline || !full.includes('\n')) {
    return <span className="barca-error-line" data-large={large || undefined} title={full || line}>{line}</span>
  }
  return (
    <details className="barca-error-line" data-large={large || undefined}>
      <summary title={line}>{line}</summary>
      <pre className="barca-error barca-traceback">
        {pathBreaks(relativeTraceback(full, root)).map((piece, i) => <Fragment key={i}>{piece}<wbr /></Fragment>)}
      </pre>
    </details>
  )
}
