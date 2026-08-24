//! `wfgen perf-diag` — 性能诊断驱动（sentinel 漂流瓶协议）。
//!
//! 与 daemon 读同一份 `perf-diag.toml`（诊断档列表 = 轮数）。对每个诊断档 k：
//!
//! 1. 轮询 `perf_sentinel.ndjson` 直到 `stage{current=k}`（引擎已切换好档 k）；
//! 2. `T0 = now()`；发预编码帧前缀（覆盖 N 行）+ 帧尾追加
//!    `__wf_sentinel{round=k, n=n_k, start_ns=T0}` 帧（同连接同 seq 尾部）；
//! 3. 轮询哨兵文件直到 `sentinel{round=k, n=n_k}`（含引擎补的 `emit_ns`）；
//! 4. `EPS = n_k / (emit_ns − start_ns)`（全程无外部记账）。
//!
//! 每 (点, N) 取多轮 max（`--rounds`），输出墙表（EPS 单调 → 增量成本归属）。
//! 数据由小到大（`--n-list "100k,1m,3m"`）：小 N 秒级出方向，大 N 区分
//! per-event 墙 vs 固定开销墙。

use std::fs::File;
use std::io::{BufRead, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arrow::array::Int64Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use orion_error::conversion::SourceErr;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

use wf_config::PerfConfig;

use crate::error::{WfgenReason, WfgenResult};
use crate::error;

// ---------------------------------------------------------------------------
// 参数
// ---------------------------------------------------------------------------

/// `wfgen perf-diag` 命令行参数（main.rs 内定义，这里集中解释）。
#[derive(Debug, Clone)]
pub struct PerfDiagArgs {
    /// 诊断配置（与 daemon 同一份；`[[stages]]` 列表 = 轮数）。
    pub diag: PathBuf,
    /// 预编码帧文件（数据部分；`wfgen dump-frames` 产物）。
    pub frames: PathBuf,
    /// TCP 数据端口（如 `127.0.0.1:9800`）。
    pub addr: String,
    /// 数据量列表（`"100k,1m,3m"`）；缺省 = 帧文件全部行。
    pub n_list: Option<String>,
    /// 每档轮数（取 max，降负载噪声）。
    pub rounds: usize,
    /// 哨兵记录文件（默认 `data/perf_sentinel.ndjson`）。
    pub sentinels: Option<PathBuf>,
    /// 墙表输出文件（默认 `data/perf_diag_wall.txt`）。
    pub output: Option<PathBuf>,
    /// 单次等待（切换完成/哨兵记录）超时秒数。
    pub timeout_secs: u64,
}

// ---------------------------------------------------------------------------
// 纯函数（可单测）
// ---------------------------------------------------------------------------

/// 解析 `--n-list`："100k,1m,3m" → [100_000, 1_000_000, 3_000_000]。
/// 支持 `k`（千）/`m`（百万）/`g`（十亿）后缀；纯数字 = 原值。空串 → 空。
pub fn parse_n_list(spec: &str) -> WfgenResult<Vec<u64>> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (num, mult) = match part.chars().last() {
            Some('k') | Some('K') => (&part[..part.len() - 1], 1_000u64),
            Some('m') | Some('M') => (&part[..part.len() - 1], 1_000_000u64),
            Some('g') | Some('G') => (&part[..part.len() - 1], 1_000_000_000u64),
            _ => (part, 1u64),
        };
        let value: u64 = num.parse().map_err(|_| {
            error::error(
                WfgenReason::Validation,
                format!("invalid --n-list entry {part:?}: expected count like 100k/1m/3m"),
            )
        })?;
        out.push(value.saturating_mul(mult));
    }
    if out.is_empty() {
        return Err(error::error(
            WfgenReason::Validation,
            "--n-list must contain at least one count",
        ));
    }
    Ok(out)
}

/// 一帧的字节区间与行数（发送前缀裁剪用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub offset: usize,
    pub len: usize,
    pub rows: u64,
}

