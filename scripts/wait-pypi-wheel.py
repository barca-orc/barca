"""Wait for an exact Linux wheel on both official PyPI metadata surfaces."""

import argparse
import json
from time import monotonic, sleep
import urllib.error
import urllib.parse
import urllib.request

MAX_WAIT = 120
MAX_BYTES = 4 * 1024 * 1024


def fetch_json(url, timeout):
    accept = "application/json" if url.endswith("/json") else "application/vnd.pypi.simple.v1+json"
    request = urllib.request.Request(url, headers={"Accept": accept})
    with urllib.request.urlopen(request, timeout=min(10, timeout)) as response:
        data = response.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError("PyPI metadata exceeds the bounded response size")
    value = json.loads(data)
    if not isinstance(value, dict):
        raise ValueError("PyPI returned invalid project metadata")
    return value


def matches(filename, version, architecture, libc):
    if not filename.startswith(f"barca-{version}-") or not filename.endswith(".whl"):
        return False
    platforms = filename.rsplit("-", 1)[-1][:-4].split(".")
    prefix = "manylinux" if libc == "gnu" else "musllinux_1_2_"
    return any(tag.startswith(prefix) and tag.endswith(f"_{architecture}") for tag in platforms)


def wait_for_wheel(version, architecture, libc):
    deadline = monotonic() + MAX_WAIT
    last = "the requested architecture/libc wheel is not listed"
    while monotonic() < deadline:
        try:
            release = fetch_json(
                f"https://pypi.org/pypi/barca/{urllib.parse.quote(version, safe='')}/json",
                deadline - monotonic(),
            )
            if release.get("info", {}).get("version") != version:
                raise ValueError("PyPI release metadata does not match the exact tagged version")
            candidates = {
                file["filename"]
                for file in release.get("urls", [])
                if matches(file.get("filename", ""), version, architecture, libc)
                and not file.get("yanked", False)
            }
            if (
                any(
                    matches(file.get("filename", ""), version, architecture, libc)
                    and file.get("yanked", False)
                    for file in release.get("urls", [])
                )
                and not candidates
            ):
                raise ValueError("the requested PyPI wheel is yanked")
            if candidates and monotonic() < deadline:
                index = fetch_json("https://pypi.org/simple/barca/", deadline - monotonic())
                visible = {
                    file.get("filename")
                    for file in index.get("files", [])
                    if not file.get("yanked", False)
                }
                if any(
                    file.get("filename") in candidates and file.get("yanked", False)
                    for file in index.get("files", [])
                ):
                    raise ValueError("the requested simple-index wheel is yanked")
                ready = sorted(candidates & visible)
                if ready:
                    return ready[0]
                last = "the wheel is uploaded but not yet visible in the official simple index"
        except urllib.error.HTTPError as error:
            if error.code not in (404, 429) and error.code < 500:
                raise
            last = f"PyPI availability returned HTTP {error.code}"
        except (urllib.error.URLError, TimeoutError) as error:
            last = f"PyPI availability request failed: {error}"
        remaining = deadline - monotonic()
        if remaining > 0:
            sleep(min(2, remaining))
    raise TimeoutError(f"PyPI wheel availability exceeded {MAX_WAIT}s: {last}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--architecture", choices=("x86_64", "aarch64"), required=True)
    parser.add_argument("--libc", choices=("gnu", "musl"), required=True)
    args = parser.parse_args()
    print(wait_for_wheel(args.version, args.architecture, args.libc))


if __name__ == "__main__":
    main()
