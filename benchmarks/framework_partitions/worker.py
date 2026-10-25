"""Competitor adapters: poll a snapshot, explicitly route only changed partitions."""

import json
from pathlib import Path
import sys

ROOT = Path.cwd()
CURRENT = json.loads((ROOT / "versions.json").read_text())
CALLS = []


def poll():
    CALLS.append("poll")
    path = ROOT / "previous.json"
    previous = json.loads(path.read_text()) if path.exists() else CURRENT
    changed = {key: value for key, value in CURRENT.items() if previous.get(key) != value}
    return changed


def cleaned_value(key, value):
    CALLS.append(f"cleaned:{key}")
    return {"value": value}


def scored_value(key, value):
    CALLS.append(f"scored:{key}")
    return {"score": value["value"] * 2}


def dagster_run():
    import dagster as dg

    partitions = dg.StaticPartitionsDefinition(list(CURRENT))

    @dg.asset(partitions_def=partitions)
    def cleaned(context: dg.AssetExecutionContext):
        return cleaned_value(context.partition_key, CURRENT[context.partition_key])

    @dg.asset(partitions_def=partitions)
    def scored(context: dg.AssetExecutionContext, cleaned):
        return scored_value(context.partition_key, cleaned)

    output = {}
    with dg.DagsterInstance.get() as instance:
        for key in poll():
            result = dg.materialize(
                [cleaned, scored],
                instance=instance,
                partition_key=key,
                run_config={"loggers": {"console": {"config": {"log_level": "ERROR"}}}},
            )
            assert result.success
            output[key] = result.output_for_node("scored")
    return output


def prefect_run():
    from prefect import flow, task
    from prefect.cache_policies import INPUTS, TASK_SOURCE
    from prefect.serializers import JSONSerializer

    options = dict(
        persist_result=True, result_serializer=JSONSerializer(), cache_policy=INPUTS + TASK_SOURCE
    )
    clean = task(cleaned_value, **options)
    score = task(scored_value, **options)
    observe = task(poll, persist_result=True, cache_policy=None)

    @flow(name="changed-partition-chain", persist_result=True, result_serializer=JSONSerializer())
    def update():
        return {key: score(key, clean(key, value)) for key, value in observe().items()}

    return update()


def dagster_full_run():
    import dagster as dg

    @dg.op
    def versions():
        CALLS.append("poll")
        return CURRENT

    @dg.op(out=dg.DynamicOut(io_manager_key="routing"))
    def route(versions):
        for key, value in versions.items():
            yield dg.DynamicOutput((key, value), mapping_key=key)

    @dg.op
    def cleaned(item):
        key, value = item
        return key, cleaned_value(key, value)

    @dg.op
    def scored(item):
        key, value = item
        return scored_value(key, value)

    @dg.job(
        resource_defs={
            "io_manager": dg.fs_io_manager.configured({"base_dir": str(ROOT / "artifacts")}),
            "routing": dg.mem_io_manager,
        }
    )
    def all_keys():
        route(versions()).map(cleaned).map(scored)

    with dg.DagsterInstance.get() as instance:
        result = all_keys.execute_in_process(
            instance=instance,
            run_config={"loggers": {"console": {"config": {"log_level": "ERROR"}}}},
        )
        assert result.success
        return result.output_for_node("scored")


def prefect_full_run():
    from prefect import flow, task
    from prefect.cache_policies import INPUTS, TASK_SOURCE
    from prefect.serializers import JSONSerializer
    from prefect.task_runners import ThreadPoolTaskRunner

    options = dict(
        persist_result=True, result_serializer=JSONSerializer(), cache_policy=INPUTS + TASK_SOURCE
    )
    clean = task(cleaned_value, **options)
    score = task(scored_value, **options)

    @task(persist_result=True, cache_policy=None, result_serializer=JSONSerializer())
    def versions():
        CALLS.append("poll")
        return CURRENT

    @flow(
        name="all-partition-chains",
        persist_result=False,
        task_runner=ThreadPoolTaskRunner(max_workers=4),
    )
    def update():
        values = versions()
        keys = list(values)
        cleaned = clean.map(keys, list(values.values()))
        scored = score.map(keys, cleaned)
        return {key: future.result() for key, future in zip(keys, scored)}

    return update()


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[2] == "full":
        output = {"dagster": dagster_full_run, "prefect": prefect_full_run}[sys.argv[1]]()
    else:
        output = {"dagster": dagster_run, "prefect": prefect_run}[sys.argv[1]]()
    # Commit the observed snapshot only after all changed chains succeed.
    (ROOT / "previous.json").write_text(json.dumps(CURRENT))
    print(json.dumps({"output": output, "calls": CALLS}))
