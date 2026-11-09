import { useState } from 'react'
import { Play } from 'lucide-react'
import { Button } from '@/components'
import { StatusGlyph } from '@/components/StatusGlyph'
import { plural } from '@/lib/plural'
import { spreadWords, type IssueGroup } from '@/lib/partitionGrid'

export interface IssueListProps {
  issues: readonly IssueGroup[]
  /** The issue whose keys the table shows, if one is. */
  active: IssueGroup | null
  onShow: (issue: IssueGroup) => void
  onRun: ((issue: IssueGroup) => void) | null
  running: boolean
}

/** How many issues show before "N more". */
const SHOWN = 4

const GLYPH = { error: 'failed', blocked: 'blocked', stale: 'stale', never_run: 'never_run' } as const

/**
 * barca · IssueList
 * What is wrong with a partitioned asset, one line per problem, most keys first: each error its
 * failed keys raised, then the keys that wait on a failed input, are stale or never ran. Each
 * says where its keys are (`region apac · day 2023-03-01 → 2023-03-20`), the fact a 10,000-key
 * grid makes you hunt for.
 */
export function IssueList({ issues, active, onShow, onRun, running }: IssueListProps) {
  const [open, setOpen] = useState(false)
  if (issues.length === 0) return null
  const shown = open ? issues : issues.slice(0, SHOWN)
  return (
    <section className="barca-issues" aria-label="What needs attention">
      <ul>
        {shown.map((issue) => {
          const where = issue.spread
            .map((s) => ({ name: s.name, words: spreadWords(s) }))
            .filter((w) => w.words !== null)
          const isActive = active !== null && active.kind === issue.kind && active.error === issue.error
          return (
            <li key={`${issue.kind}:${issue.error}`} className="barca-issue" data-active={isActive || undefined}>
              <StatusGlyph glyph={GLYPH[issue.kind]} size={14} />
              <div className="barca-issue-what">
                <div className="barca-issue-head">
                  <b>{plural(issue.cells.length, 'key')}</b>
                  <span className={issue.kind === 'error' ? 'barca-issue-error' : 'barca-issue-state'} title={issue.title}>{issue.title}</span>
                </div>
                {where.length > 0 && (
                  <div className="barca-issue-where">
                    {where.map((w) => <span key={w.name}><span className="barca-issue-dim">{w.name}</span> {w.words}</span>)}
                  </div>
                )}
              </div>
              <div className="barca-issue-actions">
                <Button variant="ghost" size="sm" onClick={() => onShow(issue)} aria-pressed={isActive}>
                  {isActive ? 'Showing' : 'Show'}
                </Button>
                {onRun && (
                  <Button size="sm" iconLeft={<Play size={11} />} loading={running} onClick={() => onRun(issue)} title="Compute these keys again">
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
