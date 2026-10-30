/**
 * Model, update, view: the Elm architecture, without a framework.
 *
 * A page that owns state beyond its URL and its server data is written as
 *
 *   - a `Model` (plain data: everything the page remembers),
 *   - a `Msg` union (everything that can happen to it: a click, a keystroke, a reply),
 *   - an `Effect` union (everything it asks the outside world to do: a request, a copy,
 *     a navigation), and
 *   - `update(model, msg) → [model, effects]`, pure and exhaustively matched.
 *
 * The view is a component of the model (and of the server data, as `Remote`s) that only
 * dispatches messages. `hooks/useProgram.ts` runs the program in React and hands each effect
 * to the page's interpreter, which does the I/O and dispatches the reply as a message.
 *
 * Any screen is therefore a value: construct a model, render the view. The kit page renders
 * each page in each of its states that way, and a test replays messages without a browser.
 */

/** The next model, and what to ask the outside world to do. */
export type Next<Model, Effect> = readonly [Model, readonly Effect[]]

export type Update<Model, Msg, Effect> = (model: Model, msg: Msg) => Next<Model, Effect>

/** The view's only way to change anything. */
export type Dispatch<Msg> = (msg: Msg) => void

/** A next model with nothing to do. */
export function only<Model>(model: Model): Next<Model, never> {
  return [model, []]
}

/**
 * Fold `msgs` through `update` from `model`: the final model and every effect asked for, in
 * order. What a test of a program calls; effects are not run, only collected.
 */
export function replay<Model, Msg, Effect>(
  update: Update<Model, Msg, Effect>,
  model: Model,
  msgs: readonly Msg[],
): { model: Model; effects: Effect[] } {
  const effects: Effect[] = []
  for (const msg of msgs) {
    const [next, asked] = update(model, msg)
    model = next
    effects.push(...asked)
  }
  return { model, effects }
}
