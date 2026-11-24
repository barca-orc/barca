"""A batched node's function is never called with a single key (RFC-0013).

The coordinator sends the keys of a batched node with their call. Should a step of one reach
the worker without it, the worker makes a call of that one key: the function gets a list.
"""

import textwrap

from barca._artifacts import deserialize

from .daemon_steps import run_steps


def test_a_batched_step_without_a_call_gets_a_list_and_reports_its_batch(tmp_path, capsys):
    module = tmp_path / "pipe.py"
    module.write_text(
        textwrap.dedent(
            """
            from pathlib import Path

            def scored(retailer):
                Path(__file__).with_name("seen.txt").open("a").write(type(retailer).__name__ + "\\n")
                return {r: {"retailer": r} for r in retailer}
            """
        )
    )
    reports = run_steps(
        [
            {
                "node_id": "pipe.py:scored",
                "function_name": "scored",
                "source_file": str(module),
                "kind": "asset",
                "batched": True,
                "partition_keys": [{"retailer": "r1"}, {"retailer": "r2"}],
            }
        ],
        art_dir=tmp_path / "art",
    )

    assert [r["type"] for r in reports] == ["result", "result"], reports
    assert (tmp_path / "seen.txt").read_text().split() == ["list", "list"]
    for report, key in zip(reports, ["r1", "r2"], strict=True):
        assert report["node_id"] == f"pipe.py:scored[retailer={key}]"
        artifact = report["artifact"]
        assert artifact["batch_keys"] == 1 and artifact["batch"]
        assert deserialize(artifact["path"], artifact["format"]) == {"retailer": key}
    # Said once per key, so the gap is seen.
    assert capsys.readouterr().err.count("arrived with no call") == 2


def test_a_step_that_is_not_batched_still_gets_its_key(tmp_path):
    module = tmp_path / "pipe.py"
    module.write_text("def plain(retailer):\n    return {'type': type(retailer).__name__}\n")
    reports = run_steps(
        [
            {
                "node_id": "pipe.py:plain",
                "function_name": "plain",
                "source_file": str(module),
                "kind": "asset",
                "partition_keys": [{"retailer": "r1"}],
            }
        ],
        art_dir=tmp_path / "art",
    )
    artifact = reports[0]["artifact"]
    assert deserialize(artifact["path"], artifact["format"]) == {"type": "str"}
