"""Python spans for a step, inside the coordinator's Datadog trace.

With `BARCA_TELEMETRY=datadog` the coordinator sends each step the context of its span
(`step["datadog"]`). When `ddtrace` is installed, the worker opens a `barca.execute` span
under it: same trace, same service (`DD_SERVICE`, default `barca`), resource the node's short
name. User spans (`tracer.trace(...)`) nest inside it.

Library spans are opt-in. ddtrace's own switches choose which libraries are patched:
`DD_PATCH_MODULES=sqlite3:true,requests:true` or `DD_TRACE_<LIB>_ENABLED=true`;
`BARCA_TRACE_LIBRARIES=1` patches every library ddtrace supports. Patched library spans keep
`DD_SERVICE` instead of becoming services of their own: the worker sets
`DD_TRACE_REMOVE_INTEGRATION_SERVICE_NAMES_ENABLED=true` (unless it is set) before ddtrace is
imported.
"""

import os

_tracer = None
_checked = False

_TRUE = ("1", "true")


def _on(name: str) -> bool:
    return os.environ.get(name, "").strip().lower() in _TRUE


def _libraries(monkey) -> "dict[str, bool] | None":
    """The integrations to patch: `None` for all of them, else `{name: True}`."""
    if _on("BARCA_TRACE_LIBRARIES"):
        return None
    wanted: dict[str, bool] = {}
    try:
        from ddtrace.internal.settings.integration import _integration_env_var_id
    except Exception:

        def _integration_env_var_id(name: str) -> str:
            return name.upper().replace("-", "_")

    for module in getattr(monkey, "PATCH_MODULES", {}):
        variable = "DD_TRACE_%s_ENABLED" % _integration_env_var_id(module)
        if variable in os.environ:
            wanted[module] = _on(variable)
    # `name:true,name:false`, separated by commas or spaces, as ddtrace reads it.
    raw = os.environ.get("DD_PATCH_MODULES", "")
    for item in raw.replace(" ", ",").split(","):
        name, _, value = item.partition(":")
        if name.strip():
            wanted[name.strip()] = value.strip().lower() in _TRUE
    return {name: True for name, on in wanted.items() if on}


def _get_tracer(service: str):
    global _tracer, _checked
    if not _checked:
        _checked = True
        try:
            # ddtrace reads these when it is imported: one service for every span.
            os.environ.setdefault("DD_SERVICE", service)
            os.environ.setdefault("DD_TRACE_REMOVE_INTEGRATION_SERVICE_NAMES_ENABLED", "true")
            import ddtrace
            from ddtrace import _monkey
            from ddtrace.trace import tracer

            libraries = _libraries(_monkey)
            if libraries is None:
                patch_all = getattr(_monkey, "_patch_all", None)
                if patch_all is not None:
                    patch_all()
                else:  # ddtrace 3: the public, not yet deprecated name
                    ddtrace.patch_all()
            elif libraries:
                ddtrace.patch(raise_errors=False, **libraries)
            _tracer = tracer
        except Exception:
            # Python tracing is optional and must never prevent execution.
            pass
    return _tracer


class Execution:
    def __init__(self, step):
        self.span = None
        self.tracer = None
        self.previous = None
        context = step.get("datadog")
        if not context:
            return
        try:
            service = context.get("service") or "barca"
            tracer = _get_tracer(service)
            if tracer is None:
                return
            from ddtrace.trace import Context

            self.tracer = tracer
            self.previous = tracer.context_provider.active()
            trace_id = (int(context.get("trace_id_high", 0)) << 64) | int(context["trace_id"])
            self.span = tracer.start_span(
                "barca.execute",
                child_of=Context(
                    trace_id=trace_id,
                    span_id=context["parent_id"],
                    # Keep: a job's trace is not a sampled request (as the coordinator's spans).
                    sampling_priority=2,
                    meta={"_dd.p.dm": "-4"},
                ),
                service=service,
                resource=context.get("resource") or step.get("node_id", "unknown"),
                span_type="worker",
                activate=True,
            )
            self.span.set_tags(context.get("tags") or {})
            self.span.set_tag("component", "python")
            self.span.set_tag("span.kind", "internal")
            self.span.set_metric("_dd.measured", 1)
            self.span.set_metric("barca.attempt", context.get("attempt", 1))
        except Exception:
            self.finish()

    def finish(self, exc=None):
        span, self.span = self.span, None
        tracer = self.tracer
        if span is None or tracer is None:
            return
        try:
            if exc is not None:
                span.set_exc_info(type(exc), exc, exc.__traceback__)
            span.set_tag("barca.outcome", "failed" if exc is not None else "ran")
            span.finish()
        except Exception:
            pass
        finally:
            try:
                tracer.context_provider.activate(self.previous)
                # The coordinator may kill this worker immediately after its completion
                # message (also on retries and parallel replacements). Deliver first.
                tracer.flush()
            except Exception:
                pass
