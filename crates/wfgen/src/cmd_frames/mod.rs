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

use crate::cmd_helpers::load_ws_files;
use crate::error::{WfgenReason, WfgenResult};
use crate::loader::load_from_uses;
use crate::output::jsonl::parse_gen_event_line;
use crate::tcp_send::connect_sender;
use crate::wfg_parser::parse_wfg;

mod shard;
#[cfg(test)]
mod tests;

use shard::*;

/// `wfgen dump-frames` 参数：JSONL → 预编码 Arrow 帧文件。
#[derive(clap::Args)]
pub struct DumpFramesArgs {
    /// Path to the .wfg scenario file (used to load schemas)
    #[arg(long)]
    pub scenario: PathBuf,

    /// Path to generated events JSONL file, or `-` to read stdin
    #[arg(long)]
    pub input: PathBuf,

    /// Runtime TCP address used only to borrow the framed encoder
    #[arg(long, default_value = "127.0.0.1:9800")]
    pub addr: String,

    /// Additional .wfs schema files (beyond those in `use` declarations)
    #[arg(long)]
    pub ws: Vec<PathBuf>,

    /// Path to write the encoded frame bytes to
    #[arg(long)]
    pub output: PathBuf,

    /// Accumulate this many events per Arrow batch (default: one-shot,
    /// matching `send` without --chunk). Bounds per-batch memory for huge
    /// event counts.
    #[arg(long)]
    pub chunk: Option<usize>,

    /// Frame byte cap (default 8388608 = 8MiB). A frame is one window commit;
    /// smaller frames → lower per-batch memory, more commits.
    #[arg(long, default_value_t = crate::output::arrow_ipc::DEFAULT_MAX_FRAME_BYTES)]
    pub max_frame_bytes: usize,

    /// Frame row cap (default 100000).
    #[arg(long, default_value_t = crate::output::arrow_ipc::DEFAULT_MAX_FRAME_ROWS)]
    pub max_frame_rows: usize,
}

/// `wfgen send-arrow` 参数：预编码帧字节多连接回放。
#[derive(clap::Args)]
pub struct SendArrowArgs {
    /// Path to the frames file produced by `wfgen dump-frames`
    #[arg(long)]
    pub input: PathBuf,

    /// Runtime TCP address, e.g. 127.0.0.1:9800
    #[arg(long, default_value = "127.0.0.1:9800")]
    pub addr: String,

    /// Concurrent TCP connections (each sends a full copy of the file)
    #[arg(long, default_value_t = 1)]
    pub connections: usize,

    /// Per-stream key field for key-sharded replay, e.g.
    /// "bid_events:auction,auction_events:id,person_events:id". When set
    /// with --connections>1, events are split by hash(key) so the same key
    /// always goes to the same connection (key closure) — multi-connection
    /// stays correct for stateful rules.
    #[arg(long)]
    pub shard_keys: Option<String>,

    /// Comma-separated pre-sharded frame files, one per connection
    /// (produced by `wfgen shard-frames`). Each connection raw-copies its
    /// file — zero decode on the send path, so multi-connection stays at
    /// raw-copy speed while preserving key closure for stateful rules.
    #[arg(long)]
    pub shard_files: Option<String>,

    /// Target replay rate in bytes/sec. 0 = unlimited (default). When > 0,
    /// send-arrow paces its raw-copy at ~this rate per connection, so a
    /// stateful engine (e.g. 450-rule qradar) is not hit with an instant
    /// burst that swamps its steady-state capacity.
    #[arg(long, default_value_t = 0)]
    pub rate_bytes: u64,

    /// Enable per-connection `__wf_sentinel` completion frames: each
    /// connection sends one after its data (round=conn id, n=that conn's
    /// actual rows, start_ns=conn start). Single connection = one frame
    /// (round=0). The engine writes {round,n,start_ns,emit_ns} tuples to
    /// perf_sentinel.ndjson once data windows drain — precise EPS for bench
    /// (multi-conn aggregate: Σn/(max emit − min start)). The value is a
    /// switch; per-conn row counts come from frame scanning.
    #[arg(long)]
    pub sentinel: Option<i64>,
}

