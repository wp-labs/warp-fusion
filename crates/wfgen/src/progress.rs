//! 终端进度条（stderr、仅 TTY）。
//!
//! - 输出走 **stderr**：gen/verify 的 stdout 是数据流（JSONL/JSON），不能污染。
//! - 非 TTY（管道/重定向）时完全静默，不输出任何控制字符。
//! - 每 ~100ms 刷新一行 `\r` 覆盖，完成时清行。
//!
//! 用法：`ProgressBar::new(total, label)` → 每处理一项 `tick()`（或 `add(n)`）
//! → 结束 `finish()`。内部一个轻量线程做定时刷新，主线程只做原子计数。

use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct ProgressBar {
    done: Arc<AtomicU64>,
    total: u64,
    label: String,
    start: std::time::Instant,
    handle: Option<JoinHandle<()>>,
}

impl ProgressBar {
    /// 创建进度条。非 TTY 时降级：不渲染，但 `finish()` 仍输出一行完成摘要
    /// （stderr；stdout 是数据流，任何场景都不碰）。
    pub fn new(total: u64, label: impl Into<String>) -> Self {
        let label = label.into();
        let start = Instant::now();
        let enabled = std::io::stderr().is_terminal() && total > 0;
        if !enabled {
            return Self {
                done: Arc::new(AtomicU64::new(0)),
                total,
                label,
                start,
                handle: None,
            };
        }
        let done = Arc::new(AtomicU64::new(0));
        let done_ref = Arc::clone(&done);
        let label_ref = label.clone();
        let total_ref = total;
        let handle = std::thread::spawn(move || {
            let start = Instant::now();
            loop {
                let d = done_ref.load(Ordering::Relaxed);
                if d >= total_ref {
                    break;
                }
                render(&label_ref, d, total_ref, start.elapsed().as_secs_f64());
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Self {
            done,
            total,
            label,
            start,
            handle: Some(handle),
        }
    }

    /// 记录一个处理项。
    #[inline]
    pub fn tick(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    /// 共享完成计数器：多线程各自累加同一进度（如 verify 的分片并行 Sim）。
    pub fn counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.done)
    }

    /// 记录 n 个处理项。
    #[inline]
    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// 完成：等进度线程打完最后一行并清行。
    /// 非 TTY 时打印一行完成摘要（stderr），避免完全静默。
    pub fn finish(self) {
        if let Some(handle) = self.handle {
            self.done.store(self.total, Ordering::Relaxed);
            let _ = handle.join();
            // 清行 + 换行：进度线程退出前打的是 `\r...`，这里补一个换行收尾
            // （渲染函数在 done==total 时会打 `\r... 100%`，随后这里清掉行尾）。
            eprint!("\r\x1b[K");
        } else {
            let d = self.done.load(Ordering::Relaxed);
            if d > 0 {
                eprintln!(
                    "{}: {} / {} 完成，耗时 {:.1}s",
                    self.label,
                    fmt_num(d),
                    fmt_num(self.total),
                    self.start.elapsed().as_secs_f64()
                );
            }
        }
    }
}

fn render(label: &str, done: u64, total: u64, elapsed: f64) {
    let pct = if total == 0 {
        100.0
    } else {
        (done as f64 / total as f64) * 100.0
    };
    // 帧宽预算 ≈ 78 列（label 16 + bar 18 + pct 7 + 计数 22 + 时间 11）——
    // 超过终端宽度会折行，`\r` 只回到折行后的行首，帧会叠成多行（实测）。
    let width = 18usize;
    let filled = ((pct / 100.0) * width as f64) as usize;
    let bar: String = (0..width)
        .map(|i| if i < filled { '█' } else { '░' })
        .collect();
    let done_disp = fmt_num(done);
    let total_disp = fmt_num(total);
    // 预计剩余：按当前速率外推
    let eta = if done > 0 {
        elapsed / done as f64 * (total - done) as f64
    } else {
        0.0
    };
    // 先清行再写：短帧残留的旧字符（更长帧的尾巴）必须擦掉，否则原地刷新会留残影。
    eprint!(
        "\r\x1b[K{label} [{bar}] {pct:5.1}% {done_disp}/{total_disp} {elapsed:.0}s ETA {eta:.0}s"
    );
}

/// 进度条活动期间输出独立状态行（如 oracle 跳过提示）。
///
/// 进度条用 `\r` 原地刷新不换行，直接 `eprintln!` 会把消息粘在当前帧尾部
/// （实测 "... 0s ETA 0soracle: 跳过..." 同行的残影）。这里先清掉当前帧再
/// 换行打印——下一帧进度条会重绘在消息下方那行。非 TTY 时退化为普通 eprintln。
pub(crate) fn note(msg: &str) {
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[K");
    }
    eprintln!("{msg}");
}

/// 数字格式化：1_234_567 → "1,234,567"（报告/进度条共用）
pub(crate) fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}
