#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

SCENARIO="${SCENARIO:-scenarios/port_scan_quick.wfg}"
CASE_NAME="$(basename "$SCENARIO" .wfg)"
GENERATED_DIR="data/generated"
ALERT_DIR="data/alerts"
ACTUAL_ALERTS="$ALERT_DIR/network.ndjson"
EXPECTED_ALERTS="$GENERATED_DIR/$CASE_NAME.except.jsonl"
EXPECTED_META="$GENERATED_DIR/$CASE_NAME.except.meta.jsonl"

mkdir -p "$GENERATED_DIR" "$ALERT_DIR" data/logs
rm -f "$ALERT_DIR"/*.ndjson data/wfusion.log

echo "1> lint scenario: $SCENARIO"
wfgen lint "$SCENARIO"

echo "2> generate events"
wfgen gen --scenario "$SCENARIO" --out "$GENERATED_DIR" --format jsonl

echo "3> run wfusion batch replay"
wfusion batch --config test/wfusion.batch.toml --work-dir .

if [[ ! -s "$ACTUAL_ALERTS" ]]; then
    echo "ERROR: expected non-empty alert output: $ACTUAL_ALERTS" >&2
    exit 1
fi

echo "4> verify alerts"
EXPECTED_COUNT="$(wc -l < "$EXPECTED_ALERTS" | tr -d ' ')"
ACTUAL_COUNT="$(wc -l < "$ACTUAL_ALERTS" | tr -d ' ')"
if [[ "$ACTUAL_COUNT" != "$EXPECTED_COUNT" ]]; then
    echo "ERROR: alert count mismatch: expected=$EXPECTED_COUNT actual=$ACTUAL_COUNT" >&2
    exit 1
fi

if awk 'index($0, "\"__wfu_rule_name\":\"port_scan\"") == 0 { bad++ } END { exit bad ? 1 : 0 }' "$ACTUAL_ALERTS"; then
    :
else
    echo "ERROR: found non-port_scan alerts in $ACTUAL_ALERTS" >&2
    exit 1
fi

echo "5> alert counts"
wc -l "$ALERT_DIR"/*.ndjson
