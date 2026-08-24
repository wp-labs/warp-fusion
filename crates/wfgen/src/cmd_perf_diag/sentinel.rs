// ---------------------------------------------------------------------------
// 哨兵记录文件（perf_sentinel.ndjson）
// ---------------------------------------------------------------------------
use std::path::Path;
use std::time::Duration;

use crate::error;
use crate::error::{WfgenReason, WfgenResult};


/// 文件里的一条记录（JSONL）。
#[derive(Debug, Clone, PartialEq)]
pub struct SentinelFileRecord {
    /// `"stage"`（切换完成信号）或 `"sentinel"`（测量记录）。
    pub record_type: String,
    /// stage 记录：已生效诊断档下标。
    pub current: Option<i64>,
    /// sentinel 记录：轮次 / 发送量 / 开始与完成时刻。
    pub round: Option<i64>,
    pub n: Option<i64>,
    pub start_ns: Option<i64>,
    pub emit_ns: Option<i64>,
}

impl SentinelFileRecord {
    /// 是否为 `stage{current=k}` 记录。
    pub fn is_stage(&self, k: usize) -> bool {
        self.record_type == "stage" && self.current == Some(k as i64)
    }

    /// 是否为 `sentinel{round=k, n=N}` 记录。
    pub fn is_sentinel(&self, round: i64, n: i64) -> bool {
        self.record_type == "sentinel" && self.round == Some(round) && self.n == Some(n)
    }
}

/// 读取哨兵记录文件（JSONL，一行一条）。字段缺失/类型不符的行跳过。
pub fn read_sentinel_file(path: &Path) -> WfgenResult<Vec<SentinelFileRecord>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(error::error(
                WfgenReason::Io,
                format!("reading {}: {e}", path.display()),
            ));
        }
    };
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let record_type = match v.get("record_type").and_then(|x| x.as_str()) {
            Some(t) => t.to_string(),
            None => continue,
        };
        let num = |k: &str| {
            v.get(k).and_then(|x| {
                x.as_i64()
                    .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
            })
        };
        out.push(SentinelFileRecord {
            record_type,
            current: num("current"),
            round: num("round"),
            n: num("n"),
            start_ns: num("start_ns"),
            emit_ns: num("emit_ns"),
        });
    }
    Ok(out)
}

/// 等待文件出现 `stage{current=k}`（引擎完成档 k 切换，含 reload）。
pub async fn wait_for_stage(path: &Path, k: usize, timeout: Duration) -> WfgenResult<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let records = read_sentinel_file(path)?;
        if records.iter().any(|r| r.is_stage(k)) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(error::error(
                WfgenReason::Network,
                format!(
                    "timeout waiting for stage{{current={k}}} in {} \
                     （最常见根因：daemon 未带 --perf-diag 启动——非诊断模式哨兵帧走 \
                     window miss 丢弃，不会落盘；其次：哨兵文件在 daemon 启动后被清空）",
                    path.display()
                ),
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 等待文件出现第 `occurrence`（0 基）条 `sentinel{round=k, n=N}` 记录。
pub async fn wait_for_sentinel(
    path: &Path,
    round: i64,
    n: i64,
    occurrence: usize,
    timeout: Duration,
) -> WfgenResult<SentinelFileRecord> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let records = read_sentinel_file(path)?;
        let matching: Vec<&SentinelFileRecord> =
            records.iter().filter(|r| r.is_sentinel(round, n)).collect();
        if let Some(rec) = matching.get(occurrence)
            && rec.start_ns.is_some()
            && rec.emit_ns.is_some()
        {
            return Ok((*rec).clone());
        }
        if std::time::Instant::now() >= deadline {
            return Err(error::error(
                WfgenReason::Network,
                format!(
                    "timeout waiting for sentinel{{round={round}, n={n}}} #{occurrence} in {} \
                     （引擎未收到/未落盘哨兵记录——检查 daemon 是否带 --perf-diag 启动、\
                     哨兵文件是否在 daemon 启动后被清空）",
                    path.display()
                ),
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
