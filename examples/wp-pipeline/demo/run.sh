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

source "$(dirname "${BASH_SOURCE[0]}")/../deps-check.sh"

echo "0> wfadm check wfusion..."
(cd wfusion && wfadm check) || { echo "  x wfusion check failed, abort"; exit 1; }

echo "============================================"
echo "  demo: wpgen -> file -> wparse -> NDJSON -> wfusion"
echo "  wfusion=$WFUSION_VER  wparse=$WPARSE_VER"
echo "============================================"

rm -rf data/in_dat data/out_dat data/alerts data/logs data/rescue
mkdir -p data/in_dat data/out_dat data/alerts data/logs data/rescue

echo "1> wpgen: $LINE_CNT lines"
(cd wparse && wpgen sample -n "$LINE_CNT" > /dev/null 2>&1)

echo "2> wparse: parse -> NDJSON with wp_oml_name"
(cd wparse && wparse batch > /dev/null 2>&1)

echo "3> wfusion: route by wp_oml_name -> window.stream_tag"
(cd wfusion && wfusion batch --config conf/wfusion.toml > /dev/null 2>&1)

echo ""
echo "Output:"
if [ -f data/out_dat/parsed.ndjson ]; then
    echo "  parsed.ndjson: $(wc -l < data/out_dat/parsed.ndjson | tr -d ' ') rows"
fi
for f in data/alerts/*.ndjson; do
    [ -f "$f" ] && echo "  $(basename "$f"): $(wc -l < "$f" | tr -d ' ') alerts"
done
