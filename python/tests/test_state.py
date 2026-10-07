"""Tests for barca._state — pull/push with optimistic concurrency.

The file:// backend (sha256 tokens + lock + atomic replace) exercises the
full contract without cloud credentials; per-cloud conditional-write calls
are covered by token-extraction unit tests with mocked fs.info shapes and by
the manual smoke checklist in the PR.
"""

import json
import subprocess
import sys
from pathlib import Path

import pytest

from barca._state import ConflictError, _is_conflict, _remote_token, pull, push


@pytest.fixture
def shared(tmp_path):
    """A 'remote' state location on the local filesystem."""
    return tmp_path / "shared" / "state" / "metadata.db"


def _write(p: Path, data: bytes):
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(data)


# ─── file:// backend round trip ──────────────────────────────────────────────


class TestFileBackend:
    def test_pull_absent_returns_none(self, shared, tmp_path):
        local = tmp_path / "local.db"
        assert pull(str(shared), local) is None
        assert not local.exists()

    def test_create_then_pull_round_trip(self, shared, tmp_path):
        local = tmp_path / "local.db"
        _write(local, b"v1-bytes")

        token = push(str(shared), local, None)  # create-only
        assert shared.read_bytes() == b"v1-bytes"

        other = tmp_path / "machine-b.db"
        pulled = pull(str(shared), other)
        assert pulled == token
        assert other.read_bytes() == b"v1-bytes"

    def test_create_only_conflicts_when_exists(self, shared, tmp_path):
        _write(shared, b"already-there")
        local = tmp_path / "local.db"
        _write(local, b"new")
        with pytest.raises(ConflictError):
            push(str(shared), local, None)

    def test_stale_token_conflicts(self, shared, tmp_path):
        local = tmp_path / "local.db"
        _write(local, b"v1")
        token = push(str(shared), local, None)

        # Someone else replaces the remote.
        _write(shared, b"v2-from-elsewhere")

        _write(local, b"v3")
        with pytest.raises(ConflictError):
            push(str(shared), local, token)

    def test_fresh_token_pushes(self, shared, tmp_path):
        local = tmp_path / "local.db"
        _write(local, b"v1")
        t1 = push(str(shared), local, None)

        _write(local, b"v2")
        t2 = push(str(shared), local, t1)
        assert t2 != t1
        assert shared.read_bytes() == b"v2"

    def test_plain_path_without_scheme(self, shared, tmp_path):
        """A plain path (no file://) uses the same backend."""
        local = tmp_path / "local.db"
        _write(local, b"data")
        token = push(str(shared), local, None)
        assert token is not None
        assert pull(str(shared), tmp_path / "b.db") == token

    def test_file_uri_scheme(self, shared, tmp_path):
        local = tmp_path / "local.db"
        _write(local, b"data")
        token = push(f"file://{shared}", local, None)
        assert pull(f"file://{shared}", tmp_path / "b.db") == token


# ─── CLI contract (what the Rust coordinator drives) ─────────────────────────


class TestCliContract:
    def _run(self, *args):
        return subprocess.run(
            [sys.executable, "-m", "barca._state", *args],
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent,
        )

    def test_pull_absent(self, shared, tmp_path):
        r = self._run("pull", str(shared), str(tmp_path / "local.db"))
        assert r.returncode == 0, r.stderr
        out = json.loads(r.stdout)
        assert out == {"exists": False, "token": None}

    def test_push_create_pull_and_conflict_exit_codes(self, shared, tmp_path):
        local = tmp_path / "local.db"
        _write(local, b"v1")

        r = self._run("push", str(shared), str(local))
        assert r.returncode == 0, r.stderr
        token = json.loads(r.stdout)["token"]

        r = self._run("pull", str(shared), str(tmp_path / "b.db"))
        assert r.returncode == 0
        assert json.loads(r.stdout) == {"exists": True, "token": token}

        # Stale token → exit 3.
        _write(shared, b"changed-behind-our-back")
        r = self._run("push", str(shared), str(local), "--token", token)
        assert r.returncode == 3
        assert "conflict" in r.stderr.lower()

        # Create-only when exists → exit 3.
        r = self._run("push", str(shared), str(local))
        assert r.returncode == 3

    def test_bad_usage_exit_1(self):
        r = self._run("frobnicate")
        assert r.returncode == 1


