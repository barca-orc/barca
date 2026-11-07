/**
 * The Runs page's recovery and hand-off logic, pure: the run requests behind "Re-run" and
 * "Retry", the log filters of the run panel, and the incident summary a failed run copies.
 */
import type { LogLine, RunRequest, RunStep, RunView } from './types'
import { triggeredByLabel } from './runs'

// ─── re-running ─────────────────────────────────────────────────────────────

/** A step id's node and partition key: `a.py:x[region=amer]` → `a.py:x`, `region=amer`. */
export function splitStepId(id: string): { node: string; key: string | null } {
  const open = id.indexOf('[')
  if (open < 0 || !id.endsWith(']')) return { node: id, key: null }
  return { node: id.slice(0, open), key: id.slice(open + 1, -1) }
}

/** The run's targets as given (`target` is comma-separated; `null` is every asset). */
function runTargets(run: Pick<RunView, 'target'>): string[] {
  return run.target ? run.target.split(',').map((t) => t.trim()).filter(Boolean) : []
}

function command(run: Pick<RunView, 'command'>): RunRequest['command'] {
  return run.command === 'run' ? 'run' : 'get'
}

/**
 * The run again, cache-aware: the steps that succeeded are cached, so this executes the failed
 * steps and what they stopped downstream, and nothing else.
 */
export function rerunFailedRequest(run: Pick<RunView, 'command' | 'target' | 'files'>): RunRequest {
  return { command: command(run), targets: runTargets(run), files: run.files }
}

/** The run again with every step recomputed (`--refresh-all`). */
export function rerunAllRequest(run: Pick<RunView, 'command' | 'target' | 'files'>): RunRequest {
  return { ...rerunFailedRequest(run), refresh_all: true }
}

/**
 * A partition key as `--partition` takes it: `region=amer` as is, a compound key
 * (`region=amer,day=1`) as a JSON object so a value's comma cannot split it.
 */
export function partitionArg(key: string): string {
  const pairs = key.split(',')
  if (pairs.length === 1) return key
  const object: Record<string, string> = {}
  for (const pair of pairs) {
    const eq = pair.indexOf('=')
    if (eq < 0) return key
    object[pair.slice(0, eq)] = pair.slice(eq + 1)
  }
  return JSON.stringify(object)
}

/**
 * One step again: a cache-aware `get` of its node (a `run` when the run ran that task), only its
 * own key when it is one key of a partitioned node. Its cached upstream is not recomputed.
 */
export function retryStepRequest(run: Pick<RunView, 'command' | 'target' | 'files'>, stepId: string): RunRequest {
  const { node, key } = splitStepId(stepId)
  const ranTask = run.command === 'run' && runTargets(run).some((t) => t === node || node.endsWith(`:${t}`))
  const request: RunRequest = { command: ranTask ? 'run' : 'get', targets: [node], files: run.files }
  if (key) request.partition = partitionArg(key)
  return request
}

// ─── logs ───────────────────────────────────────────────────────────────────

export interface LogFilter {
  /** A step id: its own lines, or with no key, the lines of every key of that node. */
  step: string | null
  /** Case-insensitive text the line contains. */
  query: string
  errorsOnly: boolean
}

export const NO_LOG_FILTER: LogFilter = { step: null, query: '', errorsOnly: false }

const ERROR_TEXT = /^Traceback \(most recent call last\)|^\s*File ".*", line \d+|^[\w.]*(Error|Exception|Interrupt|Exit)\b(:|$)/

/** A line about a failure: anything on stderr, or a traceback's lines on stdout. */
export function isErrorLine(line: Pick<LogLine, 'text' | 'stream'>): boolean {
  return line.stream === 'stderr' || ERROR_TEXT.test(line.text)
}

/** Whether a log line belongs to `step` (exactly, or to one of its keys when it has none). */
export function lineOfStep(nodeId: string, step: string): boolean {
  return nodeId === step || (!step.includes('[') && nodeId.startsWith(`${step}[`))
}

