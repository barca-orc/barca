import { match } from 'ts-pattern'
import { Age } from './Age'
import { StatusGlyph } from './StatusGlyph'
import { causeImpact, heldImpact, type Cause } from '@/lib/causesView'

export interface CausesListProps {
  causes: readonly Cause[]
  /** A node's name as this page shows it. */
  name: (id: string) => string
  /** Open a node: the failing node, or the node a signal is about. */
  onOpen: (id: string) => void
  /** How many causes to list before "N more". */
  limit?: number
  /** Open the full failures list. */
  onShowAll?: () => void
  /** The clock ages are measured against. */
  nowMs: number
}

/**
 * barca · CausesList
 * What is wrong, one entry per root as the rollup serves them (the failure holding the most
 * nodes first): a failure with how many nodes it holds (not as rows of their own), then output
 * that looks wrong, then what is behind. Each entry opens the node it is about.
 */
export function CausesList({ causes, name, onOpen, limit = 6, onShowAll, nowMs }: CausesListProps) {
  if (causes.length === 0)
    return (
      <div className="barca-causes" data-empty>
        <StatusGlyph glyph="fresh" />
        <span>Nothing is failing, blocked or behind in this scope.</span>
      </div>
    )
  const shown = causes.slice(0, limit)
  const more = causes.length - shown.length
  return (
    <ol className="barca-causes" aria-label="Causes, the most nodes held first">
      {shown.map((cause, i) =>
        match(cause)
          .with({ kind: 'failure' }, (c) => {
            const first = c.node
            const others = c.others
            return (
              <li key={`f:${i}`} className="barca-cause" data-kind="failure">
                <StatusGlyph glyph="failed" />
                <button type="button" className="barca-cause-body" onClick={() => onOpen(first)}>
                  <span className="barca-cause-headline" title={c.headline}>{c.headline}</span>
                  <span className="barca-cause-meta">
                    <span className="barca-cause-name">{name(first)}{others > 0 ? ` +${others} more` : ''}</span>
                    {causeImpact(c) && <>{' · '}{causeImpact(c)}</>}
                    {c.failingSince && <> · failing since <Age at={c.failingSince} nowMs={nowMs} /></>}
                  </span>
                </button>
              </li>
            )
          })
          .with({ kind: 'held' }, (c) => (
            <li key={`h:${c.what}:${c.node}`} className="barca-cause" data-kind="held">
              <StatusGlyph glyph="stale" />
              <button type="button" className="barca-cause-body" onClick={() => onOpen(c.node)}>
                <span className="barca-cause-headline" title={c.detail}>{c.detail}</span>
                <span className="barca-cause-meta">
                  <span className="barca-cause-name">{c.what}: {name(c.node)}</span>
                  {' · '}{heldImpact(c)}
                  {' · '}{c.action}{c.command && <>: <code>{c.command}</code></>}
                </span>
              </button>
            </li>
          ))
          .with({ kind: 'suspect' }, (c) => (
            <li key={`s:${c.node}`} className="barca-cause" data-kind="suspect">
              <StatusGlyph glyph="partial" />
              <button type="button" className="barca-cause-body" onClick={() => onOpen(c.node)}>
                <span className="barca-cause-headline">Suspect data: {c.detail}</span>
                <span className="barca-cause-meta"><span className="barca-cause-name">{name(c.node)}</span></span>
              </button>
            </li>
          ))
          .with({ kind: 'behind' }, (c) => (
            <li key={`b:${c.node}`} className="barca-cause" data-kind="behind">
              <StatusGlyph glyph="stale" />
              <button type="button" className="barca-cause-body" onClick={() => onOpen(c.node)}>
                <span className="barca-cause-headline">{c.detail}</span>
                <span className="barca-cause-meta"><span className="barca-cause-name">{name(c.node)}</span></span>
              </button>
            </li>
          ))
          .exhaustive(),
      )}
      {more > 0 && (
        <li className="barca-cause-more">
          {onShowAll ? <button type="button" onClick={onShowAll}>{more} more {more === 1 ? 'cause' : 'causes'}</button> : `${more} more`}
        </li>
      )}
    </ol>
  )
}
