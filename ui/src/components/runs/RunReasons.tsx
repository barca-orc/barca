import { Link } from 'react-router'
import { match } from 'ts-pattern'
import { shortName } from '@/lib/graph'
import { planExplanation } from '@/lib/runExplain'
import type { PlanReasons } from '@/lib/types'
import { MiddleName } from '../MiddleName'

/** How many subjects a category lists before "and N more". */
const ROWS_SHOWN = 6

/**
 * Why a run executes what it does (`plan_reasons`, issue 925): by category, each subject with its
 * steps and nodes and a bar of its share of the run's steps, linking to the first node it ran.
 */
export function RunReasons({ plan, live }: { plan: PlanReasons | null; live: boolean }) {
  const explanation = planExplanation(plan, live)
  return match(explanation)
    .with({ kind: 'none' }, ({ note }) => <p className="barca-note barca-muted">{note}</p>)
    .with({ kind: 'reasons' }, ({ headline, groups, omitted, partial }) => (
      <div className="barca-run-reasons">
        <p className="barca-run-reasons-head">{headline}</p>
        {groups.map((group) => (
          <div key={group.category} className="barca-run-reason-group">
            <span className="barca-run-reason-label">{group.label}</span>
            <ul>
              {group.rows.slice(0, ROWS_SHOWN).map((row) => (
                <li key={row.key} className="barca-run-reason">
                  <span className="barca-run-reason-bar" aria-hidden><span style={{ width: `${Math.max(2, row.share * 100)}%` }} /></span>
                  <span className="barca-run-reason-counts">{row.counts}</span>
                  <span className="barca-run-reason-subject">
                    {row.subject
                      ? <code><MiddleName name={row.subject} /></code>
                      : <Link to={`/assets?node=${encodeURIComponent(row.example)}`} title={`The first node it ran: ${row.example}`}>{shortName(row.example)}</Link>}
                  </span>
                </li>
              ))}
              {group.rows.length > ROWS_SHOWN && <li className="barca-note barca-muted">and {group.rows.length - ROWS_SHOWN} more</li>}
            </ul>
          </div>
        ))}
        {omitted > 0 && <p className="barca-note barca-muted">{omitted.toLocaleString()} smaller reasons are not listed; barca history RUN_ID --json has the largest 64.</p>}
        {partial && <p className="barca-note barca-muted">Phases not decided yet add their reasons as the run reaches them.</p>}
      </div>
    ))
    .exhaustive()
}
