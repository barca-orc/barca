#!/usr/bin/env bash
# The checks to run before pushing: what CI's fast tier (every pull request) and merge tier
# (the merge queue) gate, locally, in a few minutes (.depot/workflows/ci.yml).
#
#   scripts/check-fast.sh                  # everything below, Python tests chosen by the diff
#   scripts/check-fast.sh --python-all     # the whole Python suite instead of the selection
#   scripts/check-fast.sh --no-python      # Rust and static checks only
#   scripts/check-fast.sh --base origin/main
#
# Steps, in order (each runs even when an earlier one failed; the summary lists them all):
#   changelog   scripts/check-changelog.py (released sections unchanged)      [CI: checks]
#   specs       scripts/check-specs.py and its self-test; the TLA+ models too when java and
#               .spec-check/tla2tools.jar are there (CI caches the jar)       [CI: checks]
#   hooks       prek run --all-files: ruff, ruff format, ty (pinned), cargo fmt, version sync,
#               uv lock. The fixers may rewrite files: review and commit what they change.
#                                                                             [CI: checks]
#   clippy      cargo clippy --workspace --all-targets -- -D warnings         [CI: clippy]
#   cargo-test  cargo nextest run --workspace + doc tests when cargo-nextest is installed,
#               else cargo test                                               [CI: rust-tests]
#   build       maturin develop --profile fast into the venv (the release optimization level
#               without LTO, incremental; what CI's wheel uses), when Rust or UI sources are
#               newer than the venv's barca binary (the Python tests run that binary). Without
#               a .venv in the tree, or when the build fails, the python step is skipped and
#               the run fails: make the venv with scripts/new-worktree.sh, or
#               `uv venv && maturin develop --uv --profile fast --extras test`.
#   python      the fast subset (scripts/fast-tests.txt, the same files CI's merge tier runs),
#               the test files changed since the base, tests that name a changed python/barca
#               module, and test_cli_contract.py / test_docs_examples.py when the CLI surface
#               or the manual changed                                         [CI: python (fast)]
#
# Not run here: CI's merge tier also runs `perf` (scripts/perf-gate.py, the performance
# budgets; the prek pre-push hook runs benchmarks/perf/profile.py) and checks the UI's generated
# types against Rust (ui/scripts/regen-types.sh). CI's full tier (a release) adds
# tests/integration/*.sh, the benchmark smoke tests, the UI checks, the whole Python suite in
# shards, the duckdb 1.x pass and the object-store emulator tests (they skip without
# BARCA_TEST_*). The minute checkpoint test runs when picked (unset
# BARCA_TEST_MINUTE_CHECKPOINT); it takes 2 min. Set BARCA_POOL_SIZE=2 to reproduce CI-only
# worker-pool failures.
set -uo pipefail

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT" || exit 2

BASE_REF=origin/main
PYTHON_MODE=selected
while [ $# -gt 0 ]; do
    case "$1" in
        --base) BASE_REF="$2"; shift 2 ;;
        --python-all) PYTHON_MODE=all; shift ;;
        --no-python) PYTHON_MODE=none; shift ;;
        -h|--help) sed -n '2,42p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
    esac
done

BASE=$(git merge-base HEAD "$BASE_REF" 2>/dev/null) || {
    echo "cannot find the merge base with $BASE_REF; run git fetch origin main" >&2
    exit 2
}

# A worktree builds into its own target dir (AGENTS.md); never share the trunk's target/.
if [ -z "${CARGO_TARGET_DIR:-}" ] && [ "$(git rev-parse --git-dir)" != "$(git rev-parse --git-common-dir)" ]; then
    export CARGO_TARGET_DIR="$ROOT/target-local"
fi

# The Python tests run the venv's barca binary; without a venv there is nothing to test against.
if [ -x "$ROOT/.venv/bin/python" ]; then
    PY="$ROOT/.venv/bin/python"
    HAVE_VENV=1
else
    PY=$(command -v python3)
    HAVE_VENV=0
fi
NO_VENV_HINT="no .venv in this tree: run scripts/new-worktree.sh, or: uv venv && maturin develop --uv --profile fast --extras test"

# Changed since the base: committed, staged, unstaged and untracked.
CHANGED=$( { git diff --name-only "$BASE"; git ls-files --others --exclude-standard; } | sort -u)
changed() { grep -qE "$1" <<<"$CHANGED"; }

RESULTS=()
FAILED=0
LOG_DIR=$(mktemp -d "${TMPDIR:-/tmp}/check-fast.XXXXXX")

step() {
    local name="$1"; shift
    local log="$LOG_DIR/$name.log" start=$SECONDS
    printf '==> %-11s %s\n' "$name" "$*" >&2
    if "$@" >"$log" 2>&1; then
        RESULTS+=("PASS  $name ($((SECONDS - start))s)")
    else
        FAILED=1
        RESULTS+=("FAIL  $name ($((SECONDS - start))s)  log: $log")
        tail -n 40 "$log" >&2
    fi
}

skip() { RESULTS+=("SKIP  $1  ($2)"); }

step changelog python3 scripts/check-changelog.py --base "$BASE"

