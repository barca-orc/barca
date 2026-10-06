import { Activity, Book } from 'lucide-react'
import { EmptyState } from './EmptyState'

export function RunsPage() {
  return <EmptyState icon={Activity} title="Runs" note="run history · the graph is the good part" />
}

export function DocsPage() {
  return <EmptyState icon={Book} title="Docs" note="getting started · coming soon" />
}
