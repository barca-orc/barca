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

// The type scale is four sizes; every other size is one of their tokens.
const TYPE_SCALE = new Set(['12px', '13px', '15px', '20px'])

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

  // Every text colour and every state hue (a glyph, and the short word it may colour) reads at
  // WCAG AA for small text, 4.5:1, on every background of its theme.
  it('keeps text and state hues at 4.5:1 on every background, in both themes', () => {
    const luminance = (color: string) => {
      const [r, g, b] = [1, 3, 5].map((i) => parseInt(color.slice(i, i + 2), 16) / 255)
        .map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)) as [number, number, number]
      return 0.2126 * r + 0.7152 * g + 0.0722 * b
    }
    const contrast = (a: string, b: string) => {
      const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number]
      return (hi + 0.05) / (lo + 0.05)
    }
    const dark = readFileSync(join(SRC, 'styles', 'tokens', 'colors.css'), 'utf8')
    const light = readFileSync(join(SRC, 'styles', 'tokens', 'theme-light.css'), 'utf8')
    // A token's hex value in a theme, following `var(--x)` references (the light theme falls
    // back to the dark file for a base value it does not redefine).
    const resolve = (sources: string[], name: string): string => {
      for (const css of sources) {
        const value = css.match(new RegExp(`--${name}:\\s*([^;]+);`))?.[1]?.trim()
        if (!value) continue
        const ref = value.match(/^var\(--([a-z0-9-]+)\)$/)
        return ref ? resolve(sources, ref[1]!) : value
      }
      throw new Error(`--${name} is not defined`)
    }
    const fg = ['text-strong', 'text-default', 'text-muted', 'text-faint', 'state-failed', 'state-running', 'state-fresh', 'state-stale', 'state-blocked', 'state-partial', 'state-unsynced', 'state-pinned', 'state-idle']
    const themes = { dark: { sources: [dark], bg: ['bg-canvas', 'bg-surface', 'bg-raised', 'bg-overlay', 'bg-inset'] }, light: { sources: [light, dark], bg: ['bg-canvas', 'bg-surface', 'bg-raised', 'bg-overlay', 'bg-inset'] } }
    for (const [theme, { sources, bg }] of Object.entries(themes)) {
      for (const f of fg) {
        for (const b of bg) {
          const [fc, bc] = [resolve(sources, f), resolve(sources, b)]
          expect(fc, `${theme} --${f}`).toMatch(/^#[0-9A-Fa-f]{6}$/)
          expect(contrast(fc, bc), `${theme} --${f} on --${b}`).toBeGreaterThanOrEqual(4.5)
        }
      }
    }
  })

  it('sets font sizes from the type scale', () => {
    const hits: string[] = []
    for (const path of css) {
      for (const [, size] of readFileSync(path, 'utf8').matchAll(/font-size:\s*([\d.]+px)/g)) hits.push(`${relative(SRC, path)}: ${size}`)
    }
    expect(hits, 'use var(--text-*) instead of a raw size').toEqual([])
  })

  it('has four type sizes: 12, 13, 15 and 20px', () => {
    const tokens = readFileSync(join(SRC, 'styles', 'tokens', 'typography.css'), 'utf8')
    const sizes = [...tokens.matchAll(/--text-[a-z0-9]+:\s*([\d.]+px)/g)].map((m) => m[1]!)
    expect(sizes.length).toBeGreaterThan(0)
    expect(sizes.filter((s) => !TYPE_SCALE.has(s))).toEqual([])
  })

  // Labels and headers are sentence case: capitals and tracking read as shouting in a dense table.
  // The graph's node boxes are redrawn separately and keep their kind label for now.
  it('sets no capitals or tracked-out labels', () => {
    const hits: string[] = []
    for (const path of css) {
      for (const [, selector] of readFileSync(path, 'utf8').matchAll(/([^{}]+)\{[^}]*text-transform:\s*uppercase/g)) {
        if (!selector!.trim().startsWith('.barca-dag-')) hits.push(`${relative(SRC, path)}: ${selector!.trim()}`)
      }
    }
    expect(hits).toEqual([])
  })

  it('has no inline pixel font sizes in components', () => {
    const hits = code.filter((p) => /fontSize:\s*['"]?\d/.test(readFileSync(p, 'utf8')))
    expect(hits.map((p) => relative(SRC, p))).toEqual([])
  })
})
