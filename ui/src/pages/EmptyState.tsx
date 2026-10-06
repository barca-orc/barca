import type { LucideIcon } from 'lucide-react'

interface EmptyStateProps {
  icon: LucideIcon
  title: string
  note?: string
}

export function EmptyState({ icon: Icon, title, note }: EmptyStateProps) {
  return (
    <div className="barca-empty">
      <div className="barca-empty-icon">
        <Icon size={24} />
      </div>
      <h2>{title}</h2>
      <p>{note ?? 'nothing to show yet · coming soon'}</p>
    </div>
  )
}
