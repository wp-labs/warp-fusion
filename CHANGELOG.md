# Changelog

本文件记录 `wfusion` / `wfl` / `wfgen` / `wfadm` 面向使用者的变更。内部实现细节、依赖版本对齐和测试计数不在此展开。

## [0.5.4]

### 引擎（对齐 wp-reactor 2.0.10）

- 对齐 `wp-reactor` v2.0.10，核心是 join 索引多 key 支持与多规则对拍正确性修复：
  - **join 索引多 key**：多规则在同一窗口按不同 key 字段 join（q8 按 seller / q20 按 id 共窗）时，后注册者原先回退全窗扫描 O(全窗)×pending、混跑冻结；现在每个 key 字段各建索引，`mix` 混跑恢复可用。
  - **多规则正确性**：q8/q11/q6/q7 对拍定位的一组修复（join 索引字段校验 / 分片冲突检测 / stats 键字段注入 / close_all 水位对齐粒度）。
  - **snapshot join**：首批索引竞态 + gate 性能回退修复（q20 回退 10–12%→3.7%）。
  - 关机尾批丢失 + q13 中间管道消费竞态修复。

### 许可

- 许可证明确为 **Elastic License 2.0（ELv2）**：企业内部自用免费（含部署 / 修改 / 嵌入自有产品）；作为托管服务对外提供需商业授权。

## [0.5.3]

### 语言（WFL）

- **顶层列表 + `use` 导入（issue #73）**：`name = ("a", "b", ...)` 一处定义，多规则 `expr in <name>` / `expr not in <name>` 引用；`use "lists.wfl"` include 导入目标文件全部顶层列表（无可见性控制，全部可见）。
  - 编译期展开为字面列表 + **InList 类型检查**（元素-左值类型比对，字面与命名列表统一；混合类型/不兼容报错）。
  - 错误面：未知名 / use 目标缺失 / 循环引用 / 重名 → 报错（lint 与 compile 同一条路径）。
  - `wfl lint`/`test`/`replay`/`explain` 四命令统一走 `load_wfl_with_imports`（use 解析）；`wfl fmt`（tree-sitter 语法）对列表声明的支持待 tree-sitter-wfl 同步更新。

### 引擎（对齐 wp-reactor 2.0.9）

- 对齐 `wp-reactor` v2.0.9（内部引擎修复）。

### wfgen

- **stats 规则接入 oracle 对拍**：q15–q19 verify 一致；oracle 按绑定窗口喂行 + 中间事件入队。

## [0.5.2]

### 引擎（对齐 wp-reactor 2.0.8）

- 对齐 `wp-reactor` v2.0.8；`mimalloc` 周期回收（`WF_COLLECT_MS`，默认 5s），q18 RSS 显著优化。

## [0.5.1]

### wfusion

- **mimalloc 内存分账**：`mi_process_info` 暴露的进程内存计入 `metrics alloc.*`，可观测真实内存占用。

### wfgen

- **发送缓冲可配**：`WFGEN_SEND_BUF`（默认 1MB）；perf-diag 发送 8KB→1MB（注入 620MB/s→6.9GB/s）。
- **perf-diag 流式发送**：消除整文件读入内存（30M 6.4GB×2 峰值下降）。

## [0.5.0]

### wfgen —— NEXMark 数据生成与 oracle 对拍

- **数据生成对齐 Flink 官方**：`gen-nexmark` 分布参数逐项修正（字符串字段 / extra padding / 固定 100µs 事件速率 / nextExtra 区间 / cold 90% / horizon 毫秒取整）；`bid.url` 对齐官方 `getBaseUrl`（3 段目录，支撑 q22）；`bid` 增 `channel_id` 字段（q21 对齐）。
- **`gen-nexmark --check` 自检**：值域 / 时间戳 / 流计数 + md5 指纹 + 流序自检；`--check` / `verify-nexmark` 输出 Flink NEXMark 符合性声明。
- **`verify-nexmark` oracle 对拍**：新增 Rust 版 NEXMark ground-truth 模拟器，改用真实 WFL 规则引擎对拍；补 deferred join / 帧内跨流时间序 / join 窗口状态（q21 打通）/ 中间输出 feed 下游 + 并查集分组（q13 双规则链）；`known-diff` 机制（q12/q17）；按 auction 分片并行（100M 5min→44s）。
- **`diff` 命令**：分层文件比对（L1 哈希相同性 / L2 Myers 差异量 / L3 `--detail` 定位）。
- **终端进度条**：`gen-nexmark` / `verify-nexmark`（stderr、仅 TTY）；非 TTY 降级输出完成摘要。
- **`send-arrow` 注入控制**：`--rate-bytes` 限速（默认 0=不限速）；1MiB 大缓冲 TCP copy（替代 8KiB）。

### 性能诊断（perf-diag）

- **哨兵四元组体系**：`wfgen`/`wfusion` 支持 `--perf-diag` 驱动；`send-arrow`/`stream` `--sentinel <n>` 在数据末尾追加 `__wf_sentinel` 完成帧（分连接哨兵），用于精确 EPS/CPU 统计。