/// `wfgen shard-frames` 参数：帧文件按 key 切分成 N 个分片文件。
#[derive(clap::Args)]
pub struct ShardFramesArgs {
    /// Path to the frame file produced by `wfgen dump-frames`
    #[arg(long)]
    pub input: PathBuf,

    /// Number of shards (connections to replay with later)
    #[arg(long)]
    pub shards: usize,

    /// Per-stream key field, e.g. "bid_events:auction,auction_events:id,person_events:id"
    #[arg(long)]
    pub shard_keys: String,

    /// Output prefix: produces {prefix}.s0.frames .. {prefix}.s{N-1}.frames
    #[arg(long)]
    pub output_prefix: PathBuf,
}


/// `wfgen dump-frames`: read JSONL once and write the pre-encoded Arrow frames
/// (the byte-identical payloads `wfgen send` produces) to `output`.
///
/// A connected `TcpArrowSink` is only borrowed for its `framed` encoding mode;
/// the payloads go to `output`, not the network. `--addr` defaults to the
/// benchmark port and is where the sink connects for the encode borrow.
pub async fn dump_frames(args: DumpFramesArgs) -> WfgenResult<()> {
    let DumpFramesArgs {
        scenario,
        input,
        addr,
        ws,
        output,
        chunk,
        max_frame_bytes,
        max_frame_rows,
    } = args;
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
pub async fn send_arrow(args: SendArrowArgs) -> WfgenResult<()> {
    let SendArrowArgs {
        input,
        addr,
        connections,
        shard_keys,
        shard_files,
        rate_bytes,
        sentinel,
    } = args;
    let sentinel_n = sentinel;
    let connections = connections.max(1);
    // --sentinel 存在即启用**分连接哨兵**：每条连接 copy 完自己的数据后追加
    // 哨兵帧 {round=连接号, n=该连接实际行数, start_ns=该连接开始}——单连接
    // = 1 条（round=0，兼容旧语义）；多连接 = N 条，bench 侧汇总 Σn/(max emit −
    // min start)。传入值仅作开关（每连接行数以帧扫描为准）。
    let sentinel = sentinel_n.filter(|&n| n > 0);

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
        return send_arrow_copy_files(files, addr, rate_bytes, sentinel).await;
    }

    // --shard-keys "bid_events:auction,auction_events:id,person_events:id"
    // 按流指定分区 key 字段;同 key 事件永远同一连接(键闭包)。
    let key_by_stream = parse_shard_keys(&shard_keys);

    if key_by_stream.is_empty() {
        // 原样回放(raw copy):纯字节零解析;多连接时每条连接推完整文件
        send_arrow_raw(input, addr.clone(), connections, rate_bytes, sentinel).await
    } else {
        // 发送时按 key 分区(动态 decode;适合无预分片文件的临时注入)
        send_arrow_sharded(
            input,
            addr.clone(),
            connections,
            key_by_stream,
            rate_bytes,
            sentinel,
        )
        .await
    }
}

/// 把一个帧文件按 key 切分成 N 个分片帧文件(键闭包:同 key 同文件)。
///
/// 生成时一次切分,发送时纯 copy——避免发送端动态 decode+重编码的注入瓶颈。
/// 输出:`{output_prefix}.s0.frames` ~ `{output_prefix}.s{N-1}.frames`。
/// 未在 `--shard-keys` 列出的流:整帧写入第 0 个分片(不分区,保证数据完整)。
pub async fn shard_frames(args: ShardFramesArgs) -> WfgenResult<()> {
    let ShardFramesArgs {
        input,
        shards,
        shard_keys,
        output_prefix,
    } = args;
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