# ─── Token extraction across fs.info shapes ──────────────────────────────────


class TestRemoteToken:
    def _with_info(self, monkeypatch, info):
        class FakeFs:
            def info(self, uri):
                if info is None:
                    raise FileNotFoundError(uri)
                return info

        monkeypatch.setattr("barca._state._storage.get_fs", lambda uri: FakeFs())

    def test_adlfs_etag(self, monkeypatch):
        self._with_info(monkeypatch, {"etag": '"0x8DC123"', "size": 10})
        assert _remote_token("abfss://c@a.dfs.core.windows.net/x") == '"0x8DC123"'

    def test_s3fs_etag(self, monkeypatch):
        self._with_info(monkeypatch, {"ETag": '"abc123"', "size": 10})
        assert _remote_token("s3://b/x") == '"abc123"'

    def _with_gcs_blob(self, monkeypatch, generation):
        """Stub the GCS SDK: bucket.get_blob(name) -> object with .generation,
        or None when generation is None (absent object)."""

        class FakeFetched:
            pass

        class FakeBucket:
            def get_blob(self, name):
                if generation is None:
                    return None
                f = FakeFetched()
                f.generation = generation
                return f

        class FakeBlob:
            name = "x"
            bucket = FakeBucket()

        monkeypatch.setattr("barca._state._gcs_blob", lambda uri: FakeBlob())

    def test_gcs_token_is_sdk_generation(self, monkeypatch):
        """GCS conditions on the numeric generation, read via the SDK — never
        the base64 etag, which int(if_generation_match) could not parse."""
        self._with_gcs_blob(monkeypatch, 1712345)
        assert _remote_token("gs://b/x") == "1712345"

    def test_gcs_token_absent(self, monkeypatch):
        self._with_gcs_blob(monkeypatch, None)
        assert _remote_token("gs://b/x") is None

    def test_absent_object(self, monkeypatch):
        self._with_info(monkeypatch, None)
        assert _remote_token("s3://b/x") is None

    def test_missing_token_key_raises(self, monkeypatch):
        self._with_info(monkeypatch, {"size": 10})
        with pytest.raises(RuntimeError, match="no etag in object info"):
            _remote_token("s3://b/x")


# ─── S3 size guard ───────────────────────────────────────────────────────────


class TestS3SizeGuard:
    def test_oversized_state_blob_hard_errors(self, tmp_path, monkeypatch):
        monkeypatch.setattr("barca._state._S3_SINGLE_PUT_LIMIT", 4)
        local = tmp_path / "local.db"
        _write(local, b"12345")  # over the (patched) limit
        with pytest.raises(RuntimeError, match="larger than the single-request"):
            push("s3://bucket/state.db", local, "token")


# ─── Conflict classification ─────────────────────────────────────────────────


class TestConflictClassification:
    def test_names(self):
        class ResourceModifiedError(Exception):
            pass

        class PreconditionFailed(Exception):
            pass

        assert _is_conflict(ResourceModifiedError("x"))
        assert _is_conflict(PreconditionFailed("x"))

    def test_text_markers(self):
        assert _is_conflict(Exception("An error occurred (PreconditionFailed)"))
        assert _is_conflict(Exception("HTTP 412: precondition"))
        assert not _is_conflict(Exception("connection refused"))

    def test_file_exists_error_is_conflict(self):
        """adlfs raises FileExistsError for mode='create' against an existing
        blob — a concurrent first-push race, which must classify as a conflict
        so the coordinator re-pulls and replays instead of hard-failing."""
        assert _is_conflict(FileExistsError("barca-state/metadata.db"))
