use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use orion_error::conversion::SourceErr;

use wf_lang::WindowSchema;

use crate::error::{WfgenReason, WfgenResult};
use crate::output::arrow_ipc::events_to_typed_batches;

/// 发一条分连接哨兵帧（round=连接号）。
pub(crate) async fn send_conn_sentinel(
    addr: &str,
    round: usize,
    n: u64,
    start_ns: i64,
) -> WfgenResult<()> {
    let frame = crate::cmd_perf_diag::build_sentinel_frame(round as i64, n as i64, start_ns)?;
    crate::cmd_perf_diag::send_payload(addr, &frame).await?;
    println!("Sentinel sent (conn {round} n={n}) — EPS 以 data/perf_sentinel.ndjson 为准");
    Ok(())
}

/// 解析 --shard-keys "stream:field,..." 为 {流 → key 字段}。
pub(crate) fn parse_shard_keys(shard_keys: &Option<String>) -> HashMap<String, String> {
    match shard_keys {
        Some(spec) => spec
            .split(',')
            .filter_map(|part| {
                let mut it = part.split(':');
                let stream = it.next()?.trim().to_string();
                let field = it.next()?.trim().to_string();
                (!stream.is_empty() && !field.is_empty()).then_some((stream, field))
            })
            .collect(),
        None => HashMap::new(),
    }
}

/// 一个 batch 按 key 哈希分成 N 个子批次(键闭包:同 key 同桶)。
/// 空桶为 `None`(跳过,省 encode)。`field` 不存在报 Validation。
pub(crate) fn shard_batch(
    batch: &arrow::record_batch::RecordBatch,
    field: &str,
    n: usize,
) -> WfgenResult<Vec<Option<arrow::record_batch::RecordBatch>>> {
    let Some(col) = batch.column_by_name(field) else {
        return Err(crate::error::error(
            WfgenReason::Validation,
            format!("batch schema has no key field {field:?}"),
        ));
    };
    let rows = batch.num_rows();
    if rows == 0 {
        return Ok((0..n).map(|_| None).collect());
    }
    let mut masks: Vec<Vec<bool>> = (0..n).map(|_| vec![false; rows]).collect();
    for r in 0..rows {
        masks[row_shard(col, r, n)][r] = true;
    }
    let mut out = Vec::with_capacity(n);
    for mask in masks {
        if !mask.iter().any(|&b| b) {
            out.push(None);
            continue;
        }
        let filter = arrow::array::BooleanArray::from(mask);
        let sub = arrow::compute::filter_record_batch(batch, &filter)
            .map_err(|e| crate::error::error(WfgenReason::Serialization, format!("filter: {e}")))?;
        out.push(Some(sub));
    }
    Ok(out)
}

