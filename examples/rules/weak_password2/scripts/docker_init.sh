#!/bin/sh
# Docker init script — waits for Redis then loads weak password data.
# Runs in python:3-alpine container (pip installs redis client at runtime).
set -e

echo "redis_init: waiting for Redis..."

for i in $(seq 1 30); do
    if python3 -c "
import socket
try:
    s = socket.socket()
    s.settimeout(1)
    s.connect(('redis', 6379))
    s.close()
    exit(0)
except Exception:
    exit(1)
" 2>/dev/null; then
        echo "redis_init: Redis is ready"
        break
    fi
    sleep 1
done

echo "redis_init: loading weak passwords..."

pip install -q redis > /dev/null 2>&1

python3 -c "
import json, redis, time
r = redis.Redis(host='redis', port=6379, decode_responses=True)

count = 0
start = time.time()
with open('/data/weak_password_list.ndjson') as f:
    pipe = r.pipeline()
    batch = 0
    for line in f:
        line = line.strip()
        if not line:
            continue
        d = json.loads(line)
        hv = d['hash_value']
        pipe.sadd('weak_passwords', hv)
        pipe.hset(f'wp:{hv}', mapping={
            'hash_value': hv,
            'password_masked': d['password_masked'],
            'category': d['category'],
            'note': d['note'],
        })
        batch += 1
        count += 1
        if batch >= 500:
            pipe.execute()
            pipe = r.pipeline()
            batch = 0
    if batch > 0:
        pipe.execute()

elapsed = time.time() - start
print(f'redis_init: loaded {count} passwords in {elapsed:.1f}s')
print(f'redis_init: set size = {r.scard(\"weak_passwords\")}')
"
echo "redis_init: done."
