"""Optional Python spans under the coordinator's Datadog step context."""

import os

_tracer = None
_checked = False


def _get_tracer():
    global _tracer, _checked
    if not _checked:
        _checked = True
        try:
            from ddtrace import patch_all, tracer

            patch_all()
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
            tracer = _get_tracer()
            if tracer is None:
                return
            from ddtrace.trace import Context

            self.tracer = tracer
            self.previous = tracer.context_provider.active()
            self.span = tracer.start_span(
                "barca.execute",
                child_of=Context(
                    trace_id=context["trace_id"],
                    span_id=context["parent_id"],
                    sampling_priority=1,
                ),
                service=(os.environ.get("DD_SERVICE", "").strip() or "barca") + "-python",
                resource=context["job"],
                activate=True,
            )
            self.span.set_tags(
                {
                    "barca.job": context["job"],
                    "barca.run_id": context["run_id"],
                    "barca.node": step["node_id"],
                    "barca.kind": step.get("kind", "asset"),
                }
            )
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