/// 扫描帧文件：RFC6587 `<len> <payload>` 帧序列 → 每帧 (offset, len, rows)。
/// 行数需解码 Arrow IPC（一次性、离线），发送时纯字节复制。
pub fn scan_frames(path: &Path) -> WfgenResult<Vec<FrameInfo>> {
    let file = File::open(path).source_err(WfgenReason::Io, format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut frames = Vec::new();
    let mut offset = 0usize;
    let mut buf = Vec::new();
    loop {
        // 读 `<ascii len> <payload>`：先读到空格。
        buf.clear();
        let n = reader
            .read_until(b' ', &mut buf)
            .source_err(WfgenReason::Io, format!("reading {}", path.display()))?;
        if n == 0 {
            break; // EOF
        }
        let len_str = std::str::from_utf8(&buf[..buf.len() - 1]).map_err(|_| {
            error::error(WfgenReason::Serialization, "frame length prefix is not ascii")
        })?;
        let len: usize = len_str.trim().parse().map_err(|_| {
            error::error(
                WfgenReason::Serialization,
                format!("invalid frame length prefix {len_str:?}"),
            )
        })?;
        let mut payload = vec![0u8; len];
        reader
            .read_exact(&mut payload)
            .source_err(WfgenReason::Io, "truncated frame payload")?;
        let decoded = wp_arrow::ipc::decode_ipc(&payload).map_err(|e| {
            error::error(
                WfgenReason::Serialization,
                format!("decode frame at offset {offset}: {e}"),
            )
        })?;
        frames.push(FrameInfo {
            offset,
            len: len + buf.len(),
            rows: decoded.batch.num_rows() as u64,
        });
        offset += len + buf.len();
    }
    Ok(frames)
}

/// 取覆盖 `n_target` 行的帧前缀字节（帧行数合计 ≥ n_target；越界 = 全部）。
/// 返回 (字节切片, 实际行数合计)。
pub fn prefix_for_n<'a>(frames: &[FrameInfo], data: &'a [u8], n_target: u64) -> (&'a [u8], u64) {
    let mut rows = 0u64;
    let mut end = 0usize;
    for frame in frames {
        let next = rows + frame.rows;
        if next >= n_target {
            end = frame.offset + frame.len;
            rows = next;
            break;
        }
        rows = next;
        end = frame.offset + frame.len;
    }
    if rows < n_target && !frames.is_empty() {
        // n_target 超过全部帧：用全部。
        let last = frames.last().unwrap();
        end = last.offset + last.len;
    }
    (&data[..end.min(data.len())], rows)
}

/// 当前墙钟（epoch nanos，与引擎同机可比）。
pub fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

/// EPS = n / (emit_ns − start_ns)。时间差非正 → `None`。
pub fn compute_eps(n: i64, start_ns: i64, emit_ns: i64) -> Option<f64> {
    let dt = emit_ns.saturating_sub(start_ns);
    if dt <= 0 {
        return None;
    }
    Some(n as f64 * 1e9 / dt as f64)
}

/// 构建哨兵帧（`<len> <encode_ipc("__wf_sentinel", {round,n,start_ns})>`）。
pub fn build_sentinel_frame(round: i64, n: i64, start_ns: i64) -> WfgenResult<Vec<u8>> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("round", DataType::Int64, false),
        Field::new("n", DataType::Int64, false),
        Field::new("start_ns", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![round])),
            Arc::new(Int64Array::from(vec![n])),
            Arc::new(Int64Array::from(vec![start_ns])),
        ],
    )
    .map_err(|e| error::error(WfgenReason::Serialization, format!("sentinel batch: {e}")))?;
    let payload = wp_arrow::ipc::encode_ipc("__wf_sentinel", &batch)
        .map_err(|e| error::error(WfgenReason::Serialization, format!("sentinel encode: {e}")))?;
    let mut frame = format!("{} ", payload.len()).into_bytes();
    frame.extend_from_slice(&payload);
    Ok(frame)
}

// ---------------------------------------------------------------------------
// 哨兵记录文件（perf_sentinel.ndjson）
// ---------------------------------------------------------------------------

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
            ))
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
        let num = |k: &str| v.get(k).and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok())));
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

