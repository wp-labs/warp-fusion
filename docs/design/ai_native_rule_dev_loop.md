# AI 原生规则开发闭环设计（ai-native rule dev loop）

> 状态：**L0–L4 已落地（warp-fusion alpha，2026-09）**。本文同时是设计稿与落地记录，
> 回执 schema 以实现为准。范围 = warp-fusion 引擎侧（wfl/wfgen 工具链 + wp-reactor
> 契约执行），不含 wf-examples 基准仓（后者是消费方/验证场景）。
>
> 一句话：让「AI 写规则 → 引擎自动验证 → 结构化回执」成为一等工作流——AI 生成的每条规则，
> 都有低成本的证明路径（语义正确 + 覆盖充分 + 性能可接受）。
>
> 相关文档：[rules.md](../useage/rules.md) · [schema.md](../useage/config/schema.md) ·
> wp-reactor [rule-writing.md](../../../wp-reactor/docs/user-guide/rule-writing.md)（契约测试语法权威）·
> wf-examples `SIGMA_WFL_MAPPING_SKELETON.md`（检测意图对照，消费侧）

---

## 0. 落地记录（alpha）

| 级 | 落地内容 | 入口 / schema | commit |
|---|---|---|---|
| **L1** | `wfl test`/`wfl verify` 结构化回执 + schema 版本化（G1） | `wfl test --format json` → `wfl-test-report/v1`；verify → `wfgen-verify-report/v1` | `e8535aa` |
| **L2** | 对抗测试生成（G2）：bind-guard 反例 | `wfl test --gen-negatives`（`crates/wfl/src/gen_negatives.rs`） | `1aee3a3` |
| **L3** | 检测意图编译（G3）：`.wfi` 正/负样本 → 漏报/误报回执 | `wfl intent`（`crates/wfl/src/cmd_intent.rs`）→ `wfl-intent-report/v1` | `61b8238` |
| **L4** | 性能护栏并入（G4）：墙梯机制化为自动门禁 | `wfgen perf-diag --gate`（`crates/wfgen/src/cmd_perf_diag/gate.rs`）→ `wfgen-perf-report/v1` | `55189b6` / `4d8cc99`（6 轮 review 加固） |
| 验证 | 真实规则样例端到端（lint/test/shuffle/intent/replay/batch） | `examples/rules/ssh_brute_force`（含回放数据补 `_stream`） | `7ec75a0` |

**仍未做**（见 §2 闭环缺口）：① 统一合并回执命令（lint+test+intent+perf 一次跑出
`{compile, semantic, coverage, perf_delta, verdict}` 单份报告）；② wf-examples 消费侧
把 L3/L4 接进 Sigma 对照与规则门禁跑分（D6 先 (a) 后 (b)）；③ 自动修复策略层（本文不承诺）。

---

## 1. 背景与目标

### 1.1 现状：闭环的承重墙已存在（先如实盘点，不重复造轮子）

引擎侧**已经具备** AI 闭环的大部分原语，且独立于任何示例仓：

| 原语 | 入口 | AI 闭环作用 | 状态 |
|---|---|---|---|
| WFL 语言内嵌契约测试 | `wfl test`（`crates/wfl/src/cmd_test.rs`） | **独立语义 oracle**：`input { row/tick }` → `expect { hits cmp N; hit[i].字段 cmp … }`，断言规则对给定事件应产生什么——不断言"实现是否跑偏"，断言"意图是否正确" | ✅ 已有（含 `--shuffle/--runs` 乱序/多轮鲁棒性） |
| 契约执行引擎 | wp-reactor `wf-engine/…/match_engine/contract.rs` | 测试跑在引擎状态机上，进程内执行（快） | ✅ 已有 |
| replay + 对拍 | `wfl replay` / `wfl verify` | 真实数据流回放，逐条比对 oracle 期望文件 | ✅ 已有（replay 按每行 `_stream` 字段路由） |
| 编译期检查/意图可读 | `wfl lint` / `wfl explain` | 语法/类型错误前置拦截；规则编译后形态人/AI 可读 | ✅ 已有 |
| 确定性数据 | `wfgen gen` / datagen（seed） | 数据可复现 → 对拍可比 | ✅ 已有 |
| 性能墙诊断 | `wfgen perf-diag`（sentinel 协议） | 性能退化定位到管线段/规则族 | ✅ 已有 |

**关键认识**：WFL 的契约测试不是"同源 oracle 的回归检查"——`expect` 是**逐条手写的语义期望**，
独立于引擎实现。AI 把 `count >= 5` 改成 `count >= 3`，只要期望没跟着改，测试就会 FAIL。
这正是"AI 语义回执"的承重墙，**已经在语言层**，不需要另造语义参考实现。

### 1.2 真实缺口（曾缺四件事 → 均已闭合）

