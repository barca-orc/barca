import { useNavigate, useSearchParams } from "react-router";
import { useAssets } from "@/hooks/useAssets";
import { useAssetStates } from "@/hooks/useAssetStates";
import { useHealth } from "@/hooks/useHealth";
import { useSchedule, useToggleSchedulePause } from "@/hooks/useSchedule";
import { pipelineName } from "@/lib/pipeline";
import { remote } from "@/lib/remote";
import { SchedulesView } from "./SchedulesView";

/** The Schedules page's server data, as `SchedulesView` shows it. */
export function SchedulesPage() {
  const assets = useAssets();
  const { data: states, dataUpdatedAt } = useAssetStates();
  const health = useHealth();
  const { data: jobs } = useSchedule();
  const toggle = useToggleSchedulePause();
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const pipeline = params.get("pipeline");

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
      jobs={jobs}
      health={health.data}
      offline={health.isError}
      toggle={{
        busyId: toggle.isPending ? toggle.variables.id : null,
        error: toggle.isError ? { pause: toggle.variables.pause, message: toggle.error.message } : null,
      }}
      onOpen={open}
      onToggle={(id, pause) => toggle.mutate({ id, pause })}
      onRetry={() => void assets.refetch()}
    />
  );
}
