/**
 * The browser tab's title: what is open, most specific first, then the section and the app
 * (`orders · Assets · barca`, `a7e9b0 · Runs · barca`), so a row of barca tabs tells apart.
 */

import { shortName } from './graph'
import { pipelineName } from './pipeline'

const APP = 'barca'

/** Each route's section, as the sidebar names it. */
const SECTIONS: Record<string, string> = {
  '/assets': 'Assets',
  '/graph': 'Assets',
  '/runs': 'Runs',
  '/schedules': 'Schedules',
  '/tasks': 'Tasks',
  '/sensors': 'Sensors',
  '/keys': 'Keys',
  '/tokens': 'API tokens',
  '/docs': 'Docs',
}

/**
 * `pathname` and `params` as the hash router has them; `groupName` names a group id (null
 * while the groups load: the id's last segment stands in).
 */
export function documentTitle(pathname: string, params: URLSearchParams, groupName: (id: string) => string | null): string {
  const section = SECTIONS[pathname] ?? null
  const node = params.get('node') ?? params.get('focus')
  const group = params.get('group')
  const pipeline = params.get('pipeline')
  const run = params.get('run')
  const key = params.get('key')
  const subject =
    pathname === '/runs' ? run
    : pathname === '/keys' ? node && (key ? `${key} · ${shortName(node)}` : shortName(node))
    : node ? shortName(node)
    : group ? groupName(group) ?? group.split(':').pop() ?? group
    : pipeline ? pipelineName(pipeline)
    : null
  return [subject, section, APP].filter((part): part is string => !!part).join(' · ')
}
