"""Twenty-two marts over the grouped pipeline's summary: with them the project has more than
thirty nodes, so All pipelines draws a map of files, and marts.py is one group, which opens at once.
"""

from barca import asset, group
from grouped import summary

@asset(inputs={"s": summary})
def mart_00(s):
    return {"folds": s["folds"], "mart": "mart_00"}


@asset(inputs={"s": summary})
def mart_01(s):
    return {"folds": s["folds"], "mart": "mart_01"}


@asset(inputs={"s": summary})
def mart_02(s):
    return {"folds": s["folds"], "mart": "mart_02"}


@asset(inputs={"s": summary})
def mart_03(s):
    return {"folds": s["folds"], "mart": "mart_03"}


@asset(inputs={"s": summary})
def mart_04(s):
    return {"folds": s["folds"], "mart": "mart_04"}


@asset(inputs={"s": summary})
def mart_05(s):
    return {"folds": s["folds"], "mart": "mart_05"}


@asset(inputs={"s": summary})
def mart_06(s):
    return {"folds": s["folds"], "mart": "mart_06"}


@asset(inputs={"s": summary})
def mart_07(s):
    return {"folds": s["folds"], "mart": "mart_07"}


@asset(inputs={"s": summary})
def mart_08(s):
    return {"folds": s["folds"], "mart": "mart_08"}


@asset(inputs={"s": summary})
def mart_09(s):
    return {"folds": s["folds"], "mart": "mart_09"}


@asset(inputs={"s": summary})
def mart_10(s):
    return {"folds": s["folds"], "mart": "mart_10"}


@asset(inputs={"s": summary})
def mart_11(s):
    return {"folds": s["folds"], "mart": "mart_11"}


@asset(inputs={"s": summary})
def mart_12(s):
    return {"folds": s["folds"], "mart": "mart_12"}


@asset(inputs={"s": summary})
def mart_13(s):
    return {"folds": s["folds"], "mart": "mart_13"}


@asset(inputs={"s": summary})
def mart_14(s):
    return {"folds": s["folds"], "mart": "mart_14"}


@asset(inputs={"s": summary})
def mart_15(s):
    return {"folds": s["folds"], "mart": "mart_15"}


@asset(inputs={"s": summary})
def mart_16(s):
    return {"folds": s["folds"], "mart": "mart_16"}


@asset(inputs={"s": summary})
def mart_17(s):
    return {"folds": s["folds"], "mart": "mart_17"}


@asset(inputs={"s": summary})
def mart_18(s):
    return {"folds": s["folds"], "mart": "mart_18"}


@asset(inputs={"s": summary})
def mart_19(s):
    return {"folds": s["folds"], "mart": "mart_19"}


@asset(inputs={"s": summary})
def mart_20(s):
    return {"folds": s["folds"], "mart": "mart_20"}


@asset(inputs={"s": summary})
def mart_21(s):
    return {"folds": s["folds"], "mart": "mart_21"}


marts = group(
    "Marts",
    members=[mart_00, mart_01, mart_02, mart_03, mart_04, mart_05, mart_06, mart_07, mart_08, mart_09, mart_10, mart_11, mart_12, mart_13, mart_14, mart_15, mart_16, mart_17, mart_18, mart_19, mart_20, mart_21],
    output=mart_00,
)
