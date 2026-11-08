import type { ReactNode } from 'react'

export interface SidePanelProps {
  /** Accessible name of the panel (usually the title as text). */
  label: string
  title: ReactNode
  /** Shown beside the title: a kind tag, a status dot. */
  badge?: ReactNode
  /** A line under the title: the file, the id. Mono, one line, cut at the end. */
  subtitle?: ReactNode
  /** The whole title and subtitle, shown on hover where either is cut. */
  titleText?: string
  subtitleText?: string
  /** Inline at the end of the subtitle: its copy button. */
  subtitleAction?: ReactNode
  /** Buttons at the right of the header (close, open elsewhere). */
  actions?: ReactNode
  /** Width in px; by default 400 and less on a narrow window (styles: .barca-sidepanel). */
  width?: number
  children: ReactNode
}

/**
 * barca · SidePanel
 * The detail pane at the right of a list or graph: a header over sections. The node panel and
 * the graph inspector are both this; anything that opens beside a view should be too.
 * No card inside it: sections are spacing and a small muted label, a hairline at most.
 */
export function SidePanel({ label, title, badge, subtitle, titleText, subtitleText, subtitleAction, actions, width, children }: SidePanelProps) {
  return (
    <aside className="barca-sidepanel" aria-label={label} style={width === undefined ? undefined : { width }}>
      <header className="barca-sidepanel-head">
        <div className="barca-sidepanel-heading">
          <div className="barca-sidepanel-title">
            <h2 title={titleText}>{title}</h2>
            {badge}
          </div>
          {subtitle && (
            <div className="barca-sidepanel-sub">
              <span title={subtitleText}>{subtitle}</span>
              {subtitleAction}
            </div>
          )}
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
