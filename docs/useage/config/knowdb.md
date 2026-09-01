# knowdb.toml 配置

`knowdb.toml` 是 wp-knowledge 的运行时配置入口，wfusion 启动时自动加载（放在项目根目录，与 `wfusion.toml` 同级）。配置 Redis 连接、结果缓存和 `external()` 命名查询。

## 最小配置（仅 Redis + external()）

```toml
version = 2
base_dir = "."

[provider.redis]
connection_uri = "redis://127.0.0.1:6379"

[cache]
enabled = true
capacity = 10000

[fun.password_check]
call = "bf_exists"
key = "weak_passwords"
```

## `[provider.redis]` — Redis 连接

| 字段 | 类型 | 默认值 | 说明 |
|------|------|:-----:|------|
| `connection_uri` | string | **必填** | `redis://127.0.0.1:6379` |
| `pool_size` | int | `8` | 连接池大小 |
| `connect_timeout_ms` | int | `3000` | 建连超时（ms） |
| `command_timeout_ms` | int | `100` | 单次命令超时（ms） |

```toml
[provider.redis]
connection_uri = "redis://127.0.0.1:6379"
pool_size = 8
connect_timeout_ms = 3000
command_timeout_ms = 100
```

wfusion 启动时自动检测 `[provider.redis]`，存在则初始化 Redis 连接池。

## `[cache]` — 结果缓存

所有读查询（`bf_exists`、`hget`、`get`、`sismember`）共用 cache 配置。

| 字段 | 类型 | 默认值 | 说明 |
|------|------|:-----:|------|
| `enabled` | bool | `true` | 是否启用缓存 |
| `capacity` | int | `1024` | LRU 容量 |
| `ttl_ms` | int | `30000` | 缓存 TTL（ms） |

```toml
[cache]
enabled = true
capacity = 10000
ttl_ms = 30000
```

缓存 key 由 `(generation, cmd_tag, key_hash, args_hash)` 组成。provider reload 时 generation 递增，旧缓存自动淘汰。

## `[fun.<name>]` — `external()` 命名查询

每个 `[fun.<name>]` 定义一个 `external("name", arg)` 可调用的查询。

| 字段 | 类型 | 默认值 | 说明 |
|------|------|:-----:|------|
| `call` | string | **必填** | Redis 命令：`bf_exists` / `hget` / `get` / `sismember` |
| `key` | string | **必填** | Redis key 名 |
| `cache` | bool | `true` | 是否启用缓存 |
| `ttl_ms` | int | 无 | 缓存 TTL（覆盖全局 `[cache].ttl_ms`） |

### Bloom filter 存在性判定

```toml
[fun.password_check]
call = "bf_exists"
key = "weak_passwords"
```

WFL 调用：

```wfl
on each e where external("password_check", e.password_hash) -> score(75.0)
```

映射 `external("password_check", hash)` → `BF.EXISTS weak_passwords <hash>` → `bool`。

### Hash 字段查表

```toml
[fun.threat_actor]
call = "hget"
key = "threat_actors"
```

映射 `external("threat_actor", ip)` → `HGET threat_actors <ip>` → `Option<String>`。

### Set 成员判定

```toml
[fun.ip_whitelist]
call = "sismember"
key = "allowed_ips"
```

映射 `external("ip_whitelist", ip)` → `SISMEMBER allowed_ips <ip>` → `bool`。

### 简单 KV 查询

```toml
[fun.app_config]
call = "get"
```

`get` 忽略 `key` 字段，将 `external()` 的 arg 直接作为 Redis key：

映射 `external("app_config", "setting:v1")` → `GET setting:v1` → `Option<String>`。

## `call` 返回值类型

| `call` | 返回类型 | `external()` 语义 |
|--------|:------:|------|
| `bf_exists` | `bool` | 存在性判定：命中 → `true`，未命中 → `false` |
| `sismember` | `bool` | 成员判定：命中 → `true`，未命中 → `false` |
| `hget` | `Option<String>` | 字段查询：命中 → `"value"`，未命中 → `None` |
| `get` | `Option<String>` | KV 查询：命中 → `"value"`，未命中 → `None` |

## Redis 数据准备

### Bloom filter

```bash
# 1. 启动 Redis + RedisBloom
redis-server --loadmodule ./redisbloom.so

# 2. 创建 Bloom filter
redis-cli BF.RESERVE weak_passwords 0.0001 1000000000

# 3. 批量加载哈希
while IFS= read -r hash; do
    echo "BF.ADD weak_passwords $hash" | redis-cli --pipe
done < hashes.txt
```

### Hash / Set

```bash
# Hash 导入
redis-cli HSET threat_actors 10.0.0.1 "APT29" 10.0.0.2 "Lazarus"

# Set 导入（每个 hash 一行）
while IFS= read -r hash; do
    redis-cli SADD weak_passwords "$hash"
done < hashes.txt
```

## 与 SQL 数据库共存

`knowdb.toml` 可同时配置 SQL 和 Redis：

```toml
version = 2
base_dir = "."

[provider.sqldb]
kind = "postgres"
connection_uri = "postgres://user:pass@127.0.0.1/db"

[provider.redis]
connection_uri = "redis://127.0.0.1:6379"

[cache]
enabled = true
capacity = 10000

[fun.password_check]
call = "bf_exists"
key = "weak_passwords"
```

## 错误处理

| 场景 | external() 返回 |
|------|:------:|
| Redis 不可用 | `external_exists` → `false`，`external_value` → `None` |
| 命令超时 | 同上 |
| `[fun.<name>]` 未定义 | `None` |
| arg 类型不是 string/number | `None` |

错误兜底策略：判定式查询宁可漏报（返回 `false`），查值式查询返回 `None`，确保外部服务故障不阻塞规则执行。
