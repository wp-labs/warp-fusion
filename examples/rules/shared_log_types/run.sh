#!/usr/bin/env bash
# shared_log_types — 顶层列表 + use 导入（issue #73）
#
# 演示: 三个规则（告警/实体/证据）以 `use "../lists/security_log_types.wfl"`
# 导入同一份日志类型允许列表, 分别用 `in` / `not in` 引用——列表只维护一处,
# 三个规则无需重复编辑。
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

PROFILE="${1:-debug}"
REPO_ROOT="$(cd ../../.. && pwd)"
WFL_BIN="$REPO_ROOT/target/$PROFILE/wfl"
WFUSION_BIN="$REPO_ROOT/target/$PROFILE/wfusion"

echo "=== shared_log_types: 顶层列表 + use 导入（issue #73）==="
echo "profile: $PROFILE"

echo ""
echo "1> wfl lint + test（use 导入在 lint/compile 同一条路径解析）"
for r in rules/*.wfl; do
  echo "  -- $r"
  "$WFL_BIN" lint "$r" -s "schemas/*.wfs"
  "$WFL_BIN" test "$r" -s "schemas/*.wfs"
done

echo ""
echo "2> wfusion batch（读 data/sdm_events.ndjson → 三规则 → 三输出）"
mkdir -p data/out_dat
rm -f data/out_dat/*.ndjson
"$WFUSION_BIN" batch -c ./wfusion.toml

echo ""
echo "== 输出（预期: 告警 5 / 实体 5 / 证据 3）=="
ALERTS=0
ENTITIES=0
EVIDENCE=0
for f in data/out_dat/*.ndjson; do
  [ -s "$f" ] || continue
  n="$(wc -l < "$f" | tr -d ' ')"
  case "$(basename "$f")" in
    security_alerts.ndjson)  ALERTS=$n ;;
    alert_entities.ndjson)   ENTITIES=$n ;;
    event_evidence.ndjson)   EVIDENCE=$n ;;
  esac
  echo "  $(basename "$f"): $n lines"
done

if [ "$ALERTS" -ne 5 ] || [ "$ENTITIES" -ne 5 ] || [ "$EVIDENCE" -ne 3 ]; then
  echo "FAIL: expected alerts=5 entities=5 evidence=3, got alerts=$ALERTS entities=$ENTITIES evidence=$EVIDENCE" >&2
  exit 1
fi
echo "OK: 5 告警 + 5 实体 + 3 证据（同一份列表驱动三个规则）"
