#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
LINE_CNT=${LINE_CNT:-5000}
REPO_ROOT="$(cd ../../.. && pwd)"
BIN_PROFILE="${1:-debug}"
case "$BIN_PROFILE" in
    debug) BIN_DIR="$REPO_ROOT/target/debug" ;;
    release) BIN_DIR="$REPO_ROOT/target/release" ;;
    *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac
if [ -x "$BIN_DIR/wfusion" ] || [ -x "$BIN_DIR/wfadm" ]; then
    export PATH="$BIN_DIR:$PATH"
fi

# ---- pre-check ----
source "$(dirname "${BASH_SOURCE[0]}")/../deps-check.sh"
echo "0> wfadm check wfusion..."
(cd wfusion && wfadm check) || { echo "  ✗ wfusion check failed, abort"; exit 1; }
# -------------------

# Wait until a TCP port accepts connections (readiness probe).
# Usage: wait_port <host> <port> <timeout_sec> [pid_to_watch]
wait_port() {
    local host="$1" port="$2" timeout="${3:-15}" watch_pid="${4:-}"
    local i=0
    while [ "$i" -lt "$timeout" ]; do
        if [ -n "$watch_pid" ] && ! kill -0 "$watch_pid" 2>/dev/null; then
            echo "   ✗ process $watch_pid exited before :$port was ready" >&2
            return 1
        fi
        if (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null; then
            exec 3>&- 3<&- 2>/dev/null || true
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    echo "   ✗ timed out waiting for $host:$port after ${timeout}s" >&2
    return 1
}

cleanup() {
    [ -n "${WPARSE_PID:-}" ] && kill "$WPARSE_PID" 2>/dev/null || true
    [ -n "${WFUSION_PID:-}" ] && kill "$WFUSION_PID" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup EXIT

echo "============================================"
echo "  Kafka Pipeline: wpgen → TCP → wparse → Kafka (Arrow) → wfusion → alerts"
echo "  wfusion=$WFUSION_VER  wparse=$WPARSE_VER"
echo "============================================"

# 1. Start Kafka
echo "1> Starting Kafka..."
docker-compose up -d
# Kafka must accept broker connections before wfusion/wparse can use it.
wait_port 127.0.0.1 9092 60 || { echo "   Kafka not reachable on :9092" >&2; exit 1; }
echo "   Kafka ready at localhost:9092"

# 2. Fresh consumer group in a runtime source copy; do not mutate tracked config.
GROUP_ID="wfusion_$(date +%s)"
mkdir -p wfusion/.run/sources
cp wfusion/topology/sources/kafka_nginx.toml wfusion/.run/sources/kafka_nginx.toml
perl -0pi -e "s/^group_id\\s*=\\s*\"[^\"]*\"/group_id = \"$GROUP_ID\"/m" \
    wfusion/.run/sources/kafka_nginx.toml
cat > wfusion/.run/source-overlay.toml <<'EOF'
sources_dir = ".run/sources"
EOF

# 3. Start wfusion (daemon mode)
echo "2> wfusion: daemon, consuming from Kafka..."
cd wfusion
rm -rf ../data/alerts; mkdir -p ../data/alerts
wfusion daemon --config conf/wfusion.toml --overlay .run/source-overlay.toml &
WFUSION_PID=$!
cd ..
echo "   wfusion PID=$WFUSION_PID"
sleep 1
if ! kill -0 "$WFUSION_PID" 2>/dev/null; then
    echo "   ✗ wfusion exited before consuming Kafka; check wfusion logs above" >&2
    exit 1
fi

# 4. Start wparse (daemon mode — must listen on TCP to receive wpgen data)
echo "3> wparse: listening on TCP :9800, then wpgen sending..."
cd wparse
rm -rf .run; mkdir -p ../data/logs ../data/rescue
wparse daemon &
WPARSE_PID=$!
# wait until wparse is actually listening before wpgen sends data
wait_port 127.0.0.1 9800 20 "$WPARSE_PID" || { kill "$WPARSE_PID" 2>/dev/null || true; exit 1; }
wpgen sample -n "$LINE_CNT" > /dev/null 2>&1
echo "   → wpgen done, TCP connection closed"
# give wparse a moment to finish parsing & flushing the batch to Kafka
sleep 3
kill "$WPARSE_PID" 2>/dev/null || true
wait "$WPARSE_PID" 2>/dev/null || true
cd ..
echo "   → TCP :9800 → wparse → Kafka (wp_nginx_logs)"

sleep 2
# 5. Flush wfusion (graceful shutdown → flush windows → alerts)
echo "4> flushing wfusion windows..."
kill "$WFUSION_PID" 2>/dev/null || true
wait "$WFUSION_PID" 2>/dev/null || true
sleep 1

# 6. Show alerts
echo ""; echo "wfusion alerts:"
shopt -s nullglob
alert_files=(data/alerts/*.ndjson)
if [ "${#alert_files[@]}" -eq 0 ]; then
    echo "  (no alert files)"
else
    for f in "${alert_files[@]}"; do
        name=$(basename "$f")
        size=$(wc -c < "$f" 2>/dev/null | tr -d ' ')
        [ "$size" -gt 0 ] && echo "  $name ($size bytes)" || echo "  $name (empty)"
    done
fi
echo ""
