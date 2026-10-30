/** How many of a name's last characters stay visible: where `_00` and `_01` differ. */
const TAIL = 10

/**
 * A long name as the part that may be cut and the part that may not, so
 * `check__risk_alerts_01` truncates as `check__ri…alerts_01`, keeping what tells rows apart.
 */
export function splitMiddle(name: string): { head: string; tail: string } {
  if (name.length <= TAIL) return { head: name, tail: '' }
  return { head: name.slice(0, -TAIL), tail: name.slice(-TAIL) }
}
