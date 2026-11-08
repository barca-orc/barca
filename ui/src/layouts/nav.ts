import { Activity, Layers, Clock, Play, Radar, type LucideIcon } from 'lucide-react'

export type NavId = 'assets' | 'runs' | 'schedules' | 'tasks' | 'sensors'

interface NavItem {
  id: NavId
  label: string
  path: string
  icon: LucideIcon
}

/** The app's sections, in the sidebar's order. */
export const NAV_ITEMS: NavItem[] = [
  { id: 'assets', label: 'Assets', path: '/assets', icon: Layers },
  { id: 'runs', label: 'Runs', path: '/runs', icon: Activity },
  { id: 'schedules', label: 'Schedules', path: '/schedules', icon: Clock },
  { id: 'tasks', label: 'Tasks', path: '/tasks', icon: Play },
  { id: 'sensors', label: 'Sensors', path: '/sensors', icon: Radar },
]
