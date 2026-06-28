#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
LINE_CNT=${LINE_CNT:-5000}

echo "=== wp-pipeline batch: wpgen → wparse → wfusion ==="

echo "1> wpgen: $LINE_CNT lines"
cd wparse && wpgen sample -n "$LINE_CNT" > /dev/null 2>&1 && cd ..

echo "2> wparse: parse → NDJSON"
cd wparse && mkdir -p data/out_dat data/logs
wparse batch -p -n "$LINE_CNT" -S 1 > /dev/null 2>&1 && cd ..

echo "3> wfusion: detect → alerts"
cd wfusion && mkdir -p data/out_dat/alerts
wfusion batch --config conf/wfusion.toml > /dev/null 2>&1 && cd ..

echo ""
echo "Output:"
for f in wfusion/data/out_dat/alerts/*.ndjson; do
    [ -f "$f" ] && echo "  $(basename $f): $(wc -l < $f | tr -d ' ') alerts"
done
