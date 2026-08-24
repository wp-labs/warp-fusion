use super::*;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use arrow::array::Int64Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;

#[test]
fn n_list_parses_suffixes() {
    assert_eq!(
        parse_n_list("100k,1m,3m").unwrap(),
        vec![100_000, 1_000_000, 3_000_000]
    );
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
    let path = dir.join(format!(
        "wfgen_scan_bad_payload_{}.frames",
        std::process::id()
    ));
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
    let path = dir.join(format!(
        "wfgen_sentinel_blank_{}.ndjson",
        std::process::id()
    ));
    std::fs::write(&path, "\n\n{\"record_type\":\"stage\",\"current\":0}\n\n").unwrap();
    let records = read_sentinel_file(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(records.len(), 1, "空行跳过");
    assert!(records[0].is_stage(0));
}

#[tokio::test]
async fn wait_for_sentinel_times_out() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "wfgen_wait_sent_timeout_{}.ndjson",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::write(
        &path,
        r#"{"record_type":"stage","current":0}"#.to_string() + "\n",
    )
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
    std::fs::write(
        &sentinel_path,
        r#"{"record_type":"stage","current":0}"#.to_string() + "\n",
    )
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
            rec.push_str(&format!(r#"{{"record_type":"stage","current":{}}}"#, k + 1));
            rec.push('\n');
            std::fs::write(&sentinel_path2, rec).unwrap();
        }
    });

    let args = Args {
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
    let args = Args {
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
    std::fs::write(&diag_path, "[[stages]]\nname = \"floor\"\n").unwrap();
    let args = Args {
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
    std::fs::write(&diag_path, "[[stages]]\nname = \"floor\"\n").unwrap();
    let frames_path = dir.path().join("data.frames");
    make_frames_file(&frames_path, 2);
    let sentinel_path = dir.path().join("perf_sentinel.ndjson");
    std::fs::write(
        &sentinel_path,
        r#"{"record_type":"stage","current":0}"#.to_string() + "\n",
    )
    .unwrap();
    // 找一个肯定没监听的端口。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    let args = Args {
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
        FrameInfo {
            offset: 0,
            len: 10,
            rows: 4,
        },
        FrameInfo {
            offset: 10,
            len: 10,
            rows: 6,
        },
        FrameInfo {
            offset: 20,
            len: 10,
            rows: 8,
        },
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
    assert_eq!(
        frames[0].len,
        payload.len() + format!("{} ", payload.len()).len()
    );
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
    wait_for_stage(&path, 1, Duration::from_secs(5))
        .await
        .unwrap();
    writer.await.unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn wait_for_stage_times_out() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "wfgen_wait_stage_timeout_{}.ndjson",
        std::process::id()
    ));
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
