//! Pre-encoded Arrow frame dump + raw byte replay.
//!
//! P0 "方案 A" for breaking the `wfgen send` ~480k EPS client ceiling:
//!
//! - `wfgen dump-frames` encodes JSONL events into the **exact on-wire payload**
//!   that `wfgen send` would emit (Arrow IPC + RFC6587 framing, per-stream tag)
//!   and stores those frame bytes once, off the benchmark hot path.
//! - `wfgen send-arrow` replays the stored bytes over a single TCP connection
//!   with **zero JSON parsing / Arrow encoding**, so the measured EPS reflects
//!   the runtime's real ingest ceiling instead of the generator client.
//!
//! This mirrors wpgen's direct-mode lesson (warp-parse `wpgen`): generation
//! produces the final native format in memory and ships it straight to the sink,
//! no intermediate file re-parse on each run.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use orion_error::conversion::SourceErr;
use rayon::prelude::*;

use wf_lang::WindowSchema;

use crate::error::{WfgenReason, WfgenResult};
use crate::loader::load_from_uses;
use crate::output::arrow_ipc::events_to_typed_batches;
use crate::output::jsonl::parse_gen_event_line;
use crate::wfg_parser::parse_wfg;

use crate::cmd_helpers::load_ws_files;
use crate::tcp_send::connect_sender;

/// `wfgen dump-frames`: read JSONL once and write the pre-encoded Arrow frames
/// (the byte-identical payloads `wfgen send` produces) to `output`.
///
/// A connected `TcpArrowSink` is only borrowed for its `framed` encoding mode;
/// the payloads go to `output`, not the network. `--addr` defaults to the
/// benchmark port and is where the sink connects for the encode borrow.
#[allow(clippy::too_many_arguments)]
pub async fn dump_frames(
    scenario: PathBuf,
    input: PathBuf,
    addr: String,
    ws: Vec<PathBuf>,
    output: PathBuf,
    chunk: Option<usize>,
    max_frame_bytes: usize,
    max_frame_rows: usize,
) -> WfgenResult<()> {
    let wfg_content = std::fs::read_to_string(&scenario).source_err(
        WfgenReason::Io,
        format!("reading .wfg file: {}", scenario.display()),
    )?;
    let wfg = parse_wfg(&wfg_content)?;

    let (mut schemas, _) = load_from_uses(&wfg, &scenario, &HashMap::new(), false)?;
    schemas.extend(load_ws_files(&ws)?);

    let sink = connect_sender(&addr).await?;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).source_err(
            WfgenReason::Io,
            format!("creating output directory: {}", parent.display()),
        )?;
    }
    let mut writer = BufWriter::new(
        File::create(&output)
            .source_err(WfgenReason::Io, format!("creating {}", output.display()))?,
    );

    // `-` reads stdin, otherwise a file. Events are accumulated up to `chunk`
    // rows per Arrow batch (None = one-shot, matching `send` without --chunk);
    // a chunk bounds per-batch memory for very large event counts.
    let reader: Box<dyn BufRead> = if input == Path::new("-") {
        Box::new(BufReader::new(std::io::stdin()))
    } else {
        let file = File::open(&input)
            .source_err(WfgenReason::Io, format!("opening {}", input.display()))?;
        Box::new(BufReader::new(file))
    };

    let mut events: Vec<crate::datagen::stream_gen::GenEvent>;
    let mut total_events = 0usize;
    let mut total_frames = 0usize;
    let mut total_bytes = 0usize;

    // Read lines in `chunk`-row batches and parse them in parallel (rayon,
    // order-preserving): `parse_gen_event_line` is a pure per-line function, so
    // the 23GB JSONL parse dominates dump time and parallelizes cleanly. Batch
    // boundaries stay identical to the serial path (same lines, same order).
    let mut lines_buf: Vec<String> = Vec::with_capacity(chunk.unwrap_or(1 << 20));
    for line in reader.lines() {
        let line = line.source_err(WfgenReason::Io, format!("reading {}", input.display()))?;
        lines_buf.push(line);

        if let Some(n) = chunk
            && lines_buf.len() >= n
        {
            events = lines_buf
                .par_iter()
                .filter_map(|l| parse_gen_event_line(l, &input).transpose())
                .collect::<WfgenResult<Vec<_>>>()?;
            lines_buf.clear();
            total_frames += write_frames(
                &events,
                &schemas,
                &sink,
                &mut writer,
                &mut total_bytes,
                max_frame_bytes,
                max_frame_rows,
            )?;
            total_events += events.len();
            events.clear();
        }
    }

    if !lines_buf.is_empty() {
        events = lines_buf
            .par_iter()
            .filter_map(|l| parse_gen_event_line(l, &input).transpose())
            .collect::<WfgenResult<Vec<_>>>()?;
        lines_buf.clear();
        total_frames += write_frames(
            &events,
            &schemas,
            &sink,
            &mut writer,
            &mut total_bytes,
            max_frame_bytes,
            max_frame_rows,
        )?;
        total_events += events.len();
    }

    writer
        .flush()
        .source_err(WfgenReason::Io, format!("flushing {}", output.display()))?;

    println!(
        "Dumped {} events as {} frame(s) ({} bytes) -> {}",
        total_events,
        total_frames,
        total_bytes,
        output.display()
    );
    Ok(())
}

