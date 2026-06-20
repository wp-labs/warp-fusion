#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
LINE_CNT=${LINE_CNT:-3000}
PORT_IN=${PORT_IN:-9801}
PORT_OUT=${PORT_OUT:-9802}

# ---- pre-check ----
REQUIRED_WPARSE="0.25"; REQUIRED_WFUSION="0.1"
WF_BUILD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/target/release"
resolve_binary() { local n="$1"; [ -x "$WF_BUILD_DIR/$n" ] && export PATH="$WF_BUILD_DIR:$PATH" && return 0; command -v "$n" 2>/dev/null && return 0; return 1; }
if ! resolve_binary wfusion || ! resolve_binary wparse; then echo "ERROR: wfusion/wparse not found" >&2; exit 1; fi
WFUSION_VER=$(wfusion --version 2>&1 | grep -o '[0-9.]*' | head -1)
WPARSE_VER=$(wparse --version 2>&1 | grep -o '[0-9.]*' | head -1)
if ! printf '%s\n%s' "$REQUIRED_WFUSION" "$WFUSION_VER" | sort -V -C 2>/dev/null; then echo "ERROR: wfusion >= $REQUIRED_WFUSION required, got $WFUSION_VER" >&2; exit 1; fi
if ! printf '%s\n%s' "$REQUIRED_WPARSE" "$WPARSE_VER" | sort -V -C 2>/dev/null; then echo "ERROR: wparse >= $REQUIRED_WPARSE required, got $WPARSE_VER" >&2; exit 1; fi
# -------------------

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

# 1. Write TCP configs (subshell: heredoc + cd)
(cd wparse && rm -rf .run && mkdir -p ../data/logs ../data/rescue
cat > conf/wpgen.toml <<EOF
version = "1.0"

[generator]
count = 1000
speed = 1000
parallel = 1

[output]
connect = "tcp_sink"

[output.params]
addr = "127.0.0.1"
port = "$PORT_IN"
framing = "line"

[logging]
level = "info"
output = "file"
file_path = "../data/logs"

[presets]
EOF

cat > topology/sources/wpsrc.toml <<EOF
[[sources]]
key = "tcp_1"
enable = true
connect = "tcp_src"
tags = []

[sources.params]
addr = "127.0.0.1"
port = "$PORT_IN"
framing = "line"
EOF
)

# 2. Start wfusion (daemon mode)
echo "1> wfusion: daemon, listening on TCP :$PORT_OUT..."
cd wfusion
rm -rf ../data/alerts; mkdir -p ../data/alerts
wfusion run --config conf/wfusion.toml &
WFUSION_PID=$!
cd ..
sleep 5  # wait for TCP listener to bind
echo "   wfusion PID=$WFUSION_PID"

# 3. Start wparse (daemon mode — NOT batch)
echo "2> wparse: daemon, tcp_src :$PORT_IN → tcp_sink → wfusion :$PORT_OUT..."
cd wparse
mkdir -p ../data/logs
wparse daemon -p &
WPARSE_PID=$!
cd ..
sleep 2  # wait for tcp_src bind + tcp_sink connect
echo "   wparse PID=$WPARSE_PID"

# 4. wpgen sends data over TCP, then closes connection
echo "3> wpgen: sending $LINE_CNT nginx logs over TCP :$PORT_IN..."
(cd wparse && wpgen sample -n "$LINE_CNT" > /dev/null 2>&1)
echo "   → wpgen done, TCP connection closed"

# 5. Wait for wparse to finish processing remaining data
echo "4> waiting for wparse to process..."
sleep 3

# 6. Stop wparse (graceful — flush, close)
echo "5> stopping wparse..."
kill "$WPARSE_PID" 2>/dev/null || true
wait "$WPARSE_PID" 2>/dev/null || true
echo "   → wparse stopped"

# 7. Stop wfusion (graceful — flush windows → alerts)
echo "6> stopping wfusion..."
kill "$WFUSION_PID" 2>/dev/null || true
wait "$WFUSION_PID" 2>/dev/null || true
echo "   → wfusion stopped"

# 8. Show alerts
echo ""; echo "wfusion alerts:"
ls -la data/alerts/*.ndjson 2>/dev/null | awk '{printf "  %s  %s bytes\n", $NF, $5}'
echo ""
