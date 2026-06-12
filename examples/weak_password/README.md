# weak_password — 弱口令检测

登录成功后通过密码匹配外部弱口令/泄露凭据库，检测弱口令或泄露凭据被使用。

## 架构

```
auth_events.password_hash ──────┐
                                │  join snapshot on password_hash == hash_value
weak_password_db.hash_value ────┘
                                │
                    ┌───────────▼───────────┐
                    │ isnotnull(hash_value)? │
                    ├─── yes ──→ alert       │
                    └─── no  ──→ skip        │
```

## 规模与扩展

### 当前实现 (`join snapshot`)

`join snapshot` 将弱口令库全量加载到内存（`Vec<HashMap>`），每次 join 线性扫描匹配。适用于 **< 10 万条**的场景：

| 规模 | 内存 | 每次 join 扫描 | 可行性 |
|------|------|:--:|:--:|
| 1 万 | ~2 MB | 1 万行 | 可行 |
| 10 万 | ~20 MB | 10 万行 | 尚可 |
| 100 万 | ~200 MB | 100 万行 | 延迟不可接受 |
| 1000 万+ | ~2 GB+ | 1000 万行+ | 不可行 |

### 大规模场景的局限

现实中的密码库远超此量级：
- Have I Been Pwned: 10 亿+
- SecLists / RockYou: 百万级
- 企业内部泄露凭据: 数万 ~ 百万

当前 WFL 缺少对大规模外部数据的点查询能力。`join snapshot`（全量加载 + 线性扫描）和 `window.has()`（同样全量加载）都不适用于大数据量。

### 需要的 WFL 能力（规划中）

```
# 方案 1: 外部 lookup join（不加载全量，逐条点查询）
join weak_password_db lookup on e.password_hash == db.hash_value
# runtime: SELECT * FROM weak_password_db WHERE hash_value = ?

# 方案 2: 外部函数（运行时调用外部服务）
on event {
    e && external("password_check", e.password_hash) | count >= 1;
}
# runtime: HTTP/gRPC 调用外部服务
```

### 当前建议的分层策略

```
┌─ 上游（大规模判定）──────────────────────────┐
│  bloom filter / 外部查表服务 / 索引            │
│  登录时: password_hash → 查 bloom → 命中?    │
│  产出: is_weak_password = true/false          │
└──────────────────┬───────────────────────────┘
                   │
┌─ WFL 规则侧（小规模富化）───────────────────┐
│  is_weak_password == true                     │
│  join 小规模字典（category/note，< 10 万条）  │
└──────────────────────────────────────────────┘
```

## 设计决策

### 密码库模型

密码库采用**预计算多行方案**：同一弱口令/凭据在不同系统、不同算法下的表示形式各存一行。

| 决策 | 结论 | 理由 |
|------|------|------|
| 密码库结构 | 每行 `(hash_value, password_masked, category, note)` | 一行一种表示形式，join 单条件，规则简单 |
| hash 算法区分 | 不在规则中区分，`note` 中标注 | 字符串等值匹配，算法透明 |
| 多系统支持 | 不同系统的同口令 hash 各存一行 | 方案 B 的自然扩展 |
| 明文 vs Hash | 不区分，统一存为 `hash_value` | join 语义一致 |
| 盐+Hash | 同其他类型，`hash_value` 存实际泄露值 | 不需要特殊处理 |
| hash 函数 | 不在 WFL 中内置，由预处理/ETL 计算 | 保持规则简洁，预处理一次成本低 |
| 告警脱敏 | `password_masked` 列展示（如 `"1*****"`） | `password_plain` 不输出到告警 |

### 密码库覆盖的三种来源

| 来源 | hash_value 示例 | 说明 |
|------|----------------|------|
| 明文 | `"123456"` | 直接等值匹配 |
| 简单 Hash | `"e10adc3949ba..."`（MD5） | 预计算 `md5("123456")` |
| 盐+Hash 泄露凭据 | `"$6$abc123$xxx..."` | 实际泄露的 shadow/bcrypt，直接入库 |

### 不同系统的处理

