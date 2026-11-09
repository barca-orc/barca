import { Link } from 'react-router'
import { Copy, Play, X } from 'lucide-react'
import { Button, IconButton, KeyValue, Section, SidePanel, Skeleton } from '@/components'
import { StatusGlyph } from '@/components/StatusGlyph'
import { ErrorLine } from '@/components/ErrorLine'
import { Absolute } from '@/components/Age'
import { artifactSchema } from '@/lib/artifactSchema'
import { formatSeconds } from '@/lib/assetTable'
import { shortName } from '@/lib/graph'
import { formatBytes, previewRows, truncateCell } from '@/lib/nodeDetail'
import { plural } from '@/lib/plural'
import { KEY_STATE_GLYPH, KEY_STATE_MEANING, KEY_STATE_WORD, needsRun, type KeyState } from '@/lib/partitionGrid'
import type { KeyArtifact, PartitionKeyDetail } from '@/lib/types'

const STATE: Record<string, KeyState> = { c: 'fresh', s: 'stale', n: 'never_run', f: 'failed', b: 'blocked' }

/** A key's result or input: its type and columns, and its first rows. */
function Shape({ artifact, label }: { artifact: KeyArtifact; label: string }) {
  if (!artifact.artifact_path) return <p className="barca-schema-empty">No successful result for this key yet.</p>
  const schema = artifactSchema(artifact.shape)
  const preview = previewRows((artifact.shape as { sample?: unknown } | null)?.sample)
  return (
    <>
      <div className="barca-schema-meta">
        {schema.type && <strong>{schema.type}</strong>}
        {schema.rows !== null && <span>{schema.rows.toLocaleString()} rows</span>}
        {schema.columns.length > 0 && <span>{schema.columns.length} {schema.fields ? 'fields' : 'columns'}</span>}
        {artifact.format && <span>{artifact.format}</span>}
        {artifact.size_bytes !== null && <span>{formatBytes(artifact.size_bytes)}</span>}
      </div>
      {schema.note && <p className="barca-schema-empty">{schema.note}</p>}
      {preview ? (
        <div className="barca-schema-scroll">
          <table className="barca-schema-table" aria-label={label}>
            <thead><tr>{preview.columns.map((c) => <th key={c}>{c}</th>)}</tr></thead>
            <tbody>{preview.rows.map((row, i) => <tr key={i}>{row.map((v, j) => <td key={j} title={v}>{truncateCell(v).short}</td>)}</tr>)}</tbody>
          </table>
        </div>
      ) : schema.columns.length > 0 && (
        <div className="barca-schema-scroll">
          <table className="barca-schema-table" aria-label={label}>
            <thead><tr><th>{schema.fields ? 'Field' : 'Column'}</th><th>Type</th></tr></thead>
            <tbody>{schema.columns.map((c, i) => <tr key={i}><td>{c.name}</td><td>{c.type ?? 'not recorded'}</td></tr>)}</tbody>
          </table>
        </div>
      )}
    </>
  )
}

export interface KeyDetailProps {
  node: string
  keyName: string
  detail: PartitionKeyDetail | undefined
  loading: boolean
  error: Error | null
  /** The explorer of another node of the chain at the same key. */
  hrefFor: (node: string) => string
  onRun: (() => void) | null
  running: boolean
  onClose: () => void
}

/**
 * barca · KeyDetail
 * One key: what state it is in and why, the same key along its chain, what it wrote and what it
 * read, and its attempts. Opens beside the key explorer.
 */
