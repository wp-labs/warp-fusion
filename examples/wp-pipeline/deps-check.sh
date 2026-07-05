#!/usr/bin/env bash
# Shared pre-check for wp-pipeline examples — resolves wfusion/wparse binaries
# and checks minimum versions. Source this from any run.sh:
#
#   source "$(dirname "${BASH_SOURCE[0]}")/../deps-check.sh"
#
# Sets: WFUSION_VER, WPARSE_VER

check_binary() {
    local n="$1"
    if ! command -v "$n" 2>/dev/null >/dev/null; then
        echo "ERROR: $n not found in PATH" >&2
        return 1
    fi
}

if ! check_binary wfusion; then exit 1; fi
if ! check_binary wparse;  then exit 1; fi
if ! wfusion version --ge 0.1.0 >/dev/null 2>&1; then
    echo "ERROR: wfusion >= 0.1.0 required" >&2
    exit 1
fi
if ! wparse version --ge 0.25.4 >/dev/null 2>&1; then
    echo "ERROR: wparse >= 0.25.4 required" >&2
    exit 1
fi

WFUSION_VER=$(wfusion version 2>&1 | awk '{print $NF}')
WPARSE_VER=$(wparse version 2>&1)
