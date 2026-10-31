import { useEffect, useRef, type CSSProperties } from 'react'
import { useTimeZone } from '@/context/timeZone'
import { formatAbsolute, formatClock } from '@/lib/time'
import type { LogLine } from '@/lib/types'
import { injectStyles } from '@/lib/injectStyles'

export interface LogViewerProps {
  lines: LogLine[]
  /** Show a blinking cursor on the last line while a run is live. */
  live?: boolean
  showLineNumbers?: boolean
  height?: number | string
  style?: CSSProperties
}

const STYLE_ID = 'barca-logviewer-styles'
const CSS = `
  .barca-log-line {
    display: flex; align-items: baseline; gap: 10px;
    padding: 1px 14px 1px 0;
    font-family: var(--font-mono); font-size: var(--text-sm);
    line-height: 1.65;
  }
  .barca-log-line:hover { background: var(--bg-hover); }
  .barca-log-gutter {
    flex: none; width: 36px; text-align: right; padding-right: 6px;
    color: var(--text-disabled); user-select: none;
    font-size: var(--text-2xs); position: sticky; left: 0;
    background: var(--bg-inset);
  }
  .barca-log-text { flex: 1; min-width: 0; color: var(--text-default); white-space: pre-wrap; word-break: break-word; }
  .barca-log-line[data-stream="stderr"] .barca-log-text { color: var(--text-muted); }
  .barca-log-line[data-stream="stderr"] { box-shadow: inset 2px 0 0 var(--border-strong); }
  .barca-log-ts {
    flex: none; color: var(--text-disabled); user-select: none;
    font-size: var(--text-2xs); font-variant-numeric: tabular-nums;
  }
  .barca-log-cursor {
    display: inline-block; width: 7px; height: 1.05em; margin-left: 3px;
    background: var(--signal); vertical-align: text-bottom;
    animation: barca-blink 1.1s steps(1) infinite;
  }
  .barca-log-empty {
    display: flex; align-items: center; justify-content: center; height: 100%;
    font-family: var(--font-mono); font-size: var(--text-xs); color: var(--text-faint);
  }
`

/**
 * barca · LogViewer
 * Monospace log stream — the core barca surface. Renders captured lines with
 * line numbers, each line's time (`HH:MM:SS.mmm` in the reader's zone, zone named; the full
 * date on hover) and a blinking signal-green cursor while
 * live; stderr lines are muted, with a rule at their left. Auto-scrolls to the
 * bottom while the user is pinned there.
 */
export function LogViewer({
  lines,
  live = false,
  showLineNumbers = true,
  height = 320,
  style,
}: LogViewerProps) {
  injectStyles(STYLE_ID, CSS)
  const { zone } = useTimeZone()
  const scrollRef = useRef<HTMLDivElement>(null)
  const pinnedRef = useRef(true)

  useEffect(() => {
    const el = scrollRef.current
    if (el && pinnedRef.current) el.scrollTop = el.scrollHeight
  }, [lines.length])

  const onScroll = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24
  }

  return (
    <div
      className="barca-log"
      style={{
        position: 'relative',
        height,
        background: 'var(--bg-inset)',
        border: '1px solid var(--border-default)',
        borderRadius: 'var(--radius-lg)',
        overflow: 'hidden',
        ...style,
      }}
    >
      <div
        ref={scrollRef}
        onScroll={onScroll}
        style={{ height: '100%', overflow: 'auto' }}
      >
        {lines.length === 0 ? (
          <div className="barca-log-empty">no output yet</div>
        ) : (
          <div style={{ padding: '8px 0' }}>
            {lines.map((ln, i) => (
              <div key={i} className="barca-log-line" data-stream={ln.stream ?? 'stdout'}>
                {showLineNumbers && <span className="barca-log-gutter">{i + 1}</span>}
                {ln.ts && (
                  <span className="barca-log-ts" title={formatAbsolute(ln.ts, zone)}>
                    {formatClock(ln.ts, zone)}
                  </span>
                )}
                <span className="barca-log-text">
                  {ln.text}
                  {live && i === lines.length - 1 && <span className="barca-log-cursor" />}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
