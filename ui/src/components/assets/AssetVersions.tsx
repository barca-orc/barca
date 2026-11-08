import { useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Pin } from 'lucide-react'
import { Age, Button, ConfirmDialog, Section } from '@/components'
import { api } from '@/lib/api'
import { UPLOAD_EXTENSIONS, uploadFormat } from '@/lib/pins'
import { formatSeconds } from '@/lib/assetTable'
import { formatBytes, shortHash } from '@/lib/nodeDetail'
import { shortName } from '@/lib/graph'
import type { AssetVersion, AssetVersions as Versions, NodeState } from '@/lib/types'

/**
 * barca · Versions
 * An asset's recent successful materializations (`GET /assets/{name}/versions`), each with
 * "Roll back to this" (`POST /assets/{name}/pin`), and, while it is pinned, a banner with
 * "Unpin" (`DELETE /assets/{name}/pin`). Both ask first. `barca docs pins` is the contract:
 * a pinned asset is read by every run as the pinned materialization and never recomputed.
 * "Upload replacement…" pins it to a file the operator picks (`POST /assets/{name}/pin/upload`).
 */


export type VersionsConfirm =
  | { kind: 'pin'; version: AssetVersion }
  | { kind: 'unpin' }
  | { kind: 'upload'; file: File; format: string }

export interface AssetVersionsViewProps {
  name: string
  data: Versions
  nowMs: number
  /** A `--read-only` server refuses pins (403): the buttons are disabled. */
  readOnly: boolean
  pending: boolean
  error: string | null
  confirm: VersionsConfirm | null
  onAsk: (confirm: VersionsConfirm) => void
  onConfirm: () => void
  onCancel: () => void
  /** A file was picked to upload (asks first). */
  onPick?: (file: File) => void
  /** Why a picked file cannot be uploaded (its extension). */
  pickError?: string | null
}

const READ_ONLY = 'This server is read-only (barca serve --read-only)'

function who(by: string | null): string {
  return by ? ` by ${by}` : ''
}

