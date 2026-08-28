# MoJu Draft Review — wp-reactor/crates → warp-fusion/moju

基于 `moju-code extract`（0.1.9）对 `wp-reactor/crates`（v0.4.0）全量抽取的事实做全新合成。未复用 wp-reactor 的旧评审模型。`moju verify draft` 全部通过（14 个 verify cases）。

---

## 高置信度（代码事实 + `#[moju]` 注解，建议直接接受）

### 域划分（来自源码 `#[moju(domain=..., module=...)]` 注解）

| 域 | Crate | 模块数 | 说明 |
|----|-------|--------|------|
| config | wf-config | 9 | 配置加载/窗口/源/汇/变量/日志指标/输出/管理API |
| engine | wf-engine | 6 | 窗口管理、NFA 匹配、告警输出、汇分发、管道 |
| lang | wf-lang | 13 | WFL AST、schema、编译计划、检查器、WFG 场景 |
| orchestra | wf-runtime (+CLI) | 8 | CLI 入口、Reactor 生命周期、任务编排、接收、指标 |

### 关键类型（全部来自抽取事实，字段为代码真值）

- **config**：`FusionConfig` / `FusionConfigLoader` / `RuntimeConfig`（含新 `parse_parallelism`、`rule_parallelism`、`parse_buffer_bytes`）、`WindowConfig`/`WindowDefaults`/`WindowOverride`、`SinkConfigBundle`/`RouteGroup`/`FlexGroup`/`FixedGroup`、`RawFusionConfigTree`/`ClassifiedFusionConfigChange`/`FusionReloadPlan`、`ConfigVarContext`/`TracedValue`/`ExpandedToml`。
- **engine**：`Event`/`Value`、`CepStateMachine`/`RuleExecutor`/`OutputStatic`（新）、`MatchedContext`/`CloseOutput`、`Window`/`TimedBatch`/`ParsedWindow`/`ParsedRoute`（新 `events_bytes`/`content_bytes` 字节记账）、`RuleFanout`/`FanoutMode`(Single/Sharded/RoundRobin 分片)、`SinkDispatcher`/`SinkRuntime`、`OutputRecord`/`AlertOrigin`。
- **lang**：`WflFile`/`RuleDecl`/`MatchClause`/`Expr`/`FieldRef`、`WindowSchema`/`FieldDef`、`RulePlan`/`MatchPlan`/`BindPlan`/`LimitsPlan`、`WfgFile`/`ScenarioDecl`/`InjectBlock`/`FaultsBlock`。
- **orchestra**：`Reactor`、`RuntimeControlHandle`、`BootstrapData`、`RunRule`/`RunRuleKind`(Match/Each)、`TaskGroup`、`RuleTask`/`RuleTaskConfig`、`PreparedRuleReload`/`ReloadPreparation`、`SinkFanout`（按 sink 并行 writer）、`SinkFactoryRegistry`、`RuntimeMetrics`/`Histogram`/`RunSummary`。

### 流（14）与 verify（14）

config: `LoadConfig`/`ResolveVars`/`ClassifyChanges`；engine: `ProcessEvents`(8 步)/`EvictWindow`/`StateMachineStep`；lang: `CompileWfl`；orchestra: `Run`/`ConfigRender`/`ConfigDiff`/`ReactorStart`/`ReactorShutdown`/`DataIngest`/`HotReload`。

---

## 推断性内容（需人工审查）

| 内容 | 推断依据 | 风险 |
|------|---------|------|
| 设计级 trigger 命令（`ConfigLoadRequest`、`ResolveVarRequest`、`ConfigSnapshot`、`CompileRequest`、`IncomingBatch`、`MemoryPressure`、`NewEvents`、`RunCli`、`RunCommand`、`RenderConfigCmd`、`DiffCmd`、`ShutdownSignal`、`ConfigChange`、`IncomingData`） | 对应真实函数/CLI 入口，作为流触发器的设计抽象 | 中 — 代码无同名 struct，是设计概念 |
| 流步骤划分（如 `ReactorStart` 5 个 spawn 空步、`ProcessEvents` 8 步） | 对应 `Reactor::start()` 拉起顺序与数据路径 | 中 — 步骤粒度可能过细 |
| runtime 层 `service reactor` / `subsystem Reactor` / 单节点 `topology` | wf-runtime `cli::run_cli` 是 reactor CLI 二进制 | 低 — 组合方式为推断 |
| binding.mju 中外部适配器（FileSource、BatchSourceAdapter、ConnectorSinkRegistry、KnowledgeBackend） | 对应 wp-connector-api / wf-connector-api / wp-knowledge 外部接口 | 中 — 目标类型不在本模型 |

## 未建模（基础设施 / 实现细节）

- **wf-data**：3 个时间戳归一化纯函数，无类型 —— 基础设施，不建域。
- 检查器内部 scope/stat 管线、metrics 记录内部结构、receiver 列构建器、RedisBackend、tracing_init、Fnv1a 哈希、测试 mock。

## 字段归一化（记录在 extraction.meta.json）

`TomlValue/PathBuf→String`、`RecordBatch→Bytes`、`Duration→Int`、`Arc/SmolStr→内层`、`ResolvedSinkSpec/SchemaRef/ConnectorDef→String`、`CancellationToken/JoinHandle→String`、`AtomicU64/Mutex/RwLock→展开`、tuple→`List<Map>`、`ExprPlan`(=Expr 别名)→`Expr`。

## 命名调整

- `RuleDecl.meta`（保留字）→ `meta_block`
- AST 与 plan 重复的 `YieldField` 取 plan 版本
- `FanoutSubscription` 抽象私有 `Subscription`(Single/Sharded/RoundRobin) 枚举
- `Reactor` 仅保留领域相关字段