```
系统 A (MD5):            md5("123456") = e10adc39...  → hash_value: "e10adc39..."
系统 B (SHA256):         sha256("123456") = 8d969eef...  → hash_value: "8d969eef..."
系统 C (SHA512 crypt):   泄露 $6$salt$xxx  → hash_value: "$6$salt$xxx"
```

所有系统共享同一个 `weak_password_db`，各自产出的 `password_hash` 按字符串等值匹配。

## 运行

```bash
# 规则检查
wfl lint rules/weak_password.wfl --schemas "schemas/*.wfs"
wfl explain rules/weak_password.wfl --schemas "schemas/*.wfs"

# 完整引擎（batch 模式，同时加载 auth + password_audit 两个 source）
wfusion run -c ./wfusion.toml
```

## 规则

```wfl
rule weak_password_login {
    events {
        e : auth_events
            && e.service == "ssh"
            && e.result == "success"
    }
    match<sip:5m> {
        on event {
            e && isnotnull(weak_password_db.hash_value) | count >= 1;
        }
    } -> score(75.0)
    join weak_password_db snapshot
        on e.password_hash == weak_password_db.hash_value
    entity(ip, e.sip)
    yield security_alerts (
        sip = e.sip,
        dip = e.dip,
        user = e.user,
        found_password = coalesce(weak_password_db.password_masked, "?"),
        category = coalesce(weak_password_db.category, "unknown"),
        alert_type = "weak_password",
        detail = fmt("...", ...)
    )
}
```

**关键机制**：

| 组件 | 作用 |
|------|------|
| `join snapshot on password_hash == hash_value` | 等值匹配，算法透明 |
| `isnotnull(weak_password_db.hash_value)` guard | 只让命中的事件推进状态机 |
| `password_masked` | 脱敏展示，不暴露完整凭据 |
| `coalesce(..., "?")` | join 未命中时的安全回退 |

## 密码库示例

`data/weak_password_list.ndjson`（15 行，覆盖 3 种来源、多种算法）：

| hash_value | password_masked | category | note |
|-----------|----------------|----------|------|
| `123456` | `1*****` | top_password | 明文 |
| `e10adc...` | `1*****` | top_password | MD5 |
| `8d969e...` | `1*****` | top_password | SHA256 |
| `7c4a8d...` | `1*****` | top_password | SHA1 |
| `32ed87...` | `1*****` | top_password | NTLM |
| `$6$abc...` | `$6$ab********` | leaked_credential | 实际泄露 SHA512 crypt |
| `$2b$10...` | `$2b$10********` | leaked_credential | 实际泄露 bcrypt |

## 与其他示例的对比

| 示例 | 检测对象 | 核心模式 | 外部数据 |
|------|---------|---------|---------|
| `ssh_brute_force` | 大量失败登录 | count 阈值 + join anti | scanner_whitelist |
| **`weak_password`** | **弱口令/泄露凭据** | **join snapshot + isnotnull guard** | **weak_password_db** |
| `port_scan_whitelist` | 端口扫描 | distinct count + join anti | scanner_whitelist |
| `rat_propagation` | 攻击链 | 多步序列 scan→login→xfer | — |

## 测试说明

`wfl test` 不加载外部 window 数据，`weak_password_db` 为空，因此 `isnotnull(hash_value)` guard 始终为 false。测试验证规则编译和基本结构。跨 window join 的完整验证通过 `wfusion run`（batch 模式）完成。

## 目录结构

```
weak_password/
├── README.md                    # 本文件（含设计决策记录）
├── wfusion.toml                 # batch 模式，双 source
├── schemas/auth.wfs             # 3 个 window
├── rules/weak_password.wfl      # join snapshot + isnotnull guard
├── data/
│   ├── auth_events.ndjson       # SSH 登录事件
│   └── weak_password_list.ndjson # 弱口令/泄露凭据库
└── topology/
    ├── sources/
    │   ├── auth.toml
    │   └── password_db.toml
    └── sinks/
        ├── business.d/security_alerts.toml
        └── infra.d/{default,error}.toml
```