### wfadm

- `wfadm init` 自动生成 `business.d/sentinel.toml`（perf-diag 哨兵落盘组）；docker 默认设置补 sentinel sink 模板。

## [0.3.1]

### 引擎（对齐 wp-reactor 1.0.2）

- 对齐 `wp-reactor` v1.0.2，预读预算（`parse_buffer_bytes`）改用内容字节记账：
  - **记账单位修正**：预算不再按解码后 Arrow 内存计（IPC 解码会结构性高估真实内存 ~10×，把管线槽位卡死），改按内容字节（≈ wire）计，与窗口记账口径一致；
  - **默认 256MB → 128MB**：避开 256MB 的 12-14GB RSS 平台，实测吞吐不降反微升（q1 100M 6.13M / RSS 5.88GB）；要高吞吐显式调大（512MB-2GB 为甜点，4GB 过度缓冲反而退化）。

### wfgen

- **`shard-frames` 分片文件 + 多连接回放**：`shard-frames` 把一个帧文件按键拆成 N 个分片文件（`--shards` / `--shard-keys`，同 key 必落同一文件）；`send-arrow --shard-files` 让每条 TCP 连接 raw-copy 一个分片文件、零解码回放，同 key 同连接（键闭包），有状态规则也安全——多连接供给并发（C-UCP）的正确打开方式。
- **修复流结尾丢帧**：一个流结束时，剩余不足一帧的行以前被打成 `tail` 标签，引擎按标签路由时整帧丢弃（100M 丢 1.4M 行，bench 卡在超时等待）。现在结尾行沿用原流标签落帧，不再丢数据。

## [0.3.0 Unreleased]

### 引擎（对齐 wp-reactor 1.0.0）

- 对齐 `wp-reactor` v1.0.0（本地 `crates/wf-*` 路径），获得规则分片聚合与共享资源约束：
  - **`conv` 规则可跨 shard 聚合**：fixed 窗口带 `conv`（sort/top/dedup/where）的规则现在可分片，各 shard 的 close 输出经水位 barrier 汇聚后在合并批上做全局 `apply_conv`（全局 top-N / sort），再统一限流输出；EOS 排空为完整数据出口，cancel 丢弃未 seal 部分桶。
  - **跨 shard 共享限流/预算**：`max_throttle` / `max_instances` / `max_memory_bytes` 由各 shard 独立执行改为 `SharedLimits` 共享原子约束（限流为共享滑窗、`max_instances` 精确 CAS、FailRule 规则级 latch），`rule_instances` 指标跨 shard 求和。
  - 语义修复：`max_instances` 分片下精确（原 read-then-act 可超限 ≤shard_count-1）；conv 阶段超限按 `on_exceed` 分派（FailRule 正确 latch）；`shards=1` 行为不变。

### 语言（WFL）

- `on event<accu>` 窗口内累积：达到阈值后，窗口内计数与证据持续累积、不清零，后续每条满足条件的事件都触发一次输出，展示运行累计值与全部证据，直到窗口过期才整体重启（对齐 `wp-reactor` v0.4.0）。

### 示例

- `ssh_brute_force` 示例改用 `on event<accu>`：检测到爆破后（count >= 10）每次失败登录都触发，输出运行累计 count 与逐条递增的证据。

### wfgen

- `--no-wfl` 跳过整个 WFL 管线（不加载/编译规则、无 injection），生成纯背景随机事件。
- `--no-oracle` 仍编译 WFL（**保留 injection `use()` 固定值**），只跳过 oracle/expected 输出（不产出 `.except.*` 侧车文件）。
- 规则同目录的 `_global.wfl` 声明的 `yield preset` 自动合并进规则，`yield <target> : <preset>` 可复用公共输出字段。

### wfusion

- `[metrics] console_output = false` 可关闭控制台统计日志。

## [0.1.43]

- 版本推进，对齐 `wp-reactor` v0.1.42（含其内部引擎修复）。

## [0.1.42]

- 版本推进（alpha），对齐 `wp-reactor` 内部修复；无新增用户功能。

## [0.1.41]

### 语言（WFL）

- `on event seq { ... }` 有序序列与 `on event any { ... }` 无序共现：攻击链检测，支持步间 `within` 时间约束、`not has ... within` 否定步、`consec` 严格相邻（`skip = to_next` 延后到 L3）。

### wfusion

- 输入静默时规则实例按窗口 TTL 自动过期释放，不再残留状态。

## [0.1.40]

### wfusion

- `[logging] level` 是日志等级唯一权威，不再被 `RUST_LOG` 静默覆盖；模块提级用 `[logging].modules`。

## [0.1.39]

### wfgen

- `--out` 改为可选，与 `--send` 解耦为四种组合；两者都缺时返回清晰的参数错误。
- `--no-oracle` 重命名为 `--no-wfl`（跳过整个 WFL 管线）；`--no-oracle` 保留为向后兼容别名。

