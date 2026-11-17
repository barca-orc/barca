import { useRouteError } from 'react-router'
import { Button } from './Button'
import { EmptyNote } from './EmptyNote'

/**
 * barca · RouteError
 * A page that failed to render, inside the shell: the sidebar and the other pages stay usable,
 * the error is named, and the page can be reloaded. Never the router's bare developer screen.
 */
export function RouteError() {
  const error = useRouteError()
  const message = error instanceof Error ? error.message : String(error)
  return (
    <div className="barca-view">
      <div className="barca-view-body barca-route-error">
        <EmptyNote title="This page could not be shown">
          {message} The other pages still work. If it happens again, the browser console has the details.
        </EmptyNote>
        <Button size="sm" onClick={() => window.location.reload()}>Reload</Button>
      </div>
    </div>
  )
}