| # | 缺口 | 落地（要点见 §4） |
|---|---|---|
| G1 | **契约测试无结构化回执** | ✅ `wfl test --format json` → `wfl-test-report/v1`；human 输出不变；exit 与 verdict 一致 |
| G2 | **测试输入手工构造 → AI 自证** | ✅ `wfl test --gen-negatives`：从规则结构静态反演 bind-guard 反例（`field == lit` / `field != lit`），断言独立于 AI 写的 expect；复合 guard **如实跳过**不假装覆盖 |
| G3 | **检测意图未编译为断言** | ✅ `wfl intent` + `.wfi` 正/负样本集：漏报（应触发未触发）/误报（不该触发却触发）可测、结构化入回执 |
| G4 | **性能护栏未并入回执** | ✅ `wfgen perf-diag --gate`：墙梯测量后自动断言（绝对兜底 + 相对防回归），FAIL → exit 1 |

---

## 2. 目标闭环（定义）

```
AI / 开发者
   │  写 .wfl（规则 + 契约）
   ▼
wfl lint ──编译期错误（语法/类型）───────────┐
   ▼                                        │
wfl test ──语义 oracle（input/expect）───────┤── G1 ✅ 结构化回执 JSON
   ▼                                        │
wfl test --gen-negatives ──对抗/边界（G2）──┤── G2 ✅ 覆盖统计
   ▼                                        │
wfl intent ──检测意图 ↔ 断言（G3）──────────┤── G3 ✅ 漏报/误报计数
   ▼                                        │
wfgen perf-diag --gate ──性能护栏（G4）─────┘── G4 ✅ 单规则成本增量门禁
   ▼
回执：{ compile, semantic, coverage, perf_delta, verdict }   ← ①统一合并命令未做（分命令各自 verdict）
```

**判定语义**：verdict = PASS 当且仅当四者全绿。任何一环 FAIL → 回执带可定位的失败原因
（哪条规则、哪个 expect、哪个 guard 分支未覆盖、哪段性能墙），AI 据此迭代。
当前各环独立成命令、各自 `verdict`/退出码；**统一合并回执命令**是后续第一步（见 §0 未做①）。

---

## 3. 演进分级（落地状态）

| 级别 | 内容 | 依赖 | 解决的问题 | 状态 |
|---|---|---|---|---|
| **L0** | 契约测试 + replay/verify + lint | — | 手写规则的语义自证 | ✅ 已有 |
| **L1** | `wfl test`/`verify` 结构化回执（`--format json`）+ 回执 schema 版本化 | G1 | AI/CI 程序化消费 | ✅ `e8535aa` |
| **L2** | 对抗测试生成：guard 分支/阈值边界覆盖引导 + 乱序/时序扰动（在 `--shuffle/--runs` 之上叠加"生成 input"） | G2 | 打破 AI 自证，逼近属性测试 | ✅ `1aee3a3`（v1 范围：bind-guard 反例；复合 guard 如实跳过） |
| **L3** | 检测意图编译：正/负样本集（已知攻击 vs 正常）驱动断言生成，接 Sigma/ATT&CK 或自然语言意图 | G3 | 从"规则按意图执行"到"意图本身对"（漏报/误报可测） | ✅ `61b8238`（`.wfi` = 纯 test 块集合，wp-reactor 零改动） |
| **L4** | 性能护栏并入回执：单规则成本增量上限（perf-diag 墙梯机制化为自动门禁） | G4 | 语义对 + 性能可接受才能上线 | ✅ `55189b6` + `4d8cc99` |

---

## 4. 回执（已落地 schema 速览）

三条命令共用同一约定：`--format json` → **stdout 纯净单份 JSON**、进度/错误走 stderr；
`verdict` 与退出码一致（FAIL → exit 1）；schema 字段版本化。

### 4.1 `wfl test --format json` → `wfl-test-report/v1`

```json
{
  "schema": "wfl-test-report/v1",
  "rule_file": "rules/ssh_brute_force.wfl",
  "shuffle": false,
  "runs": null,
  "summary": { "total": 4, "passed": 4, "failed": 0 },
  "tests": [
    { "name": "brute_force_detected", "rule": "ssh_brute_force",
      "passed": true, "output_count": 1, "failures": [] }
  ],
  "status": "pass",
  "verdict": "PASS"
}
```
要点：失败断言带 expected/got 文本（如 `hits: expected hits == 0, got 1`）；L2 反例变体
并入同一 `tests[]`，name 标注 `[neg: bind …]`；空测试集 = pass。

### 4.2 `wfl intent` → `wfl-intent-report/v1`（L3，检测意图）

`.wfi` = 纯 test 块集合（合法 .wfl 子集，无 rule），每个 test 块即一条样本：

```
wfl intent rules/ssh_brute_force.wfl --intent samples.wfi [--format json]
```

- `expect { hits >= 1 }` → **正样本**（漏报检查：该检出）；`expect { hits == 0 }` → **负样本**（误报检查：不该检出）
- `classify` 严格 canonical：命中数阈值（`hits == 5` / `>= 3`）、无 hits 断言、正负断言并存
  （自相矛盾）→ 明确报错，不静默归类
