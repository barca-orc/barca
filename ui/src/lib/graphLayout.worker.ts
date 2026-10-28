import { layout, type LayoutInput } from './graphLayout'

/** Lays out graphs off the main thread (`useGraphLayout`): one answer per request, by id. */
self.onmessage = (event: MessageEvent<{ id: number; input: LayoutInput }>) => {
  const { id, input } = event.data
  self.postMessage({ id, positions: layout(input) })
}
