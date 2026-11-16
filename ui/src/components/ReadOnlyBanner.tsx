import { Eye } from 'lucide-react'
import { useWriteControl } from '@/hooks/useWriteControl'
import { READ_ONLY_LABEL, READ_ONLY_NOTE } from '@/lib/writeAccess'

/**
 * barca · ReadOnlyNotice
 * The app's one word on a `barca serve --read-only`: quiet, one line, across the top of every
 * page. The write actions it explains are not drawn at all (`lib/writeAccess.ts`), so no button
 * needs a tooltip to say why it does nothing.
 */
export function ReadOnlyNotice() {
  return (
    <div role="note" className="barca-offline barca-readonly-banner">
      <Eye size={14} aria-hidden />
      <span><strong>{READ_ONLY_LABEL}.</strong> {READ_ONLY_NOTE}</span>
    </div>
  )
}

/** `ReadOnlyNotice`, on a server that is read-only. */
export function ReadOnlyBanner() {
  return useWriteControl() === 'hidden' ? <ReadOnlyNotice /> : null
}
