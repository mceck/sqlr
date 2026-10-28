#!/bin/sh
# Runs the workspace test suite under `cargo llvm-cov` and writes a report.
#
#   scripts/coverage.sh            full suite; starts and stops the Docker services
#   scripts/coverage.sh --fast     no Docker; UI and unit tests only
#   scripts/coverage.sh --open     open the HTML report when done
#   scripts/coverage.sh --no-check report only, do not fail under the threshold
#
# The threshold is per crate and checked from the line counts of every source file:
# `sqlr-driver-hana` is excluded, as requested.
set -eu
cd "$(dirname "$0")/.."

OUT="${SQLR_COVERAGE_DIR:-target/coverage}"
MIN="${SQLR_COVERAGE_MIN:-80}"
FAST=0
OPEN=0
CHECK=1
PROFILE=containers

for arg in "$@"; do
    case "$arg" in
        --fast) FAST=1; PROFILE=default; CHECK=0 ;;
        --open) OPEN=1 ;;
        --check) CHECK=1 ;;
        --no-check) CHECK=0 ;;
        --min=*) MIN="${arg#--min=}" ;;
        -h|--help)
            sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "coverage: unknown option $arg" >&2
            exit 2
            ;;
    esac
done

if [ -z "${SQLR_COVERAGE_DIR:-}" ]; then
    OUT="target/coverage"
fi
mkdir -p "$OUT"

for tool in cargo-nextest cargo-llvm-cov; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "coverage: installing $tool"
        cargo install --locked "$tool"
    fi
done

if [ "$FAST" -eq 1 ]; then
    export SQLR_SKIP_CONTAINER_TESTS=1
else
    # Coverage must exercise every driver, including the container-only ones.
    scripts/test-services.sh up
    # Stop the databases however the tests end.
    trap 'scripts/test-services.sh down' EXIT INT TERM
    export SQLR_TEST_POSTGRES=127.0.0.1:55432 \
        SQLR_TEST_POSTGRES_GLOBAL=127.0.0.1:55433 \
        SQLR_TEST_MYSQL=127.0.0.1:53306 \
        SQLR_TEST_MARIADB=127.0.0.1:53307 \
        SQLR_TEST_MYSQL_GLOBAL=127.0.0.1:53310 \
        SQLR_TEST_MARIADB_GLOBAL=127.0.0.1:53311 \
        SQLR_TEST_MYSQL_NOTLS=127.0.0.1:53312 \
        SQLR_TEST_MARIADB_NOTLS=127.0.0.1:53313 \
        SQLR_TEST_MONGO=127.0.0.1:57017 \
        SQLR_TEST_MONGO_AUTH=127.0.0.1:57018 \
        SQLR_TEST_REDIS=127.0.0.1:56379 \
        SQLR_TEST_REDIS_TLS=127.0.0.1:56380 \
        SQLR_TEST_ELASTIC=127.0.0.1:59200 \
        SQLR_TEST_ELASTIC7=127.0.0.1:59201 \
        SQLR_TEST_ELASTIC_AUTH=127.0.0.1:59202 \
        SQLR_TEST_MSSQL=127.0.0.1:51433 \
        SQLR_TEST_ORACLE=127.0.0.1:51521 \
        SQLR_TEST_CA="$PWD/.test-services/certs/ca.crt"
fi

# Old instrumented binaries also carry coverage mappings. Keeping them after source
# changes makes llvm-cov count multiple versions of the same file in the report.
cargo llvm-cov clean --workspace

echo "coverage: running tests (profile: $PROFILE)"
status=0
# Build HANA with the workspace: the app tests install its binary as a fixture. Its
# live tests stay ignored, and it remains excluded from the coverage report.
cargo llvm-cov nextest \
    --workspace \
    --exclude-from-report sqlr-driver-hana \
    --profile "$PROFILE" \
    --no-fail-fast \
    --no-report || status=$?

echo "coverage: writing reports to $OUT"
IGNORE='drivers/sqlr-driver-hana/'
cargo llvm-cov report --json --summary-only \
    --ignore-filename-regex "$IGNORE" --output-path "$OUT/summary.json"
cargo llvm-cov report --text \
    --ignore-filename-regex "$IGNORE" --output-path "$OUT/summary.txt"
cargo llvm-cov report --html \
    --ignore-filename-regex "$IGNORE" --output-dir "$OUT"
cargo llvm-cov report --lcov \
    --ignore-filename-regex "$IGNORE" --output-path "$OUT/lcov.info"

check=0
python3 - "$OUT/summary.json" "$MIN" "$CHECK" > "$OUT/per-crate.txt" <<'PY' || check=$?
import collections
import json
import os
import sys

summary, minimum, enforce = sys.argv[1], float(sys.argv[2]), sys.argv[3] == "1"
root = os.getcwd() + os.sep
crates = collections.defaultdict(lambda: [0, 0])
with open(summary, encoding="utf-8") as f:
    files = json.load(f)["data"][0]["files"]
for entry in files:
    name = entry["filename"].removeprefix(root)
    parts = name.split("/")
    if len(parts) < 3 or parts[0] not in ("crates", "drivers"):
        continue
    # Both bundled crates and sidecars (including MongoDB) count toward the gates.
    crate = parts[1]
    if crate == "sqlr-driver-hana":
        continue
    lines = entry["summary"]["lines"]
    crates[crate][0] += lines["covered"]
    crates[crate][1] += lines["count"]

rows = sorted(
    ((covered / count * 100 if count else 100.0), covered, count, crate)
    for crate, (covered, count) in crates.items()
    if count
)
print(f"{'crate':28} {'lines':>8} {'covered':>8} {'cover':>8}")
print("-" * 56)
failed = False
for percent, covered, count, crate in rows:
    mark = "" if percent >= minimum else "  <-- below"
    if percent < minimum:
        failed = True
    print(f"{crate:28} {count:8} {covered:8} {percent:7.2f}%{mark}")
print("-" * 56)
total = sum(count for _, count in crates.values())
covered = sum(covered for covered, _ in crates.values())
print(f"{'workspace (without HANA)':28} {total:8} {covered:8} {100 * covered / total:7.2f}%")
if failed and enforce:
    print(f"\ncoverage: at least one crate is below {minimum:.0f}%", file=sys.stderr)
    sys.exit(1)
PY

# The databases go down before the summary, so the coverage result is the last
# thing printed instead of being buried under the Docker teardown output.
if [ "$FAST" -eq 0 ]; then
    trap - EXIT INT TERM
    scripts/test-services.sh down
fi

cat "$OUT/per-crate.txt"

if [ "$OPEN" -eq 1 ]; then
    if command -v open >/dev/null 2>&1; then
        open "$OUT/html/index.html"
    elif command -v xdg-open >/dev/null 2>&1; then
        xdg-open "$OUT/html/index.html"
    fi
fi

if [ "$status" -ne 0 ]; then
    exit "$status"
fi
exit "$check"
