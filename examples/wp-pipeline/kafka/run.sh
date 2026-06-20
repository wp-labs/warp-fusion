#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
LINE_CNT=${LINE_CNT:-5000}

# ---- pre-check ----
source "$(dirname "${BASH_SOURCE[0]}")/../deps-check.sh"
# -------------------

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
sleep 3
echo "   Kafka ready at localhost:9092"

# 2. Fresh consumer group
GROUP_ID="wfusion_$(date +%s)"
sed -i '' "s/group_id = .*/group_id = \"$GROUP_ID\"/" wfusion/topology/sources/kafka_nginx.toml

# 3. Start wfusion (daemon mode)
echo "2> wfusion: daemon, consuming from Kafka..."
cd wfusion
rm -rf ../data/alerts; mkdir -p ../data/alerts
wfusion run --config conf/wfusion.toml &
WFUSION_PID=$!
cd ..
echo "   wfusion PID=$WFUSION_PID"

# 4. Start wparse (batch mode — kafka_sink is async, no persistent connection needed)
echo "3> wparse: listening on TCP :9800, then wpgen sending..."
cd wparse
rm -rf .run; mkdir -p ../data/logs ../data/rescue
wparse batch -p -n "$LINE_CNT" -S 1 &
WPARSE_PID=$!
sleep 2
wpgen sample -n "$LINE_CNT" > /dev/null 2>&1
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
