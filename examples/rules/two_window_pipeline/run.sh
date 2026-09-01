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

if [ ! -x "$WFL_BIN" ]; then
    echo "ERROR: wfl binary not found or not executable: $WFL_BIN" >&2
    echo "       build it first, for example: cargo build --bin wfl" >&2
    exit 1
fi

echo "Using profile: $PROFILE"
echo "  wfl     = $WFL_BIN"
echo

echo "1> lint rule"
"$WFL_BIN" lint rules/two_window_pipeline.wfl --schemas "schemas/*.wfs"

echo "2> run inline tests"
TEST_OUT="$("$WFL_BIN" test rules/two_window_pipeline.wfl --schemas "schemas/*.wfs")"
echo "$TEST_OUT"
if echo "$TEST_OUT" | grep -q '^FAIL[[:space:]]'; then
    echo "ERROR: inline tests failed" >&2
    exit 1
fi

echo "3> clean previous batch output"
rm -rf data/out_dat

echo "4> replay pipeline"
REPLAY_OUT="$("$WFL_BIN" replay rules/two_window_pipeline.wfl --schemas "schemas/*.wfs" --input data/auth_events.ndjson 2>&1)"
echo "$REPLAY_OUT"

echo "5> verify replay alert"
if ! grep -Fq 'Replay complete: 8 events processed, 1 matches, 0 errors' <<<"$REPLAY_OUT"; then
    echo "ERROR: expected replay to process 8 events with 1 match and 0 errors" >&2
    exit 1
fi

if ! grep -Fq '"rule_name":"two_window_pipeline_alert"' <<<"$REPLAY_OUT"; then
    echo "ERROR: missing two_window_pipeline_alert replay output" >&2
    exit 1
fi

if ! grep -Fq '"entity_id":"10.0.0.9"' <<<"$REPLAY_OUT"; then
    echo "ERROR: missing expected source entity in replay output" >&2
    exit 1
fi

echo "OK: two-window pipeline replay produced 1 final alert"
