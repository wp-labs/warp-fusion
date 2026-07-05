#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
LINE_CNT=${LINE_CNT:-5000}

# ---- pre-check ----
source "$(dirname "${BASH_SOURCE[0]}")/../deps-check.sh"
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

# 2. Fresh consumer group
GROUP_ID="wfusion_$(date +%s)"
sed -i '' "s/group_id = .*/group_id = \"$GROUP_ID\"/" wfusion/topology/sources/kafka_nginx.toml

# 3. Start wfusion (daemon mode)
echo "2> wfusion: daemon, consuming from Kafka..."
cd wfusion
rm -rf ../data/alerts; mkdir -p ../data/alerts
wfusion daemon --config conf/wfusion.toml &
WFUSION_PID=$!
cd ..
echo "   wfusion PID=$WFUSION_PID"

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
for f in data/alerts/*.ndjson; do
    name=$(basename "$f")
    size=$(wc -c < "$f" 2>/dev/null || echo 0 | tr -d ' ')
    [ "$size" -gt 0 ] && echo "  $name ($size bytes)" || echo "  $name (empty)"
done
echo ""
