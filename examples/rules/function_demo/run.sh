#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

PROFILE="debug"
case "${1:-}" in
    "" ) ;;
    release | --release ) PROFILE="release" ;;
    debug | --debug ) PROFILE="debug" ;;
    * )
        echo "Usage: ./run.sh [debug|release]" >&2
        exit 2
        ;;
esac

REPO_ROOT="$(cd ../../.. && pwd)"
WFL_BIN="$REPO_ROOT/target/$PROFILE/wfl"
WFUSION_BIN="$REPO_ROOT/target/$PROFILE/wfusion"

if [ ! -x "$WFL_BIN" ]; then
    echo "ERROR: wfl binary not found or not executable: $WFL_BIN" >&2
    echo "       build it first, for example: cargo build --bin wfl" >&2
    exit 1
fi

if [ ! -x "$WFUSION_BIN" ]; then
    echo "ERROR: wfusion binary not found or not executable: $WFUSION_BIN" >&2
    echo "       build it first, for example: cargo build --bin wfusion" >&2
    exit 1
fi

ALERT_FILE="data/out_dat/alerts.ndjson"
ERROR_FILE="data/out_dat/error.ndjson"

echo "Using profile: $PROFILE"
echo "  wfl     = $WFL_BIN"
echo "  wfusion = $WFUSION_BIN"
echo

echo "1> lint rule"
"$WFL_BIN" lint rules/function_demo.wfl --schemas "schemas/*.wfs"

echo "2> run inline tests"
TEST_OUT="$("$WFL_BIN" test rules/function_demo.wfl --schemas "schemas/*.wfs")"
echo "$TEST_OUT"
if echo "$TEST_OUT" | grep -q '^FAIL[[:space:]]'; then
    echo "ERROR: inline tests failed" >&2
    exit 1
fi

echo "3> clean previous batch output"
rm -rf data/out_dat

echo "4> run batch replay"
"$WFUSION_BIN" batch --config wfusion.toml --work-dir .

echo "5> verify alerts"
if [ ! -f "$ALERT_FILE" ]; then
    echo "ERROR: missing alert output: $ALERT_FILE" >&2
    exit 1
fi

ALERT_COUNT="$(wc -l < "$ALERT_FILE" | tr -d ' ')"
if [ "$ALERT_COUNT" != "1" ]; then
    echo "ERROR: expected 1 alert, got $ALERT_COUNT" >&2
    cat "$ALERT_FILE" >&2
    exit 1
fi

if ! grep -q '"__wfu_rule_name":"function_demo"' "$ALERT_FILE"; then
    echo "ERROR: missing function_demo alert" >&2
    cat "$ALERT_FILE" >&2
    exit 1
fi

if ! grep -q '"joined_by":"tenant|A|function_demo||host%01"' "$ALERT_FILE"; then
    echo "ERROR: missing expected join_by output" >&2
    cat "$ALERT_FILE" >&2
    exit 1
fi

if [ -s "$ERROR_FILE" ]; then
    echo "ERROR: unexpected error sink output" >&2
    cat "$ERROR_FILE" >&2
    exit 1
fi

echo "OK: function_demo produced 1 expected alert"
