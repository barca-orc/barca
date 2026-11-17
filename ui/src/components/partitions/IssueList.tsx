import { useState } from 'react'
import { Play } from 'lucide-react'
import { Button } from '@/components'
import { StatusGlyph } from '@/components/StatusGlyph'
import { keyStateDisplay, keyStateOf } from '@/lib/keyStateDisplay'
import { plural } from '@/lib/plural'
import type { KeyIssue } from '@/lib/types'

export interface IssueListProps {
  /** What is wrong and where, as the server lists it (`issues` of the overview). */
  issues: readonly KeyIssue[]
  /** The issue whose keys the table shows, if one is. */
  active: KeyIssue | null
  onShow: (issue: KeyIssue) => void
  onRun: ((issue: KeyIssue) => void) | null
  running: boolean
}

/** How many issues show before "N more". */
const SHOWN = 4

/**
 * barca · IssueList
 * What is wrong with a partitioned asset, one line per problem, most keys first: each error its
 * failed keys raised, then the keys that wait on a failed input, are stale or never ran. Each
 * says where its keys are (`region apac · day 2023-03-01 → 2023-03-20`), the fact a 10,000-key
 * grid makes you hunt for. The groups, their order and where each sits are the server's.
 */
export function IssueList({ issues, active, onShow, onRun, running }: IssueListProps) {
  const [open, setOpen] = useState(false)
  if (issues.length === 0) return null
  const shown = open ? issues : issues.slice(0, SHOWN)
  return (
    <section className="barca-issues" aria-label="What needs attention">
      <ul>
        {shown.map((issue) => {
          const isActive = active !== null && active.state === issue.state && active.error === issue.error
          const state = keyStateOf(issue.state)
          return (
            <li key={`${issue.state}:${issue.error}`} className="barca-issue" data-active={isActive || undefined}>
              <StatusGlyph glyph={state ? keyStateDisplay(state).glyph : 'unknown'} size={14} />
              <div className="barca-issue-what">
                <div className="barca-issue-head">
                  <b>{plural(issue.keys, 'key')}</b>
                  <span className={issue.error !== null ? 'barca-issue-error' : 'barca-issue-state'} title={issue.title}>{issue.title}</span>
                </div>
                {issue.places.length > 0 && (
                  <div className="barca-issue-where">
                    {issue.places.map((p) => <span key={p.dimension}><span className="barca-issue-dim">{p.dimension}</span> {p.words}</span>)}
                  </div>
                )}
              </div>
              <div className="barca-issue-actions">
                <Button variant="ghost" size="sm" onClick={() => onShow(issue)} aria-pressed={isActive}>
                  {isActive ? 'Showing' : 'Show'}
                </Button>
                {onRun && (
                  <Button size="sm" iconLeft={<Play size={11} />} loading={running} disabled={issue.selection === null} onClick={() => onRun(issue)}
                    title={issue.selection === null ? 'Too many scattered keys to name in one run: show them and narrow the filter' : 'Compute these keys again'}>
                    Run
                  </Button>
                )}
              </div>
            </li>
          )
        })}
      </ul>
      {issues.length > SHOWN && (
        <button type="button" className="barca-link barca-issues-more" onClick={() => setOpen(!open)}>
          {open ? 'Fewer' : `${issues.length - SHOWN} more`}
        </button>
      )}
    </section>
  )
}