export function KeyDetail({ node, keyName, detail, loading, error, hrefFor, onRun, running, onClose }: KeyDetailProps) {
  const state = detail ? STATE[detail.state] ?? null : null
  const latest = detail?.attempts[0]
  return (
    <SidePanel
      label={`Key ${keyName}`}
      title={<code>{keyName}</code>}
      titleText={keyName}
      badge={state ? <span className="barca-keydetail-state"><StatusGlyph glyph={KEY_STATE_GLYPH[state]} size={14} />{KEY_STATE_WORD[state]}</span> : undefined}
      subtitle={shortName(node)}
      actions={<>
        <IconButton label="Copy key" size="sm" onClick={() => void navigator.clipboard?.writeText(keyName)}><Copy size={13} /></IconButton>
        <IconButton label="Close (Esc)" size="sm" onClick={onClose}><X size={14} /></IconButton>
      </>}
    >
      {loading && !detail && <Skeleton height={160} />}
      {error && !detail && <p className="barca-note">Couldn't load this key: {error.message}</p>}
      {detail && state && (
        <>
          {/* What is wrong with it, first and loud; otherwise what its state means. */}
          {state === 'failed' && latest?.error ? (
            <div className="barca-keydetail-error" role="alert">
              <ErrorLine error={latest.error} />
              <p className="barca-cell-sub">
                Failed <Absolute at={latest.created_at} />
                {latest.run_id && <> · <Link to={`/runs?run=${encodeURIComponent(latest.run_id)}`}>open the run</Link></>}
              </p>
            </div>
          ) : (
            <p className="barca-keydetail-meaning">
              {KEY_STATE_MEANING[state]}
              {latest && <span className="barca-cell-sub"> Last attempt <Absolute at={latest.created_at} />{latest.run_id && <> · <Link to={`/runs?run=${encodeURIComponent(latest.run_id)}`}>run</Link></>}</span>}
            </p>
          )}
          {onRun && needsRun(state) && (
            <div className="barca-keydetail-actions">
              <Button size="sm" iconLeft={<Play size={11} />} loading={running} onClick={onRun}>Run this key</Button>
            </div>
          )}
          {/* The same key along its chain: where it broke, upstream first. */}
          {detail.chain.length > 1 && (
            <ol className="barca-keychain" aria-label="This key along its chain">
              {detail.chain.map((c, i) => {
                const s = STATE[c.state] ?? null
                return (
                  <li key={c.id} aria-current={c.id === node ? 'true' : undefined}>
                    {i > 0 && <span aria-hidden className="barca-keys-chain-arrow">→</span>}
                    {s ? <StatusGlyph glyph={KEY_STATE_GLYPH[s]} size={12} label={KEY_STATE_WORD[s]} /> : <StatusGlyph glyph="unknown" size={12} label="keys not known yet" />}
                    {c.id === node ? <b>{shortName(c.id)}</b> : <Link to={hrefFor(c.id)}>{shortName(c.id)}</Link>}
                  </li>
                )
              })}
            </ol>
          )}
          {/* What it read comes before what it wrote: for a failed key, the input is the evidence. */}
          {detail.inputs.length > 0 && (
            <Section title={detail.inputs.length === 1 ? `Input · ${shortName(detail.inputs[0]!.id)}` : 'Inputs'}>
              {detail.inputs.map((input) => (
                <div key={input.step} className="barca-keydetail-input">
                  {detail.inputs.length > 1 && <KeyValue label={shortName(input.id)}><code className="barca-cell-sub">{input.step === input.id ? 'whole result' : 'same key'}</code></KeyValue>}
                  <Shape artifact={input} label={`${shortName(input.id)} for ${keyName}`} />
                </div>
              ))}
            </Section>
          )}
          <Section title={state === 'fresh' ? 'Output' : 'Last successful output'}>
            {detail.output && state === 'failed' && (
              <p className="barca-note">The latest attempt failed; this data is from an earlier successful attempt.</p>
            )}
            {detail.output && state !== 'fresh' && state !== 'failed' && (
              <p className="barca-note">This retained result may be out of date for the current code and inputs.</p>
            )}
            {detail.output ? <Shape artifact={detail.output} label={`${keyName} output`} /> : <p className="barca-schema-empty">No successful result for this key yet.</p>}
          </Section>
          {detail.attempts.length > 0 && (
            <details className="barca-keydetail-attempts">
              <summary>{plural(detail.attempts.length, 'attempt')}</summary>
              <ul className="barca-keyattempts">
                {detail.attempts.map((a, i) => (
                  <li key={i}>
                    <StatusGlyph glyph={a.status === 'success' ? 'fresh' : 'failed'} size={12} />
                    <Absolute at={a.created_at} />
                    {a.elapsed_seconds !== null && <span className="barca-cell-sub">{formatSeconds(a.elapsed_seconds)}</span>}
                    {a.run_id && <Link className="barca-cell-sub" to={`/runs?run=${encodeURIComponent(a.run_id)}`}>run</Link>}
                  </li>
                ))}
              </ul>
            </details>
          )}
        </>
      )}
    </SidePanel>
  )
}