/// 分片文件回放:每条连接纯 copy 一个已按 key 分区的帧文件(零解析)。
/// 数据在生成/切分阶段按 key 分桶(键闭包),发送端不 decode——C-UCP × 键闭包
/// 的最优注入形态(实测 100M 16 连接 ~19.8M EPS,与全量 copy 同级)。
/// `--sentinel` 时逐文件 scan 行数（仅统计不解析），每条连接 copy 完追加
/// 自己的哨兵帧（round=文件序号, n=该文件实际行数）。
pub(crate) async fn send_arrow_copy_files(
    files: Vec<PathBuf>,
    addr: String,
    rate_bytes: u64,
    sentinel: Option<i64>,
) -> WfgenResult<()> {
    use tokio::io::AsyncWriteExt;

    // 逐文件预 scan 行数（--sentinel 才需要；默认路径保持纯 copy 零解析）。
    let rows_by_file: Vec<u64> = if sentinel.is_some() {
        let mut out = Vec::with_capacity(files.len());
        for f in &files {
            let rows: u64 = crate::cmd_perf_diag::scan_frames(f)
                .map_err(|e| {
                    crate::error::error(
                        WfgenReason::Validation,
                        format!("scan {}: {e}", f.display()),
                    )
                })?
                .iter()
                .map(|fi| fi.rows)
                .sum();
            out.push(rows);
        }
        out
    } else {
        Vec::new()
    };

    let start = std::time::Instant::now();
    let n = files.len();
    let mut handles: Vec<tokio::task::JoinHandle<WfgenResult<u64>>> = Vec::with_capacity(n);
    for (idx, file) in files.into_iter().enumerate() {
        let addr = addr.clone();
        let conn_rows = rows_by_file.get(idx).copied().unwrap_or(0);
        let send_sentinel = sentinel.is_some();
        handles.push(tokio::spawn(async move {
            let conn_start = crate::cmd_perf_diag::now_nanos();
            let mut f = tokio::fs::File::open(&file)
                .await
                .source_err(WfgenReason::Io, format!("opening {}", file.display()))?;
            let stream = tokio::net::TcpStream::connect(&addr).await.source_err(
                WfgenReason::Network,
                format!("connecting to runtime: {addr}"),
            )?;
            stream
                .set_nodelay(true)
                .source_err(WfgenReason::Network, "set_nodelay")?;
            let mut sink = stream;
            let copied = copy_tcp(&mut f, &mut sink, rate_bytes).await?;
            if send_sentinel && conn_rows > 0 {
                send_conn_sentinel(&addr, idx, conn_rows, conn_start).await?;
            }
            sink.shutdown()
                .await
                .source_err(WfgenReason::Network, "tcp replay shutdown")?;
            Ok(copied)
        }));
    }
    let mut total = 0u64;
    for handle in handles {
        let inner = handle.await.map_err(|e| {
            crate::error::error(WfgenReason::Network, format!("replay task aborted: {e}"))
        })?;
        total += inner?;
    }
    let elapsed = start.elapsed();
    println!(
        "Replayed {} bytes over {} shard file(s) in {:.3}s -> {} ({:.1} MB/file)",
        total,
        n,
        elapsed.as_secs_f64(),
        addr,
        total as f64 / n as f64 / (1024.0 * 1024.0),
    );
    Ok(())
}

/// 大块缓冲 TCP 回放:1 MiB 读写缓冲替代 `tokio::io::copy` 的 8 KiB 栈缓冲。
///
/// `tokio::io::copy` 用固定 8 KiB 内部缓冲——100M 数据(7.76GB)约 100 万次
/// 文件读,每次都是 syscall + `tokio::fs` 的 spawn_blocking 线程池交接,把注入
/// 卡在 ~20M EPS;1 MiB 缓冲把迭代降到 ~8k 次,恢复磁盘/网络级吞吐。
pub(crate) async fn copy_tcp<R, W>(
    reader: &mut R,
    writer: &mut W,
    rate_bytes: u64,
) -> WfgenResult<u64>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 1 << 22]; // 4 MiB
    let mut total = 0u64;
    // 可选限速（rate_bytes/秒，0 = 不限速）：按目标速率控制平均注入，避免对
    // 有状态引擎（如 qradar 450 规则）的瞬时 burst 积压。保持 raw-copy 零解码，
    // 只在累计速率超前时 sleep 补齐——纯字节节流，不解析帧。
    let start = Instant::now();
    loop {
        let n = reader
            .read(&mut buf)
            .await
            .source_err(WfgenReason::Io, "read input file")?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .await
            .source_err(WfgenReason::Network, "tcp replay write")?;
        total += n as u64;
        if rate_bytes > 0 {
            let expect_secs = total as f64 / rate_bytes as f64;
            let actual_secs = start.elapsed().as_secs_f64();
            if actual_secs < expect_secs {
                tokio::time::sleep(tokio::time::Duration::from_secs_f64(
                    expect_secs - actual_secs,
                ))
                .await;
            }
        }
    }
    Ok(total)
}

/// 单个分片的攒批状态:按流分组,tag/schema 取自该流首个批次。
pub(crate) struct ShardPending {
    pub(crate) tag: String,
    pub(crate) schema: arrow::datatypes::SchemaRef,
    pub(crate) batches: Vec<arrow::record_batch::RecordBatch>,
    pub(crate) rows: usize,
}

