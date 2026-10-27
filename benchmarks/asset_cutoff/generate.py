"""Write a global asset that every customer reads, and a two-asset chain partitioned by customer.

    python generate.py N DEST [cutoff|plain]

`feed` is a sensor over `rate.txt`; `rates` reads it and rounds it, so most moves of the feed
rerun `rates` and write the same bytes. `billed` and `invoice` have one key per customer (N),
from `partitions_from(customers)`.

cutoff (the default): `@asset(cutoff=True)` on `rates`, so a rerun that writes the same bytes
leaves every key below it cached. plain: no cutoff, every key reruns whenever `rates` does.

`bump.py` in DEST moves the feed by 0.001 (the same rounded rate).
"""

import pathlib
import sys

n = int(sys.argv[1])
out = pathlib.Path(sys.argv[2])
mode = sys.argv[3] if len(sys.argv) > 3 else "cutoff"
out.mkdir(parents=True, exist_ok=True)
(out / "rate.txt").write_text("0.9100")
(out / "bump.py").write_text(
    "import pathlib\n"
    "p = pathlib.Path(__file__).with_name('rate.txt')\n"
    "p.write_text(f'{float(p.read_text()) + 0.001:.4f}')\n"
)
cutoff = ", cutoff=True" if mode == "cutoff" else ""
(out / "pipeline.py").write_text(
    f"""import pathlib

from barca import asset, partitions_from, sensor

HERE = pathlib.Path(__file__).parent


@sensor()
def feed() -> tuple[bool, float]:
    return True, float((HERE / "rate.txt").read_text())


@asset()
def customers() -> list:
    return [f"c{{i:06d}}" for i in range({n})]


@asset(inputs={{"raw": feed}}{cutoff})
def rates(raw: float) -> dict:
    return {{"usd_eur": round(raw, 1)}}


@asset(inputs={{"rates": rates}}, partitions={{"customer": partitions_from(customers)}})
def billed(customer: str, rates: dict) -> dict:
    return {{"customer": customer, "eur": 100 * rates["usd_eur"]}}


@asset(partitions={{"customer": partitions_from(billed)}})
def invoice(customer: str, billed: dict) -> str:
    return f"{{customer}}: {{billed['eur']:.2f}}"
"""
)
