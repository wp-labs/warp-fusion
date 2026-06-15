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
echo "  Kafka Pipeline: wpgen → wparse → Kafka → wfusion → Kafka"
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

# 3. Start wfusion daemon first (consumes from Kafka)
echo "3> wfusion: starting daemon, consuming from Kafka..."
cd wfusion
rm -rf data/out_dat
mkdir -p data/out_dat/alerts
wfusion run --config conf/wfusion.toml &
WFUSION_PID=$!
sleep 3
cd ..
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

# 5. Wait for processing and show results
sleep 3
echo "wfusion alerts from Kafka topic wp_alerts:"
docker-compose exec -T kafka /opt/kafka/bin/kafka-console-consumer.sh \
    --bootstrap-server localhost:9092 \
    --topic wp_alerts \
    --from-beginning \
    --timeout-ms 5000 \
    --max-messages 10 || true
echo ""
echo "Press Ctrl+C to stop"
wait $WFUSION_PID
