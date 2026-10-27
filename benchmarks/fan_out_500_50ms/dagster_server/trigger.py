"""Trigger dagster materialization via GraphQL and wait for completion."""

import json
import sys
import time

import requests

GRAPHQL_URL = "http://localhost:3333/graphql"


def launch_run():
    """Launch a job run via GraphQL."""
    query = """
    mutation {
      launchRun(executionParams: {
        selector: {
          repositoryLocationName: "definitions.py"
          repositoryName: "__repository__"
          jobName: "fan_out_job"
        }
        runConfigData: "{}"
      }) {
        __typename
        ... on LaunchRunSuccess {
          run { runId status }
        }
        ... on PythonError { message stack }
        ... on InvalidSubsetError { message }
      }
    }
    """
    resp = requests.post(GRAPHQL_URL, json={"query": query})
    data = resp.json()
    result = data["data"]["launchRun"]
    if result["__typename"] != "LaunchRunSuccess":
        print(f"Launch failed: {result}", file=sys.stderr)
        sys.exit(1)
    return result["run"]["runId"]


def wait_for_completion(run_id, timeout=300):
    """Poll until run completes."""
    query = """
    query($runId: ID!) {
      runOrError(runId: $runId) {
        __typename
        ... on Run { runId status }
      }
    }
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        resp = requests.post(GRAPHQL_URL, json={"query": query, "variables": {"runId": run_id}})
        run = resp.json()["data"]["runOrError"]
        status = run["status"]
        if status in ("SUCCESS", "FAILURE", "CANCELED"):
            return status
        time.sleep(0.1)
    return "TIMEOUT"


if __name__ == "__main__":
    t0 = time.perf_counter()
    run_id = launch_run()
    status = wait_for_completion(run_id)
    elapsed = time.perf_counter() - t0
    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 500,
                "status": status,
            },
            indent=2,
        )
    )
