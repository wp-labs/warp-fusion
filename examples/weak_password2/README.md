# weak_password2 — 弱口令检测（Redis external() 点查）

`weak_password` 的 Redis 版本：不把密码库全量加载进内存，而是用 `external()` 逐条点查 Redis，适用于**亿级**密码库（Have I Been Pwned / SecLists / 企业泄露凭据等）。

## 与 weak_password 的对比

| 维度 | weak_password | weak_password2（本示例） |
|------|---------------|--------------------------|
| 密码库存储 | NDJSON 文件 → `weak_password_db` window（内存） | Redis SET `weak_passwords` |
| 查询方式 | `join snapshot` 全量加载 + 线性扫描 | `external("password_check", e.password_hash)` 逐条 `SISMEMBER` 点查 |
| 规则结构 | `match<sip:5m>` + `on event { count>=1 }` + `join snapshot` | `on each e where external(...)` 逐条判定 |
| 告警语义 | 按源 IP 5 分钟窗口聚合后告警 | 每条弱口令登录立即告警 |
| 适用规模 | < 10 万条（内存 + 扫描成本） | 10 亿级（Redis 承载，点查 O(1)） |
| 富化 | `join` 直接取 `password_masked`/`category` | sismember 仅返回 bool（富化需第二次 `external_value` 查 `HASH wp:<hash>`） |

> 语义差异说明：`match` 的 `on event` 谓词语法只允许 measure 比较（`count|sum|... cmp value`），不能直接写 `external()`，故本示例用 `on each`（逐条触发）。如需"按 IP 聚合后告警"，可在外层 ETL 先打 `is_weak` 标签，再用 match 统计。

## 架构

```
auth_events (NDJSON)
   │  e.password_hash
   ▼
external("password_check", e.password_hash)   ← knowdb.toml [fun.password_check]
   │                                            call=sismember, key=weak_passwords
   ├── SISMEMBER = 1  → score 75 → security_alerts 告警
   └── SISMEMBER = 0  → 跳过
```

Redis 数据模型（由 `scripts/docker_init.sh` 从 `data/weak_password_list.ndjson` 加载）：

| Key | Type | 内容 |
|-----|------|------|
| `weak_passwords` | SET | 所有 `hash_value`（sismember 判定） |
| `wp:<hash_value>` | HASH | `password_masked` / `category` / `note`（富化用，本规则未使用） |

## 运行

```bash
# 一键：起 Redis + 加载密码库 + 跑 wfusion batch
./run.sh

# 或分步
docker compose up -d                  # Redis + 自动加载弱口令库
wfl lint rules/weak_password.wfl -s "schemas/*.wfs"
wfusion batch -c ./wfusion.toml         # batch 模式，读 auth_events → external() 查 Redis
```

Redis 映射到宿主机 **6380** 端口（避免与其他示例的 6379 冲突）；`knowdb.toml` 连 `redis://127.0.0.1:6380`。

## 预期结果

`data/auth_events.ndjson` 含 6 条 SSH 登录成功事件，其中 5 条 `password_hash` 命中弱口令库：

| user | password_hash | 命中 |
|------|---------------|:--:|
| admin | `e10adc39...`（MD5 of 123456） | ✅ |
| deploy | `5f4dcc3b...`（MD5 of password） | ✅ |
| webmaster | `abcdef01...`（非弱口令） | ❌ |
| root | `123456`（明文） | ✅ |
| dbadmin | `$6$abc123$...`（泄露 SHA512 crypt） | ✅ |
| ops | `8d969eef...`（SHA256 of 123456） | ✅ |

→ `data/out_dat/alerts.ndjson` 产出 **5 条**告警，webmaster 被跳过。

## 目录结构

```
weak_password2/
├── README.md
├── run.sh                         # 一键运行
├── wfusion.toml                   # batch 模式，单 source（auth_events）
├── knowdb.toml                    # Redis provider + [fun.password_check]
├── docker-compose.yml             # redis(6380) + redis-init
├── scripts/docker_init.sh         # 加载弱口令库到 Redis
├── schemas/auth.wfs               # auth_events + security_alerts（无 weak_password_db）
├── rules/weak_password.wfl        # on each where external(...)
├── data/
│   ├── auth_events.ndjson         # 6 条 SSH 登录事件
│   └── weak_password_list.ndjson  # 15 条弱口令库（加载到 Redis）
└── topology/
    ├── sources/auth.toml
    └── sinks/{business.d,infra.d}/
```

## knowdb.toml 关键配置

```toml
[provider.redis]
connection_uri = "redis://127.0.0.1:6380"

[fun.password_check]    # external("password_check", <hash>) → SISMEMBER weak_passwords <hash>
call = "sismember"
key  = "weak_passwords"
```

`external()` 经 wf-engine 求值 → `wp_knowledge` facade → Redis `SISMEMBER`，返回 bool。命中（1）→ `on each where` 为真 → 出告警；未命中（0）→ 跳过。
