import { useState } from 'react'
import { Check, Copy } from 'lucide-react'
import { Age } from './Age'
import { IconButton } from './IconButton'
import { StatusGlyph } from './StatusGlyph'
import { causeDisplay, leadingPart, mapPartColor } from '@/lib/causeDisplay'
import { ROOT_KIND_WORD, actionWords, explained, heldCauses, heldGap, heldLine, type Held, type Signal, type Todo } from '@/lib/causesView'
import { plural } from '@/lib/plural'
import type { RollupEntry } from '@/lib/rollupView'

export interface CausesListProps {
  /** The to-do list of the scope (`todos`): one entry per root, in the order served. */
  todos: readonly Todo[]
  /** The scope's entry of the rollup: what its roots hold is out of its `not_fresh`. */
  scope: Pick<RollupEntry, 'held_by' | 'not_fresh'>
  /** A node's name as this page shows it. */
  name: (id: string) => string
  /** Open a node: the node a root is about. */
  onOpen: (id: string) => void
  /** The root whose nodes are lit on the map and in the table, by id. */
  selected?: string | null
  /** Light what a root holds; null to clear. */
  onSelect?: (root: string | null) => void
  /** Since when a failing node has been failing (its row's `failing_since`), when known. */
  failingSince?: (node: string) => string | null
  /** How many entries to list before "N more". */
  limit?: number
  /** The clock ages are measured against. */
  nowMs: number
}

/** A command with a button that copies it. */
function Command({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  const copy = () => {
    void navigator.clipboard?.writeText(text).then(() => {
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1500)
    })
  }
  return (
    <span className="barca-todo-command">
      <code title={text}>{text}</code>
      <IconButton size="sm" label={copied ? 'Copied' : 'Copy command'} onClick={(event) => { event.stopPropagation(); copy() }}>
        {copied ? <Check size={12} /> : <Copy size={12} />}
      </IconButton>
    </span>
  )
}

/** The share of the scope's not-fresh nodes each root holds, as one bar: the head of the list first. */
function ExplainedBar({ todos, scope, head }: { todos: readonly Todo[]; scope: CausesListProps['scope']; head: number }) {
  const told = explained(scope, head)
  const roots = scope.held_by.length
  return (
    <div className="barca-todo-explained">
      <div className="barca-todo-explained-bar" role="img" aria-label="Nodes held, by root">
        {scope.held_by.map((held, i) => (
          // Width is geometry: this root's nodes out of the scope's not-fresh nodes, both served.
          <span key={held.root} data-head={i < head || undefined} style={{ flexGrow: held.nodes, background: mapPartColor(leadingPart(held.causes)) }} title={`${plural(held.nodes, 'node')}`} />
        ))}
      </div>
      <span className="barca-todo-explained-words">
        {told
          ? <>The first {told.roots} roots hold <strong>{told.nodes.toLocaleString()} of {told.of.toLocaleString()}</strong> nodes that are not fresh</>
          : <>{plural(roots, 'root')} {roots === 1 ? 'holds' : 'hold'} {scope.not_fresh === 1 ? 'the 1 node that is' : <>all <strong>{scope.not_fresh.toLocaleString()}</strong> nodes that are</>} not fresh</>}
        {todos.length !== roots && <> · {plural(todos.length, 'entry', 'entries')}, failures with one error together</>}
      </span>
    </div>
  )
}

/**
 * barca · CausesList
 * The to-do list of a scope: what holds the nodes that are not fresh, one entry per root, ranked
 * as the server ranks them (roots holding nodes with no estimate first, the most nodes first).
 * Each entry says what it is (the root's kind and node), why (the fact in a line), what it holds
 * here (nodes and assets, by cause), the gap it holds or how many nodes have no estimate, and
 * what to do: the action, its command with a copy button, and the compute it takes. Every
 * not-fresh node of the scope is under one entry; "N more" opens the rest in place. Clicking an
 * entry lights what it holds on the map and in the table; its node's name opens the node.
 */
