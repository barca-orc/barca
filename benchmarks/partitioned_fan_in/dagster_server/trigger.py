"""Trigger dagster server-mode materialization and wait."""

import json
import sys
import time
import requests

GRAPHQL_URL = "http://localhost:3333/graphql"


def launch():
    query = """mutation { launchRun(executionParams: { selector: { repositoryLocationName: "definitions.py", repositoryName: "__repository__", jobName: "partitioned_fan_in_job" }, runConfigData: "{}" }) { __typename ... on LaunchRunSuccess { run { runId } } ... on PythonError { message } } }"""
    r = requests.post(GRAPHQL_URL, json={"query": query}).json()
    result = r["data"]["launchRun"]
    if result["__typename"] != "LaunchRunSuccess":
        print(f"Launch failed: {result}", file=sys.stderr)
        sys.exit(1)
    return result["run"]["runId"]


def wait(run_id):
    query = "query($id: ID!) { runOrError(runId: $id) { __typename ... on Run { status } } }"
    while True:
        r = requests.post(GRAPHQL_URL, json={"query": query, "variables": {"id": run_id}}).json()
        s = r["data"]["runOrError"]["status"]
        if s in ("SUCCESS", "FAILURE", "CANCELED"):
            return s
        time.sleep(0.1)


t0 = time.perf_counter()
rid = launch()
status = wait(rid)
elapsed = time.perf_counter() - t0
print(json.dumps({"elapsed_seconds": round(elapsed, 6), "status": status}, indent=2))