- 通过与否以引擎 `run_test` 的 expect 全量校验为准（可附加 `hit[i].score` 等断言增强精确性）
- **漏报** = 正样本 0 命中；**误报** = 负样本 >0 命中；样本执行错误（引用不存在字段/alias）
  单列 `errors`，**不虚增漏报/误报**

```json
{
  "schema": "wfl-intent-report/v1",
  "rule_file": "rules/ssh_brute_force.wfl",
  "intent_file": "samples.wfi",
  "summary": { "total": 4, "passed": 4, "failed": 0, "errors": 0,
               "false_negatives": 0, "false_positives": 0 },
  "samples": [
    { "name": "should_fire", "kind": "positive", "rule": "ssh_brute_force",
      "passed": true, "hits": 1, "failures": [] }
  ],
  "status": "pass", "verdict": "PASS"
}
```

### 4.3 `wfgen perf-diag --gate` → `wfgen-perf-report/v1`（L4，性能门禁）

墙梯测量完成后按项目门禁自动断言；先 `--record-baseline` 留存同机同 n-list 基线，
再 `--gate conf/perf-gate.toml` 门禁：

```toml
rule_count = 376                 # 规则集大小（摊单规则成本）
[absolute]                       # 绝对兜底（机器校准硬上限）
rules_eps_min = 150000           # 整集 rules 档 EPS 下限
per_rule_ns_max = 300            # 单规则成本 = (1e9/eps_rules − 1e9/eps_floor)/rule_count
[relative]                       # 相对防回归（与基线墙表同 (档,N) 比）
baseline = "data/perf_wall.baseline.txt"
max_regression_pct = 20
```

- 每档取**最大 N** 行（固定开销摊薄后最稳）；相对回归只与基线的同 `(档,N)` 比，
  缺档/缺 N 明确报错提示重录——不静默跳过
- 配置自检 + `deny_unknown_fields` + 正值校验：空门禁 / 拼错 key / 负阈值 / `--gate`
  与 `--record-baseline` 同给 → 全部显式报错（**门禁开着却"没拦任何东西" = 静默失效**）
- 失败项带自解释 detail（如 `漏报：…实际 0 命中` / `rules 档 EPS=150000 vs 基线 200000`）

```json
{
  "schema": "wfgen-perf-report/v1",
  "wall": [ { "stage": "rules", "eps": 168000, "n": 1000000, "rounds": 1 } ],
  "gate": { "config": "conf/perf-gate.toml", "passed": false,
            "checks": [ { "metric": "per_rule_ns", "stage": "rules-floor",
                          "measured": 412.3, "threshold": 300.0,
                          "unit": "ns/evt/rule", "relation": "<=",
                          "passed": false, "detail": "…" } ] },
  "verdict": "FAIL"
}
```

---

## 5. 决策记录（已闭环）

| # | 问题 | 决策 | 落地证据 |
|---|---|---|---|
| D1 | G1 先做 `wfl test` 还是连 `wfl verify` 一起？ | **(b) 两者同 schema 风格** | L1：test `wfl-test-report/v1` + verify `wfgen-verify-report/v1` |
| D2 | 回执 schema 版本化方式 | **(a) 字段 `schema`** | 三个 report 均带 `schema` 字段 |
| D3 | G2 对抗生成的宿主 | **(a) wfl 内 `--gen-cases`**（实现名 `--gen-negatives`） | `crates/wfl/src/gen_negatives.rs` |
| D4 | G3 正负样本集格式 | **(a) 复用 `input { row }` 语法的样本文件**（`.wfi`） | 合法 .wfl 子集，复用公开 `parse_wfl`，wp-reactor 零改动 |
| D5 | G4 性能护栏阈值语义 | **(c) 绝对兜底 + 相对防回归** | `perf-gate.toml` 的 `[absolute]` + `[relative]` |
| D6 | 与 wf-examples Sigma 骨架的关系 | **(a) 骨架留在消费侧当参考**（(b) 收编待定） | 引擎侧 L1–L4 已打通；Sigma→断言编译未做 |

---

## 6. 与现有文档的关系

- 契约测试语法（input/expect）以 wp-reactor wf-lang 实现为准；本文不重复定义语法。
- 性能口径（忙墙/等墙、增量成本）沿用 wp-reactor perf-diag 设计；L4 已把墙梯从
  "人工诊断"变成"自动门禁"（`perf-gate.toml`），口径与 perf-diag 墙表逐行一致。
- replay 输入按每行 `_stream` 字段路由（与 `wfgen verify` 对拍同语义）——消费侧造
  数据文件须带流标签（见 `examples/rules/ssh_brute_force` 验证记录 `7ec75a0`）。
- 本文不承诺"AI 自动修规则"——只承诺"AI 改规则后，回执能证明它对在哪、错在哪"。
  自动修复是回执之上的策略层，后续单独立项。
