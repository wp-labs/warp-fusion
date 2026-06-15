#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
LINE_CNT=${LINE_CNT:-5000}

cleanup() {
    if [ -n "${WFUSION_PID:-}" ] && kill -0 "$WFUSION_PID" 2>/dev/null; then
        kill "$WFUSION_PID" 2>/dev/null || true
        wait "$WFUSION_PID" 2>/dev/null || true
    fi
    echo ""
    echo "stopped."
}
trap cleanup EXIT

echo "============================================"
echo "  Kafka Pipeline: wpgen → wparse → Kafka → wfusion → alerts"
echo "============================================"
echo ""

# 1. Start Kafka
echo "1> Starting Kafka..."
docker-compose up -d
sleep 5
echo "   Kafka ready at localhost:9092"
echo ""

# 2. Init + generate data
echo "2> wpgen: generating $LINE_CNT nginx logs..."
cd wparse
wproj init -m full > /dev/null 2>&1
wpgen sample -n "$LINE_CNT" > /dev/null 2>&1
echo "   → data/in_dat/conn_events.ndjson"
cd ..
echo ""

# 3. Use a fresh consumer group to always replay from beginning
GROUP_ID="wfusion_$(date +%s)"
sed -i '' "s/group_id = .*/group_id = \"$GROUP_ID\"/" wfusion/topology/sources/kafka_nginx.toml

echo "3> wfusion: starting daemon, consuming from Kafka..."
cd wfusion
rm -rf data/out_dat
mkdir -p data/out_dat/alerts
wfusion run --config conf/wfusion.toml &
WFUSION_PID=$!
sleep 3
cd ..
echo "   group_id=$GROUP_ID"
echo "   wfusion PID=$WFUSION_PID"
echo ""

# 4. Parse → Kafka (wfusion consumes in real-time)
echo "4> wparse: parsing → Kafka (wp_nginx_logs)..."
cd wparse
mkdir -p data/out_dat data/logs
wparse batch -p -n "$LINE_CNT" -S 1 > /dev/null 2>&1
echo "   → topic: wp_nginx_logs"
cd ..
echo ""

# 5. Graceful shutdown to flush windows and write alerts
echo "5> flushing wfusion windows..."
kill "$WFUSION_PID" 2>/dev/null || true
wait "$WFUSION_PID" 2>/dev/null || true
sleep 1

# 6. Show local alert files
echo ""
echo "wfusion alerts (local files):"
for f in wfusion/data/out_dat/alerts/*.ndjson; do
    name=$(basename "$f")
    size=$(wc -c < "$f" | tr -d ' ')
    if [ "$size" -gt 0 ]; then
        echo "  $name ($size bytes)"
        cat "$f" | python3 -m json.tool 2>/dev/null || cat "$f"
    else
        echo "  $name (empty)"
    fi
done
echo ""
