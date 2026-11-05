import { useEffect, useRef, type CSSProperties } from 'react'
import { useTimeZone } from '@/context/timeZone'
import { formatAbsolute, formatClock } from '@/lib/time'
import type { LogLine } from '@/lib/types'

export interface LogViewerProps {
  lines: LogLine[]
  /** Show a blinking cursor on the last line while a run is live. */
  live?: boolean
  showLineNumbers?: boolean
  height?: number | string
  style?: CSSProperties
}


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
    <div className="barca-log" style={{ height, ...style }}>
      <div ref={scrollRef} onScroll={onScroll} className="barca-log-scroll">
        {lines.length === 0 ? (
          <div className="barca-log-empty">no output yet</div>
        ) : (
          <div className="barca-log-lines">
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
