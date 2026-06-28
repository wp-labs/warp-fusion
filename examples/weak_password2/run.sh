#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

echo "=== weak_password2: Redis external() 弱口令检测 ==="

echo "1> 启动 Redis 并加载弱口令库..."
docker compose up -d
for i in $(seq 1 40); do
  st=$(docker inspect -f '{{.State.Status}}' redis_wp2_init 2>/dev/null || true)
  [ "$st" = "exited" ] && break
  sleep 1
done
docker logs redis_wp2_init 2>&1 | tail -3

echo ""
echo "2> wfusion: 读 auth_events → external() 点查 Redis → 告警..."
mkdir -p data/out_dat
rm -f data/out_dat/*.ndjson
wfusion batch -c ./wfusion.toml

echo ""
echo "=== alerts ==="
for f in data/out_dat/*.ndjson; do
  [ -s "$f" ] && echo "  $(basename $f): $(wc -l < $f | tr -d ' ') lines"
done
echo ""
echo "redis set size: $(docker exec redis_wp2 redis-cli scard weak_passwords 2>/dev/null)"