/// 把攒批的行 concat 成一帧写入分片文件,并清空积累器。
/// 帧 tag 必须沿用原流 tag——引擎按 tag 路由,改写标签会导致整帧行被丢弃。
pub(crate) fn flush_pending(
    writer: &mut impl std::io::Write,
    pending: &mut ShardPending,
) -> WfgenResult<()> {
    if pending.batches.is_empty() {
        return Ok(());
    }
    let refs: Vec<&arrow::record_batch::RecordBatch> = pending.batches.iter().collect();
    // arrow 59 的 concat_batches 签名是 `IntoIterator<Item = &RecordBatch>`,
    // 传 Vec<&RecordBatch>(move)即可,`as_slice()` 会迭代出 `&&RecordBatch` 类型不匹配。
    let merged = arrow::compute::concat_batches(&pending.schema, refs)
        .map_err(|e| crate::error::error(WfgenReason::Serialization, format!("concat: {e}")))?;
    let p = wp_arrow::ipc::encode_ipc(&pending.tag, &merged)
        .map_err(|e| crate::error::error(WfgenReason::Serialization, format!("encode: {e}")))?;
    write!(writer, "{} ", p.len()).source_err(WfgenReason::Io, "write frame length")?;
    writer
        .write_all(&p)
        .source_err(WfgenReason::Io, "write frame payload")?;
    pending.batches.clear();
    pending.rows = 0;
    Ok(())
}

/// 写一帧:`<payload_len> <payload>`(与 dump-frames / read_frame 同格式)。
pub(crate) fn write_frame(w: &mut impl std::io::Write, payload: &[u8]) -> WfgenResult<()> {
    write!(w, "{} ", payload.len()).source_err(WfgenReason::Io, "write frame length")?;
    w.write_all(payload)
        .source_err(WfgenReason::Io, "write frame payload")?;
    Ok(())
}

/// 原样回放:每条连接 `tokio::io::copy` 完整帧文件(零解析)。
/// `connections=1` 为单连接基线;`connections>1` 为 C-UCP 供给档位(只适合无状态负载)。
/// `--sentinel` 时预 scan 文件总行数（仅统计不解析），每条连接 copy 完追加
/// 自己的哨兵帧（round=连接号, n=文件行数, start_ns=该连接开始）。
pub(crate) async fn send_arrow_raw(
    input: PathBuf,
    addr: String,
    connections: usize,
    rate_bytes: u64,
    sentinel: Option<i64>,
) -> WfgenResult<()> {
    use tokio::io::AsyncWriteExt;

    let connections = connections.max(1);
    // --sentinel 才需要行数；默认路径保持纯 copy 零解析。
    let file_rows: u64 = if sentinel.is_some() {
        crate::cmd_perf_diag::scan_frames(&input)
            .map_err(|e| {
                crate::error::error(
                    WfgenReason::Validation,
                    format!("scan {}: {e}", input.display()),
                )
            })?
            .iter()
            .map(|fi| fi.rows)
            .sum()
    } else {
        0
    };
    let start = std::time::Instant::now();

    let mut handles: Vec<tokio::task::JoinHandle<WfgenResult<u64>>> =
        Vec::with_capacity(connections);
    for idx in 0..connections {
        let input = input.clone();
        let addr = addr.clone();
        let conn_rows = file_rows;
        let send_sentinel = sentinel.is_some();
        handles.push(tokio::spawn(async move {
            let conn_start = crate::cmd_perf_diag::now_nanos();
            let mut file = tokio::fs::File::open(&input)
                .await
                .source_err(WfgenReason::Io, format!("opening {}", input.display()))?;
            let stream = tokio::net::TcpStream::connect(&addr).await.source_err(
                WfgenReason::Network,
                format!("connecting to runtime: {addr}"),
            )?;
            stream
                .set_nodelay(true)
                .source_err(WfgenReason::Network, "set_nodelay")?;
            let mut sink = stream;
            let copied = copy_tcp(&mut file, &mut sink, rate_bytes).await?;
            if send_sentinel && conn_rows > 0 {
                send_conn_sentinel(&addr, idx, conn_rows, conn_start).await?;
            }
            sink.shutdown()
                .await
                .source_err(WfgenReason::Network, "tcp replay shutdown")?;
            Ok(copied)
        }));
    }

    let mut total = 0u64;
    for handle in handles {
        let inner = handle.await.map_err(|e| {
            crate::error::error(WfgenReason::Network, format!("replay task aborted: {e}"))
        })?;
        total += inner?;
    }
    let elapsed = start.elapsed();

    println!(
        "Replayed {} bytes over {} connection(s) in {:.3}s -> {} ({:.1} MB/conn)",
        total,
        connections,
        elapsed.as_secs_f64(),
        addr,
        total as f64 / connections as f64 / (1024.0 * 1024.0),
    );
    Ok(())
}