## 已捕获的近期 drift（相对旧模型 2026-07-28）

on-each 规则分片（`RuleFanout`/`RulePush`/`FanoutMode`）、窗口字节记账（`TimedBatch.byte_size`、`ParsedWindow.events_bytes`、`ParsedRoute.content_bytes`）、`parse_buffer_bytes` 背压、两阶段驱逐、`SinkFanout` 按 sink 并行投递、`OutputStatic` 预计算、`RuleTask`/`RuleTaskConfig`。

## 2026-08-16 追加：warp-fusion/crates 产品建模（合并入同一模型）

对 `warp-fusion/crates`（wfusion / wfgen / wfl / wfadm / wf-project-remote，0.1.45）做全新抽取，并合并进 `warp-fusion/moju/model`，与 reactor 模型组成整体产品模型。新增 5 个域：

| 域 | Crate | 模块 | 说明 |
|----|-------|------|------|
| fusion | wfusion | FusionCli, AdminApi | daemon CLI + 管理 HTTP API（status/reload） |
| generator | wfgen | GenCli, ScenarioLoad, DataGen, Oracle, Verify, ArrowOutput | WFG 场景生成、oracle 期望、verify 报告 |
| tooling | wfl | WflCli, Replay | 规则工具 CLI（explain/lint/replay/verify/test） |
| admin | wfadm | AdminCli, ConfOps, EngineOps, Check, SelfUpdate | 管理 CLI（init/conf/check/engine/自更新） |
| project | wf-project-remote | RemoteUpdate | 项目远程版本锁定与重载工件 |

### 产品域的关键决策

- **wfgen 自带 vendored wfg_ast**（与 wf-lang 的 WFG AST 同名但有差异，如 `SeqStep` 形态不同）——为避免重复定义同一 WFG 契约，**generator 域引用 `Lang.*` 的 WFG 类型**（`LoadedScenario.wfg: Lang.WfgFile` 等），vendored 副本不重复建模。
- **trigger 命令为设计抽象**：`DaemonCommand`、`ReloadCommand`、`GenCommand`、`VerifyCommand`、`ReplayCommand`、`CheckCommand`、`EngineReloadCmd`、`ProjectUpdateCommand` 对应各 CLI 子命令/管理 API，代码中无同名 struct。
- **runtime 新增 4 个 service**（warp-fusion / wfgen / wfl / wfadm，均 `bin<cli>`）+ `subsystem WarpFusion`；因当前 parser 每个 kind 只允许一个匿名 `target`，仅保留 reactor 的 `target<bin,cli>`。
- **architecture.mju 新增 `ProductDeps`** 跨域规则；**dataflow.mju 新增 `ProductPipeline`**。

### 产品域新增流（8）与 verify（8）

fusion: `DaemonRun`/`ReloadFlow`；generator: `GenerateScenario`/`VerifyRun`；tooling: `ReplayRun`；admin: `CheckFlow`/`EngineReload`；project: `UpdateProject`。合并后共 **22 流 / 22 verify / 52 模块 / 277 structs / 97 states**，`moju verify model` 全部通过。

## 2026-08-16 wfusion main 关键执行流程分析（fusion/behavior.mju）

按 `wfusion/src/main.rs` + `cli_config.rs` 实际代码，把 main 执行路径落成 flow：
- **`WfusionMain`**：`main → run_cli →` 按子命令分发（Daemon/Batch → `run_engine_command`；Version → `version_ge` 检查）。
- **`EngineCommand`**（`run_engine_inner`，daemon/batch 共用）：`ResolveConfig → LoadConfig → InitTracing → RegisterConnectors → LoadRawTree → StartReactor → GetControl → BuildConfigSource → StartAdminApi → RunLoop`。其中 `Reactor::start`/`reactor.run()` 属 Orchestra 域，经 `dataflow ProductPipeline` 与 `Orchestra.ReactorStart`/`HotReload` 衔接。
- **parser 限制**：flow 的 `create`/`creates` 仅限本域类型，跨域类型（`FusionConfig`/`RawFusionConfigTree`/`Reactor`/`RuntimeControlHandle`）无法在 fusion 域 flow 中 create，以空步+注释标注，由 dataflow 表达衔接。
- **`ReactorStart` 深化**（orchestra/behavior.mju）：按 `Reactor::start`（mod.rs:281）+ `load_and_compile`（bootstrap.rs:33）实际代码拆成两阶段 15 步 —— Phase 1 `LoadAndCompile → BuildPipelineWindows → BuildRouter → BuildRunRules → BuildSinkDispatcher → InitExternalRuntime`；Phase 2 `BuildMetrics → SpawnAlert → SpawnEvictor → BuildPipeRegistry → SpawnRules → SpawnReceiver → SpawnMetrics → CreateControlChannel → Ready`。本域 create（BootstrapData/RunRule/SinkFactoryRegistry/RuntimeMetrics/SinkFanout/TaskGroup/Reactor），跨域步骤（Router/Dispatcher/WindowSchema/plans）注释标注。

## 建议的下一步

1. **人工审查** 推断性 trigger 命令与流步骤粒度（尤其 `ReactorStart` 空步、产品域各 CLI 触发命令）。
2. **确认** runtime 层 `service`/`subsystem`/`topology` 组成是否符合预期（尤其 WarpFusion 子系统包含 5 个 service）。
3. **确认** wfgen vendored wfg_ast → `Lang.*` 的映射是否符合预期（若希望 generator 域独立建模需另议）。
4. **可选后续**：`moju-code align --write` + `moju-code diff` 同步注解、收敛模型-代码差异。
