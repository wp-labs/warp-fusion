# AI 原生规则开发闭环设计（ai-native rule dev loop）

> 状态：**草案（待评审）**。范围 = warp-fusion 引擎侧（wfl/wfgen 工具链 + wp-reactor 契约执行），
> 不含 wf-examples 基准仓（后者是消费方/验证场景）。
>
> 一句话：让「AI 写规则 → 引擎自动验证 → 结构化回执」成为一等工作流——AI 生成的每条规则，
> 都有低成本的证明路径（语义正确 + 覆盖充分 + 性能可接受）。
>
> 相关文档：[rules.md](../useage/rules.md) · [schema.md](../useage/config/schema.md) ·
> wp-reactor [rule-writing.md](../../../wp-reactor/docs/user-guide/rule-writing.md)（契约测试语法权威）·
> wf-examples `SIGMA_WFL_MAPPING_SKELETON.md`（检测意图对照，消费侧）

---

## 1. 背景与目标

### 1.1 现状：闭环的承重墙已存在（先如实盘点，不重复造轮子）

引擎侧**已经具备** AI 闭环的大部分原语，且独立于任何示例仓：

| 原语 | 入口 | AI 闭环作用 | 状态 |
|---|---|---|---|
| WFL 语言内嵌契约测试 | `wfl test`（`crates/wfl/src/cmd_test.rs`） | **独立语义 oracle**：`input { row/tick }` → `expect { hits cmp N; hit[i].字段 cmp … }`，断言规则对给定事件应产生什么——不断言"实现是否跑偏"，断言"意图是否正确" | ✅ 已有（含 `--shuffle/--runs` 乱序/多轮鲁棒性） |
| 契约执行引擎 | wp-reactor `wf-engine/…/match_engine/contract.rs` | 测试跑在引擎状态机上，进程内执行（快） | ✅ 已有 |
| replay + 对拍 | `wfl replay` / `wfl verify` | 真实数据流回放，逐条比对 oracle 期望文件 | ✅ 已有 |
| 编译期检查/意图可读 | `wfl lint` / `wfl explain` | 语法/类型错误前置拦截；规则编译后形态人/AI 可读 | ✅ 已有 |
| 确定性数据 | `wfgen gen` / datagen（seed） | 数据可复现 → 对拍可比 | ✅ 已有 |
| 性能墙诊断 | `wfgen perf-diag`（sentinel 协议） | 性能退化定位到管线段/规则族 | ✅ 已有 |

**关键认识**：WFL 的契约测试不是"同源 oracle 的回归检查"——`expect` 是**逐条手写的语义期望**，
独立于引擎实现。AI 把 `count >= 5` 改成 `count >= 3`，只要期望没跟着改，测试就会 FAIL。
这正是"AI 语义回执"的承重墙，**已经在语言层**，不需要另造语义参考实现。

### 1.2 真实缺口：闭环还差四件事

| # | 缺口 | 现状 | 影响 |
|---|---|---|---|
| G1 | **契约测试无结构化回执** | `wfl test` 输出走 stderr 的彩色 PASS/FAIL（`eprintln!`），无 `--format json` | AI/CI 无法程序化消费"编译、逐测试、覆盖、失败 diff"；回执只能靠人读终端 |
| G2 | **测试输入手工构造 → AI 自证** | `input` 只支持手写 `row(...)`/`tick(...)` | AI 写规则 + AI 写测试 = 用符合自己实现的用例自证，测不出语义偏差 |
| G3 | **检测意图未编译为断言** | "这条规则该检出什么"（漏报/误报）无正负样本集驱动；wf-examples 的 Sigma 对照是人工表，未引擎化 | 契约测试证明"按作者意图执行"，不证明"意图对" |
| G4 | **性能护栏未并入回执** | 正确性（wfl test）与性能（perf-diag）是两条独立路径 | AI 产出一条语义对但全窗扫描/高基数 key 的规则，回执不会拦 |

---

## 2. 目标闭环（定义）

```
AI / 开发者
   │  写 .wfl（规则 + 契约）
   ▼
wfl lint ──编译期错误（语法/类型）───────────┐
   ▼                                        │
wfl test ──语义 oracle（input/expect）───────┤── G1: 结构化回执 JSON
   ▼                                        │
[G2] 对抗/边界测试生成（第三方断言）─────────┤── G2/G3: 覆盖统计
   ▼                                        │
[G3] 检测意图 ↔ 断言（正负样本集）───────────┤
   ▼                                        │
[G4] 单规则成本增量护栏（perf-diag 并入）────┘
   ▼
回执：{ compile, semantic, coverage, perf_delta, verdict }
```