// ---------------------------------------------------------------------------
// 驱动
// ---------------------------------------------------------------------------

/// 执行一轮诊断：切档 → 发帧+哨兵 → 读完成信号 → 算 EPS。
pub async fn run_perf_diag(args: PerfDiagArgs) -> WfgenResult<()> {
    let config = PerfConfig::load(&args.diag).map_err(|e| {
        error::error(
            WfgenReason::Validation,
            format!("load {}: {e}", args.diag.display()),
        )
    })?;
    let stages = config.stages;
    if stages.is_empty() {
        return Err(error::error(
            WfgenReason::Validation,
            format!("{} 需至少一个 [[stages]]", args.diag.display()),
        ));
    }
    let rounds = args.rounds.max(1);

    // 扫描帧文件（一次性：行数/字节区间），发送时纯字节复制。
    let frames = scan_frames(&args.frames)?;
    if frames.is_empty() {
        return Err(error::error(
            WfgenReason::Validation,
            format!("{} 无帧", args.frames.display()),
        ));
    }
    let total_rows: u64 = frames.iter().map(|f| f.rows).sum();
    let data = std::fs::read(&args.frames)
        .source_err(WfgenReason::Io, format!("reading {}", args.frames.display()))?;

    let n_list = if let Some(spec) = &args.n_list {
        parse_n_list(spec)?
    } else {
        vec![total_rows]
    };
    for &n in &n_list {
        if n > total_rows {
            return Err(error::error(
                WfgenReason::Validation,
                format!("--n-list 含 {n} 行，但帧文件仅 {total_rows} 行"),
            ));
        }
    }

    let sentinels = args
        .sentinels
        .unwrap_or_else(|| PathBuf::from("data/perf_sentinel.ndjson"));
    let output = args
        .output
        .unwrap_or_else(|| PathBuf::from("data/perf_diag_wall.txt"));
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .source_err(WfgenReason::Io, format!("creating {}", parent.display()))?;
    }
    let timeout = Duration::from_secs(args.timeout_secs.max(1));

    println!(
        "== perf-diag: stages={} n-list={:?} rounds={} frames={} total_rows={} ==",
        stages.len(),
        n_list,
        rounds,
        frames.len(),
        total_rows
    );

    let mut wall_lines: Vec<String> = Vec::new();
    for (k, stage) in stages.iter().enumerate() {
        // 1. 等引擎切到档 k（启动即 stages[0]，后续 sentinel 驱动）。
        wait_for_stage(&sentinels, k, timeout).await?;
        println!("== stage {k} [{}] applied — sending ==", stage.name);
        for &n_target in &n_list {
            let mut best_eps = 0.0f64;
            for r in 0..rounds {
                // 2. 帧前缀（覆盖 n_target 行）+ 哨兵帧；T0 = 构建时刻 ≈ 发送开始。
                let (prefix, sent_n) = prefix_for_n(&frames, &data, n_target);
                let start_ns = now_nanos();
                let sentinel_frame = build_sentinel_frame(k as i64, sent_n as i64, start_ns)?;
                let mut payload = Vec::with_capacity(prefix.len() + sentinel_frame.len());
                payload.extend_from_slice(prefix);
                payload.extend_from_slice(&sentinel_frame);
                send_payload(&args.addr, &payload).await?;

                // 3. 读完成信号：sentinel{round=k, n=sent_n}（第 r 条）。
                let rec = wait_for_sentinel(&sentinels, k as i64, sent_n as i64, r, timeout).await?;
                let eps = compute_eps(rec.n.unwrap_or(sent_n as i64), rec.start_ns.unwrap(), rec.emit_ns.unwrap())
                    .ok_or_else(|| {
                        error::error(
                            WfgenReason::Validation,
                            format!(
                                "sentinel 时间序异常: emit_ns={:?} start_ns={:?}",
                                rec.emit_ns, rec.start_ns
                            ),
                        )
                    })?;
                best_eps = best_eps.max(eps);
                println!(
                    "  {}/{}: sent {} rows in {:?} → eps={:.0}",
                    stage.name,
                    r + 1,
                    sent_n,
                    Duration::from_nanos((rec.emit_ns.unwrap() - rec.start_ns.unwrap()) as u64),
                    eps
                );
            }
            wall_lines.push(format!(
                "{}  eps={:.0} n={} rounds={}",
                stage.name, best_eps, n_target, rounds
            ));
        }
    }

    let table = wall_lines.join("\n");
    std::fs::write(&output, table.clone() + "\n").source_err(
        WfgenReason::Io,
        format!("writing {}", output.display()),
    )?;
    println!("\n== wall table ==\n{table}\n== done: 结果在 {} ==", output.display());
    Ok(())
}