/// 按 key 分区回放:解码帧 → 按 `hash(key) % N` 分桶 → 每桶子批次发对应连接。
///
/// 同 key 事件永远同连接(键闭包),因此多连接对有状态规则也安全——这是
/// 把实例分片的键闭包原理做进注入器,整体负载(含 stateful)都能吃上
/// C-UCP 供给并行。
pub(crate) async fn send_arrow_sharded(
    input: PathBuf,
    addr: String,
    connections: usize,
    key_by_stream: HashMap<String, String>,
    _rate_bytes: u64,
    sentinel: Option<i64>,
) -> WfgenResult<()> {
    let start = std::time::Instant::now();
    /// 发给 writer 的消息:分桶子批次(需编码)或原始帧字节(未分桶流直发,零解码)。
    enum OutMsg {
        Batch(String, arrow::record_batch::RecordBatch),
        Bytes(Vec<u8>),
    }
    let mut writer_txs: Vec<tokio::sync::mpsc::Sender<OutMsg>> = Vec::with_capacity(connections);
    let mut writer_handles: Vec<tokio::task::JoinHandle<WfgenResult<()>>> =
        Vec::with_capacity(connections);
    for idx in 0..connections {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<OutMsg>(16);
        let addr = addr.clone();
        let send_sentinel = sentinel.is_some();
        writer_handles.push(tokio::spawn(async move {
            let conn_start = crate::cmd_perf_diag::now_nanos();
            let mut sent_rows: u64 = 0;
            let mut sink = crate::tcp_send::connect_sender(&addr).await?;
            while let Some(msg) = rx.recv().await {
                match msg {
                    OutMsg::Batch(tag, batch) => {
                        sent_rows += batch.num_rows() as u64;
                        let payload =
                            sink.encode_batch_payload_with_tag(&tag, &batch)
                                .map_err(|e| {
                                    crate::error::error(
                                        WfgenReason::Network,
                                        format!("encode {tag}: {e}"),
                                    )
                                })?;
                        sink.send_payload(&payload).await.map_err(|e| {
                            crate::error::error(WfgenReason::Network, format!("send {tag}: {e}"))
                        })?;
                    }
                    OutMsg::Bytes(payload) => {
                        // 未分区流原始帧（零解码）——行数无法在不解码时统计，
                        // 不计入该连接哨兵 n（nexmark 为 bid 之外的小流，可忽略）。
                        sink.send_payload(&payload).await.map_err(|e| {
                            crate::error::error(WfgenReason::Network, format!("send: {e}"))
                        })?;
                    }
                }
            }
            if send_sentinel && sent_rows > 0 {
                send_conn_sentinel(&addr, idx, sent_rows, conn_start).await?;
            }
            Ok(())
        }));
        writer_txs.push(tx);
    }

    let mut file = tokio::fs::File::open(&input)
        .await
        .source_err(WfgenReason::Io, format!("opening {}", input.display()))?;
    let mut frames = 0u64;
    let mut total_rows = 0u64;
    while let Some(payload) = read_frame(&mut file)
        .await
        .source_err(WfgenReason::Io, "read frame")?
    {
        frames += 1;
        match frame_tag(&payload)
            .as_deref()
            .and_then(|t| key_by_stream.get(t))
        {
            Some(field) => {
                let frame = wp_arrow::ipc::decode_ipc(&payload).map_err(|e| {
                    crate::error::error(WfgenReason::Serialization, format!("decode frame: {e}"))
                })?;
                total_rows += frame.batch.num_rows() as u64;
                let tag = frame.tag.clone();
                let subs = shard_batch(&frame.batch, field, connections)?;
                for (i, sub) in subs.into_iter().enumerate() {
                    let Some(sub) = sub else { continue };
                    writer_txs[i]
                        .send(OutMsg::Batch(tag.clone(), sub))
                        .await
                        .map_err(|_| {
                            crate::error::error(WfgenReason::Network, "writer channel closed")
                        })?;
                }
            }
            None => {
                // 未指定分区 key 的流:不分区,原始字节直发第 0 连接(零解码)。
                // 该流事件不发分桶,保持数据完整(nexmark 场景为 bid 之外的小流)。
                writer_txs[0]
                    .send(OutMsg::Bytes(payload))
                    .await
                    .map_err(|_| {
                        crate::error::error(WfgenReason::Network, "writer channel closed")
                    })?;
            }
        }
    }
    drop(writer_txs);
    for h in writer_handles {
        h.await.map_err(|e| {
            crate::error::error(WfgenReason::Network, format!("writer task aborted: {e}"))
        })??;
    }
    let elapsed = start.elapsed();
    println!(
        "Sharded {} rows / {} frames over {} connection(s) in {:.3}s -> {}",
        total_rows,
        frames,
        connections,
        elapsed.as_secs_f64(),
        addr
    );
    Ok(())
}