/// `wfgen send-arrow`: replay pre-encoded frame bytes over one TCP connection.
///
/// The frames file is a byte stream of RFC6587 length-prefixed Arrow messages,
/// so sending is a plain `copy` — the client does no JSON parsing and no Arrow
/// encoding, isolating the runtime's ingest capacity.
/// Replay pre-encoded Arrow frame bytes over `connections` concurrent TCP
/// connections (each connection sends a full copy of the frames file; the
/// runtime's TCP source splits connections across its `instances` reader
/// loops — the C-UCP supply lever, see docs/design/concurrency-scaling.md).
/// `connections=1` keeps the single-connection baseline.
pub async fn send_arrow(
    input: PathBuf,
    addr: String,
    connections: usize,
    shard_keys: Option<String>,
    shard_files: Option<String>,
    rate_bytes: u64,
) -> WfgenResult<()> {
    let connections = connections.max(1);

    // 分片文件模式:数据已按 key 分区(生成/切分阶段),每条连接纯 copy 一个文件,
    // 发送零解析(恢复 19.8M 级)——C-UCP × 键闭包的最优注入形态。
    if let Some(files) = shard_files {
        let files: Vec<PathBuf> = files
            .split(',')
            .map(|f| PathBuf::from(f.trim()))
            .filter(|f| !f.as_os_str().is_empty())
            .collect();
        if files.is_empty() {
            return Err(crate::error::error(
                WfgenReason::Validation,
                "--shard-files must list at least one file",
            ));
        }
        return send_arrow_copy_files(files, addr, rate_bytes).await;
    }

    // --shard-keys "bid_events:auction,auction_events:id,person_events:id"
    // 按流指定分区 key 字段;同 key 事件永远同一连接(键闭包)。
    let key_by_stream = parse_shard_keys(&shard_keys);

    if key_by_stream.is_empty() {
        // 原样回放(raw copy):纯字节零解析;多连接时每条连接推完整文件
        send_arrow_raw(input, addr, connections, rate_bytes).await
    } else {
        // 发送时按 key 分区(动态 decode;适合无预分片文件的临时注入)
        send_arrow_sharded(input, addr, connections, key_by_stream, rate_bytes).await
    }
}

