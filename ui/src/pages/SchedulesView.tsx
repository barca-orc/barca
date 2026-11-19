import type { ReactNode } from "react";
import { match } from "ts-pattern";
import { Age, Button, EmptyNote, StatusBadge } from "@/components";
import { useTimeZone } from "@/context/timeZone";
import { inPipeline } from "@/lib/pipeline";
import type { Remote } from "@/lib/remote";
import { QUEUE_BOUND_HINT, lastStatusLabel, scheduleRows, type ScheduleRow } from "@/lib/schedules";
import { nodeKindLabel } from "@/lib/status";
import { formatInstant, formatSlot } from "@/lib/time";
import type { ServerHealth, ListNode, NodeState } from "@/lib/types";

export interface SchedulesViewProps {
  title: string;
  /** `?pipeline=`: only this file's nodes. */
  pipeline: string | null;
  /** The asset graph; mock placeholder data is `loading` (a schedule list never shows invented schedules). */
  assets: Remote<ListNode[]>;
  states: NodeState[] | undefined;
  /** When `states` was fetched: what "in 5m" is relative to. */
  dataUpdatedAt: number;
  health: ServerHealth | undefined;
  /** The server did not answer the last health poll: `health` is what it last said. */
  offline: boolean;
  toggle: {
    /** The schedule being paused or resumed right now. */
    busyId: string | null;
    error: { pause: boolean; message: string } | null;
  };
  onOpen: (id: string) => void;
  onToggle: (id: string, pause: boolean) => void;
  onRetry: () => void;
  /** At the end of the toolbar: the page's Run (`PageRunButton`). */
  actions?: ReactNode;
}

/**
 * Every node with a cron schedule: when it runs (in words and as cron), when
 * it next fires, and how its last run went. Soonest first.
 */
export function SchedulesView({ title, pipeline, assets, states, dataUpdatedAt, health, offline, toggle, onOpen, onToggle, onRetry, actions }: SchedulesViewProps) {
  const rows =
    assets.kind === "ready" && states
      ? scheduleRows(assets.data.filter((a) => inPipeline(a.id, pipeline)), states)
      : null;
  // A count next to "not reachable" would be read as current; it is shown only with a live server.
  const current = !offline && assets.kind === "ready" && assets.refreshError === null;
  const canPause = health ? health.scheduler && !health.read_only : false;
  const late = rows?.filter((r) => r.late !== null).length ?? 0;
  const queueBound = rows?.filter((r) => r.queueBound).length ?? 0;

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{title}</h1>
            {current && rows && <span className="barca-count">{rows.length.toLocaleString()}<span className="barca-sr-only"> scheduled</span></span>}
          </div>
          <div className="barca-view-actions">
            {current && health && rows?.length !== 0 && (
              <span className="barca-count">
                {health.read_only ? "read-only view" : health.scheduler ? "scheduler running" : "scheduler off (--no-schedule)"}
                {late > 0 && ` · ${late.toLocaleString()} late`}
                {queueBound > 0 && ` · ${queueBound.toLocaleString()} queue-bound`}
              </span>
            )}
            {/* No schedules: the page has nothing of its own to run. */}
            {rows?.length !== 0 && actions}
          </div>
        </div>
        {health && !health.scheduler && rows && rows.length > 0 && (
          health.read_only ? (
            <p className="barca-note">
              Schedules fire on the server that owns the scheduler (a <code>barca serve</code>{" "}
              without <code>--read-only</code>). This view only reads their runs and the queue
              from the shared metadata, as of each refresh: a schedule is late when no tick has
              fired for a slot that passed longer ago than its grace and its queue wait.
            </p>
          ) : (
            <p className="barca-note">
              This server doesn't fire schedules (<code>--no-schedule</code>). "Next run" is when
              each cron next matches — it happens only where a <code>barca serve</code> with the
              scheduler is running.
            </p>
          )
        )}
        {toggle.error && (
          <p className="barca-note" role="alert">
            Can't {toggle.error.pause ? "pause" : "resume"} the schedule: {toggle.error.message}
          </p>
        )}
        {assets.kind === "ready" && assets.refreshError && (
          <p className="barca-note" role="alert">
            Showing the last loaded schedules; they may be out of date: {assets.refreshError}{" "}
            <Button size="sm" onClick={onRetry}>Retry</Button>
          </p>
        )}
      </div>

      <div className="barca-view-body barca-table-scroll">
        {match(assets)
          .with({ kind: "failed" }, ({ error }) => (
            <p className="barca-table-empty" role="alert">
              Can't load schedules: {error}{" "}
              <Button size="sm" onClick={onRetry}>Retry</Button>
            </p>
          ))
          .with({ kind: "loading" }, () => <p className="barca-table-empty">Loading…</p>)
          .with({ kind: "ready" }, () =>
            !rows ? (
              <p className="barca-table-empty">Loading…</p>
            ) : rows.length === 0 ? (
              <EmptyNote title={`No schedules${pipeline ? " in this pipeline" : ""}`}>
                Give an asset or task <code>freshness=Schedule("0 6 * * *")</code> to run it
                on a cron schedule while <code>barca serve</code> is running.
              </EmptyNote>
            ) : (
              <ScheduleTable rows={rows} nowMs={dataUpdatedAt} canPause={canPause} toggle={toggle} onOpen={onOpen} onToggle={onToggle} />
            ),
          )
          .exhaustive()}
      </div>
    </div>
  );
}