**判定语义**：verdict = PASS 当且仅当四者全绿。任何一环 FAIL → 回执带可定位的失败原因
（哪条规则、哪个 expect、哪个 guard 分支未覆盖、哪段性能墙），AI 据此迭代。

---

## 3. 演进分级

| 级别 | 内容 | 依赖 | 解决的问题 |
|---|---|---|---|
| **L0（已有）** | 契约测试 + replay/verify + lint | — | 手写规则的语义自证 |
| **L1** | `wfl test`/`verify` 结构化回执（`--format json`）+ 回执 schema 版本化 | G1 | AI/CI 程序化消费 |
| **L2** | 对抗测试生成：guard 分支/阈值边界覆盖引导 + 乱序/时序扰动（在 `--shuffle/--runs` 之上叠加"生成 input"） | G2 | 打破 AI 自证，逼近属性测试 |
| **L3** | 检测意图编译：正/负样本集（已知攻击 vs 正常）驱动断言生成，接 Sigma/ATT&CK 或自然语言意图 | G3 | 从"规则按意图执行"到"意图本身对"（漏报/误报可测） |
| **L4** | 性能护栏并入回执：单规则成本增量上限（perf-diag 墙梯机制化为自动门禁） | G4 | 语义对 + 性能可接受才能上线 |

---

## 4. 结构化回执草案（G1，最小可落地项）

`wfl test --format json` 输出（stdout，`--format markdown` 保留现状给人看）：

```json
{
  "schema": "wfl-test-report/v1",
  "rule_file": "rules/brute_force.wfl",
  "compile": { "ok": true, "rules": 1 },
  "tests": [
    {
      "name": "close_hit",
      "rule": "brute_force_then_scan",
      "passed": true,
      "runs": 10,
      "shuffle": true,
      "hits_expected": 1,
      "hits_actual": 1
    },
    {
      "name": "below_threshold",
      "rule": "brute_force_then_scan",
      "passed": false,
      "failures": ["expect hit[0].score == 70.0; actual 30.0"]
    }
  ],
  "summary": { "total": 2, "passed": 1, "failed": 1 },
  "verdict": "FAIL"
}
```

要点：
- 失败项带**结构化 diff**（expected vs actual 的字段级差异），不是只有人类可读字符串——
  AI 迭代需要精确的"差在哪"，而非"失败了"。
- `verdict` 与进程退出码一致（FAIL → exit 1，现有行为保留）。
- 与 `wfgen verify`（已有 `--format json|markdown`）复用同一回执风格，避免两套方言。

---

## 5. 决策点（开放，待评审）

| # | 问题 | 候选 | 倾向 |
|---|---|---|---|
| D1 | G1 先做 `wfl test` 还是连 `wfl verify` 一起？ | (a) 只 test；(b) 两者同 schema | (b)——test 是快反馈，verify 是深度对拍，同一回执 schema 才成一个闭环 |
| D2 | 回执 schema 版本化方式 | (a) 字段 `schema`；(b) 单独 `report.md` 文档维护 | (a)，跟随现有 `wfgen verify` 风格 |
| D3 | G2 对抗生成的宿主 | (a) wfl 内 `--gen-cases`；(b) 独立 crate `wfl-proptest`；(c) 消费侧脚本 | (a)——测试生成应贴近契约语法（可写回 .wfl 供人审），非外部黑盒 |
| D4 | G3 正负样本集格式 | (a) 复用 `input { row }` 语法的样本文件；(b) datagen 场景（.wfg）派生 | (a)——样本就是"大号契约 input"，同一语法、同一执行路径 |
| D5 | G4 性能护栏阈值语义 | (a) 绝对值（每规则 ns 上限）；(b) 相对基线增量；(c) 两者 | (c)——绝对兜底 + 相对防回归，与 perf-diag 家族档口径一致 |
| D6 | 与 wf-examples Sigma 骨架的关系 | (a) 骨架留在消费侧当参考；(b) Sigma→断言编译收进 wfgen | 先 (a) 后 (b)——引擎侧先打通 G1~G2，G3 的意图编译再收编 |

---

## 6. 与现有文档的关系

- 契约测试语法（input/expect）以 wp-reactor wf-lang 实现为准；本文不重复定义语法。
- 性能口径（忙墙/等墙、增量成本）沿用 wp-reactor perf-diag 设计；G4 只是把墙梯从
  "人工诊断"变成"自动门禁"。
- 本文不承诺"AI 自动修规则"——只承诺"AI 改规则后，回执能证明它对在哪、错在哪"。
  自动修复是回执之上的策略层，后续单独立项。