## [0.1.38]

### 语言（WFL）

- 项目级 `_global.wfl` 规则 prelude：声明公共 `yield preset`，`yield <target> : <preset>` 复用。
- 字符串 helper：`sha1_n(text, n)`、`join(...)`、`join_by(sep, ...)`。
- 规则执行 DEBUG 漏斗日志与状态机 progress 诊断（便于定位规则卡在哪步）。

### wfusion

- stream window 支持 `object` / `array` / `array/T` 输入字段，`merge()` 做浅合并富化。

## [0.1.35-alpha] — 2026-07-22

### 语言（WFL）

- `object { ... }` / `array [ ... ]` 结构化字面量、`merge()`；stream window 支持结构化输入字段。

### wfadm

- `wfadm self update`（含 `self check`），安装 warp-fusion 套件。

## [0.1.34-alpha] — 2026-07-20

### wfadm

- `self update` 安装 warp-fusion 全套二进制；manifest channel 选择修复。

## [0.1.32-alpha] — 2026-07-20

### wfadm

- `self update` 默认远端 manifest URL 按 channel 选择分支（`alpha` / `beta`），避免错误读取其他分支的 manifest。

## [0.1.31-alpha] — 2026-07-19

### wfadm

- `self update` 对齐 `wpadm`：新增 `--channel` / `--updates-base-url` / `--updates-root` / `--json` / `--yes` / `--dry-run` / `--force`；更新源改用 manifest canonical target triple，修复 macOS arm64 短名 404。

## [0.1.30-alpha] — 2026-07-19

### wfusion

- 内置 `__window_miss` 诊断窗口：未知 stream schema / 缺失 stream tag 字段可通过 monitor sink 观察。

## [0.1.29-alpha] — 2026-07-14

### wfusion

- sink 组支持 `wf_meta_disable`，用 `__wfu_*` 等 wildmatch pattern 禁用元字段输出。
- admin API reload 语义：requires-restart 变更返回 `restart_required`，不再作为 409 失败。

## [0.1.28] — 2026-07-13

### 语言（WFL）

- helper：`now()` / `now_s()` / `now_ms()` / `now_us()` / `now_ns()`、`is_blank()` / `null_if_blank()` / `default_if_blank()`、`md5()` / `sha1()` / `sha256()` / `hex()` / `stable_id()`。
- 源码感知的 WFL 解析/编译诊断：出错时输出文件、行列和源码片段。

### wfusion

- `.wfs` 用 `window.stream_tag` 作为数据分发键（替换旧 `stream`）；上游 carrier 统一为 `wp_oml_name`。

## [0.1.24] — 2026-07-09

### wfusion — Admin API 发布协议对齐 wparse（Break）

- `POST /admin/v1/reloads/model` 参数改为 `wait` / `update` / `version` / `group` / `timeout_ms` / `reason`；移除旧 `full` / `update_remote` 语义。
- 非 loopback `admin_api.bind` 必须启用 TLS；`admin_api.auth.mode` 仅接受 `bearer_token`。

## [0.1.23] — 2026-07-08

### wfusion / wfadm

- daemon 支持 `update=true` 远程更新（git fetch + 版本解析 + sync managed dirs）后热重载，失败自动回滚。
- `wfadm conf update` 委托远程更新 API。

## [0.1.22] — 2026-07-07

### wfusion / wfgen

- 修复：非 loopback admin API 允许 TLS disabled 启动。
- `wfgen --version`。

## [0.1.21] — 2026-07-06

### wfusion — 路径基准变更（Break）

- 相对路径默认相对**工作目录**，不再相对配置文件所在目录。`wfusion.toml` 中的相对路径（`schemas` / `rules` / `sinks` / `sources_dir` 等）需去掉一层 `..`；`business.d/*.toml` 的 `base` 路径同理。`--work-dir` 显式指定时优先。

## [0.1.17] — 2026-07-01

### wfusion

- admin API 在线热重载：`POST /admin/v1/reloads/model`（L1 规则热替换 / L2 增量增窗 / L3 局部重建 / L4 重启）。
- **Break**: 移除 `wfusion config` 子命令（功能迁移到 `wfadm config`）。

### wfadm

- `conf update`（远程规则源同步）、`init --repo`（从远程模板初始化项目）。

## [0.1.16] — 2026-06-28

### wfusion

- **Break**: `wfusion run` 拆为 `wfusion daemon` / `wfusion batch`，`mode` 由 CLI 显式控制，不再读取配置。
- **Break**: 移除 `wfusion rule`（与 `wfl` 二进制重复）。
- admin API HTTP server（`GET /admin/v1/runtime/status`）。

### wfadm

- `check`（WFL/WFS/WFG 深度校验）、`conf diff`、`engine status` / `engine reload`、`self-update`。

## [0.1.11] — 2026-06-21

### wfgen

- 通过 `wp-core-connectors` 的 TcpArrowSink 发送数据（RFC6587 framing，匹配 wfusion `tcp_src`）。