function ScheduleTable({ rows, nowMs, canPause, toggle, onOpen, onToggle }: Pick<SchedulesViewProps, "toggle" | "onOpen" | "onToggle"> & { rows: ScheduleRow[]; nowMs: number; canPause: boolean }) {
  const { zone } = useTimeZone();
  // One line a row: the schedule in words with its cron after it, the next and last runs as
  // "in 5m" and "3m ago" with the absolute time on hover, the name with its id on hover.
  return (
    <table className="barca-table barca-schedule-table">
      <colgroup>
        <col />
        <col className="col-schedule" />
        <col className="col-when" />
        <col className="col-when" />
        <col className="col-wait" />
        <col className="col-state" />
        {canPause && <col className="col-action" />}
      </colgroup>
      <thead>
        <tr>
          <th>Name</th>
          <th>Schedule</th>
          <th title="The next slot of its cron: when its next tick is due. A late schedule waits behind the slots it missed">Next slot</th>
          <th>Last tick</th>
          <th className="col-wait" title="How long its ticks' runs typically wait for a run slot, and how long they typically take: medians of its newest 20 ticks">Typical wait · run</th>
          <th title="Where the schedule stands, as the server says: late (no tick fired for a slot that passed longer ago than its grace and queue wait), queue-bound (its ticks wait longer than they are apart), paused, or on time">State</th>
          {canPause && <th aria-label="Actions" />}
        </tr>
      </thead>
      <tbody>
        {rows.map((r) => (
          <tr
            key={r.id}
            tabIndex={0}
            onClick={() => onOpen(r.id)}
            onKeyDown={(e) => e.key === "Enter" && onOpen(r.id)}
          >
            <td title={r.id}>
              <div className="barca-cell-name">
                <span className="name barca-cell-cut">{r.name}</span>
                {/* The kind in muted words, as beside a panel's name: not a badge. */}
                {r.kind !== "asset" && <span className="barca-cell-kind">{nodeKindLabel(r)}</span>}
              </div>
            </td>
            <td title={r.cron}>
              {r.human ?? <code>{r.cron}</code>}
              {r.human && <code className="barca-schedule-cron">{r.cron}</code>}
            </td>
            <td>
              {r.late ? (
                // Missed a slot: the next one is not when it runs, so no countdown.
                <time
                  dateTime={new Date(r.late.sinceMs).toISOString()}
                  title={`no tick has fired for the ${formatInstant(r.late.sinceMs, zone)} slot${r.nextRunMs !== null ? `; next slot ${formatInstant(r.nextRunMs, zone)}` : ""}`}
                >
                  <StatusBadge glyph="stale" label={r.late.by} size="sm" />
                </time>
              ) : r.nextRunMs !== null ? (
                <span title={formatInstant(r.nextRunMs, zone)}>
                  {formatSlot(new Date(r.nextRunMs).toISOString(), zone, nowMs)} <span className="barca-cell-sub">· <Age at={r.nextRunMs} nowMs={nowMs} /></span>
                </span>
              ) : (
                <Age at={r.nextRunMs} nowMs={nowMs} />
              )}
            </td>
            <td>
              {r.last ? (
                <span
                  className={r.last.status === "failed" ? "barca-cell-failed" : undefined}
                  title={r.last.runId ? `run ${r.last.runId}` : undefined}
                >
                  {r.last.status !== null && r.last.status !== "success" && `${lastStatusLabel(r.last.status)} `}<Age at={r.last.at} nowMs={nowMs} />
                </span>
              ) : (
                "never"
              )}
            </td>
            <td className="col-wait" data-queue-wait={r.queueWait ?? undefined}>
              {r.queueWait ?? <span className="barca-cell-sub">none</span>}
              <span className="barca-cell-sub"> · {r.typicalRun ?? "–"}</span>
            </td>
            <td>
              {r.paused ? (
                <span
                  title={
                    r.paused.until !== null
                      ? `paused until ${formatInstant(r.paused.until, zone)}`
                      : "paused until resumed"
                  }
                >
                  <StatusBadge glyph="paused" label="paused" size="sm" />
                </span>
              ) : r.standing === "held" || r.standing === "overrunning" ? (
                <span title={r.detail ?? undefined}>{r.standing === "held" ? r.detail ?? r.standing : r.standing}</span>
              ) : r.late ? (
                <span title="No tick has fired for a slot that passed longer ago than its grace and its typical queue wait">
                  <StatusBadge glyph="stale" label="late" size="sm" />
                </span>
              ) : r.queueBound ? (
                <span data-queue-bound title={QUEUE_BOUND_HINT}>
                  <StatusBadge glyph="queued" label="queue-bound" size="sm" />
                  <span className="barca-cell-sub barca-schedule-todo">raise max_runs or space the schedules</span>
                </span>
              ) : (
                <span className="barca-cell-sub">on time</span>
              )}
            </td>
            {canPause && (
              <td className="barca-row-action">
                {r.standing !== "held" && <Button
                  size="sm"
                  variant="ghost"
                  loading={toggle.busyId === r.id}
                  disabled={toggle.busyId !== null}
                  onClick={(e) => {
                    e.stopPropagation();
                    onToggle(r.id, r.paused === null);
                  }}
                  onKeyDown={(e) => e.stopPropagation()}
                >
                  {r.paused ? "Resume" : "Pause"}
                </Button>}
              </td>
            )}
          </tr>
        ))}
      </tbody>
    </table>
  );
}
