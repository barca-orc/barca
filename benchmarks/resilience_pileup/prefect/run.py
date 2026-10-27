"""Prefect: resilience / anti-pileup workload (sequential task calls)."""

import json
import os
import time
from pathlib import Path
from prefect import flow, task

_STATE = Path(os.environ.get("BENCH_STATE", "/tmp/bench_pileup_state"))
_STATE.mkdir(parents=True, exist_ok=True)


def _poison_attempt():
    c = _STATE / "poison"
    n = int(c.read_text()) if c.exists() else 0
    c.write_text(str(n + 1))
    return n


def _work():
    time.sleep(50 / 1000.0)


@task
def h0():
    _work()
    return 0


@task
def t0(x):
    _work()
    return x


@task
def h1():
    _work()
    return 1


@task
def t1(x):
    _work()
    return x


@task
def h2():
    _work()
    return 2


@task
def t2(x):
    _work()
    return x


@task
def h3():
    _work()
    return 3


@task
def t3(x):
    _work()
    return x


@task
def h4():
    _work()
    return 4


@task
def t4(x):
    _work()
    return x


@task
def h5():
    _work()
    return 5


@task
def t5(x):
    _work()
    return x


@task
def h6():
    _work()
    return 6


@task
def t6(x):
    _work()
    return x


@task
def h7():
    _work()
    return 7


@task
def t7(x):
    _work()
    return x


@task(retries=2, retry_delay_seconds=0.5)
def poison_head():
    n = _poison_attempt()
    if n < 2:
        raise RuntimeError(f"poison transient {n}")
    return n


@task
def poison_tail(x):
    _work()
    return True


@flow(name="resilience_pileup")
def pileup_flow():
    t0(h0())
    t1(h1())
    t2(h2())
    t3(h3())
    t4(h4())
    t5(h5())
    t6(h6())
    t7(h7())
    poison_tail(poison_head())


if __name__ == "__main__":
    _t0 = time.perf_counter()
    pileup_flow()
    elapsed = time.perf_counter() - _t0
    print(json.dumps({"elapsed_seconds": round(elapsed, 6), "steps_executed": 18}, indent=2))