/** The section alone, for each state: a pure function of its props. */
export function AssetVersionsView({ name, data, nowMs, readOnly, pending, error, confirm, onAsk, onConfirm, onCancel, onPick, pickError }: AssetVersionsViewProps) {
  const pin = data.pin
  const input = useRef<HTMLInputElement>(null)
  const pinnedUpload = pin ? data.versions.find((v) => v.run_hash === pin.run_hash)?.upload ?? null : null
  return (
    <Section title="Versions">
      {pin && (
        <div className="barca-pin-banner" role="status">
          <Pin size={14} aria-hidden />
          <div>
            <strong>Pinned at {shortHash(pin.run_hash)}</strong>
            {pinnedUpload && (
              <p className="barca-note">
                Override uploaded{who(pinnedUpload.uploaded_by)}{pinnedUpload.file_name ? ` from ${pinnedUpload.file_name}` : ''}.
              </p>
            )}
            <p className="barca-note">
              Every run reads this version and none recomputes {name} until it is unpinned. Pinned{' '}
              <Age at={pin.pinned_at} nowMs={nowMs} />{who(pin.pinned_by)}{pin.reason ? `: ${pin.reason}` : ''}.
            </p>
          </div>
          <Button variant="ghost" size="sm" disabled={readOnly || pending} title={readOnly ? READ_ONLY : undefined}
            onClick={() => onAsk({ kind: 'unpin' })}>
            Unpin
          </Button>
        </div>
      )}
      {error && <p className="barca-note" role="alert">Could not change the pin: {error}</p>}
      {pickError && <p className="barca-note" role="alert">{pickError}</p>}
      {data.pinnable && onPick && (
        <div className="barca-version-upload">
          <input ref={input} type="file" hidden accept={UPLOAD_EXTENSIONS.join(',')} aria-label={`Replacement data for ${name}`}
            onChange={(e) => {
              const file = e.target.files?.[0]
              e.target.value = ''
              if (file) onPick(file)
            }} />
          <Button variant="ghost" size="sm" disabled={readOnly || pending} title={readOnly ? READ_ONLY : 'Pin it to data you supply: .json, .parquet, .csv or .pkl'}
            onClick={() => input.current?.click()}>
            Upload replacement…
          </Button>
        </div>
      )}
      {data.versions.length === 0 ? (
        <p className="barca-note">No successful materialization with an artifact yet.</p>
      ) : (
        <ul className="barca-versions">
          {data.versions.map((v) => (
            <li key={v.run_hash} className={v.pinned ? 'barca-version barca-version-pinned' : 'barca-version'}>
              <div className="barca-version-what">
                <code title={v.run_hash}>{shortHash(v.run_hash)}</code>
                <span className="barca-cell-sub">
                  <Age at={v.materialized_at} nowMs={nowMs} />
                  {v.run_id && <> · run {v.run_id}</>}
                  {v.elapsed_seconds !== null && <> · {formatSeconds(v.elapsed_seconds)}</>}
                  {v.size_bytes !== null && <> · {formatBytes(v.size_bytes)}</>}
                  {v.current && <> · latest</>}
                  {v.pinned && <> · pinned</>}
                  {!v.available && <> · evicted</>}
                  {v.upload && <> · uploaded{v.upload.file_name ? ` (${v.upload.file_name})` : ''}</>}
                </span>
              </div>
              {data.pinnable && !v.pinned && (
                <Button variant="ghost" size="sm" disabled={readOnly || pending || !v.available}
                  title={readOnly ? READ_ONLY : !v.available ? 'Its artifact was evicted from this disk' : undefined}
                  onClick={() => onAsk({ kind: 'pin', version: v })}>
                  Roll back to this
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      {!data.pinnable && <p className="barca-note">Only an asset without partitions can be pinned.</p>}
      {confirm?.kind === 'pin' && (
        <ConfirmDialog
          title={`Pin ${name} to ${shortHash(confirm.version.run_hash)}?`}
          body={`Every run, scheduled or requested, will read the version materialized ${confirm.version.materialized_at}${confirm.version.run_id ? ` (run ${confirm.version.run_id})` : ''} and none will recompute ${name} until it is unpinned. What reads it is computed against this version.`}
          action="Pin this version"
          onConfirm={onConfirm}
          onCancel={onCancel}
        />
      )}
      {confirm?.kind === 'upload' && (
        <ConfirmDialog
          title={`Override ${name} with ${confirm.file.name}?`}
          body={`${confirm.file.name} (${formatBytes(confirm.file.size)}, ${confirm.format}) is checked by loading it, stored as a new version of ${name} and pinned: every run reads it and none recomputes ${name} until it is unpinned. What reads it is computed against it.`}
          action="Upload and pin"
          onConfirm={onConfirm}
          onCancel={onCancel}
        />
      )}
      {confirm?.kind === 'unpin' && (
        <ConfirmDialog
          title={`Unpin ${name}?`}
          body={`The next run decides ${name} as before: it is recomputed if its latest result is out of date, and what reads it follows.`}
          action="Unpin"
          onConfirm={onConfirm}
          onCancel={onCancel}
        />
      )}
    </Section>
  )
}

export interface AssetVersionsProps {
  node: NodeState
  nowMs: number
  readOnly: boolean
}

/** The Versions section of an asset's panel, with its data and actions. Nothing for a task or sensor. */
export function AssetVersions({ node, nowMs, readOnly }: AssetVersionsProps) {
  const client = useQueryClient()
  const [confirm, setConfirm] = useState<VersionsConfirm | null>(null)
  const [pickError, setPickError] = useState<string | null>(null)
  const isAsset = node.kind === 'asset'
  const versions = useQuery({
    queryKey: ['asset-versions', node.id],
    queryFn: () => api.assetVersions(node.id),
    enabled: isAsset,
    retry: false,
  })
  const change = useMutation({
    mutationFn: async (c: VersionsConfirm): Promise<unknown> =>
      c.kind === 'pin' ? api.pinAsset(node.id, c.version.run_hash) : c.kind === 'upload' ? api.uploadPin(node.id, c.file, c.file.name) : api.unpinAsset(node.id),
    onSettled: () => client.invalidateQueries(),
  })
  if (!isAsset || !versions.data) return null
  return (
    <AssetVersionsView
      name={shortName(node.id)}
      data={versions.data}
      nowMs={nowMs}
      readOnly={readOnly}
      pending={change.isPending}
      error={change.error ? change.error.message : null}
      confirm={confirm}
      onAsk={(c) => {
        change.reset()
        setConfirm(c)
      }}
      onConfirm={() => {
        if (confirm) change.mutate(confirm)
        setConfirm(null)
      }}
      onCancel={() => setConfirm(null)}
      pickError={pickError}
      onPick={(file) => {
        change.reset()
        const format = uploadFormat(file.name)
        if (!format) {
          setPickError(`${file.name}: pick a .json, .parquet, .csv or .pkl file`)
          return
        }
        setPickError(null)
        setConfirm({ kind: 'upload', file, format })
      }}
    />
  )
}
