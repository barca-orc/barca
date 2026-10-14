"""Exercise an installed wheel and standalone CLI in a compiler-free runtime."""

import argparse
import importlib.metadata
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--native", type=Path, required=True)
    args = parser.parse_args()
    import barca

    assert barca.__version__ == importlib.metadata.version("barca") == args.version
    assert Path(barca.__file__).resolve().is_relative_to(Path(sys.prefix).resolve())
    assert all(not shutil.which(tool) for tool in ("cargo", "rustc", "cc", "gcc", "clang"))
    wheel_cli = Path(shutil.which("barca"))
    env = {k: v for k, v in os.environ.items() if not k.startswith(("BARCA_", "DD_"))}
    env.pop("PYTHONPATH", None)
    env["BARCA_REMOTE"] = "off"
    source = """from pathlib import Path
from barca import asset
Path("imported").touch()
@asset()
def producer() -> int: return 41
@asset(inputs={"value": producer})
def answer(value: int) -> int: return value + 1
"""
    for cli in (wheel_cli, args.native.resolve()):
        assert subprocess.check_output([str(cli), "--version"], text=True).strip() == (
            f"barca {args.version}"
        )
        with tempfile.TemporaryDirectory(prefix="barca-package-smoke-") as temporary:
            project = Path(temporary)
            (project / "barca.toml").write_text("")
            (project / "pipeline.py").write_text(source)

            def command(*arguments):
                result = subprocess.run(
                    [str(cli), *arguments, "--json"],
                    cwd=project,
                    env=env,
                    text=True,
                    capture_output=True,
                    timeout=30,
                )
                assert result.returncode == 0, (result.stdout, result.stderr)
                return json.loads(result.stdout)

            preview = command("get", "answer", "pipeline.py", "--dry-run")
            assert preview["summary"]["will_run"] == 2
            assert not (project / "imported").exists()
            hashes = []
            for count in (2, 0):
                result = command("get", "answer", "pipeline.py")
                assert result["status"] == "success" and result["final_output"] == 42
                assert result["steps_executed"] == count, result
                hashes.append([(step["id"], step["run_hash"]) for step in result["steps"]])
            assert hashes[0] == hashes[1]
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            server = subprocess.Popen(
                [str(cli), "serve", "pipeline.py", "--no-schedule", "--port", str(port)],
                cwd=project,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            try:
                base = f"http://127.0.0.1:{port}"
                deadline = time.monotonic() + 15
                while True:
                    try:
                        with urllib.request.urlopen(f"{base}/health", timeout=1) as reply:
                            health = json.load(reply)
                        break
                    except (OSError, urllib.error.URLError):
                        assert server.poll() is None and time.monotonic() < deadline
                        time.sleep(0.05)
                assert health["version"] == args.version and health["status"] == "ok"
                assert not health["scheduler"]
                with urllib.request.urlopen(f"{base}/ui/", timeout=3) as reply:
                    html = reply.read().decode()
                assert '<div id="root">' in html
                assets = [
                    path
                    for path in re.findall(r'(?:src|href)="([^"]+)"', html)
                    if "assets/" in path
                ]
                assert assets, html
                for asset in assets:
                    with urllib.request.urlopen(
                        urllib.parse.urljoin(f"{base}/ui/", asset), timeout=3
                    ) as reply:
                        assert reply.status == 200 and reply.read()
            finally:
                server.terminate()
                try:
                    stdout, stderr = server.communicate(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    stdout, stderr = server.communicate()
                assert server.returncode == 0, (stdout, stderr)
        print(f"{cli.name}: installed pipeline, cache, health and embedded UI verified")


if __name__ == "__main__":
    main()
