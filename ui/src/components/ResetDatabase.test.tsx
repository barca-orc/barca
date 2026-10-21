import { describe, it, expect } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { ResetDatabaseDialog, type ResetPhase } from './ResetDatabase'

const noop = () => {}
const render = (phase: ResetPhase) =>
  renderToStaticMarkup(
    <ResetDatabaseDialog phase={phase} onConfirm={noop} onCancel={noop} onReload={noop} />,
  )
const text = (html: string) => html.replace(/<[^>]+>/g, '').replace(/&lt;/g, '<').replace(/&gt;/g, '>')

describe('ResetDatabaseDialog', () => {
  it('asks first, naming what happens and how to undo it', () => {
    const html = render({ kind: 'confirm' })
    expect(html).toContain('role="dialog"')
    const shown = text(html)
    expect(shown).toContain('Reset the metadata database?')
    expect(shown).toContain('moved aside to metadata.db.reset-<UTC>')
    expect(shown).toContain('Nothing is deleted')
    expect(shown).toContain('barca state restore --from <the moved file>')
    expect(shown).toContain('Cancel')
    expect(shown).toContain('Reset database')
  })

  it('shows the refusal, and lets the reset be tried again', () => {
    const shown = text(
      render({
        kind: 'failed',
        error: 'cannot reset the metadata database while a run is in flight (r1). Nothing was changed.',
        remediation: 'Wait for it to finish, or cancel it (DELETE /runs/{run_id}).',
      }),
    )
    expect(shown).toContain('Not reset: cannot reset the metadata database while a run is in flight (r1)')
    expect(shown).toContain('Wait for it to finish')
    expect(shown).toContain('Reset database')
  })

  it('disables Cancel while the reset is in progress', () => {
    const html = render({ kind: 'pending' })
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Cancel<\/button>/)
  })

  it('after a reset, names the moved file and the undo command, then offers a reload', () => {
    const shown = text(
      render({
        kind: 'done',
        result: {
          database: '.barca/metadata.db',
          moved_to: '.barca/metadata.db.reset-2026-10-20T18-02-11.482Z',
          moved: ['.barca/metadata.db.reset-2026-10-20T18-02-11.482Z'],
          undo: 'barca state restore --from .barca/metadata.db.reset-2026-10-20T18-02-11.482Z',
        },
      }),
    )
    expect(shown).toContain('The old database is now .barca/metadata.db.reset-2026-10-20T18-02-11.482Z')
    expect(shown).toContain('run barca state restore --from .barca/metadata.db.reset-2026-10-20T18-02-11.482Z')
    expect(shown).toContain('Reload')
  })

  it('says so when there was no database to move', () => {
    const shown = text(
      render({ kind: 'done', result: { database: '.barca/metadata.db', moved_to: null, moved: [], undo: null } }),
    )
    expect(shown).toContain('There was no database')
  })
})
