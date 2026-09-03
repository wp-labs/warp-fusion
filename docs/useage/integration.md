# 开发者集成指南：把 WarpFusion 接入你自己的系统

把 WarpFusion 作为检测/分析子系统集成进自有产品。搭建一个检测任务的顺序是
固定的——**数据流先于规则**：

> **① 设置数据来源 → ② 定义输入窗口 → ③ 定义告警输出窗口 → ④ 设置告警输出 → ⑤ 编写计算规则**

先让数据进得来、出得去，再写规则——规则只是把前四步串起来的那一层。
本文以 `examples/rules/port_scan_whitelist`（端口扫描检测）为贯穿示例；每个
文件都有对应参考文档。

## 最小工程布局

```text
your_detection_project/
├── wfusion.toml               # 引擎配置（windows / runtime / admin_api 等）
├── windows.toml               # 窗口资源/时间策略（条目可省，文件被引用）
├── schemas/*.wfs              # 输入窗口 + 告警输出窗口定义
├── rules/*.wfl                # 计算规则（可多文件，可有 _global.wfl preset）
├── topology/sources/          # 数据来源声明（或写在 wfusion.toml [[sources]]）
└── topology/sinks/            # 告警输出路由
```

---

## 第 1 步：设置数据的来源

决定**事件怎么进入引擎**：你的系统作为发送方，引擎作为监听/回放方。

```toml
# wfusion.toml —— 常驻模式 + TCP 来源
mode = "daemon"

[[sources]]
type = "tcp"
enable = true
key = "ingest"

[sources.params]
listen = "tcp://0.0.0.0:9800"
data_format = "ndjson"            # ndjson | arrow_framed | arrow_ipc
stream_tag_field = "wp_oml_name"  # 每行此字段的值 → 路由到哪个输入窗口
```

- 推荐 **TCP + ndjson**：无需 SDK，任何语言逐行发 JSON 即可：
  `{"wp_oml_name":"netflow","sip":"10.0.0.1","dip":"10.0.0.2",...,"event_time":"..."}`；
- **与 warp-parse 联动**用 `arrow_framed`（帧级 tag 路由）；
- 还有 **Kafka** 源、**文件回放**（ndjson/csv/arrow 回放历史数据，`mode="batch"`）。
  完整参考：[config/source.md](./config/source.md)、[wparse-window-routing.md](./wparse-window-routing.md)。

关键决策：**stream_tag 策略**——单业务源可固定 `stream_tag = "netflow"`；
多业务源（一个端口多类事件）用 `stream_tag_field` 逐行分发。tag 值直接决定
第 2 步哪个窗口接收。

## 第 2 步：定义数据的输入窗口

每个来源 stream_tag 对应**一个输入窗口**：窗口声明了事件契约——字段名、类型、
时间字段与保留时长。来源与窗口靠 `stream_tag` 对齐。

```wfs
# schemas/network.wfs
window conn_events {
    stream_tag = "netflow"      # 与第 1 步的 tag 一致
    time = event_time           # 事件时间字段（每条事件必须携带）
    over = 30m                  # 窗口保留时长
    fields {
        sip: ip
        dip: ip
        dport: digit
        protocol: chars
        action: chars
        event_time: time
    }
}
```

- 类型：`ip / digit / chars / bool / time / hex / float` + 结构化 `object / array`
  （完整类型见 [schema.md](./schema.md)）；
- 来源-窗口对应关系：`stream_tag`（固定或 `stream_tag_field` 字段值）==
  window 的 `stream_tag`。未知 tag 进内置 miss 诊断（不崩引擎）；
- `over` 决定窗口保留多久（数据量/内存规划，见 [config/window.md](../config/window.md)）。

## 第 3 步：定义告警的输出窗口

`yield` 的目标也是一个 window（`over = 0` 的**输出窗口**）——它的字段就是
**一条告警的结构**，由规则的 yield 子句逐字段填充。

