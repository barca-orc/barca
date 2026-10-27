"""`BARCA_SLACK_WEBHOOK_URL`: a Slack post when a job starts failing, recovers, or is still
failing a day later (RFC-0010).

A local HTTP server stands in for the Slack incoming webhook and records what it is sent.
`BARCA_TEST_SLACK_ALLOW_HTTP` lets barca post to it over plain HTTP, and
`BARCA_TEST_SLACK_REMIND_SECS` shortens the 24-hour reminder; both are test hooks.
"""

import json
import os
import signal
import socket
import sqlite3
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest
from barca.api import _find_binary

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "DD_", "SLACK_")

PIPELINE = """
import time
from pathlib import Path

from barca import asset, task


@task()
def nightly() -> None:
    if Path("fail").exists():
        raise KeyError("region")


@task()
def slow() -> None:
    time.sleep(30)


@asset()
def orders() -> dict:
    return {"rows": 3}
"""

JOB = "pipeline.py:nightly"


class FakeWebhook:
    """Records every POST; answers with `status`, or not at all while `hang` is set."""

    def __init__(self):
        self.posts: list[dict] = []
        self.status = 200
        self.hang = False
        hook = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                if hook.hang:
                    time.sleep(5)
                    return
                if hook.status == 200:
                    hook.posts.append(
                        {
                            "path": self.path,
                            "headers": {k.lower(): v for k, v in self.headers.items()},
                            "body": json.loads(body),
                        }
                    )
                self.send_response(hook.status)
                if hook.status == 429:
                    self.send_header("Retry-After", "30")
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b"ok")

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_port}/services/T000/B000/SECRETTOKEN"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()

    def headlines(self) -> list[str]:
        return [p["body"]["text"].splitlines()[0] for p in self.posts]


@pytest.fixture
def hook():
    fake = FakeWebhook()
    yield fake
    fake.close()


@pytest.fixture
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def base_env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}


def slack(hook: FakeWebhook, **extra: str) -> dict[str, str]:
    return {
        "BARCA_SLACK_WEBHOOK_URL": hook.url,
        "BARCA_TEST_SLACK_ALLOW_HTTP": "1",
        **extra,
    }


def cli(cwd: Path, *args: str, **env: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**base_env(), **env},
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )


def nightly(project: Path, ok: bool, **env: str) -> subprocess.CompletedProcess:
    """One `barca run` of the job, succeeding or failing."""
    flag = project / "fail"
    if ok:
        flag.unlink(missing_ok=True)
    else:
        flag.write_text("")
    proc = cli(project, "run", "nightly", "--json", **env)
    assert proc.returncode == (0 if ok else 1), proc.stderr
    return proc


def test_success_failure_failure_success_posts_failing_and_recovered(project, hook):
    for ok in (True, False, False, True):
        nightly(project, ok, **slack(hook))
    assert hook.headlines() == [f"🔴 {JOB} is failing", f"✅ {JOB} recovered"]
    recovered = hook.posts[1]["body"]["text"]
    assert "Failing since " in recovered and "UTC (2 runs)" in recovered, recovered


def test_the_post_is_block_kit_with_a_text_fallback_and_no_traceback(project, hook):
    nightly(project, False, **slack(hook, BARCA_UI_URL="https://barca.example.com/ui/"))
    (post,) = hook.posts
    assert post["headers"]["content-type"] == "application/json"
    body = post["body"]
    lines = body["text"].splitlines()
    assert lines[0] == f"🔴 {JOB} is failing"
    assert lines[1].startswith(f"Step {JOB} failed after 1 attempt in "), lines
    assert lines[2] == "KeyError: 'region'", lines
    assert "· run " in lines[3]
    run_id = lines[3].rsplit(" ", 1)[1]
    assert lines[4] == f"barca history {run_id}"
    assert "Traceback" not in json.dumps(body)
    blocks = body["blocks"]
    assert [b["type"] for b in blocks] == ["section", "section", "context"]
    for block in blocks[:2]:
        assert block["text"]["type"] == "mrkdwn" and block["text"]["text"]
    for element in blocks[2]["elements"]:
        assert element["type"] == "mrkdwn" and element["text"]
    link = f"<https://barca.example.com/ui/#/runs?run={run_id}|{run_id}>"
    assert link in blocks[2]["elements"][0]["text"]


@pytest.mark.parametrize(
    ("mode", "expected"),
    [
        ("failures", ["is failing", "is still failing", "recovered"]),
        ("all", ["succeeded", "is failing", "is still failing", "recovered", "succeeded"]),
    ],
)
def test_failures_and_all_post_what_the_table_says(project, hook, mode, expected):
    for ok in (True, False, False, True, True):
        nightly(project, ok, **slack(hook, BARCA_SLACK_NOTIFY=mode))
    assert [h.split(" ", 2)[2] for h in hook.headlines()] == expected


def test_a_cached_run_posts_nothing_even_in_all_mode(project, hook):
    env = slack(hook, BARCA_SLACK_NOTIFY="all")
    first = cli(project, "get", "orders", "--json", **env)
    assert first.returncode == 0, first.stderr
    assert hook.headlines() == ["✅ pipeline.py:orders succeeded"]
    cached = cli(project, "get", "orders", "--json", **env)
    assert cached.returncode == 0, cached.stderr
    assert len(hook.posts) == 1


