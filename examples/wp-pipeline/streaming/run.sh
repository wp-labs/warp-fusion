#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
LINE_CNT=${LINE_CNT:-3000}
PORT_IN=${PORT_IN:-9801}
PORT_OUT=${PORT_OUT:-9802}

# ---- pre-check ----
REQUIRED_WPARSE="0.25"; REQUIRED_WFUSION="0.1"
WF_BUILD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/target/release"
resolve_binary() { local n="$1"; command -v "$n" 2>/dev/null && return 0; [ -x "$WF_BUILD_DIR/$n" ] && export PATH="$WF_BUILD_DIR:$PATH" && return 0; return 1; }
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

# 1. Write TCP configs (subshell: heredoc + cd, no background procs)
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

# 2. Start wfusion (direct cd — $! captures wfusion PID)
echo "1> wfusion: starting daemon (tcp://0.0.0.0:$PORT_OUT)..."
cd wfusion
rm -rf ../data/alerts; mkdir -p ../data/alerts
wfusion run --config conf/wfusion.toml &
WFUSION_PID=$!
cd ..
sleep 5
echo "   wfusion PID=$WFUSION_PID"

# 3. Start wparse (direct cd — $! captures wparse PID)
echo "2> wparse: listening on TCP :$PORT_IN..."
cd wparse
mkdir -p ../data/logs
wparse batch -p -n "$LINE_CNT" -S 1 &
WPARSE_PID=$!
cd ..
sleep 0
echo "   wparse PID=$WPARSE_PID"

# 4. Run wpgen (foreground, sends data over TCP)
echo "3> wpgen: sending $LINE_CNT nginx logs over TCP :$PORT_IN..."
cd wparse
wpgen sample -n "$LINE_CNT" > /dev/null 2>&1
cd ..
echo "   → TCP :$PORT_IN → wparse → Arrow TCP :$PORT_OUT → wfusion"

# 5. Wait for wparse to finish
echo "4> waiting for wparse to finish..."
wait "$WPARSE_PID" 2>/dev/null || true
echo "   → wparse complete"

# 6. Flush wfusion
echo "5> flushing wfusion windows..."
kill "$WFUSION_PID" 2>/dev/null || true
wait "$WFUSION_PID" 2>/dev/null || true
sleep 1

# 7. Show alerts
echo ""; echo "wfusion alerts:"
ls -la data/alerts/*.ndjson 2>/dev/null | awk '{printf "  %s  %s bytes\n", $NF, $5}'
echo ""
