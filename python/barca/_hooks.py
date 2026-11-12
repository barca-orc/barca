"""Callbacks a project registers to run in the worker before each step (``barca.on_step_start``).

A worker process runs many steps one after another, and settings it holds (DuckDB's
``memory_limit`` on the shared connection, thread counts) were set for whatever ran last. A
callback runs after the step's module is imported and before its inputs are read, so it can
size the process for this step from its ``weight=``.

The registry is per worker process, not per module: a callback runs for every step the worker
runs after the module that registered it was imported, whichever file the step is in.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class StepStart:
    """The step a worker is about to run, as ``barca.on_step_start`` callbacks see it."""

    #: The step's node id, with its partition key when it has one (``pipeline.py:orders[day=1]``).
    node_id: str
    #: The step's ``weight=``, 1 unless the decorator set it.
    weight: int


_callbacks: list[Callable[[StepStart], Any]] = []


def on_step_start(callback: Callable[[StepStart], Any]) -> Callable[[StepStart], Any]:
    """Register ``callback``; registering the same function again does nothing."""
    if not callable(callback):
        kind = type(callback).__name__
        raise TypeError(f"on_step_start takes a function of one StepStart, not {kind}")
    if callback not in _callbacks:
        _callbacks.append(callback)
    return callback


def step_start(step: dict[str, Any]) -> StepStart:
    """The ``StepStart`` for a step frame; a frame without ``weight`` has the default, 1."""
    return StepStart(node_id=step.get("node_id", "unknown"), weight=int(step.get("weight", 1)))


def registered() -> bool:
    """Whether any callback is registered, so the worker can skip the call when none is."""
    return bool(_callbacks)


def run_step_start(step: dict[str, Any]) -> None:
    """Call every registered callback, in registration order. One that raises fails the step."""
    started = step_start(step)
    for callback in list(_callbacks):
        callback(started)
