# AI Agent 指南：wf-rules 测试管线排查

> 供 AI agent 排查 `wf-rules/test/run.sh`（wfgen → wfusion → 告警文件）管线问题时遵循。
> 涵盖最常见的失败模式、根因特征和验证方法。
>
> 配套：配置见 [`wf-config-guide.md`](wf-config-guide.md)，规则见 [`wfl-guide.md`](wfl-guide.md)，
> schema 见 [`wfs-guide.md`](wfs-guide.md)。

## 管线总览

```
wfgen stream ──TCP/Arrow IPC──▶ wfusion tcp_src ──▶ window ──▶ rule_task(match)
                                                                  │
                                                            emit/alert_tx
                                                                  ▼
                                                            alert_task
                                                                  │
                                                            SinkDispatcher
                                                                  ▼
                                                          data/alerts/*.ndjson
```

任何一环断了，**症状往往是同一个：`data/alerts/` 空**。所以"没告警"不能定位到哪一环——必须**逐段验证**。

## 核心规则

### 1. 改了 wp-reactor 后必须重建二进制（最大的坑）

wp-reactor 是本地路径依赖（`warp-fusion/Cargo.toml` 用 `path = "../wp-reactor/..."`）。改了 wf-engine / wf-runtime 的代码后，**必须**重建 wfusion 并 copy：

```bash
cd warp-fusion && cargo build && cp target/debug/wfusion ~/bin/wfusion
```

**症状**：代码改了、单测过了，但集成行为没变 → 八成是 `~/bin/wfusion` 是**旧二进制**。
**验证**：改一行 `log::warn!`，重建+copy，重跑，确认日志出现。

### 2. `sinks/` 必须是分层布局，不能拍平进 `sink.d/`

`load_sink_config`（`wf-config/src/sink/io.rs`）期望的目录结构：

```
sinks/
├── connectors/sink.d/*.toml   # 连接器定义（file/kafka/blackhole...）
├── business.d/*.toml          # 业务 sink group（路由 yield_target → sink）
├── infra.d/default.toml       # 兜底 default sink
├── infra.d/error.toml         # 错误升级 sink
└── defaults.toml              # 全局默认 tags
```

**❌ 错误**：把所有 `.toml` 塞进 `sink.d/`（AI 改动常见"拍平"）。结果 `bundle.business`/`bundle.infra_default` 全空 → **0 个 sink 构建** → dispatcher 对空 Vec 遍历 → **3000 条告警静默丢弃，日志无任何报错**。

**✅ 正确**：参考 `warp-fusion/examples/close_demo/sinks/` 的布局。

**守卫**：`build_sink_dispatcher` 现在在 `total_routes==0 && default_sinks.is_empty()` 时**启动失败**（exit 1）。如果 wfusion 启动直接失败并报 `no sinks configured`，就是这个布局问题。

### 3. `event_time` 单位是整数纳秒，不是秒

wfusion 全链路按**整数纳秒**处理 time 字段（`extract_event_time` / `parse_time_value` 都直接当纳秒读）。

```json
// ✅ 正确（纳秒）
"event_time": 1782054549657648542

// ❌ 错误（Unix 秒）—— 会被当成 ~1.78 秒（1970），时间窗口/close 语义全错
"event_time": 1782025837
```

wfgen 当前版本（`wfg_parser/syntax.rs` 默认 `start = Utc::now()`）会生成正确的纳秒时间戳。**旧测试数据可能是秒级**——重生成即可，代码无需改：

```bash
wfgen gen --scenario scenarios/ssh_brute_quick.wfg --ws schemas/auth.wfs \
  --wfl rules/02-initial_access/ssh_brute_force.wfl --out /tmp/out
```

### 4. wfgen 没有 `seq()` 生成器，字面值会原样塞入

wfgen 的 `dispatch_gen_func` 只支持：`ipv4` / `pattern` / `enum` / `range` / `timestamp`。

```wfg
# ❌ 错误 — seq(1-100) 不是已知生成器，dport 会变成字面字符串 "seq(1-100)"
use(dport="seq(1-100)") with(20,0s)

# ✅ 正确 — 删掉覆盖，让 dport 走 schema 默认随机生成（digit）
use(action="syn") with(20,10s)
```