```wfs
# schemas/network.wfs（续）
window network_alerts {
    over = 0
    fields {
        sip: ip
        alert_type: chars
        detail: chars
        department: chars
        owner: chars
    }
}
```

> 输出窗口不存事件、不做窗口聚合，只是「告警管道」的类型声明：第 4 步的
> sink 按它路由，第 5 步的规则向它 yield。

## 第 4 步：设置告警窗口的输出

把第 3 步的输出窗口**接到你的消费通道**：sink 按 `windows` 命中输出窗口名。

```toml
# topology/sinks/business.d/network_alerts.toml
[sink_group]
name = "alerts"
windows = ["network_alerts"]          # 命中第 3 步的输出窗口

[[sink_group.sinks]]
connect = "file_json"                 # connector：文件（ndjson，一行一条告警）
name = "alerts_out"
[sink_group.sinks.params]
file = "alerts.ndjson"
```

- 你的系统消费 `alerts.ndjson`（或换成 Kafka / 自定义 connector——`connect`
  引用 `connectors/sink.d/` 下的 connector 定义）；
- 每行 = 规则 yield 的业务字段 + `__wfu_*` 元字段（`__wfu_rule_name` /
  `__wfu_score` / `__wfu_entity_id` / `__wfu_fired_at` …），可用
  `[sink_group] wf_meta_disable = [...]` 裁掉；
- 目录约定三层：`business.d/`（业务告警）· `infra.d/default.toml`（兜底）·
  `infra.d/error.toml`（错误通道）· `infra.d/monitor.toml`（指标）。
  完整参考：[config/sink.md](../config/sink.md)。

## 第 5 步：编写计算规则

规则把前四步串起来：绑定输入窗（第 2 步）→ 窗口内聚合/时序匹配 → 命中评分 →
向输出窗（第 3 步）产出告警（第 4 步送出）。

```wfl
# rules/port_scan_whitelist.wfl
use "network.wfs"

rule port_scan_whitelist {
    events { c : conn_events && action == "syn" }     // ① 绑定输入窗口（第 2 步）
    match<sip:5m> {                                   // 按源 IP 开 5 分钟窗口
        on event { c.dport | distinct | count >= 10; }   // 5 分钟内 ≥10 个不同端口
        and close { total: c | count >= 10; }
    } -> score(80.0)
    entity(ip, c.sip)                                 // 实体 = 源 IP（跨规则评分）
    yield network_alerts (                            // ③ → 输出窗口（第 3 步）
        sip = c.sip,
        alert_type = "port_scan",
        detail = "distinct ports >= 10"
    )
}
```

- 规则内联测试自带回归资产（`test { input { row(...) } expect { ... } }`）；
  发布前 `wfl lint` + `wfl test` 把关；
- 多规则共享的输出模板用 `_global.wfl` 的 yield preset；
- 语法全集见 [rules.md](./rules.md)。

---

## 简单示例：hello_detection（照抄可跑）

想整目录照抄的最小工程，看 `examples/rules/hello_detection`：3 条 failed 登录
事件，同源 1 分钟窗口内计数 ≥ 3 → 产出 1 条 `brute_login_mini` 告警。下面按
「数据流先于规则」的次序给出全部文件。

```text
hello_detection/
├── wfusion.toml                   # 引擎配置：目录 + runtime
├── windows.toml                   # 窗口资源调优（可缺省）
├── schemas/hello.wfs              # ②输入窗 auth_events ③输出窗 mini_alerts
├── rules/hello_detection.wfl      # ⑤规则（含内联测试）
├── data/mini.ndjson               # 输入数据（3 行 failed）
└── topology/
    ├── sources/ingest.toml        # ①数据来源：文件回放 stream_tag=auth
    └── sinks/
        ├── business.d/mini_alerts.toml   # ④输出窗 → alerts.ndjson
        ├── connectors/sink.d/file.toml   # ④file connector 定义
        └── infra.d/               # 兜底 / 错误通道（约定目录，可后补）
```

### ① 数据来源 `topology/sources/ingest.toml`

