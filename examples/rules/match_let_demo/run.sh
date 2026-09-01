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
"$WFL_BIN" lint rules/match_let_demo.wfl --schemas "schemas/*.wfs"

echo "2> run inline tests"
TEST_OUT="$("$WFL_BIN" test rules/match_let_demo.wfl --schemas "schemas/*.wfs")"
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
if [ "$ALERT_COUNT" != "4" ]; then
    echo "ERROR: expected 4 alerts, got $ALERT_COUNT" >&2
    cat "$ALERT_FILE" >&2
    exit 1
fi

# let 派生字段复用：dedup_key 链式依赖 tenant_id；alert_id 依赖 dedup_key。
if ! grep -q '"dedup_key":"t1|login|2026-01-01T00:00:01Z"' "$ALERT_FILE"; then
    echo "ERROR: missing derived dedup_key" >&2
    cat "$ALERT_FILE" >&2
    exit 1
fi
if ! grep -q '"alert_id":"alert_[0-9a-f]\{24\}"' "$ALERT_FILE"; then
    echo "ERROR: missing derived alert_id (sha256 truncation of dedup_key)" >&2
    cat "$ALERT_FILE" >&2
    exit 1
fi

# match 表达式：severity 枚举归一化（多模式 | + 默认 _）。
for sev in CRITICAL HIGH MEDIUM INFO; do
    if ! grep -q "\"severity\":\"$sev\"" "$ALERT_FILE"; then
        echo "ERROR: missing severity=$sev (case mapping)" >&2
        cat "$ALERT_FILE" >&2
        exit 1
    fi
done

if [ -s "$ERROR_FILE" ]; then
    echo "ERROR: unexpected error sink output" >&2
    cat "$ERROR_FILE" >&2
    exit 1
fi

echo "OK: match_let_demo produced 4 alerts (let-derived dedup/alert_id + case severity mapping)"