/// 读一帧:`<digits> <payload>`(RFC6587 length-prefixed,与 wf-runtime 的
/// `read_frame` 同格式——dump-frames 写的就是这个格式)。
/// 只读帧的 stream tag(前 4 字节长度 + tag 字节),不完整解码——用于
/// 判断该流是否参与 key 分区(未分区流整帧直发,零解码开销)。
pub(crate) fn frame_tag(payload: &[u8]) -> Option<String> {
    if payload.len() < 4 {
        return None;
    }
    let tag_len = u32::from_be_bytes(payload[0..4].try_into().ok()?) as usize;
    let tag_end = 4 + tag_len;
    if payload.len() < tag_end {
        return None;
    }
    String::from_utf8(payload[4..tag_end].to_vec()).ok()
}

const MAX_FRAME_PREFIX_DIGITS: usize = 16;
pub(crate) async fn read_frame(
    reader: &mut (impl tokio::io::AsyncReadExt + Unpin),
) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf: Vec<u8> = Vec::with_capacity(MAX_FRAME_PREFIX_DIGITS);
    loop {
        let mut byte = [0u8; 1];
        match reader.read_exact(&mut byte).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return if len_buf.is_empty() { Ok(None) } else { Err(e) };
            }
            Err(e) => return Err(e),
        }
        if byte[0] == b' ' {
            break;
        }
        if !byte[0].is_ascii_digit() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid frame length prefix",
            ));
        }
        len_buf.push(byte[0]);
        if len_buf.len() > MAX_FRAME_PREFIX_DIGITS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame length prefix too long",
            ));
        }
    }
    let len_str = std::str::from_utf8(&len_buf)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad frame length"))?;
    let frame_len: usize = len_str
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad frame length"))?;
    let mut payload = vec![0u8; frame_len];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

/// 按行 key 哈希分桶:同 key 同桶(键闭包)。支持数值/字符串列;未知类型按行号(仍确定性)。
pub(crate) fn row_shard(col: &dyn arrow::array::Array, row: usize, n: usize) -> usize {
    use arrow::array::{Int32Array, Int64Array, LargeStringArray, StringArray};
    let bytes: Vec<u8> = match col.data_type() {
        arrow::datatypes::DataType::Int64 => col
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(row)
            .to_le_bytes()
            .to_vec(),
        arrow::datatypes::DataType::Int32 => col
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap()
            .value(row)
            .to_le_bytes()
            .to_vec(),
        arrow::datatypes::DataType::Utf8 => col
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(row)
            .as_bytes()
            .to_vec(),
        arrow::datatypes::DataType::LargeUtf8 => col
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .unwrap()
            .value(row)
            .as_bytes()
            .to_vec(),
        _ => (row as u64).to_le_bytes().to_vec(),
    };
    (fnv1a(&bytes) % n as u64) as usize
}

pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Encode `events` into typed Arrow batches and append each framed payload.
pub(crate) fn write_frames(
    events: &[crate::datagen::stream_gen::GenEvent],
    schemas: &[WindowSchema],
    sink: &wp_core_connectors::sinks::tcp::TcpArrowSink,
    writer: &mut impl Write,
    total_bytes: &mut usize,
    max_frame_bytes: usize,
    max_frame_rows: usize,
) -> WfgenResult<usize> {
    let batches = events_to_typed_batches(events, schemas, max_frame_bytes, max_frame_rows)?;
    let mut frames = 0usize;
    for (stream_name, batch) in &batches {
        let payload = sink
            .encode_batch_payload_with_tag(stream_name, batch)
            .source_err(WfgenReason::Serialization, "encode_batch_payload failed")?;
        writer
            .write_all(&payload)
            .source_err(WfgenReason::Io, "writing frame bytes")?;
        *total_bytes += payload.len();
        frames += 1;
    }
    Ok(frames)
}
