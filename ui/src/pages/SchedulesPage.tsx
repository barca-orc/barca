import { useNavigate, useSearchParams } from "react-router";
import { ConnectionBadge, StatusBadge, Tag } from "@/components";
import { useAssets } from "@/hooks/useAssets";
import { useAssetStates } from "@/hooks/useAssetStates";
import { useHealth } from "@/hooks/useHealth";
import { severityStatus } from "@/lib/assetTable";
import { connection } from "@/lib/connection";
import { inPipeline, pipelineName } from "@/lib/pipeline";
import { scheduleRows } from "@/lib/schedules";

/**
 * Every node with a cron schedule: when it runs (in words and as cron), when
 * it next fires, and how its last run went. Soonest first.
 */
export function SchedulesPage() {
  // `useAssets` shows mock data until the server answers; a schedule list must
  // never show invented schedules, so placeholder data is treated as loading.
  const { data: assets, isPlaceholderData, isError } = useAssets();
  const { data: states, dataUpdatedAt } = useAssetStates();
  const { data: health, isError: healthError } = useHealth();
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const pipeline = params.get("pipeline");

  const loaded = assets && !isPlaceholderData && states;
  const rows = loaded
    ? scheduleRows(
        assets.filter((a) => inPipeline(a.id, pipeline)),
        states,
        dataUpdatedAt,
      )
    : [];

  const open = (id: string) => {
    const p = new URLSearchParams();
    if (pipeline) p.set("pipeline", pipeline);
    p.set("node", id);
    const kind = states?.find((node) => node.id === id)?.kind;
    const path = kind === "task" ? "/tasks" : kind === "sensor" ? "/sensors" : "/assets";
    navigate(`${path}?${p}`);
  };

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>
              {pipeline ? `Schedules · ${pipelineName(pipeline)}` : "Schedules"}
            </h1>
            {loaded && (
              <span className="barca-count">{rows.length} scheduled</span>
            )}
          </div>
          <div className="barca-view-actions">
            {health?.read_only && <Tag tone="bare">read-only</Tag>}
            <ConnectionBadge
              connection={connection(health, healthError)}
              onlineLabel={
                health?.scheduler ? "scheduler running" : "scheduler not running on this server"
              }
            />
          </div>
        </div>
        {health && !health.scheduler && rows.length > 0 && (
          <p className="barca-note">
            This server doesn't fire schedules (
            {health.read_only ? "--read-only" : "--no-schedule"}
            ). "Next run" is when each cron next matches — it happens only where
            a <code>barca serve</code> with the scheduler is running.
          </p>
        )}
      </div>

      <div className="barca-view-body barca-table-scroll">
        {isError ? (
          <p className="barca-table-empty">
            Can't load schedules: barca serve is not reachable.
          </p>
        ) : !loaded ? (
          <p className="barca-table-empty">Loading…</p>
        ) : rows.length === 0 ? (
          <p className="barca-table-empty">
            No scheduled nodes{pipeline ? " in this pipeline" : ""}. Give an
            asset or task <code>freshness=Schedule("0 6 * * *")</code> to run it
            on a cron schedule while <code>barca serve</code> is running.
          </p>
        ) : (
          <table className="barca-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Schedule</th>
                <th>Next run</th>
                <th>Last run</th>
                <th>State</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <tr
                  key={r.id}
                  tabIndex={0}
                  onClick={() => open(r.id)}
                  onKeyDown={(e) => e.key === "Enter" && open(r.id)}
                >
                  <td>
                    <div className="barca-cell-name">
                      <span className="name">{r.name}</span>
                      {r.kind !== "asset" && <Tag size="sm">{r.kind}</Tag>}
                    </div>
                    <div className="barca-cell-sub">{r.file}</div>
                  </td>
                  <td>
                    <div>{r.human ?? r.cron}</div>
                    {r.human && <div className="barca-cell-sub">{r.cron}</div>}
                  </td>
                  <td>
                    {r.nextRunMs !== null ? (
                      <>
                        <div>{r.nextIn}</div>
                        <div className="barca-cell-sub">
                          {new Date(r.nextRunMs).toLocaleString()}
                        </div>
                      </>
                    ) : (
                      "–"
                    )}
                  </td>
                  <td>
                    {r.last ? (
                      <span
                        className={
                          r.last.status === "failed"
                            ? "barca-cell-failed"
                            : undefined
                        }
                      >
                        {r.last.status} · {r.last.ago}
                      </span>
                    ) : (
                      <span className="barca-cell-sub">never</span>
                    )}
                  </td>
                  <td>
                    {r.severity && (
                      <StatusBadge
                        status={severityStatus(r.severity)}
                        label={r.severity.replace("_", " ")}
                        size="sm"
                      />
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