**症状**：`distinct(dport)` 永远=1（全是同一个字符串）→ 阈值永远不满足 → 规则不触发。
**验证**：检查生成数据 `grep dport ... | sort -u | wc -l`，如果都是同一个值就是踩坑了。

### 5. 大数据集用 `wfgen stream`（分块），别用单帧 `send`

`wfgen send` 把所有事件打成**一个** Arrow IPC 帧。wfusion 的 tcp_src 按 64KB batch 处理，单帧过大时：

- on-event 规则（ssh_brute）：能出告警，但处理慢（单实例要消化整个大帧）。
- close 规则（port_scan）：可能读不到完整数据。

**✅ 用 `wfgen stream`**：内部按 `CHUNK_SIZE=1000` 分块发送，wfusion 能增量处理。
`run.sh` 用的就是 stream 模式。

### 6. close 模式规则只在窗口 close/flush 时出告警

```wfl
match<sip:5m> {
    on event { c.dport | distinct | count >= 10; }   # 满足只置 event_ok
    and close { c | count >= 10; }                   # 还要等窗口 close
}  # CloseMode::And → 必须 event_ok && close_ok 才 emit
```

- **on-event 规则**（只有 `on event`，无 `close`）：窗口内满足即立即出告警。**短时验证用这类**（ssh_brute）。
- **close 规则**（有 `close`）：只在窗口超时（≥窗口时长）或优雅 shutdown（flush）时 emit。
- **run.sh 短跑**（<5min）出不了 close 规则的告警是**正常的**，不是 bug。

**优雅 flush**：wfusion 已有信号处理（`wf-runtime/lifecycle/signal.rs`）。SIGINT/SIGTERM → `shutdown()` → 规则任务 `flush()`（`close_all` → emit close 告警）→ sink flush。run.sh 的 trap `kill $WFUSION_PID`（SIGTERM）会触发它。

## 排查清单：没告警时按顺序查

### Step 1：wfusion 起来了吗？sink 建了吗？

```bash
grep 'building sink' data/wfusion.log | wc -l   # 期望 >0
grep 'no sinks configured' data/wfusion.log      # 有 = 布局错，启动失败
```

0 个 `building sink` → sinks 布局问题（见规则 2）。

### Step 2：数据流进来了吗？

```bash
grep 'frame decoded' data/wfusion.log | grep -oE 'stream="[^"]+"' | sort | uniq -c
```

- 没有 frame → wfgen 没发 / TCP 没连上 / 帧格式错（见 `wf-config-guide.md`）。
- 只有部分 stream → wfgen 场景轮转未到（`stream` 模式按 `--interval` 串行轮场景）。
- 有 frame 但 `route report dropped_late>0` → event_time 是秒级（见规则 3）。

### Step 3：规则匹配了吗？

on-event 规则：看告警文件。
close 规则：**短跑不出是正常的**，要么跑够窗口时长，要么靠 SIGTERM flush。

```bash
# 规则读了多少数据
grep '<RULE_NAME>' data/wfusion.log | grep new_cursor | tail
```

### Step 4：告警序列化/下发了吗？

```bash
grep -c 'alert export error' data/wfusion.log     # 序列化失败（如 invalid ip literal ""）
grep -c 'matched no sink' data/wfusion.log        # yield_target 无路由 + 无 default
ls -la data/alerts/                                # 最终文件
```

- `alert export error: invalid ip literal ""` → yield 字段（如 `e.dip`）求值为空字符串。根因常是 compiler 没把别名纳入 tracked_bind_aliases（见"字段为空"小节）。
- `matched no sink` → 该 yield_target 没配路由且无 default sink。

## 常见错误速查

