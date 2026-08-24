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
    shard_frames(ShardFramesArgs {
        input,
        shards: 4,
        shard_keys: "bid_events:auction".into(),
        output_prefix: prefix.clone(),
    })
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
    shard_frames(ShardFramesArgs {
        input,
        shards: 4,
        shard_keys: "bid_events:some_key,auc_events:some_key".into(),
        output_prefix: prefix.clone(),
    })
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
