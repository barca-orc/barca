import { CauseBadge, KeyValue } from '@/components'
import { ROOT_KIND_WORD, actionWords } from '@/lib/causesView'
import type { RollupRoot } from '@/lib/rollupView'
import { shortName } from '@/lib/graph'
import type { Cause } from '@/lib/causeDisplay'
import type { Root } from '@/lib/generated/Root'

export interface CauseLineProps {
  /** The node's own id: a root about the node itself is not a link. */
  node: string
  /** The node's `cause`; `null` for a fresh node. */
  cause: Cause | null
  /** The node's `root`: the upstream fact that explains it; `null` for a fresh node. */
  root: Root | null
  /** The root as the rollup's root list has it, when that has loaded: its fact in a line and what clears it. */
  held?: RollupRoot | null
  onSelect: (id: string) => void
}

/**
 * Why the node is not fresh and what explains it: its cause in the health map's word and
 * colour, then its root, the one upstream fact that holds it (`code change in prices`), with
 * a link to the root's node and the fact in a line. Both are fields of the row; the walk that
 * finds the root is `barca-core`'s.
 */
export function CauseLine({ node, cause, root, held = null, onSelect }: CauseLineProps) {
  const action = held ? actionWords(held) : null
  if (cause === null) return null
  return (
    <KeyValue label="why">
      <span className="barca-state-line" data-cause-line={cause}>
        <CauseBadge cause={cause} />
        {root && (
          <span className="barca-cell-sub" data-root={root.id}>
            {ROOT_KIND_WORD[root.kind]}{' '}
            {root.node === node
              ? <>here</>
              : <>in <button type="button" className="barca-link" title={`Open ${root.node}`} onClick={() => onSelect(root.node)}>{shortName(root.node)}</button></>}
          </span>
        )}
      </span>
      {held && <div className="barca-cell-sub" data-root-detail>{held.detail}</div>}
      {action && (
        <div className="barca-cell-sub" data-root-action={held?.action.kind}>
          to clear it: <strong>{action.word}</strong>
          {action.command && <> <code>{action.command}</code></>}
          {action.hint && <> {action.hint}</>}
        </div>
      )}
    </KeyValue>
  )
}
