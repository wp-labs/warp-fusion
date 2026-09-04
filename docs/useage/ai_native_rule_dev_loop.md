# AI 原生规则开发闭环 · 能力说明（ai-native rule dev loop）

> 一句话：**让「AI 写规则 → 引擎自动验证 → 结构化回执」成为一等公民工作流**——
> 规则（wfl/wfgen 工具链 + wp-reactor 契约执行）对每条 AI 生成的规则，都提供
> 低成本的证明路径：语义正确 + 覆盖充分 + 性能可接受，回执让 AI 自己读得懂"差在哪"。
>
> 适用对象：AI 编程助手（自动消费回执迭代规则）、规则开发者（快速自证）、
> CI（把回执当门禁）。本说明描述 warp-fusion 引擎侧已落地能力（alpha，2026-09）；
> wf-examples 是消费方/验证场景，不在本说明范围。

## 1. 能力总览

| # | 能力 | 命令 | 解决的问题 |
|---|---|---|---|
| 0 | 编译期检查 | `wfl lint` / `wfl explain` | 语法/类型错误在运行前拦住；编译后形态人/AI 可读 |
| 1 | 语义自证（契约测试） | `wfl test` | 逐条手写的 `expect` 证明"规则按作者意图执行" |
| 2 | 对抗测试生成 | `wfl test --gen-negatives` | 机器从规则结构补负例，打破"AI 写规则 + AI 自证" |
| 3 | 检测意图验证 | `wfl intent`（`.wfi` 样本集） | 从正/负样本证明"意图本身对"——漏报/误报可测 |
| 4 | 真实回放 / 对拍 | `wfl replay` / `wfl verify` | 真实数据流 + oracle 逐条比对 |
| 5 | 性能护栏 | `wfgen perf-diag --gate` | 语义对还不够，性能可接受才放行 |

**核心承诺**：每条规则跑出的回执都带 schema 版本、结构化明细与自解释失败原因；
`verdict` 与退出码一致（FAIL → exit 1），AI/CI 可按退出码直接拦。

---

## 2. 逐项能力

### 2.0 编译期检查 —— `wfl lint` / `wfl explain`

```bash
wfl lint rules/ssh_brute_force.wfl --schemas "schemas/*.wfs"
wfl explain rules/ssh_brute_force.wfl --schemas "schemas/*.wfs"   # 编译后规则结构可读
```
- 与真实编译同一条 `use` 导入解析路径：未知名/循环/重名不绕过
- 无问题 → exit 0；有问题 → 逐条可定位

### 2.1 语义自证 —— `wfl test`（契约测试）

WFL 语言内嵌 `test` 块：`input { row/tick }` → `expect { hits cmp N; hit[i].字段 cmp … }`。
`expect` 是**独立语义 oracle**——AI 改规则没跟着改期望，测试就 FAIL。

```bash
wfl test rules/ssh_brute_force.wfl --schemas "schemas/*.wfs"          # human
wfl test rules/ssh_brute_force.wfl --schemas "schemas/*.wfs" \
      --shuffle --runs 10 --format json                               # 乱序多轮 + JSON 回执
```
- 引擎状态机进程内执行（秒级）；失败断言带 expected/got 文本
- 空测试集 = pass（exit 0），与 human 模式语义一致

### 2.2 对抗测试生成 —— `wfl test --gen-negatives`（L2）

从**规则结构静态反演**出反例，断言独立于 AI 写的 expect——AI 不写负例，机器从 guard 补：

```bash
wfl test rules/ssh_brute_force.wfl --schemas "schemas/*.wfs" --gen-negatives
```
- 覆盖形态：bind guard `field == literal` / `field != literal`；对引用该 alias 的每行
  row 克隆并把 guard 字段改为违反值追加，断言"反例行被过滤、hits 不变"
- 变体并入回执 `tests[]`，name 标注 `[neg: bind …]`；基线失败/无 guard/字段未设置 →
  不生成
- **如实原则**：复合 guard（函数/嵌套/逻辑组合）不假装覆盖——如实跳过，回执不会虚报覆盖
- `--shuffle/--runs` 提供乱序与时序扰动的鲁棒性维度

### 2.3 检测意图验证 —— `wfl intent`（L3，.wfi 正/负样本集）

契约测试证明"按作者意图执行"，`wfl intent` 进一步证明**意图本身对不对**（漏报/误报）。

`.wfi` = 纯 `test` 块集合（合法 .wfl 子集，无 rule），每个 test 块即一条样本：

```bash
wfl intent rules/ssh_brute_force.wfl --intent samples.wfi \
      --schemas "schemas/*.wfs" --format json
```

| .wfi 中的 expect | 样本类别 | 检查 |
|---|---|---|
| `expect { hits >= 1; }` | **正样本**（该检出） | 漏报：0 命中 → FAIL |
| `expect { hits == 0; }` | **负样本**（不该检出） | 误报：>0 命中 → FAIL |

- 分类严格 canonical：命中数阈值（`hits == 5`/`>= 3`）、无 hits 断言、正负断言并存
  （自相矛盾）→ 明确报错，**不静默归类**
- 可附加 `hit[i].score` 等字段断言增强精确性（引擎对 expect 全量校验）
- 样本执行错误（引用不存在字段/alias = 样本写错）单列 `errors`——**不虚增漏报/误报**

### 2.4 真实回放 / 对拍 —— `wfl replay` / `wfl verify`