export function CausesList({ todos, scope, name, onOpen, selected = null, onSelect, failingSince, limit = 3, nowMs }: CausesListProps) {
  const [all, setAll] = useState(false)
  if (todos.length === 0)
    return (
      <div className="barca-causes" data-empty>
        <StatusGlyph glyph="fresh" />
        <span>Every node in this scope is fresh.</span>
      </div>
    )
  const shown = all ? todos : todos.slice(0, limit)
  const more = todos.length - shown.length
  const select = (id: string) => onSelect?.(selected === id ? null : id)
  const title = ({ root }: Held) => `${ROOT_KIND_WORD[root.kind]}: ${name(root.node)}`
  return (
    <div className="barca-todos">
      <ExplainedBar todos={todos} scope={scope} head={limit} />
      <ol className="barca-causes" aria-label="What holds the nodes that are not fresh, as the server ranks it">
        {shown.map((todo, i) => {
          const { root, held, same } = todo
          const lead = leadingPart(held.causes)
          const action = actionWords(root)
          // A node that is not failing has no `failing_since`: the row says, nothing is decided here.
          const since = failingSince?.(root.node) ?? null
          const gap = heldGap(todo)
          return (
            <li
              key={root.id}
              className="barca-todo"
              data-kind={root.kind}
              data-root={root.id}
              data-alarm={(lead !== 'fresh' && causeDisplay(lead).alarm) || undefined}
              data-selected={selected === root.id || undefined}
              onClick={() => select(root.id)}
            >
              <span className="barca-todo-rank">{i + 1}</span>
              {lead !== 'fresh' && <StatusGlyph glyph={causeDisplay(lead).glyph} />}
              <div className="barca-todo-body">
                <div className="barca-todo-head">
                  <button
                    type="button"
                    className="barca-todo-what"
                    aria-pressed={selected === root.id}
                    title="Show what it holds on the map and in the table"
                    onClick={(event) => { event.stopPropagation(); select(root.id) }}
                  >
                    {ROOT_KIND_WORD[root.kind]}
                  </button>
                  <button
                    type="button"
                    className="barca-todo-node"
                    title={`Open ${root.node}`}
                    onClick={(event) => { event.stopPropagation(); onOpen(root.node) }}
                  >
                    {name(root.node)}
                  </button>
                  <span className="barca-todo-why" title={root.detail}>{root.detail}</span>
                  <span className="barca-todo-held">{heldLine(todo)}</span>
                </div>
                <span className="barca-todo-meta">
                  {heldCauses(todo)}
                  {gap && <> · {gap}</>}
                  {since && <> · failing since <Age at={since} nowMs={nowMs} /></>}
                </span>
                <span className="barca-todo-do">
                  <span className="barca-todo-do-label">do</span>
                  <strong>{action.word}</strong>
                  {action.command && <Command text={action.command} />}
                  {action.hint && <span className="barca-todo-hint">{action.hint}</span>}
                  {action.cost && <span className="barca-todo-cost">{action.cost}</span>}
                </span>
                {same.length > 0 && (
                  <details className="barca-todo-same" onClick={(event) => event.stopPropagation()}>
                    <summary>{plural(same.length, 'more failure')} with this error</summary>
                    <ul>
                      {same.map((other) => (
                        <li key={other.root.id} data-root={other.root.id}>
                          <button type="button" className="barca-todo-node" title={`Open ${other.root.node}`} onClick={() => onOpen(other.root.node)}>{name(other.root.node)}</button>
                          <span className="barca-todo-held">{heldLine(other)}</span>
                          {actionWords(other.root).command && <Command text={actionWords(other.root).command!} />}
                          <button type="button" className="barca-clear" aria-pressed={selected === other.root.id} aria-label={`Show what ${title(other)} holds`} onClick={() => select(other.root.id)}>show</button>
                        </li>
                      ))}
                    </ul>
                  </details>
                )}
              </div>
            </li>
          )
        })}
      </ol>
      {(more > 0 || all) && todos.length > limit && (
        <button type="button" className="barca-todo-more" aria-expanded={all} onClick={() => setAll(!all)}>
          {all ? 'Show the first ' + limit : `${more} more: show all ${todos.length}`}
        </button>
      )}
    </div>
  )
}

export interface DataSignalsProps {
  /** The data signals in scope (`signals`): not roots, so listed apart from the to-do list. */
  signals: readonly Signal[]
  name: (id: string) => string
  onOpen: (id: string) => void
}

/**
 * barca · DataSignals
 * What the data says, apart from freshness: output that looks wrong (empty, rows changed) and
 * nodes older than an input. These are signals about a node, not roots: a node here may also be
 * held by a root above, where it is counted.
 */
export function DataSignals({ signals, name, onOpen }: DataSignalsProps) {
  if (signals.length === 0) return null
  return (
    <details className="barca-data-signals">
      <summary>Data signals · {signals.length.toLocaleString()}</summary>
      <ul>
        {signals.map((signal) => (
          <li key={`${signal.node}:${signal.text}`}>
            <StatusGlyph glyph={signal.glyph} size={12} />
            <button type="button" className="barca-todo-node" onClick={() => onOpen(signal.node)}>{name(signal.node)}</button>
            <span>{signal.text}</span>
          </li>
        ))}
      </ul>
    </details>
  )
}