specs() {
    python3 scripts/check-specs.py && python3 scripts/test-check-specs.py || return 1
    if java -version >/dev/null 2>&1 && [ -f .spec-check/tla2tools.jar ]; then
        python3 specs/models/check.py --jar .spec-check/tla2tools.jar
    else
        echo "TLA+ models skipped: needs java and .spec-check/tla2tools.jar"
    fi
}
step specs specs

if command -v prek >/dev/null; then
    step hooks prek run --all-files
elif command -v pre-commit >/dev/null; then
    step hooks pre-commit run --all-files
else
    skip hooks "install prek: uv tool install prek"
fi

step clippy cargo clippy --workspace --all-targets -- -D warnings

if cargo nextest --version >/dev/null 2>&1; then
    cargo_tests() { cargo nextest run --workspace --no-fail-fast && cargo test --workspace --doc; }
    step cargo-test cargo_tests
else
    step cargo-test cargo test --workspace --no-fail-fast
fi

if [ "$PYTHON_MODE" = none ]; then
    skip build "--no-python"
    skip python "--no-python"
elif [ "$HAVE_VENV" -eq 0 ]; then
    FAILED=1
    RESULTS+=("FAIL  build  ($NO_VENV_HINT)")
    RESULTS+=("SKIP  python  (no venv to run the tests against)")
    echo "$NO_VENV_HINT" >&2
else
    BIN="$(dirname "$PY")/barca"
    BUILD_OK=1
    if [ ! -x "$BIN" ] || [ -n "$(find crates ui/src Cargo.toml Cargo.lock -newer "$BIN" -type f -print -quit 2>/dev/null)" ]; then
        if command -v maturin >/dev/null || [ -x "$(dirname "$PY")/maturin" ]; then
            MATURIN=$(command -v "$(dirname "$PY")/maturin" || command -v maturin)
            step build "$MATURIN" develop --uv --profile fast --extras test
            [ "${RESULTS[-1]}" = "${RESULTS[-1]#FAIL}" ] || BUILD_OK=0
        else
            FAILED=1
            BUILD_OK=0
            RESULTS+=("FAIL  build  (the venv's barca is stale and maturin is not installed)")
        fi
    else
        skip build "the venv's barca is newer than the Rust sources"
    fi
fi
if [ "$PYTHON_MODE" != none ] && [ "$HAVE_VENV" -eq 1 ] && [ "$BUILD_OK" -eq 0 ]; then
    skip python "the venv's barca did not build; the tests would run a stale or missing binary"
    echo "python tests skipped: fix the build first" >&2
elif [ "$PYTHON_MODE" != none ] && [ "$HAVE_VENV" -eq 1 ]; then

    # The quick subset, shared with CI's merge tier (.depot/workflows/ci.yml).
    FAST=()
    while IFS= read -r f; do FAST+=("$f"); done < <(grep -v '^#' scripts/fast-tests.txt | grep .)
    SELECTED=()
    if [ "$PYTHON_MODE" = all ]; then
        SELECTED=(python/tests)
    else
        for f in "${FAST[@]}"; do
            [ -f "python/tests/$f" ] && SELECTED+=("python/tests/$f")
        done
        while IFS= read -r f; do
            [ -f "$f" ] && SELECTED+=("$f")
        done < <(grep -E '^python/tests/test_[^/]*\.py$' <<<"$CHANGED")
        # Tests that name a changed module (python/barca/_storage.py -> "_storage").
        while IFS= read -r mod; do
            case "$mod" in ''|__init__|__main__) continue ;; esac
            while IFS= read -r f; do SELECTED+=("$f"); done \
                < <(grep -lw -- "$mod" python/tests/test_*.py)
        done < <(grep -E '^python/barca/[^/]*\.py$' <<<"$CHANGED" | sed -E 's#.*/([^/]*)\.py#\1#')
        # The CLI surface and the manual: contract snapshots and the manual's examples.
        if changed '^crates/barca-cli/(src|docs|snapshots)/|^python/tests/snapshots/|^crates/barca-core/src/decorator_args\.rs'; then
            SELECTED+=(python/tests/test_cli_contract.py python/tests/test_docs_examples.py)
        fi
        UNIQUE=()
        while IFS= read -r f; do UNIQUE+=("$f"); done \
            < <(printf '%s\n' ${SELECTED[@]+"${SELECTED[@]}"} | sort -u)
        SELECTED=(${UNIQUE[@]+"${UNIQUE[@]}"})
    fi
    if "$PY" -c "import xdist" 2>/dev/null; then
        XDIST=(-n 4 --dist loadscope)
    else
        XDIST=()
    fi
    echo "python tests: ${SELECTED[*]}" >&2
    step python "$PY" -m pytest -q -p no:cacheprovider ${XDIST[@]+"${XDIST[@]}"} "${SELECTED[@]}"
fi

echo >&2
echo "check-fast summary (base $(git rev-parse --short "$BASE")):" >&2
printf '  %s\n' ${RESULTS[@]+"${RESULTS[@]}"} >&2
if [ "$FAILED" -ne 0 ]; then
    echo "FAILED" >&2
    exit 1
fi
echo "all passed" >&2
