"""Prefect scheduler benchmark — idle-footprint fixture (10 far-future schedules).

Serves ten deployments of a no-op flow, each on a `0 0 1 1 *` (00:00 Jan 1)
cron: registered and polled by the single `.serve()` process but never firing
during the measurement window, isolating idle footprint from execution.
"""

from prefect import flow, serve


@flow
def noop_flow():
    pass


if __name__ == "__main__":
    deployments = [
        noop_flow.to_deployment(name=f"idle_{i:02d}", cron="0 0 1 1 *") for i in range(10)
    ]
    serve(*deployments)
