import { GitBranch, Activity, Layers, Clock, Book, type LucideIcon } from 'lucide-react'

export interface NavItem {
  id: string
  label: string
  path: string
  icon: LucideIcon
}

export const NAV_ITEMS: NavItem[] = [
  { id: 'assets', label: 'Assets', path: '/assets', icon: Layers },
  { id: 'graph', label: 'Graph', path: '/graph', icon: GitBranch },
  { id: 'runs', label: 'Runs', path: '/runs', icon: Activity },
  { id: 'schedules', label: 'Schedules', path: '/schedules', icon: Clock },
  { id: 'docs', label: 'Docs', path: '/docs', icon: Book },
]
