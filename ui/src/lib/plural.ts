/** `1 run`, `2 runs`, `1,234 keys`: a count and its noun, in the right number. */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString()} ${n === 1 ? one : many}`
}
