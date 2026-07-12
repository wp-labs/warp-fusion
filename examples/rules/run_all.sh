#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

PROFILE="debug"
case "${1:-}" in
    "" ) ;;
    release | --release ) PROFILE="release" ;;
    debug | --debug ) PROFILE="debug" ;;
    * )
        echo "Usage: ./run_all.sh [debug|release]" >&2
        exit 2
        ;;
esac

REPO_ROOT="$(cd ../.. && pwd)"
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

echo "Using profile: $PROFILE"
echo "  wfl     = $WFL_BIN"
echo "  wfusion = $WFUSION_BIN"
echo

TMP_OUT="$(mktemp)"
trap 'rm -f "$TMP_OUT"' EXIT

total=0
failed=0
batch_total=0
batch_skipped=0
passed=0
skipped=0
summary=()

record_result() {
    local status="$1"
    local name="$2"
    local detail="$3"
    summary+=("$status|$name|$detail")
}

run_rule() {
    local rule="$1"
    local case_dir
    local rel_rule

    case_dir="$(dirname "$(dirname "$rule")")"
    rel_rule="${rule#"$case_dir"/}"

    echo "==> $case_dir/$rel_rule"

    echo "  lint"
    if ! (cd "$case_dir" && "$WFL_BIN" lint "$rel_rule" --schemas "schemas/*.wfs"); then
        echo "  FAIL: lint failed"
        record_result "FAIL" "$case_dir/$rel_rule" "lint failed"
        failed=$((failed + 1))
        return
    fi

    echo "  test"
    if ! (cd "$case_dir" && "$WFL_BIN" test "$rel_rule" --schemas "schemas/*.wfs") >"$TMP_OUT" 2>&1; then
        cat "$TMP_OUT"
        echo "  FAIL: wfl test command failed"
        record_result "FAIL" "$case_dir/$rel_rule" "wfl test command failed"
        failed=$((failed + 1))
        return
    fi

    cat "$TMP_OUT"
    if grep -q '^FAIL[[:space:]]' "$TMP_OUT"; then
        echo "  FAIL: one or more inline tests failed"
        record_result "FAIL" "$case_dir/$rel_rule" "inline tests failed"
        failed=$((failed + 1))
        return
    fi

    if grep -q '^No tests found\.' "$TMP_OUT"; then
        echo "  OK: lint passed; no inline tests"
        record_result "PASS" "$case_dir/$rel_rule" "lint passed; no inline tests"
    else
        local test_count
        test_count="$(awk '/^[0-9]+ tests:/ {print $1}' "$TMP_OUT" | tail -1)"
        echo "  OK: ${test_count:-?} inline tests passed"
        record_result "PASS" "$case_dir/$rel_rule" "${test_count:-?} inline tests passed"
    fi
    passed=$((passed + 1))
}

run_batch_case() {
    local case_dir="$1"
    local alert_file="$2"
    local expected_alerts="$3"
    local reason="${4:-}"
    local actual_alerts=0

    if [ -n "$reason" ]; then
        echo "==> $case_dir"
        echo "  SKIP batch: $reason"
        record_result "SKIP" "$case_dir wfusion batch" "$reason"
        batch_skipped=$((batch_skipped + 1))
        skipped=$((skipped + 1))
        return
    fi

    batch_total=$((batch_total + 1))
    echo "==> $case_dir"
    echo "  wfusion batch"
    rm -rf "$case_dir/out"
    rm -rf "$case_dir/data/out_dat"
    if ! (cd "$case_dir" && "$WFUSION_BIN" batch --config wfusion.toml --work-dir .); then
        echo "  FAIL: wfusion batch failed"
        record_result "FAIL" "$case_dir wfusion batch" "batch command failed"
        failed=$((failed + 1))
        return
    fi

    if [ -f "$case_dir/$alert_file" ]; then
        actual_alerts="$(wc -l < "$case_dir/$alert_file" | tr -d ' ')"
    fi

    if [ "$actual_alerts" != "$expected_alerts" ]; then
        echo "  FAIL: expected $expected_alerts alerts, got $actual_alerts ($case_dir/$alert_file)"
        record_result "FAIL" "$case_dir wfusion batch" "expected $expected_alerts alerts, got $actual_alerts"
        failed=$((failed + 1))
        return
    fi

    echo "  OK: $actual_alerts alerts (expected $expected_alerts)"
    record_result "PASS" "$case_dir wfusion batch" "$actual_alerts alerts"
    passed=$((passed + 1))
}

echo "## Rule lint and inline tests"
while IFS= read -r rule; do
    total=$((total + 1))
    run_rule "$rule"
done < <(find . -path './*/rules/*.wfl' -type f | sort)

echo
echo "## WFusion batch replay"
run_batch_case "./close_demo" "data/out_dat/out/alerts.ndjson" 2
run_batch_case "./multi_stream_multi_window" "data/out_dat/alerts.ndjson" 2
run_batch_case "./port_scan_whitelist" "data/out_dat/alerts.ndjson" 1
run_batch_case "./rat_propagation" "data/out_dat/alerts.ndjson" 0
run_batch_case "./single_stream_multi_window" "data/out_dat/alerts.ndjson" 2
run_batch_case "./sqli_probe" "data/out_dat/out/alerts.ndjson" 1
run_batch_case "./ssh_brute_force" "data/out_dat/alerts.ndjson" 1
run_batch_case "./weak_password" "data/out_dat/alerts.ndjson" 0
if [ "${RUN_EXTERNAL:-0}" = "1" ]; then
    run_batch_case "./weak_password2" "data/out_dat/alerts.ndjson" 5
else
    run_batch_case "./weak_password2" "data/out_dat/alerts.ndjson" 5 "requires Redis; run with RUN_EXTERNAL=1 after starting dependencies"
fi

echo
echo "## Case summary"
for row in "${summary[@]}"; do
    IFS='|' read -r status name detail <<<"$row"
    printf '%-4s  %-58s %s\n' "$status" "$name" "$detail"
done
echo
echo "rules checked: $total"
echo "batch cases checked: $batch_total"
echo "batch cases skipped: $batch_skipped"
echo "cases passed: $passed"
echo "cases skipped: $skipped"
if [ "$failed" -ne 0 ]; then
    echo "failed: $failed"
    exit 1
fi

echo "all checks passed"
