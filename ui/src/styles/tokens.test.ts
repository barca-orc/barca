/// <reference types="node" />
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

// The design tokens (styles/tokens/) are the one place a color or a type size is defined.
// Everything else refers to them, so the look can change in one place and a new screen
// cannot drift. This test is the lint for that.
const SRC = fileURLToPath(new URL('..', import.meta.url))

function files(dir: string, ext: RegExp): string[] {
  return readdirSync(dir).flatMap((name: string) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return files(path, ext)
    return ext.test(name) ? [path] : []
  })
}

const outsideTokens = (path: string) => !relative(SRC, path).startsWith(join('styles', 'tokens'))
const css = files(SRC, /\.css$/).filter(outsideTokens)
const code = files(SRC, /\.tsx?$/).filter((p) => !/generated|\.test\./.test(p))

// Sizes that sit between steps of the type scale. Before adding a size, add a token (or use
// the nearest step); do not extend this list.
const OFF_SCALE_FONT_SIZES = new Set(['9px', '12.5px', '15px', '18px', '22px'])

describe('design tokens', () => {
  it('has no raw color values outside styles/tokens/', () => {
    const raw = /#[0-9a-fA-F]{3,8}\b|\brgba?\(|\bhsla?\(/
    const hits = [...css, ...code].filter((p) => raw.test(readFileSync(p, 'utf8')))
    expect(hits.map((p) => relative(SRC, p))).toEqual([])
  })

  it('sets font sizes from the type scale', () => {
    const hits: string[] = []
    for (const path of css) {
      for (const [, size] of readFileSync(path, 'utf8').matchAll(/font-size:\s*([\d.]+px)/g)) {
        if (!OFF_SCALE_FONT_SIZES.has(size!)) hits.push(`${relative(SRC, path)}: ${size}`)
      }
    }
    expect(hits, 'use var(--text-*) instead of a raw size').toEqual([])
  })

  it('has no inline pixel font sizes in components', () => {
    const hits = code.filter((p) => /fontSize:\s*['"]?\d/.test(readFileSync(p, 'utf8')))
    expect(hits.map((p) => relative(SRC, p))).toEqual([])
  })
})
