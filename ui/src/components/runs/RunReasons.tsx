import { Link } from 'react-router'
import type { WhyRan } from '@/lib/types'
import { MiddleName } from '../MiddleName'

/** How many subjects a category lists before "and N more". */
const ROWS_SHOWN = 6

/**
 * Why a run executes what it does (`explanation.why`, which the server computes): by reason, the
 * nodes and steps that ran for it with a bar of their share of the run's steps, and what
 * changed or an example node to open.
 */
export function RunReasons({ why }: { why: WhyRan }) {
  if (why.groups.length === 0) return <p className="barca-note barca-muted">{why.headline}</p>
  return (
    <div className="barca-run-reasons">
      <p className="barca-run-reasons-head">{why.headline}</p>
      {why.groups.map((group) => (
        <div key={group.category} className="barca-run-reason-group">
          <span className="barca-run-reason-label">{group.label}</span>
          <ul>
            {group.rows.slice(0, ROWS_SHOWN).map((row) => (
              <li key={`${row.subject ?? ''}:${row.example}`} className="barca-run-reason">
                <span className="barca-run-reason-bar" aria-hidden><span style={{ width: `${Math.max(2, row.share * 100)}%` }} /></span>
                <span className="barca-run-reason-counts">{row.counts}</span>
                <span className="barca-run-reason-subject">
                  {row.subject
                    ? <code><MiddleName name={row.subject} /></code>
                    : <Link to={`/assets?node=${encodeURIComponent(row.example)}`} title={`The first node it ran: ${row.example}`}>{row.example_text}</Link>}
                </span>
              </li>
            ))}
            {group.rows.length > ROWS_SHOWN && <li className="barca-note barca-muted">and {group.rows.length - ROWS_SHOWN} more</li>}
          </ul>
        </div>
      ))}
      {why.omitted > 0 && <p className="barca-note barca-muted">{why.omitted.toLocaleString()} smaller reasons are not listed; barca history RUN_ID --json has the largest 64.</p>}
      {why.partial && <p className="barca-note barca-muted">Phases not decided yet add their reasons as the run reaches them.</p>}
    </div>
  )
}
