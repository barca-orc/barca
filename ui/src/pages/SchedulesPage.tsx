import { useNavigate, useSearchParams } from "react-router";
import { useAssets } from "@/hooks/useAssets";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { recovering } from "@/hooks/polling";
import { useHealth } from "@/hooks/useHealth";
import { usePageTopbarRun } from "@/hooks/useTopbarRun";
import { useToggleSchedulePause } from "@/hooks/useSchedule";
import { inPipeline, pipelineName } from "@/lib/pipeline";
import { remote } from "@/lib/remote";
import { PageRunButton } from "@/components";
import { SchedulesView } from "./SchedulesView";

/** The Schedules page's server data, as `SchedulesView` shows it. */
export function SchedulesPage() {
  const assets = useAssets();
  const [params] = useSearchParams();
  const pipeline = params.get("pipeline");
  const targets = assets.data?.filter((a) => a.schedule && inPipeline(a.id, pipeline)).map((a) => a.id) ?? [];
  const { data: states, dataUpdatedAt } = useQuery({
    queryKey: ['state', { targets }],
    queryFn: ({ signal }) => targets.length ? api.state({ targets }, signal) : Promise.resolve([]),
    enabled: !!assets.data,
    refetchInterval: recovering(10_000),
    retry: false,
  });
  const health = useHealth();
  const toggle = useToggleSchedulePause();
  const navigate = useNavigate();
  const { scopeRun } = usePageTopbarRun((handle) => navigate(`/runs?run=${encodeURIComponent(handle)}`));

  const open = (id: string) => {
    const p = new URLSearchParams();
    if (pipeline) p.set("pipeline", pipeline);
    p.set("node", id);
    const kind = states?.find((node) => node.id === id)?.kind;
    const path = kind === "task" ? "/tasks" : kind === "sensor" ? "/sensors" : "/assets";
    navigate(`${path}?${p}`);
  };

  return (
    <SchedulesView
      title={pipeline ? `Schedules · ${pipelineName(pipeline)}` : "Schedules"}
      pipeline={pipeline}
      assets={remote(assets)}
      states={states}
      dataUpdatedAt={dataUpdatedAt}
      health={health.data}
      offline={health.isError}
      toggle={{
        busyId: toggle.isPending ? toggle.variables.id : null,
        error: toggle.isError ? { pause: toggle.variables.pause, message: toggle.error.message } : null,
      }}
      onOpen={open}
      onToggle={(id, pause) => toggle.mutate({ id, pause })}
      onRetry={() => void assets.refetch()}
      actions={<PageRunButton run={scopeRun} />}
    />
  );
}
