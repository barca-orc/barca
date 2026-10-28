import { useEffect, useMemo, useRef, useState } from 'react'
import { buildGraph, graphAt, layoutInput, type GraphEntry, type LayoutDir } from '@/lib/graph'
import type { LayoutPositions } from '@/lib/graphLayout'

type Graph = ReturnType<typeof buildGraph>

/**
 * A graph this big is laid out in a worker: dagre takes about 90 ms for a thousand nodes on a
 * fast machine and several times that on a slow one, all of it a frozen page on the main
 * thread. A smaller one is laid out in place, so it renders with its data as before.
 */
const IN_A_WORKER_FROM = 300

/** What a layout depends on: two lists with the same key lay out the same. */
function structureKey(assets: GraphEntry[], dir: LayoutDir): string {
  return JSON.stringify([
    dir,
    assets.map((a) => [a.id, a.kind, a.inputs, a.name, a.summary, a.outputName]),
  ])
}

/**
 * `buildGraph(assets, dir)`, laid out off the main thread for a big graph: `null` until that
 * layout arrives. A poll that returns the same graph keeps the layout it has.
 */
export function useGraphLayout(assets: GraphEntry[], dir: LayoutDir): Graph | null {
  const inPlace = assets.length < IN_A_WORKER_FROM
  const key = useMemo(() => structureKey(assets, dir), [assets, dir])
  const here = useMemo(() => (inPlace ? buildGraph(assets, dir) : null), [inPlace, assets, dir])
  const [laidOut, setLaidOut] = useState<{ key: string; graph: Graph } | null>(null)
  const worker = useRef<Worker | null>(null)
  const requests = useRef(0)
  // The newest assets for a key, read when its layout arrives.
  const latest = useRef({ assets, dir })
  useEffect(() => {
    latest.current = { assets, dir }
  })

  useEffect(() => {
    if (inPlace) return
    worker.current ??= new Worker(new URL('../lib/graphLayout.worker.ts', import.meta.url), {
      type: 'module',
    })
    const w = worker.current
    const id = ++requests.current
    const onMessage = (event: MessageEvent<{ id: number; positions: LayoutPositions }>) => {
      if (event.data.id !== id) return
      const { assets, dir } = latest.current
      setLaidOut({ key, graph: graphAt(assets, dir, event.data.positions) })
    }
    w.addEventListener('message', onMessage)
    const { assets, dir } = latest.current
    w.postMessage({ id, input: layoutInput(assets, dir) })
    return () => w.removeEventListener('message', onMessage)
  }, [inPlace, key])

  useEffect(
    () => () => {
      worker.current?.terminate()
      worker.current = null
    },
    [],
  )

  if (inPlace) return here
  return laidOut?.key === key ? laidOut.graph : null
}