/// 解析 --shard-keys "stream:field,..." 为 {流 → key 字段}。
fn parse_shard_keys(shard_keys: &Option<String>) -> HashMap<String, String> {
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
fn shard_batch(
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
async fn send_arrow_copy_files(
    files: Vec<PathBuf>,
    addr: String,
    rate_bytes: u64,
) -> WfgenResult<()> {
    use tokio::io::AsyncWriteExt;

    let start = std::time::Instant::now();
    let n = files.len();
    let mut handles: Vec<tokio::task::JoinHandle<WfgenResult<u64>>> = Vec::with_capacity(n);
    for file in files {
        let addr = addr.clone();
        handles.push(tokio::spawn(async move {
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
async fn copy_tcp<R, W>(reader: &mut R, writer: &mut W, rate_bytes: u64) -> WfgenResult<u64>
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

/// 把一个帧文件按 key 切分成 N 个分片帧文件(键闭包:同 key 同文件)。
///
/// 生成时一次切分,发送时纯 copy——避免发送端动态 decode+重编码的注入瓶颈。
/// 输出:`{output_prefix}.s0.frames` ~ `{output_prefix}.s{N-1}.frames`。
/// 未在 `--shard-keys` 列出的流:整帧写入第 0 个分片(不分区,保证数据完整)。
pub async fn shard_frames(
    input: PathBuf,
    shards: usize,
    shard_keys: String,
    output_prefix: PathBuf,
) -> WfgenResult<()> {
    let shards = shards.max(1);
    let key_by_stream = parse_shard_keys(&Some(shard_keys));
    let mut writers: Vec<std::io::BufWriter<std::fs::File>> = Vec::with_capacity(shards);
    for i in 0..shards {
        let path = format!("{}.s{}.frames", output_prefix.display(), i);
        let f =
            std::fs::File::create(&path).source_err(WfgenReason::Io, format!("create {path}"))?;
        writers.push(std::io::BufWriter::new(f));
    }

    let mut file = tokio::fs::File::open(&input)
        .await
        .source_err(WfgenReason::Io, format!("opening {}", input.display()))?;
    let mut frames = 0u64;
    let mut total_rows = 0u64;
    // 每分片按行攒批:保持分片文件帧大小 ≈ 原始帧(默认 100k 行),避免把
    // 大帧拆成小帧放大引擎的每帧固定成本(append/broadcast/seq)——实验实测:
    // 小帧分片使 q1(入流瓶颈)从 19.8M 掉到 5.2M;攒批后恢复。
    // EXPERIMENT-A 结论(2026-08-17):400k 大帧(29MB)EPS 不升反降(5.57M),
    // 内存 +65%——批大小不是门,维持 100k。
    // EXPERIMENT-B 结论(2026-08-17):25k 小批 EPS 5.56M,也不升——
    // 批大小在 25k~400k 均无影响,维持 100k。
    // EXPERIMENT-C 结论(2026-08-17):preread 预算 256MB→1GB 仅 +3.4%,
    // 墙主体是规则计算(每行 ~1.7us,10 任务饱和 ~5.9-6.1M/s)。
    const TARGET_ROWS: usize = 100_000;
    let mut pending: Vec<Option<ShardPending>> = (0..shards).map(|_| None).collect();

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
                let schema = frame.batch.schema();
                let subs = shard_batch(&frame.batch, field, shards)?;
                for (i, sub) in subs.into_iter().enumerate() {
                    let Some(sub) = sub else { continue };
                    let rows = sub.num_rows();
                    // 同一分片内按流分组:换流先 flush 上一组,避免不同 schema/tag
                    // 混在同一个 pending 里(concat 会错、tag 会串流)。
                    let need_new = match &pending[i] {
                        None => true,
                        Some(p) => p.tag != tag,
                    };
                    if need_new {
                        if let Some(p) = pending[i].as_mut() {
                            flush_pending(&mut writers[i], p)?;
                        }
                        pending[i] = Some(ShardPending {
                            tag: tag.clone(),
                            schema: schema.clone(),
                            batches: Vec::new(),
                            rows: 0,
                        });
                    }
                    let p = pending[i].as_mut().expect("just initialized");
                    p.batches.push(sub);
                    p.rows += rows;
                    if p.rows >= TARGET_ROWS {
                        flush_pending(&mut writers[i], p)?;
                    }
                }
            }
            None => {
                // 未指定分区 key 的流:整帧写第 0 个分片(原始字节,零解码)
                write_frame(&mut writers[0], &payload)?;
            }
        }
    }
    // flush 各分片剩余不足一帧的行(沿用各自流 tag,不得改写标签——引擎按
    // tag 路由,标签改了整帧行都会被丢弃)。
    for i in 0..shards {
        if let Some(p) = pending[i].as_mut() {
            flush_pending(&mut writers[i], p)?;
        }
    }
    for mut w in writers {
        w.flush().source_err(WfgenReason::Io, "flush shard file")?;
    }
    println!(
        "Sharded {} frames / {} rows into {} file(s): {}.s0.frames .. s{}.frames",
        frames,
        total_rows,
        shards,
        output_prefix.display(),
        shards - 1,
    );
    Ok(())
}

/// 单个分片的攒批状态:按流分组,tag/schema 取自该流首个批次。
struct ShardPending {
    tag: String,
    schema: arrow::datatypes::SchemaRef,
    batches: Vec<arrow::record_batch::RecordBatch>,
    rows: usize,
}

/// 把攒批的行 concat 成一帧写入分片文件,并清空积累器。
/// 帧 tag 必须沿用原流 tag——引擎按 tag 路由,改写标签会导致整帧行被丢弃。
fn flush_pending(writer: &mut impl std::io::Write, pending: &mut ShardPending) -> WfgenResult<()> {
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
fn write_frame(w: &mut impl std::io::Write, payload: &[u8]) -> WfgenResult<()> {
    write!(w, "{} ", payload.len()).source_err(WfgenReason::Io, "write frame length")?;
    w.write_all(payload)
        .source_err(WfgenReason::Io, "write frame payload")?;
    Ok(())
}

/// 原样回放:每条连接 `tokio::io::copy` 完整帧文件(零解析)。
/// `connections=1` 为单连接基线;`connections>1` 为 C-UCP 供给档位(只适合无状态负载)。
async fn send_arrow_raw(
    input: PathBuf,
    addr: String,
    connections: usize,
    rate_bytes: u64,
) -> WfgenResult<()> {
    use tokio::io::AsyncWriteExt;

    let connections = connections.max(1);
    let start = std::time::Instant::now();

    let mut handles: Vec<tokio::task::JoinHandle<WfgenResult<u64>>> =
        Vec::with_capacity(connections);
    for _ in 0..connections {
        let input = input.clone();
        let addr = addr.clone();
        handles.push(tokio::spawn(async move {
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
async fn send_arrow_sharded(
    input: PathBuf,
    addr: String,
    connections: usize,
    key_by_stream: HashMap<String, String>,
    _rate_bytes: u64,
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
    for _ in 0..connections {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<OutMsg>(16);
        let addr = addr.clone();
        writer_handles.push(tokio::spawn(async move {
            let mut sink = crate::tcp_send::connect_sender(&addr).await?;
            while let Some(msg) = rx.recv().await {
                match msg {
                    OutMsg::Batch(tag, batch) => {
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
                        sink.send_payload(&payload).await.map_err(|e| {
                            crate::error::error(WfgenReason::Network, format!("send: {e}"))
                        })?;
                    }
                }
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
fn frame_tag(payload: &[u8]) -> Option<String> {
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
async fn read_frame(
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
fn row_shard(col: &dyn arrow::array::Array, row: usize, n: usize) -> usize {
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

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Encode `events` into typed Arrow batches and append each framed payload.
fn write_frames(
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

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Int64Array;
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use std::io::Read as _;
    use std::sync::Arc;

    fn int64_batch(columns: &[(&str, Vec<i64>)]) -> RecordBatch {
        let schema = Arc::new(Schema::new(
            columns
                .iter()
                .map(|(n, _)| Field::new(*n, DataType::Int64, false))
                .collect::<Vec<_>>(),
        ));
        let arrays: Vec<Arc<dyn arrow::array::Array>> = columns
            .iter()
            .map(|(_, v)| Arc::new(Int64Array::from(v.clone())) as Arc<dyn arrow::array::Array>)
            .collect();
        RecordBatch::try_new(schema, arrays).unwrap()
    }

    fn write_input(dir: &Path, frames: &[(String, RecordBatch)]) -> PathBuf {
        let input = dir.join("input.frames");
        let mut f = File::create(&input).unwrap();
        for (tag, batch) in frames {
            let payload = wp_arrow::ipc::encode_ipc(tag, batch).unwrap();
            write_frame(&mut f, &payload).unwrap();
        }
        input
    }

    /// 同步读分片帧文件:按出现顺序返回每帧 (tag, rows)。
    fn read_shard(path: &Path) -> Vec<(String, usize)> {
        let mut buf = Vec::new();
        let mut f = File::open(path).unwrap();
        f.read_to_end(&mut buf).unwrap();
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < buf.len() {
            let sp = buf[pos..]
                .iter()
                .position(|&b| b == b' ')
                .expect("frame len separator");
            let len: usize = std::str::from_utf8(&buf[pos..pos + sp])
                .unwrap()
                .parse()
                .unwrap();
            let payload = &buf[pos + sp + 1..pos + sp + 1 + len];
            let tag_len = u32::from_be_bytes(payload[0..4].try_into().unwrap()) as usize;
            let tag = String::from_utf8(payload[4..4 + tag_len].to_vec()).unwrap();
            let ipc = &payload[4 + tag_len..];
            let reader = arrow::ipc::reader::StreamReader::try_new(ipc, None).unwrap();
            let rows: usize = reader.map(|r| r.unwrap().num_rows()).sum();
            out.push((tag, rows));
            pos += sp + 1 + len;
        }
        out
    }

    /// 断言分片文件集:总行数守恒、无 `tail` 脏标签、各流帧 tag 正确。
    fn assert_files_ok(
        prefix: &Path,
        shards: usize,
        expected_rows: usize,
        tags: &[&str],
    ) -> Vec<Vec<(String, usize)>> {
        let mut all = Vec::new();
        let mut total = 0usize;
        for i in 0..shards {
            let path = format!("{}.s{i}.frames", prefix.display());
            let frames = read_shard(Path::new(&path));
            for (tag, rows) in &frames {
                assert!(
                    tags.contains(&tag.as_str()),
                    "unexpected tag {tag:?} in {path}"
                );
                total += rows;
            }
            all.push(frames);
        }
        assert_eq!(total, expected_rows, "row count must be preserved");
        all
    }

    #[tokio::test]
    async fn shard_frames_preserves_tags_and_rows() {
        let dir = tempfile::tempdir().unwrap();
        // 3 个 bid 帧 + 2 个未列出的 auction 帧(不同 schema,整帧走 s0)。
        let bids = int64_batch(&[("auction", (1..=900).collect()), ("bidder", vec![1; 900])]);
        let bids2 = int64_batch(&[
            ("auction", (901..=1800).collect()),
            ("bidder", vec![1; 900]),
        ]);
        let bids3 = int64_batch(&[
            ("auction", (1801..=2700).collect()),
            ("bidder", vec![1; 900]),
        ]);
        let auctions = int64_batch(&[("id", (1..=600).collect()), ("seller", vec![7; 600])]);
        let auctions2 = int64_batch(&[("id", (601..=1200).collect()), ("seller", vec![7; 600])]);
        let input = write_input(
            dir.path(),
            &[
                ("bid_events".into(), bids),
                ("auction_events".into(), auctions),
                ("bid_events".into(), bids2),
                ("auction_events".into(), auctions2),
                ("bid_events".into(), bids3),
            ],
        );
        let prefix = dir.path().join("out");
        shard_frames(input, 4, "bid_events:auction".into(), prefix.clone())
            .await
            .unwrap();

        let all = assert_files_ok(&prefix, 4, 2700 + 1200, &["bid_events", "auction_events"]);
        // 未列出流整帧只进 s0;
        let s0_tags: Vec<&str> = all[0].iter().map(|(t, _)| t.as_str()).collect();
        assert!(
            s0_tags.contains(&"auction_events"),
            "unlisted stream must go to s0"
        );
        for (i, frames) in all.iter().enumerate().skip(1) {
            for (t, _) in frames {
                assert_ne!(t, "auction_events", "unlisted stream leaked to s{i}");
            }
        }
        // 键闭包:同一 auction 只出现在按 fnv1a % shards 计算的那一个分片。
        // 逐值验证:预期分片 = fnv1a(auction.to_le_bytes()) % 4
        let mut shard_of: std::collections::HashMap<i64, Vec<usize>> =
            std::collections::HashMap::new();
        for i in 0..4 {
            let path = format!("{}.s{i}.frames", prefix.display());
            let mut pos = 0usize;
            let buf = std::fs::read(&path).unwrap();
            while pos < buf.len() {
                let sp = buf[pos..].iter().position(|&b| b == b' ').expect("len sep");
                let len: usize = std::str::from_utf8(&buf[pos..pos + sp])
                    .unwrap()
                    .parse()
                    .unwrap();
                let payload = &buf[pos + sp + 1..pos + sp + 1 + len];
                let tag_len = u32::from_be_bytes(payload[0..4].try_into().unwrap()) as usize;
                let tag = String::from_utf8(payload[4..4 + tag_len].to_vec()).unwrap();
                if tag == "bid_events" {
                    let reader =
                        arrow::ipc::reader::StreamReader::try_new(&payload[4 + tag_len..], None)
                            .unwrap();
                    for rb in reader {
                        let rb = rb.unwrap();
                        let col = rb.column_by_name("auction").unwrap();
                        let arr = col.as_any().downcast_ref::<Int64Array>().unwrap();
                        for r in 0..rb.num_rows() {
                            let a = arr.value(r);
                            shard_of.entry(a).or_default().push(i);
                        }
                    }
                }
                pos += sp + 1 + len;
            }
        }
        for (a, shards) in &shard_of {
            let expect = (fnv1a(&a.to_le_bytes()) % 4) as usize;
            assert_eq!(
                shards.as_slice(),
                &[expect],
                "auction {a} must live only in shard {expect}"
            );
        }
        assert_eq!(shard_of.len(), 2700, "all bid auctions present");
    }

    #[tokio::test]
    async fn shard_frames_mid_stream_flush_and_multi_stream_grouping() {
        let dir = tempfile::tempdir().unwrap();
        // 两个流共享同一 key 列(some_key),同进分片路径——攒批中换流必须
        // 先 flush 上一组并各自保留 tag/schema。大帧触发中途 flush。
        let big_bid = int64_batch(&[("some_key", (0..450_000).map(|i| (i % 97) as i64).collect())]);
        let big_auc = int64_batch(&[("some_key", (0..450_000).map(|i| (i % 89) as i64).collect())]);
        let tail_bid = int64_batch(&[("some_key", (0..500).map(|i| (i % 97) as i64).collect())]);
        let input = write_input(
            dir.path(),
            &[
                ("bid_events".into(), big_bid),
                ("auc_events".into(), big_auc),
                ("bid_events".into(), tail_bid),
            ],
        );
        let prefix = dir.path().join("out");
        shard_frames(
            input,
            4,
            "bid_events:some_key,auc_events:some_key".into(),
            prefix.clone(),
        )
        .await
        .unwrap();

        let all = assert_files_ok(
            &prefix,
            4,
            450_000 + 450_000 + 500,
            &["bid_events", "auc_events"],
        );
        let mut bid_rows = 0usize;
        let mut auc_rows = 0usize;
        for frames in &all {
            for (tag, rows) in frames {
                match tag.as_str() {
                    "bid_events" => bid_rows += rows,
                    "auc_events" => auc_rows += rows,
                    other => panic!("bad tag {other}"),
                }
            }
        }
        assert_eq!(bid_rows, 450_500);
        assert_eq!(auc_rows, 450_000);
    }
}
