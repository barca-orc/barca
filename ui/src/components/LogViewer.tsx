import { useEffect, useMemo, useRef, useState, type CSSProperties } from 'react'
import { Check, Copy, Download, X } from 'lucide-react'
import { useTimeZone } from '@/context/timeZone'
import { formatAbsolute, formatClock } from '@/lib/time'
import { filterLogs, highlightParts, isErrorLine, logsText } from '@/lib/runRecovery'
import type { LogLine } from '@/lib/types'
import { Button } from './Button'
import { Chip } from './Chip'
import { IconButton } from './IconButton'
import { SearchInput } from './SearchInput'

export interface LogViewerProps {
  lines: LogLine[]
  /** Show a blinking cursor on the last line while a run is live. */
  live?: boolean
  showLineNumbers?: boolean
  height?: number | string
  style?: CSSProperties
  /**
   * The step the lines are filtered to (its own lines, or every key's for a node id without
   * one); `null` shows every step. Controlled by the caller, so a step list can set it.
   */
  step?: string | null
  onStepChange?: (step: string | null) => void
  /** Show the toolbar: search, errors only, jump to first error, copy and download. */
  tools?: boolean
  /** The download's file name. */
  downloadName?: string
}

/**
 * barca · LogViewer
 * Monospace log stream — the core barca surface. Renders captured lines with
 * line numbers, each line's time (`HH:MM:SS.mmm` in the reader's zone, zone named; the full
 * date on hover) and a blinking signal-green cursor while
 * live; stderr lines are muted, with a rule at their left. Auto-scrolls to the
 * bottom while the user is pinned there. With `tools`, a toolbar filters the lines by step,
 * text (matches highlighted) and errors (stderr and traceback lines), jumps to the first
 * error, and copies or downloads the lines shown.
 */
export function LogViewer({
  lines,
  live = false,
  showLineNumbers = true,
  height = 320,
  style,
  step = null,
  onStepChange,
  tools = false,
  downloadName = 'barca-logs.txt',
}: LogViewerProps) {
  const { zone } = useTimeZone()
  const scrollRef = useRef<HTMLDivElement>(null)
  const pinnedRef = useRef(true)
  const [query, setQuery] = useState('')
  const [errorsOnly, setErrorsOnly] = useState(false)
  const [copied, setCopied] = useState(false)
  const shown = useMemo(() => filterLogs(lines, { step, query, errorsOnly }), [lines, step, query, errorsOnly])
  const firstError = shown.findIndex(({ line }) => isErrorLine(line))

  useEffect(() => {
    const el = scrollRef.current
    if (el && pinnedRef.current) el.scrollTop = el.scrollHeight
  }, [shown.length])

  const onScroll = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24
  }

  const jumpToError = () => {
    const row = scrollRef.current?.querySelector<HTMLElement>(`[data-index="${firstError}"]`)
    if (!row || !scrollRef.current) return
    pinnedRef.current = false
    scrollRef.current.scrollTop = row.offsetTop - 8
  }
  const text = () => logsText(shown.map(({ line }) => line))
  const copy = () => {
    void navigator.clipboard?.writeText(text()).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }
  const download = () => {
    const url = URL.createObjectURL(new Blob([text()], { type: 'text/plain' }))
    const a = document.createElement('a')
    a.href = url
    a.download = downloadName
    a.click()
    URL.revokeObjectURL(url)
  }

  return <>
    {tools && <div className="barca-log-tools">
      <SearchInput placeholder="Search logs" value={query} onChange={setQuery} />
      <Chip pressed={errorsOnly} onClick={() => setErrorsOnly(!errorsOnly)}>Errors only</Chip>
      {step !== null && <Chip pressed onClick={() => onStepChange?.(null)}>step {step} <X size={12} aria-label="Show every step" /></Chip>}
      <Button size="sm" variant="ghost" disabled={firstError < 0} onClick={jumpToError}>First error</Button>
      {/* Copy and Download as icons, named in their tooltips: the tools stay one line at a panel's width. */}
      <IconButton size="sm" label={copied ? 'Copied' : 'Copy logs'} disabled={shown.length === 0} onClick={copy}>{copied ? <Check size={14} /> : <Copy size={14} />}</IconButton>
      <IconButton size="sm" label="Download logs" disabled={shown.length === 0} onClick={download}><Download size={14} /></IconButton>
      {shown.length !== lines.length && <span className="barca-count">{shown.length.toLocaleString()} of {lines.length.toLocaleString()} lines</span>}
    </div>}
    <div className="barca-log" style={{ height, ...style }}>
      <div ref={scrollRef} onScroll={onScroll} className="barca-log-scroll">
        {shown.length === 0 ? (
          <div className="barca-log-empty">{lines.length === 0 ? 'no output yet' : 'no line matches'}</div>
        ) : (
          <div className="barca-log-lines">
            {shown.map(({ line: ln, number }, i) => (
              <div key={number} data-index={i} className="barca-log-line" data-stream={ln.stream ?? 'stdout'} data-error={isErrorLine(ln) || undefined}>
                {showLineNumbers && <span className="barca-log-gutter">{number}</span>}
                {ln.ts && (
                  <span className="barca-log-ts" title={formatAbsolute(ln.ts, zone)}>
                    {formatClock(ln.ts, zone)}
                  </span>
                )}
                {tools && step === null && ln.nodeId && (
                  <button type="button" className="barca-log-step" title={`Show only ${ln.nodeId}`} onClick={() => onStepChange?.(ln.nodeId)}>
                    {shortStep(ln.nodeId)}
                  </button>
                )}
                <span className="barca-log-text">
                  {highlightParts(ln.text, query).map((part, j) => part.match ? <mark key={j}>{part.text}</mark> : part.text)}
                  {live && i === shown.length - 1 && <span className="barca-log-cursor" />}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  </>
}

/** `a/b.py:regional[region=amer]` → `regional[region=amer]`. */
function shortStep(id: string): string {
  const colon = id.lastIndexOf(':', id.indexOf('[') < 0 ? id.length : id.indexOf('['))
  return colon < 0 ? id : id.slice(colon + 1)
}