/** The lines the filter keeps, each with its line number in the full log (1-based). */
export function filterLogs(lines: readonly LogLine[], filter: LogFilter): { line: LogLine; number: number }[] {
  const query = filter.query.trim().toLowerCase()
  const kept: { line: LogLine; number: number }[] = []
  lines.forEach((line, i) => {
    if (filter.step !== null && !lineOfStep(line.nodeId, filter.step)) return
    if (filter.errorsOnly && !isErrorLine(line)) return
    if (query && !line.text.toLowerCase().includes(query)) return
    kept.push({ line, number: i + 1 })
  })
  return kept
}

/** `text` split around each case-insensitive match of `query`, for highlighting. */
export function highlightParts(text: string, query: string): { text: string; match: boolean }[] {
  const q = query.trim().toLowerCase()
  if (!q) return [{ text, match: false }]
  const parts: { text: string; match: boolean }[] = []
  const lower = text.toLowerCase()
  let at = 0
  for (let hit = lower.indexOf(q); hit >= 0; hit = lower.indexOf(q, at)) {
    if (hit > at) parts.push({ text: text.slice(at, hit), match: false })
    parts.push({ text: text.slice(hit, hit + q.length), match: true })
    at = hit + q.length
  }
  if (at < text.length) parts.push({ text: text.slice(at), match: false })
  return parts
}

/** Lines as plain text, as `barca logs` prints them with `--timestamps`, each step named. */
export function logsText(lines: readonly LogLine[]): string {
  return lines.map((l) => [l.ts, l.nodeId ? `[${l.nodeId}]` : '[barca]', l.stream === 'stderr' ? '(stderr)' : null, l.text]
    .filter(Boolean).join(' ')).join('\n') + (lines.length ? '\n' : '')
}

// ─── incident summary ───────────────────────────────────────────────────────

/** Where a failed step raised: the last `File "...", line N, in f` frame of its traceback. */
export function raisedAt(traceback: string | null): string | null {
  if (!traceback) return null
  const frames = [...traceback.matchAll(/File "([^"]+)", line (\d+), in (\S+)/g)]
  const last = frames.at(-1)
  return last ? `${last[1]}:${last[2]} in ${last[3]}` : null
}

function firstLine(text: string | null | undefined): string | null {
  const line = text?.split('\n').find((l) => l.trim())
  return line ? line.trim() : null
}

function seconds(s: number | null): string {
  if (s === null) return '–'
  return s < 60 ? `${s.toFixed(1)}s` : `${Math.floor(s / 60)}m ${Math.round(s % 60)}s`
}

/** The run's failed steps, as recorded. */
export function failedSteps(run: Pick<RunView, 'steps'>): RunStep[] {
  return run.steps.filter((s) => s.status === 'failed')
}

/**
 * A markdown summary of a run to paste into an incident or a chat: the run and its link, what
 * it ran and who started it, when and for how long, and each failed step with its error's first
 * line and where it raised.
 */
export function incidentSummary(run: RunView, link: string): string {
  const failed = failedSteps(run)
  const out = [
    `### barca run \`${run.run_id}\` ${run.status}`,
    '',
    `- **Run:** [${run.run_id}](${link})`,
    `- **Target:** \`${run.command} ${run.target ?? 'all assets'}\``,
    `- **Triggered by:** ${triggeredByLabel(run.triggered_by)}`,
    `- **Started:** ${run.started_at}`,
    `- **Duration:** ${seconds(run.elapsed_seconds)}`,
  ]
  const runError = firstLine(run.error ?? run.failure?.error)
  if (runError) out.push(`- **Error:** ${runError}`)
  if (failed.length > 0) {
    out.push('', `**Failed steps (${failed.length}):**`, '')
    for (const step of failed) {
      const error = firstLine(step.error_type && step.error_message ? `${step.error_type}: ${step.error_message}` : step.error) ?? 'no error recorded'
      const where = raisedAt(step.error_traceback ?? step.error)
      out.push(`- \`${step.node_id}\`: ${error}${where ? ` (raised at \`${where}\`)` : ''}`)
    }
  } else if (run.failure) {
    out.push('', `**Failed step:** \`${run.failure.node}\`: ${run.failure.error}`)
  }
  out.push('', `Logs: \`barca logs ${run.run_id}\``)
  return out.join('\n') + '\n'
}