```bash
wfl replay  rules/ssh_brute_force.wfl --schemas "schemas/*.wfs" --input data/auth_events.ndjson
wfl verify  rules/ssh_brute_force.wfl --schemas "schemas/*.wfs" \
      --input data/auth_events.ndjson --expected data/expected.ndjson [--format json]
```
- replay 按每行 JSON 的 `_stream` 字段路由到 window/bind（数据文件须带流标签，见
  `examples/rules/ssh_brute_force` 验证记录）；EOF 统一 close_all，与引擎 flush 收口一致
- verify 逐事件按真实 watermark 扫窗口过期，与引擎逐批语义对齐；oracle 比对支持
  score/time 容差（CLI 或 meta 文件）

### 2.5 性能护栏 —— `wfgen perf-diag --gate`（L4）

把 perf-diag 墙梯测量**机制化为自动门禁**：语义对的规则若把整集拖垮（全窗扫描/
高基数 key），回执必须能拦。

```bash
# 首次校准：留存本机同 n-list 基线
wfgen perf-diag --diag conf/perf-diag.toml --frames data/burst.frames \
      --record-baseline data/perf_wall.baseline.txt
# 门禁：任一断言 FAIL → verdict=FAIL → exit 1
wfgen perf-diag --diag conf/perf-diag.toml --frames data/burst.frames \
      --gate conf/perf-gate.toml --format json
```

`conf/perf-gate.toml`：

```toml
rule_count = 376                 # 规则集大小（摊单规则成本）
[absolute]                       # 绝对兜底（机器校准硬上限）
rules_eps_min = 150000           # 整集 rules 档 EPS 下限
per_rule_ns_max = 300            # 单规则成本 = (1e9/eps_rules − 1e9/eps_floor)/rule_count
[relative]                       # 相对防回归（与基线墙表同 (档,N) 比）
baseline = "data/perf_wall.baseline.txt"
max_regression_pct = 20
```

- 每档取**最大 N** 行（固定开销摊薄后最稳）；相对回归只与基线同 `(档,N)` 比，
  缺档/缺 N 明确报错提示重录
- **防静默失效**：空门禁 / 拼错 key（deny_unknown_fields）/ 负阈值 / `--gate` 与
  `--record-baseline` 同给 → 全部显式报错——门禁开着却"没拦任何东西"不允许存在

---

## 3. 回执契约（三份 schema）

通用约定：`--format json` → **stdout 纯净单份 JSON**、进度/错误走 stderr；
`verdict` 与退出码一致；`schema` 字段版本化。

| 命令 | schema | verdict 依据 |
|---|---|---|
| `wfl test --format json` | `wfl-test-report/v1` | `summary.failed > 0` → FAIL |
| `wfl intent --format json` | `wfl-intent-report/v1` | 同上；附 `false_negatives`/`false_positives`/`errors` 计数 |
| `wfgen perf-diag --format json` | `wfgen-perf-report/v1` | 门禁任一 check FAIL → FAIL |

失败项一律自解释（示例）：
- `漏报：正样本应触发（expect hits >= 1），实际 0 命中——规则漏检该输入`
- `hits: expected hits == 0, got 1`（引擎断言 diff）
- `rules 档 EPS=150000 vs 基线 200000（回退 25.0%，允许 ≤20.0%）`

---

## 4. 已用真实样例验证（能力不是纸面）

`examples/rules/ssh_brute_force`（单 IP 5 分钟 ≥10 次 SSH 失败 → 告警，yield 带统计证据 + 证据 ID 集合 + 时间边界字段）端到端：

| 能力 | 结果 |
|---|---|
| `wfl lint` | 无问题 |
| `wfl test`（4 契约：触发/不足阈值/成功登录/多 IP 隔离） | 4/4 PASS；`--shuffle --runs 10` 亦 PASS |
| `wfl test --gen-negatives` | 如实 0 反例（bind guard 为复合条件，不假装覆盖） |
| `wfl intent`（1 正 + 3 负样本） | 漏报=0 误报=0 PASS；故意写反的样本 → 漏报=1 误报=1、exit 1 |
| `wfl replay`（22 事件 fixture） | 1 match（origin=event，第 10 次失败触发） |
| `wfusion batch`（全引擎） | 1 条告警：evidence `auth-001..010`×10、trigger=10.0、error sink 空 |

---

## 5. 边界（诚实声明）

- **本闭环不做自动修规则**——只承诺"AI 改规则后，回执能证明它对在哪、错在哪"；
  自动修复是回执之上的策略层
- L2 反例生成只覆盖简单 bind guard；复合 guard 如实跳过（不会虚报覆盖）
- L4 绝对阈值与机器相关，须在目标机用 `--record-baseline` + 同 n-list 校准；相对回归跨机器稳健
- 尚待：统一合并回执命令（lint+test+intent+perf 一次出单份 verdict）、wf-examples 消费侧接入、Sigma 意图 → 样本收编

## 6. 相关文档

- 契约测试语法（input/expect 权威）：wp-reactor [rule-writing.md](../../../wp-reactor/docs/user-guide/rule-writing.md)
- 规则/配置语言：[rules.md](rules.md) · [schema.md](config/schema.md)
- 规则内存上限校准（limits.max_memory 怎么定）：[memory-limits.md](memory-limits.md)
- 性能口径（忙墙/等墙、墙梯）：wp-reactor perf-diag 设计（L4 门禁与其逐行同口径）