/// 单连接发送载荷（字节复制，零解析）并 shutdown。
pub(crate) async fn send_payload(addr: &str, payload: &[u8]) -> WfgenResult<()> {
    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .source_err(WfgenReason::Network, format!("connecting to runtime: {addr}"))?;
    stream
        .set_nodelay(true)
        .source_err(WfgenReason::Network, "set_nodelay")?;
    let mut sink = stream;
    sink.write_all(payload)
        .await
        .source_err(WfgenReason::Network, "tcp send")?;
    sink.shutdown()
        .await
        .source_err(WfgenReason::Network, "tcp shutdown")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn n_list_parses_suffixes() {
        assert_eq!(parse_n_list("100k,1m,3m").unwrap(), vec![100_000, 1_000_000, 3_000_000]);
        assert_eq!(parse_n_list("100000").unwrap(), vec![100_000]);
        assert_eq!(parse_n_list("2M").unwrap(), vec![2_000_000]);
        assert_eq!(parse_n_list("1g").unwrap(), vec![1_000_000_000]);
        assert_eq!(parse_n_list("").unwrap(), Vec::<u64>::new());
    }

    fn make_frames_file(path: &Path, rows: i64) -> Vec<u8> {
        // 单帧：`<len> <encode_ipc("events", batch)>`，rows 行。
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int64Array::from((0..rows).collect::<Vec<_>>()))],
        )
        .unwrap();
        let payload = wp_arrow::ipc::encode_ipc("events", &batch).unwrap();
        let mut body = format!("{} ", payload.len()).into_bytes();
        body.extend_from_slice(&payload);
        std::fs::write(path, &body).unwrap();
        body
    }

    #[test]
    fn scan_frames_rejects_bad_prefixes() {
        let dir = std::env::temp_dir();
        // 非 ascii 长度前缀。
        let path = dir.join(format!("wfgen_scan_bad_utf8_{}.frames", std::process::id()));
        std::fs::write(&path, [0xffu8, 0xfe, b' ', 0u8]).unwrap();
        let err = scan_frames(&path).unwrap_err();
        assert!(err.to_string().contains("not ascii"));
        let _ = std::fs::remove_file(&path);
        // 非法数字长度前缀。
        let path = dir.join(format!("wfgen_scan_bad_len_{}.frames", std::process::id()));
        std::fs::write(&path, b"abc payload").unwrap();
        let err = scan_frames(&path).unwrap_err();
        assert!(err.to_string().contains("invalid frame length"));
        let _ = std::fs::remove_file(&path);
        // 长度合法但载荷不可解码。
        let path = dir.join(format!("wfgen_scan_bad_payload_{}.frames", std::process::id()));
        std::fs::write(&path, b"4 junk").unwrap();
        let err = scan_frames(&path).unwrap_err();
        assert!(err.to_string().contains("decode frame"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn scan_frames_missing_file_errors() {
        let path = std::env::temp_dir().join("wfgen_scan_missing.frames");
        let err = scan_frames(&path).unwrap_err();
        assert!(err.to_string().contains("opening"));
    }

    #[test]
    fn read_sentinel_file_unreadable_path_errors() {
        // 目录路径 → 读取报错。
        let dir = std::env::temp_dir().join(format!("wfgen_sentinel_dir_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = read_sentinel_file(&dir).unwrap_err();
        assert!(err.to_string().contains("reading"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_sentinel_file_skips_blank_and_bad_record_type_lines() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wfgen_sentinel_blank_{}.ndjson", std::process::id()));
        std::fs::write(
            &path,
            "\n\n{\"record_type\":\"stage\",\"current\":0}\n\n",
        )
        .unwrap();
        let records = read_sentinel_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(records.len(), 1, "空行跳过");
        assert!(records[0].is_stage(0));
    }

    #[tokio::test]
    async fn wait_for_sentinel_times_out() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wfgen_wait_sent_timeout_{}.ndjson", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, r#"{"record_type":"stage","current":0}"#.to_string() + "\n")
            .unwrap();
        let err = wait_for_sentinel(&path, 9, 99, 0, Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timeout"));
        let _ = std::fs::remove_file(&path);
    }

    // -- 驱动端到端（mock TCP 服务器模拟引擎）--------------------------------

    /// mock 引擎：逐连接接收载荷（含哨兵帧），按 sentinel 驱动模拟状态机——
    /// 收到 round=k 哨兵后写 `sentinel{round=k,n=N}` + `stage{current=k+1}`。
    /// 返回 (TempDir, sentinel 路径, wall 路径)——TempDir 保持存活到断言结束。
    async fn run_driver_with_mock_engine(
        stages_toml: &str,
        n_list: &str,
        stages: usize,
    ) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let diag_path = dir.path().join("perf-diag.toml");
        std::fs::write(&diag_path, stages_toml).unwrap();
        let frames_path = dir.path().join("data.frames");
        make_frames_file(&frames_path, 2); // 2 行
        let sentinel_path = dir.path().join("perf_sentinel.ndjson");
        let wall_path = dir.path().join("wall.txt");
        // 启动信号：stage{current=0} 预先存在（模拟 daemon 启动即写）。
        std::fs::write(&sentinel_path, r#"{"record_type":"stage","current":0}"#.to_string() + "\n")
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let sentinel_path2 = sentinel_path.clone();
        let server = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            for k in 0..stages {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let _ = sock.read_to_end(&mut buf).await.unwrap();
                // 载荷必须含哨兵帧 tag（原始字节搜索，Arrow IPC 含非 UTF8）。
                assert!(
                    buf.windows(b"__wf_sentinel".len())
                        .any(|w| w == b"__wf_sentinel"),
                    "载荷必须含哨兵帧（tag=__wf_sentinel）"
                );
                // 模拟引擎处理：落盘 sentinel{round=k, n=2} + 切换信号 stage{current=k+1}。
                tokio::time::sleep(Duration::from_millis(30)).await;
                let mut rec = std::fs::read_to_string(&sentinel_path2).unwrap();
                rec.push_str(&format!(
                    r#"{{"record_type":"sentinel","round":{k},"n":2,"start_ns":"1000000000","emit_ns":"1100000000"}}"#
                ));
                rec.push('\n');
                rec.push_str(&format!(
                    r#"{{"record_type":"stage","current":{}}}"#,
                    k + 1
                ));
                rec.push('\n');
                std::fs::write(&sentinel_path2, rec).unwrap();
            }
        });

        let args = PerfDiagArgs {
            diag: diag_path,
            frames: frames_path,
            addr,
            n_list: Some(n_list.to_string()),
            rounds: 1,
            sentinels: Some(sentinel_path.clone()),
            output: Some(wall_path.clone()),
            timeout_secs: 10,
        };
        run_perf_diag(args).await.unwrap();
        server.await.unwrap();
        assert_eq!(
            stages,
            read_sentinel_file(&sentinel_path)
                .unwrap()
                .iter()
                .filter(|r| r.record_type == "sentinel")
                .count(),
            "每档一条 sentinel 记录"
        );
        (dir, sentinel_path, wall_path)
    }

    #[tokio::test]
    async fn driver_end_to_end_single_stage_writes_wall_table() {
        let (_dir, _sent, wall) = run_driver_with_mock_engine(
            r#"

[[stages]]
name = "floor"
cut_rules = true
cut_output = true
rules = ""
"#,
            "2",
            1,
        )
        .await;
        let table = std::fs::read_to_string(&wall).unwrap();
        assert!(
            table.contains("floor  eps="),
            "墙表必须含 floor 行: {table}"
        );
        // EPS = 2 / (1.1s − 1.0s) = 20。
        assert!(table.contains("eps=20"), "墙表 EPS 应可算: {table}");
        assert!(table.contains("n=2"), "墙表应记发送量: {table}");
    }

    #[tokio::test]
    async fn driver_end_to_end_two_stages_produce_two_wall_rows() {
        let (_dir, _sent, wall) = run_driver_with_mock_engine(
            r#"

[[stages]]
name = "floor"
cut_rules = true
cut_output = true
rules = ""
[[stages]]
name = "full"
cut_rules = false
cut_output = false
rules = ""
"#,
            "2",
            2,
        )
        .await;
        let table = std::fs::read_to_string(&wall).unwrap();
        assert!(table.contains("floor  eps="), "{table}");
        assert!(table.contains("full  eps="), "{table}");
    }

    #[tokio::test]
    async fn driver_rejects_empty_stages_and_over_budget_n() {
        // 空 stages。
        let dir = tempfile::tempdir().unwrap();
        let diag_path = dir.path().join("perf-diag.toml");
        std::fs::write(&diag_path, "").unwrap();
        let frames_path = dir.path().join("data.frames");
        make_frames_file(&frames_path, 2);
        let args = PerfDiagArgs {
            diag: diag_path.clone(),
            frames: frames_path.clone(),
            addr: "127.0.0.1:1".into(),
            n_list: Some("2".into()),
            rounds: 1,
            sentinels: None,
            output: None,
            timeout_secs: 2,
        };
        let err = run_perf_diag(args).await.unwrap_err();
        assert!(err.to_string().contains("至少一个 [[stages]]"));
        // n 超过帧行数。
        std::fs::write(
            &diag_path,
            "[[stages]]\nname = \"floor\"\n",
        )
        .unwrap();
        let args = PerfDiagArgs {
            diag: diag_path,
            frames: frames_path,
            addr: "127.0.0.1:1".into(),
            n_list: Some("99".into()), // 帧仅 2 行
            rounds: 1,
            sentinels: None,
            output: None,
            timeout_secs: 2,
        };
        let err = run_perf_diag(args).await.unwrap_err();
        assert!(err.to_string().contains("帧文件仅 2 行"));
    }

    #[tokio::test]
    async fn driver_send_failure_is_reported() {
        // 无服务器监听 → send_payload 连接失败。
        let dir = tempfile::tempdir().unwrap();
        let diag_path = dir.path().join("perf-diag.toml");
        std::fs::write(
            &diag_path,
            "[[stages]]\nname = \"floor\"\n",
        )
        .unwrap();
        let frames_path = dir.path().join("data.frames");
        make_frames_file(&frames_path, 2);
        let sentinel_path = dir.path().join("perf_sentinel.ndjson");
        std::fs::write(&sentinel_path, r#"{"record_type":"stage","current":0}"#.to_string() + "\n")
            .unwrap();
        // 找一个肯定没监听的端口。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let args = PerfDiagArgs {
            diag: diag_path,
            frames: frames_path,
            addr,
            n_list: Some("2".into()),
            rounds: 1,
            sentinels: Some(sentinel_path),
            output: None,
            timeout_secs: 2,
        };
        let err = run_perf_diag(args).await.unwrap_err();
        assert!(err.to_string().contains("connecting to runtime"), "{err}");
    }

    #[test]
    fn n_list_skips_empty_entries_but_requires_one_count() {
        // 空条目（逗号间）跳过；全空/无有效条目 → 报错。
        assert_eq!(parse_n_list("1k,,2k").unwrap(), vec![1_000, 2_000]);
        assert!(parse_n_list(" , ").is_err(), "全部条目为空 → 报错");
        assert!(parse_n_list(",").is_err());
    }

    #[test]
    fn now_nanos_is_positive_and_advances() {
        let a = now_nanos();
        std::thread::sleep(Duration::from_millis(2));
        let b = now_nanos();
        assert!(a > 0);
        assert!(b > a);
    }

    #[test]
    fn prefix_for_n_picks_frames_covering_rows() {
        let frames = vec![
            FrameInfo { offset: 0, len: 10, rows: 4 },
            FrameInfo { offset: 10, len: 10, rows: 6 },
            FrameInfo { offset: 20, len: 10, rows: 8 },
        ];
        let data = vec![0u8; 30];
        // 4 行 → 第一帧。
        let (slice, rows) = prefix_for_n(&frames, &data, 4);
        assert_eq!(rows, 4);
        assert_eq!(slice.len(), 10);
        // 5 行 → 前两帧（10 行）。
        let (slice, rows) = prefix_for_n(&frames, &data, 5);
        assert_eq!(rows, 10);
        assert_eq!(slice.len(), 20);
        // 边界 10 行 → 前两帧。
        let (slice, rows) = prefix_for_n(&frames, &data, 10);
        assert_eq!(rows, 10);
        assert_eq!(slice.len(), 20);
        // 超界 → 全部。
        let (slice, rows) = prefix_for_n(&frames, &data, 100);
        assert_eq!(rows, 18);
        assert_eq!(slice.len(), 30);
        // 空帧 → 空。
        let (slice, rows) = prefix_for_n(&[], &data, 10);
        assert_eq!(rows, 0);
        assert_eq!(slice.len(), 0);
    }

    #[test]
    fn eps_computes_and_guards() {
        let eps = compute_eps(1_000_000, 0, 100_000_000).unwrap();
        assert!((eps - 10_000_000.0).abs() < 1.0);
        assert!(compute_eps(100, 200, 100).is_none());
        assert!(compute_eps(100, 200, 200).is_none());
    }

    #[test]
    fn sentinel_frame_roundtrips() {
        let frame = build_sentinel_frame(3, 500, 1_722_000_000_000_000_000).unwrap();
        // 帧 = `<len> <payload>`；payload 含 tag `__wf_sentinel`。
        let sp = frame.iter().position(|&b| b == b' ').unwrap();
        let len: usize = std::str::from_utf8(&frame[..sp]).unwrap().parse().unwrap();
        let payload = &frame[sp + 1..];
        assert_eq!(payload.len(), len);
        let decoded = wp_arrow::ipc::decode_ipc(payload).unwrap();
        assert_eq!(decoded.tag, "__wf_sentinel");
        assert_eq!(decoded.batch.num_rows(), 1);
        let col = decoded
            .batch
            .column_by_name("round")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(col.value(0), 3);
        let n = decoded
            .batch
            .column_by_name("n")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(n.value(0), 500);
        let s = decoded
            .batch
            .column_by_name("start_ns")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(s.value(0), 1_722_000_000_000_000_000);
    }

    #[test]
    fn scan_frames_reads_len_prefixed_frames() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wfgen_scan_{}.frames", std::process::id()));
        let batch = sentinel_batch(&[1, 2], &[3, 4], &[5, 6]);
        let payload = wp_arrow::ipc::encode_ipc("events", &batch).unwrap();
        let mut body = format!("{} ", payload.len()).into_bytes();
        body.extend_from_slice(&payload);
        let batch2 = sentinel_batch(&[7], &[8], &[9]);
        let payload2 = wp_arrow::ipc::encode_ipc("events", &batch2).unwrap();
        body.extend_from_slice(format!("{} ", payload2.len()).as_bytes());
        body.extend_from_slice(&payload2);
        std::fs::write(&path, &body).unwrap();
        let frames = scan_frames(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].rows, 2);
        assert_eq!(frames[1].rows, 1);
        assert_eq!(frames[0].len, payload.len() + format!("{} ", payload.len()).len());
        assert_eq!(frames[1].offset, frames[0].offset + frames[0].len);
    }

    fn sentinel_batch(rounds: &[i64], ns: &[i64], starts: &[i64]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("round", DataType::Int64, false),
            Field::new("n", DataType::Int64, false),
            Field::new("start_ns", DataType::Int64, false),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(rounds.to_vec())),
                Arc::new(Int64Array::from(ns.to_vec())),
                Arc::new(Int64Array::from(starts.to_vec())),
            ],
        )
        .unwrap()
    }

    fn write_records(path: &Path, lines: &[&str]) {
        let mut body = String::new();
        for l in lines {
            body.push_str(l);
            body.push('\n');
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn read_sentinel_file_parses_stage_and_sentinel_lines() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wfgen_sentinel_{}.ndjson", std::process::id()));
        write_records(
            &path,
            &[
                r#"{"record_type":"stage","current":0,"wfx_id":"perf-stage-0"}"#,
                r#"{"record_type":"sentinel","round":0,"n":100000,"start_ns":"1722000000000000000","emit_ns":"1722000000100000000"}"#,
                r#"{"record_type":"sentinel","round":0,"n":100000,"start_ns":1722000000000000000,"emit_ns":1722000000100000000}"#,
                r#"not-json"#,
            ],
        );
        let records = read_sentinel_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(records.len(), 3, "非 JSON 行跳过");
        assert!(records[0].is_stage(0));
        assert!(!records[0].is_stage(1));
        assert!(records[1].is_sentinel(0, 100_000));
        assert!(!records[1].is_sentinel(1, 100_000));
        // start_ns/emit_ns 字符串与数字两种形态都解析为 i64。
        assert_eq!(records[1].start_ns, Some(1_722_000_000_000_000_000));
        assert_eq!(records[1].emit_ns, Some(1_722_000_000_100_000_000));
        assert_eq!(records[2].start_ns, Some(1_722_000_000_000_000_000));
        let eps = compute_eps(
            records[1].n.unwrap(),
            records[1].start_ns.unwrap(),
            records[1].emit_ns.unwrap(),
        )
        .unwrap();
        assert!(
            (eps - 1_000_000.0).abs() < 1.0,
            "1e5 rows / 0.1s = 1e6 EPS, got {eps}"
        );
    }

    #[test]
    fn read_sentinel_file_missing_is_empty() {
        let dir = std::env::temp_dir();
        let path = dir.join("wfgen_sentinel_missing.ndjson");
        assert!(read_sentinel_file(&path).unwrap().is_empty());
    }

    #[tokio::test]
    async fn wait_for_stage_returns_when_record_appears() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wfgen_wait_stage_{}.ndjson", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let writer = tokio::spawn({
            let path = path.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                std::fs::write(
                    &path,
                    r#"{"record_type":"stage","current":1}"#.to_string() + "\n",
                )
                .unwrap();
            }
        });
        wait_for_stage(&path, 1, Duration::from_secs(5)).await.unwrap();
        writer.await.unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn wait_for_stage_times_out() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wfgen_wait_stage_timeout_{}.ndjson", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let err = wait_for_stage(&path, 2, Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timeout"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn wait_for_sentinel_counts_occurrences() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wfgen_wait_sent_{}.ndjson", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let writer = tokio::spawn({
            let path = path.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                std::fs::write(
                    &path,
                    concat!(
                        r#"{"record_type":"sentinel","round":0,"n":10,"start_ns":"100","emit_ns":"200"}"#,
                        "\n",
                        r#"{"record_type":"sentinel","round":0,"n":10,"start_ns":"300","emit_ns":"400"}"#,
                        "\n",
                    ),
                )
                .unwrap();
            }
        });
        let first = wait_for_sentinel(&path, 0, 10, 0, Duration::from_secs(5))
            .await
            .unwrap();
        let second = wait_for_sentinel(&path, 0, 10, 1, Duration::from_secs(5))
            .await
            .unwrap();
        writer.await.unwrap();
        assert_eq!(first.emit_ns, Some(200));
        assert_eq!(second.emit_ns, Some(400));
        let _ = std::fs::remove_file(&path);
    }
}