| 症状 | 根因 | 修复 |
|------|------|------|
| 改了代码行为没变 | `~/bin/wfusion` 是旧二进制 | `cargo build && cp`（规则 1）|
| 0 sink，告警全丢，无报错 | sinks 拍平进 `sink.d/` | 改回分层布局（规则 2）|
| 启动失败 `no sinks configured` | 同上（守卫触发）| 同上 |
| 时间是 1970，窗口错乱 | event_time 是秒级 | 重生成数据（规则 3）|
| `distinct(x)` 永远=1 | 用了不存在的生成器（`seq`）| 删覆盖走默认（规则 4）|
| on-event 出告警，close 不出 | close 模式需窗口超时/flush | 跑够时长或 SIGTERM flush（规则 6）|
| `invalid ip literal ""` | yield 字段求值为空 | 见下"字段为空" |
| wfusion 起不来但端口被占 | 残留进程 | `pkill -9 -f 'wfusion run'` |

## 诊断技巧：临时日志逐段追踪

管线断了但不知道哪一环时，在 `wf-runtime/src/engine_task/rule_task.rs` 的 `pull_and_advance` 加 `[TEMP-DIAG]` 临时日志，统计每 batch 的 pass/fail/matched：

```rust
let mut _diag_pass = 0u32; let mut _diag_fail = 0u32; let mut _diag_matched = 0u32;
// ... 在 event_matches_alias 失败时 _diag_fail++，advance 返回 Matched 时 _diag_matched++
// batch 结束打一行 summary
```

**用完务必删掉**（grep `TEMP-DIAG` 确认清零）。这个方法比静态分析快得多——本次定位"sink 从未构建""close 不产出"都靠它。

## 字段为空（`invalid ip literal ""`）的根因链

当 yield 里引用 `e.dip` 但求值为空字符串时，完整链路是：

1. `collect_bind_tracking_aliases`（`wf-lang/compiler`）**只处理 series 函数**（如 `count(e)`），不处理纯字段引用（`e.dip`）→ `tracked_bind_aliases` 不含 `e`。
2. `should_track_bind_alias("e")` 返回 false → `collect_alias_event` 不调用 → alias state 无字段值。
3. `build_eval_context` 里 `ctx.fields.get("dip")` 找不到 → 兜底 `Value::Str("")`。
4. `parse_ip_value("")` → `invalid ip literal ""` → 告警被丢弃。

**修复**（已在 wp-reactor）：compiler 的 `collect_bind_tracking_aliases` 增加 `Expr::Field(FieldRef::Qualified/Bracketed)` 分支，把 `e` 纳入 tracked；`build_eval_context` 暴露纯字段名（不只前缀格式 `_bind_e_field_dip`）。

## 内存治理：tracked alias 的 field_values 已封顶

`collect_alias_event` / `collect_event_fields` 原先对每个事件的**全字段无界累积**，高频窗口会 OOM。现在共用 `MAX_TRACKED_FIELD_VALUES=1024`（软上限 2×，超限才 trim，push 摊还 O(1)）：

- yield `.last()` 不受影响。
- L3 的 `collect_set`/`last` 在有界样本上工作。
- `first`/`stddev`/`percentile` 大窗口下变近似。
- close step 的阈值判定走独立累加器，**不受影响**。

**注意**：`max_memory`（如 `"64MB"`）是有效且必要的治理参数，不要无脑删。对 wf-rules 的规则 64MB 足够（单实例 ~360KB）；生产环境按容量定。

## 快速验证命令

```bash
cd wf-rules

# 清理 + 启动
pkill -9 -f 'wfusion run' 2>/dev/null; sleep 1
rm -f data/alerts/* data/wfusion.log
wfusion run --config test/wfusion.toml --work-dir . > /dev/null 2>&1 &
sleep 3

# 用 on-event 规则（ssh_brute）做最快验证——它会立即出告警
wfgen send --scenario scenarios/ssh_brute_quick.wfg \
  --input <regenerated-data>.jsonl --ws schemas/auth.wfs --addr 127.0.0.1:9800
sleep 15   # 单大帧处理较慢，等够

# 检查
grep -c 'building sink' data/wfusion.log        # >0
grep -c 'alert export error' data/wfusion.log    # 0
wc -l data/alerts/security.ndjson                # >0
pkill -9 -f 'wfusion run' 2>/dev/null
```

**记住**：先验证 on-event 规则（快），再处理 close 规则（慢，需 flush）。close 规则不出告警时，优先确认是不是窗口时长没跑够，而不是急着改代码。
