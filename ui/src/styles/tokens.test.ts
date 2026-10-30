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

  it('keeps actions, links and selection off the status colours', () => {
    for (const theme of ['colors.css', 'theme-light.css']) {
      const tokens = readFileSync(join(SRC, 'styles', 'tokens', theme), 'utf8')
      for (const name of ['signal', 'text-link', 'focus-ring', 'bg-selected', 'border-accent']) {
        const value = tokens.match(new RegExp(`--${name}:\\s*([^;]+);`))?.[1]
        expect(value, `${theme} --${name}`).toBeDefined()
        expect(value, `${theme} --${name}`).not.toMatch(/green|status-|cyan/)
      }
    }
  })

  it('keeps faint text readable on every dark background', () => {
    const tokens = readFileSync(join(SRC, 'styles', 'tokens', 'colors.css'), 'utf8')
    const hex = (name: string) => tokens.match(new RegExp(`--${name}:\\s*(#[0-9A-Fa-f]{6})`))![1]!
    const luminance = (color: string) => {
      const [r, g, b] = [1, 3, 5].map((i) => parseInt(color.slice(i, i + 2), 16) / 255)
        .map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)) as [number, number, number]
      return 0.2126 * r + 0.7152 * g + 0.0722 * b
    }
    for (const bg of ['c-ink-900', 'c-ink-850', 'c-ink-800', 'c-ink-700', 'c-ink-600']) {
      const ratio = (luminance(hex('c-ink-150')) + 0.05) / (luminance(hex(bg)) + 0.05)
      expect(ratio, bg).toBeGreaterThanOrEqual(4.5)
    }
    expect(tokens).toMatch(/--text-faint:\s*var\(--c-ink-150\)/)
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
