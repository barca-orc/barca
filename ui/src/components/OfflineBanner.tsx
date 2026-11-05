import { useEffect, useRef } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { WifiOff } from 'lucide-react'
import { Button } from './Button'
import { useHealth } from '@/hooks/useHealth'

/**
 * barca · OfflineBanner
 * Across the top of every page while `barca serve` does not answer: the pages keep what they
 * last loaded, and this says that it may be out of date. Gone the moment a health poll
 * succeeds again, and then every query is asked again at once, so no page waits for its own
 * next poll to drop its error.
 */
export function OfflineBanner() {
  const health = useHealth()
  const client = useQueryClient()
  const wasOffline = useRef(false)
  const offline = health.isError
  const back = !offline && health.isSuccess
  useEffect(() => {
    if (offline) wasOffline.current = true
    else if (back && wasOffline.current) {
      wasOffline.current = false
      void client.invalidateQueries()
    }
  }, [offline, back, client])
  if (!offline) return null
  return (
    <div role="alert" className="barca-offline">
      <WifiOff size={14} aria-hidden />
      <span>
        <strong>barca serve is not reachable.</strong> What is shown was loaded before the connection was lost and
        may be out of date; retrying every few seconds.
      </span>
      <Button size="sm" className="barca-offline-retry" onClick={() => void client.invalidateQueries()}>
        Retry now
      </Button>
    </div>
  )
}