`file_src` 逐行读入 `data/mini.ndjson`，统一打上 `stream_tag = "auth"`：

```toml
connect = "file_src"
enable = true
key = "mini_input"

path = "data/mini.ndjson"
stream_tag = "auth"
data_format = "ndjson"
```

要接你的事件流：把 source 换成第 1 步的 TCP / Kafka 即可，后四步不动。

### ② 输入窗 + ③ 输出窗 `schemas/hello.wfs`

同一个文件里两个 window：`auth_events` 接收来源（`stream_tag = "auth"`、
保留 1h）；`mini_alerts` 是 `over = 0` 的输出窗——它的字段就是一条告警的结构：

```wfs
window auth_events {
    stream_tag = "auth"
    time = event_time
    over = 1h
    fields {
        sip: ip
        user: chars
        action: chars
        event_time: time
    }
}

window mini_alerts {
    over = 0
    fields {
        sip: ip
        alert_type: chars
        detail: chars
    }
}
```

### ④ 告警输出 `topology/sinks/`

- `business.d/mini_alerts.toml`：命中输出窗 `mini_alerts`，经 `file_json`
  connector 写入 `alerts.ndjson`（batch 时落在 work-dir 的 `data/out_dat/`）：

```toml
version = "1.0"

[sink_group]
name = "mini_alerts_out"
windows = ["mini_alerts"]

[[sink_group.sinks]]
connect = "file_json"
name = "alerts"

[sink_group.sinks.params]
file = "alerts.ndjson"
```

- `connectors/sink.d/file.toml`：声明上面 `connect = "file_json"` 引用的
  connector：

```toml
[[connectors]]
id = "file_json"
type = "file"
allow_override = ["file"]

[connectors.params]
fmt = "json"
file = "out/default.jsonl"
```

- `infra.d/` 的 `default.toml`（`windows = ["*"]` 兜底）与 `error.toml`
  （错误通道）是约定目录，去掉不影响本例跑通（说明见第 4 步）。

### ⑤ 规则 `rules/hello_detection.wfl`

绑定输入窗并只收 `action == "failed"`，按 `sip` 开 1 分钟匹配窗，组内计数
≥ 3 命中后评分并向输出窗 `yield`；文件尾部的 `test` 块是内联回归资产，
`row(a, ...)` 的别名 `a` 对应 `events { a : ... }`：

```wfl
use "hello.wfs"

rule brute_login_mini {
    events { a : auth_events && action == "failed" }
    match<sip:1m> {
        on event { a | count >= 3; }
    } -> score(60.0)
    entity(ip, a.sip)
    yield mini_alerts (
        sip = a.sip,
        alert_type = "brute_login_mini",
        detail = "3 failed logins in 1m"
    )

    limits {
        max_memory = "16MB";
        max_instances = 1000;
        on_exceed = throttle;
    }
}

test brute_login_fires_once for brute_login_mini {
  input {
    row(a, sip = "10.0.0.5", user = "bob", action = "failed", event_time = "2026-01-01T00:00:01Z");
    row(a, sip = "10.0.0.5", user = "bob", action = "failed", event_time = "2026-01-01T00:00:02Z");
    row(a, sip = "10.0.0.5", user = "bob", action = "failed", event_time = "2026-01-01T00:00:03Z");
  }
  expect { hits == 1; }
}
```

### 引擎配置与输入数据

`wfusion.toml` 把上面各目录串起来并给出 runtime 参数；`windows.toml` 是窗口
资源/时间策略（各条目可省、落到 `[window_defaults]` 默认，但文件被
`wfusion.toml` 引用、不能缺）。三份配套文件如下：

```toml
# wfusion.toml —— 指向 sources/sinks/windows，并给 runtime 参数
sources_dir = "topology/sources"
sinks = "topology/sinks"
windows = "windows.toml"

[runtime]
executor_parallelism = 2
rule_exec_timeout = "30s"
schemas = "schemas/*.wfs"
rules = "rules/*.wfl"

[logging]
level = "info"
format = "plain"
```