def test_a_cancelled_run_posts_nothing(project, hook):
    nightly(project, False, **slack(hook))
    proc = subprocess.Popen(
        [_find_binary(), "run", "slow", "--json"],
        cwd=project,
        env={**base_env(), **slack(hook)},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    time.sleep(3)
    proc.send_signal(signal.SIGINT)
    _, err = proc.communicate(timeout=60)
    assert proc.returncode == 130, err
    assert hook.headlines() == [f"🔴 {JOB} is failing"]


def test_an_interrupted_run_does_not_change_the_job_state(project, hook):
    nightly(project, False, **slack(hook))
    # What `barca history --reconcile` records for a run whose process died.
    db = sqlite3.connect(project / ".barca" / "metadata.db")
    try:
        with db:
            db.execute(
                "INSERT INTO runs (run_id, command, files, status, job) "
                "VALUES ('dead00000000', 'run', '[]', 'interrupted', ?)",
                (JOB,),
            )
    finally:
        db.close()
    nightly(project, False, **slack(hook))
    assert len(hook.posts) == 1
    nightly(project, True, **slack(hook))
    assert hook.headlines() == [f"🔴 {JOB} is failing", f"✅ {JOB} recovered"]


@pytest.mark.parametrize(
    ("status", "hang", "problem"),
    [(404, False, "HTTP 404"), (429, False, "HTTP 429"), (200, True, "no reply within 3s")],
)
def test_a_failing_webhook_warns_once_without_the_url_and_the_transition_is_retried(
    project, hook, status, hang, problem
):
    hook.status, hook.hang = status, hang
    proc = nightly(project, False, **slack(hook))
    assert proc.stderr.count("[barca] warning: telemetry 'slack' did not receive run") == 1
    assert problem in proc.stderr, proc.stderr
    assert "SECRETTOKEN" not in proc.stderr and "127.0.0.1" not in proc.stderr, proc.stderr
    assert hook.posts == []
    # The webhook is back: the failure that was never announced is posted now.
    hook.status, hook.hang = 200, False
    proc = nightly(project, False, **slack(hook))
    assert "telemetry" not in proc.stderr, proc.stderr
    assert hook.headlines() == [f"🔴 {JOB} is failing"]


def test_an_unreachable_https_webhook_warns_with_the_category_only(project):
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    url = f"https://127.0.0.1:{port}/services/T000/B000/SECRETTOKEN"
    proc = nightly(project, False, BARCA_SLACK_WEBHOOK_URL=url)
    assert "telemetry 'slack' did not receive run" in proc.stderr, proc.stderr
    assert "connection failed" in proc.stderr, proc.stderr
    assert "SECRETTOKEN" not in proc.stderr and str(port) not in proc.stderr


def test_a_webhook_that_is_not_https_is_a_warning_and_is_not_printed(project, hook):
    url = "http://hooks.slack.com/services/T000/B000/SECRETTOKEN"
    proc = nightly(project, False, BARCA_SLACK_WEBHOOK_URL=url)
    assert (
        "telemetry 'slack' is off: BARCA_SLACK_WEBHOOK_URL is not an https:// URL" in proc.stderr
    ), proc.stderr
    assert "SECRETTOKEN" not in proc.stderr


def test_a_generic_slack_webhook_url_posts_nothing(project, hook):
    proc = nightly(project, False, SLACK_WEBHOOK_URL=hook.url, BARCA_TEST_SLACK_ALLOW_HTTP="1")
    assert hook.posts == []
    assert "telemetry" not in proc.stderr, proc.stderr


def test_slack_in_barca_telemetry_is_unknown_and_names_the_variable(project, hook):
    proc = nightly(project, True, BARCA_TELEMETRY="slack")
    assert "unknown telemetry integration 'slack'" in proc.stderr, proc.stderr
    assert "BARCA_SLACK_WEBHOOK_URL" in proc.stderr
    assert hook.posts == []


def test_unset_or_empty_is_off_and_silent(project, hook):
    proc = nightly(project, False, BARCA_SLACK_WEBHOOK_URL="")
    assert "telemetry" not in proc.stderr, proc.stderr
    assert hook.posts == []


def test_under_serve_a_failing_job_posts_once_then_a_reminder(tmp_path, hook):
    # Fails every second-tick, slower than the schedule, so some ticks are skipped too.
    (tmp_path / "pipeline.py").write_text(
        "import time\n"
        "from barca import task, Schedule\n\n\n"
        '@task(freshness=Schedule("* * * * * *"))\n'
        "def heartbeat() -> None:\n"
        "    time.sleep(1.5)\n"
        '    raise KeyError("region")\n'
    )
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port)],
        cwd=tmp_path,
        env={**base_env(), **slack(hook, BARCA_TEST_SLACK_REMIND_SECS="4")},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    started = time.monotonic()
    try:
        deadline = started + 60
        while (
            not any("still failing" in h for h in hook.headlines()) and time.monotonic() < deadline
        ):
            time.sleep(0.25)
        time.sleep(1)
    finally:
        serve.send_signal(signal.SIGTERM)
        try:
            _, err = serve.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            serve.kill()
            _, err = serve.communicate()
    elapsed = time.monotonic() - started

    job = "pipeline.py:heartbeat"
    headlines = hook.headlines()
    assert err.count("scheduled run") >= 3, err
    assert "skipped" in err, err
    assert headlines[0] == f"🔴 {job} is failing", headlines
    assert headlines.count(f"🔴 {job} is failing") == 1, headlines
    still = headlines.count(f"🔴 {job} is still failing")
    assert 1 <= still <= elapsed / 4 + 1, (headlines, elapsed)
    assert len(headlines) == 1 + still, headlines
