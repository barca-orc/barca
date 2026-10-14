import type { ReactNode } from 'react'

export interface SidePanelProps {
  /** Accessible name of the panel (usually the title as text). */
  label: string
  title: ReactNode
  /** Shown beside the title: a kind tag, a status dot. */
  badge?: ReactNode
  /** A line under the title: the file, the id. */
  subtitle?: ReactNode
  /** Buttons at the right of the header (close, open elsewhere). */
  actions?: ReactNode
  /** Width in px. */
  width?: number
  children: ReactNode
}

/**
 * barca · SidePanel
 * The detail pane at the right of a list or graph: a header over sections. The node panel and
 * the graph inspector are both this; anything that opens beside a view should be too.
 */
export function SidePanel({ label, title, badge, subtitle, actions, width = 440, children }: SidePanelProps) {
  return (
    <aside className="barca-sidepanel" aria-label={label} style={{ width }}>
      <header className="barca-sidepanel-head">
        <div style={{ minWidth: 0 }}>
          <div className="barca-sidepanel-title">
            <h2>{title}</h2>
            {badge}
          </div>
          {subtitle && <div className="barca-cell-sub">{subtitle}</div>}
        </div>
        {actions && <div className="barca-sidepanel-actions">{actions}</div>}
      </header>
      {children}
    </aside>
  )
}

/** barca · Section — a titled group inside a `SidePanel`. */
export function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="barca-section">
      <h3>{title}</h3>
      {children}
    </section>
  )
}

/** barca · KeyValue — one labelled row in a `Section`. */
export function KeyValue({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="barca-kv">
      <span>{label}</span>
      <span>{children}</span>
    </div>
  )
}