```toml
# windows.toml —— [window_defaults] 给默认，再按窗口名逐窗覆盖
[window_defaults]
evict_interval = "30s"
max_window_bytes = "64MB"
max_total_bytes = "256MB"
evict_policy = "time_first"
watermark = "1s"
allowed_lateness = "0s"
late_policy = "drop"

[window.auth_events]
mode = "local"
max_window_bytes = "64MB"
over_cap = "24h"

[window.mini_alerts]
mode = "local"
max_window_bytes = "1MB"
over_cap = "0s"
```

`data/mini.ndjson` 是 3 行输入——`_stream = "auth"` 对齐第 ② 步的
`stream_tag`，`event_time` 是窗口时间字段：

```json
{"_stream":"auth","sip":"10.0.0.5","user":"bob","action":"failed","event_time":"2026-01-01T00:00:01Z"}
{"_stream":"auth","sip":"10.0.0.5","user":"bob","action":"failed","event_time":"2026-01-01T00:00:02Z"}
{"_stream":"auth","sip":"10.0.0.5","user":"bob","action":"failed","event_time":"2026-01-01T00:00:03Z"}
```

### 跑起来看结果

```bash
cd examples/rules/hello_detection
wfl lint rules/hello_detection.wfl --schemas "schemas/*.wfs"   # No issues found.
wfl test rules/hello_detection.wfl --schemas "schemas/*.wfs"   # 1 tests: 1 passed
wfusion batch --config wfusion.toml --work-dir .               # rows=3，正常退出
cat data/out_dat/alerts.ndjson                                 # 恰好 1 条
```

`alerts.ndjson` 一行即为告警：`__wfu_rule_name=brute_login_mini`、
`__wfu_entity_id=10.0.0.5`（entity 来自 `entity(ip, a.sip)`），加上规则
`yield` 的 `sip / alert_type / detail` 业务字段；`error.ndjson` 为空。
`./run.sh` 把 lint + 内联测试 + batch + 断言串成一条命令。

### 想从更完整的工程起步？

- `examples/rules/port_scan_whitelist`：本页第 1～5 步正文的完整可运行版
  （多了白名单 provider 表、IP 富化 join、`_global.wfl` yield preset 与
  docker 编排），适合对照扩展；
- `examples/rules/` 其余目录按检测场景组织（见该目录 README），各自带
  `run.sh` 可一键验证；
- [wf-examples](https://github.com/wp-labs/wf-examples) 的 `getting_started`
  一键生成 17 规则的完整项目。

## 管理面：被你的系统观测与控制

- **Admin API**（HTTP，回环 + bearer）：状态查询、在线 reload 规则/配置、
  配置了 `[project_remote]` 时在线发布 → [admin_api.md](./cli/admin_api.md)；
- **metrics**：monitor sink / `--metrics`（[config/metrics.md](../config/metrics.md)）；
- **结构化日志**：级别/输出可配（[config/logging.md](../config/logging.md)）；
- **规则生命周期 CLI**：`wfadm`（init / 校验 / 发布），见 [cli.md](./cli/cli.md)。

## 集成注意事项

- **单机内存维度**：引擎为单机、纯内存、无分布式 exactly-once；高吞吐按
  窗口/规则容量规划节点，窗口内存上限在 `[window_defaults]`
  （[config/window.md](../config/window.md)）；
- **时间与迟到**：窗口按事件时间推进，配 `watermark` / `allowed_lateness` /
  `late_policy` 决定迟到事件行为；
- **超时与并行**：`[runtime] rule_exec_timeout` / `executor_parallelism`
  （[config/runtime.md](../config/runtime.md)）；
- **许可**：ELv2 允许企业内部自用与嵌入自有产品（含商业售卖你的产品、客户
  自行部署）；以**托管/SaaS 形态向第三方提供引擎能力**需另行商业授权
  （见仓库 README License 段）。
