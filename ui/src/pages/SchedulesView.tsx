import type { ReactNode } from "react";
import { match } from "ts-pattern";
import { Button, EmptyNote, StatusBadge, Tag } from "@/components";
import { useTimeZone } from "@/context/timeZone";
import { nodeStateDisplay } from "@/lib/cacheStateDisplay";
import { inPipeline } from "@/lib/pipeline";
import type { Remote } from "@/lib/remote";
import { scheduleRows, type ScheduleRow } from "@/lib/schedules";
import { nodeKindLabel } from "@/lib/status";
import { formatInstant } from "@/lib/time";
import type { Health, ListNode, NodeState, ScheduleJob } from "@/lib/types";

export interface SchedulesViewProps {
  title: string;
  /** `?pipeline=`: only this file's nodes. */
  pipeline: string | null;
  /** The asset graph; mock placeholder data is `loading` (a schedule list never shows invented schedules). */
  assets: Remote<ListNode[]>;
  states: NodeState[] | undefined;
  /** When `states` was fetched: what "in 5m" is relative to. */
  dataUpdatedAt: number;
  /** The scheduler's registry (which jobs are paused); without it rows show unpaused. */
  jobs: ScheduleJob[] | undefined;
  health: Health | undefined;
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
export function SchedulesView({ title, pipeline, assets, states, dataUpdatedAt, jobs, health, offline, toggle, onOpen, onToggle, onRetry, actions }: SchedulesViewProps) {
  const rows =
    assets.kind === "ready" && states
      ? scheduleRows(assets.data.filter((a) => inPipeline(a.id, pipeline)), states, dataUpdatedAt, jobs ?? [])
      : null;
  // A count next to "not reachable" would be read as current; it is shown only with a live server.
  const current = !offline && assets.kind === "ready" && assets.refreshError === null;
  const canPause = health ? health.scheduler && !health.read_only : false;

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{title}</h1>
            {current && rows && <span className="barca-count">{rows.length.toLocaleString()}<span className="barca-sr-only"> scheduled</span></span>}
          </div>
          <div className="barca-view-actions">
            {health?.read_only && <Tag tone="bare">read-only</Tag>}
            {current && health && rows?.length !== 0 && <span className="barca-count">{health.scheduler ? "scheduler running" : "scheduler not running on this server"}</span>}
            {/* No schedules: the page has nothing of its own to run. */}
            {rows?.length !== 0 && actions}
          </div>
        </div>
        {health && !health.scheduler && rows && rows.length > 0 && (
          <p className="barca-note">
            This server doesn't fire schedules (
            {health.read_only ? "--read-only" : "--no-schedule"}
            ). "Next run" is when each cron next matches — it happens only where
            a <code>barca serve</code> with the scheduler is running.
          </p>
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
              <ScheduleTable rows={rows} canPause={canPause} toggle={toggle} onOpen={onOpen} onToggle={onToggle} />
            ),
          )
          .exhaustive()}
      </div>
    </div>
  );
}

function ScheduleTable({ rows, canPause, toggle, onOpen, onToggle }: Pick<SchedulesViewProps, "toggle" | "onOpen" | "onToggle"> & { rows: ScheduleRow[]; canPause: boolean }) {
  const { zone } = useTimeZone();
  // One line a row: the schedule in words with its cron after it, the next run as "in 5m" with
  // the absolute time on hover, the name with its id on hover.
  return (
    <table className="barca-table barca-schedule-table">
      <colgroup>
        <col />
        <col className="col-schedule" />
        <col className="col-when" />
        <col className="col-when" />
        <col className="col-state" />
        {canPause && <col className="col-action" />}
      </colgroup>
      <thead>
        <tr>
          <th>Name</th>
          <th>Schedule</th>
          <th>Next run</th>
          <th>Last run</th>
          <th>State</th>
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
              {r.nextRunMs !== null ? (
                <time dateTime={new Date(r.nextRunMs).toISOString()} title={formatInstant(r.nextRunMs, zone)}>{r.nextIn}</time>
              ) : (
                "–"
              )}
            </td>
            <td>
              {r.last ? (
                <span className={r.last.status === "failed" ? "barca-cell-failed" : undefined}>
                  {r.last.status === "success" ? r.last.ago : `${r.last.status} ${r.last.ago}`}
                </span>
              ) : (
                "never"
              )}
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
              ) : (
                r.state && (
                  <span className="barca-state-line">
                    <StatusBadge {...nodeStateDisplay({ state: r.state, lastFailed: r.lastFailed })} size="sm" />
                  </span>
                )
              )}
            </td>
            {canPause && (
              <td className="barca-row-action">
                <Button
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
                </Button>
              </td>
            )}
          </tr>
        ))}
      </tbody>
    </table>
  );
}
