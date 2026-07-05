#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
LINE_CNT=${LINE_CNT:-3000}

# ---- pre-check ----
source "$(dirname "${BASH_SOURCE[0]}")/../deps-check.sh"
# -------------------

# Wait until a TCP port accepts connections (readiness probe).
# Usage: wait_port <host> <port> <timeout_sec> [pid_to_watch]
wait_port() {
    local host="$1" port="$2" timeout="${3:-15}" watch_pid="${4:-}"
    local i=0
    while [ "$i" -lt "$timeout" ]; do
        # bail out early if a watched process already died
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
echo "  streaming: wpgen → TCP → wparse → Arrow TCP → wfusion"
echo "  wfusion=$WFUSION_VER  wparse=$WPARSE_VER"
echo "============================================"

# 1. Start wfusion (daemon mode)
echo "1> wfusion: daemon, listening on TCP :9802..."
cd wfusion
rm -rf ../data/alerts; mkdir -p ../data/alerts
wfusion daemon --config conf/wfusion.toml &
WFUSION_PID=$!
cd ..
echo "   wfusion PID=$WFUSION_PID"
# wparse's arrow_tcp sink connects eagerly at startup, so wfusion MUST be
# accepting on :9802 before we launch wparse, otherwise wparse fails to load.
wait_port 127.0.0.1 9802 20 "$WFUSION_PID" || exit 1
echo "   wfusion ready on :9802"

# 2. Start wparse (daemon mode)
echo "2> wparse: daemon, tcp_src :9801 → tcp_sink → wfusion :9802..."
cd wparse
rm -rf .run; mkdir -p ../data/logs ../data/rescue
wparse daemon &
WPARSE_PID=$!
cd ..
echo "   wparse PID=$WPARSE_PID"
# wait until wparse is actually listening before wpgen sends data
wait_port 127.0.0.1 9801 20 "$WPARSE_PID" || exit 1
echo "   wparse ready on :9801"

# 3. wpgen sends data over TCP, then closes connection
echo "3> wpgen: sending $LINE_CNT nginx logs over TCP :9801..."
(cd wparse && wpgen sample -n "$LINE_CNT" > /dev/null 2>&1)
echo "   → wpgen done, TCP connection closed"

# 4. Wait for wparse to finish processing
echo "4> waiting for wparse to process..."
sleep 3

# 5. Stop wparse (graceful)
echo "5> stopping wparse..."
kill "$WPARSE_PID" 2>/dev/null || true
wait "$WPARSE_PID" 2>/dev/null || true
echo "   → wparse stopped"

# 6. Stop wfusion (graceful — flush windows → alerts)
echo "6> stopping wfusion..."
kill "$WFUSION_PID" 2>/dev/null || true
wait "$WFUSION_PID" 2>/dev/null || true
echo "   → wfusion stopped"

# 7. Show alerts
echo ""; echo "wfusion alerts:"
ls -la data/alerts/*.ndjson 2>/dev/null | awk '{printf "  %s  %s bytes\n", $NF, $5}'
echo ""
