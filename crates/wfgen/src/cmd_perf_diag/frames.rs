// ---------------------------------------------------------------------------
// 纯函数（可单测）
// ---------------------------------------------------------------------------
use std::fs::File;
use std::io::{BufRead, BufReader, Read as _};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow::array::Int64Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use orion_error::conversion::SourceErr;

use crate::error;
use crate::error::{WfgenReason, WfgenResult};


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
    let file =
        File::open(path).source_err(WfgenReason::Io, format!("opening {}", path.display()))?;
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
            error::error(
                WfgenReason::Serialization,
                "frame length prefix is not ascii",
            )
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
