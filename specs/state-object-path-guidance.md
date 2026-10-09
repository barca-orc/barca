# Shared state file/object path guidance (#298)

Keep BARCA_STATE_URI as the exact shared history object, distinct from the
artifact-prefix BARCA_REMOTE_URI. Do not normalize a directory into a new object
or add any listing/permission requirement. When the existing state helper reports
IsADirectoryError, reject with existing infrastructure exit3 and explain that the
setting must name a file/object, with local and provider object-path examples.
Preserve the directory and local history; no user module executes after failed
startup. Retain existing state-off recovery guidance and diagnostic sanitization.

Regression evidence must exercise actual CLI/helper startup for a local directory
and file:// directory, asserting no user import/work and unchanged directory.
Provider-shaped URI error formatting is tested directly without claiming cloud
acceptance. Preserve ordinary existing-object pull/push and CLI contract shapes.

Actual directory rejection reproduces the old missing-setting diagnostic, then
passes both plain/file cases with preserved directory contents and no user import.
All 53 state helper/pull/object-path actual checks pass on release main f881a6d,
as does the provider-shaped formatter regression, strict all-target Clippy and
fmt. Rebased onto recorder main13a92a4 for exact-head CI acceptance.
