//! 分层文件比对（`wfgen diff <a> <b> [--detail]`）。
//!
//! 面向"两份结果文件对拍"（引擎 alerts vs 模拟器期望）设计，三层递进：
//!
//! - **L1 相同性**：流式哈希（SipHash）逐行更新，O(N) 快扫、内存有界。
//!   哈希相同 → 判相同，直接结束（不跑 Myers）。
//! - **L2 差异量**：Myers diff（`similar` crate，git diff 同款算法）→
//!   编辑操作数（删+插）/ 总行数 = **精确差异比例**。防退化：两边行数差
//!   超过 `DEGRADE_RATIO` 时 Myers 可能退化 O(N·M)，降级为**排序归并**
//!   （O(N log N)，差异大时更稳）。
//! - **L3 定位（可选 `--detail`）**：输出差异行明细（行号 + 删/插内容）；
//!   降级模式输出排序后的 missing/extra 行。
//!
//! 退出码：0 = 相同；1 = 不同（供脚本判定）。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::error::{self, WfgenReason, WfgenResult};

/// Myers 退化的行数差阈值：|N-M| > total*RATIO 时降级排序归并。
const DEGRADE_RATIO: f64 = 0.10;

/// 退出码：调用方依据返回的 `same` 决定（0 = 相同；1 = 不同）。
pub fn run(path_a: &str, path_b: &str, detail: bool) -> WfgenResult<bool> {
    let text_a = std::fs::read_to_string(path_a)
        .map_err(|e| error::error(WfgenReason::Io, format!("read {}: {e}", path_a)))?;
    let text_b = std::fs::read_to_string(path_b)
        .map_err(|e| error::error(WfgenReason::Io, format!("read {}: {e}", path_b)))?;

    let (lines_a, lines_b) = (split_lines(&text_a), split_lines(&text_b));
    Ok(compare_lines(&lines_a, &lines_b, detail))
}

/// 对拍两份行列表（git diff 同款分层方法，供 verify-nexmark --engine-emit
/// 在 wfgen 内部直接对拍，避免脚本侧 python 中介）。
/// 返回 `true` = 相同；`false` = 不同。
pub fn compare_lines(lines_a: &[&str], lines_b: &[&str], detail: bool) -> bool {
    // L1：流式哈希相同 → 相同（跳过 Myers）。
    let hash_a = hash_lines(lines_a);
    let hash_b = hash_lines(lines_b);
    if hash_a == hash_b {
        println!("identical ✅ (L1 hash, {} lines)", lines_a.len());
        return true;
    }
    println!(
        "different (L1 hash): {} vs {} lines",
        lines_a.len(),
        lines_b.len()
    );

    // L2：差异量。行数差过大 → 降级排序归并（Myers 退化保护）。
    // 统一行终止符（similar 按行 diff 需要一致行边界；L1 已按行哈希，行为不变）。
    let text_a = join_lines(lines_a);
    let text_b = join_lines(lines_b);
    let total = (lines_a.len() + lines_b.len()) as f64;
    let size_diff = (lines_a.len() as i64 - lines_b.len() as i64).unsigned_abs() as f64;
    let degraded = total > 0.0 && size_diff / total > DEGRADE_RATIO;

    let (diff_lines, ops_desc) = if degraded {
        let (missing, extra) = merge_count(lines_a, lines_b);
        let n = missing + extra;
        (
            n,
            format!(
                "degraded to sorted-merge (size gap {:.1}% > {:.0}%): {} missing + {} extra",
                size_diff / total * 100.0,
                DEGRADE_RATIO * 100.0,
                missing,
                extra
            ),
        )
    } else {
        let diff = similar::TextDiff::from_lines(&text_a, &text_b);
        let mut del = 0usize;
        let mut ins = 0usize;
        for op in diff.iter_all_changes() {
            match op.tag() {
                similar::ChangeTag::Delete => del += 1,
                similar::ChangeTag::Insert => ins += 1,
                similar::ChangeTag::Equal => {}
            }
        }
        (
            del + ins,
            format!("Myers: {} deleted + {} inserted", del, ins),
        )
    };

    let ratio = if total > 0.0 {
        diff_lines as f64 / total
    } else {
        0.0
    };
    println!(
        "diff: {:.2}% ({}) of {} lines differ",
        ratio * 100.0,
        ops_desc,
        lines_a.len().max(lines_b.len())
    );

    // L3：定位（可选）。
    if detail {
        print_detail(&text_a, &text_b, degraded);
    }

    false
}

/// 行列表 → 统一文本（每行补 \n，供 similar 按行 diff）。
fn join_lines(lines: &[&str]) -> String {
    let mut s = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// 按行切分（保留行尾不换行；忽略末尾空段——read_to_string 末尾无换行时
/// 不产生多余空行，有换行时 split('\n') 会多一个空段，统一去掉）。
fn split_lines(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text.split('\n').collect();
    if v.last().is_some_and(|last| last.is_empty()) {
        v.pop();
    }
    v
}

/// 逐行 SipHash（std DefaultHasher，确定性 seed）。
fn hash_lines(lines: &[&str]) -> u64 {
    let mut h = DefaultHasher::new();
    for line in lines {
        line.hash(&mut h);
        h.write_u8(0xff); // 行分隔，防 "ab\nc" == "a\nbc" 碰撞
    }
    h.finish()
}

/// 排序归并统计差异：返回 (missing=仅 A 有, extra=仅 B 有)。
fn merge_count(a: &[&str], b: &[&str]) -> (usize, usize) {
    let mut sa: Vec<&str> = a.to_vec();
    let mut sb: Vec<&str> = b.to_vec();
    sa.sort_unstable();
    sb.sort_unstable();
    let (mut i, mut j, mut missing, mut extra) = (0usize, 0usize, 0usize, 0usize);
    while i < sa.len() && j < sb.len() {
        match sa[i].cmp(sb[j]) {
            std::cmp::Ordering::Less => {
                missing += 1;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                extra += 1;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    missing += sa.len() - i;
    extra += sb.len() - j;
    (missing, extra)
}

/// 差异明细：Myers 模式输出编辑脚本（带行号）；降级模式输出排序后的 missing/extra。
fn print_detail(text_a: &str, text_b: &str, degraded: bool) {
    println!("-- detail --");
    if degraded {
        let a: Vec<&str> = split_lines(text_a);
        let b: Vec<&str> = split_lines(text_b);
        let mut sa: Vec<&str> = a.iter().copied().collect();
        let mut sb: Vec<&str> = b.iter().copied().collect();
        sa.sort_unstable();
        sb.sort_unstable();
        let (mut i, mut j) = (0usize, 0usize);
        while i < sa.len() && j < sb.len() {
            match sa[i].cmp(sb[j]) {
                std::cmp::Ordering::Less => {
                    println!("- {} (only in {})", sa[i], "a");
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    println!("+ {} (only in b)", sb[j]);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
            }
        }
        while i < sa.len() {
            println!("- {} (only in a)", sa[i]);
            i += 1;
        }
        while j < sb.len() {
            println!("+ {} (only in b)", sb[j]);
            j += 1;
        }
        return;
    }

    // Myers 编辑脚本：带两侧行号（1-based，与 diff 惯例一致）。
    let diff = similar::TextDiff::from_lines(text_a, text_b);
    for (idx, op) in diff.ops().iter().enumerate() {
        if idx > 0 {
            println!("@@");
        }
        for change in diff.iter_changes(op) {
            let sign = match change.tag() {
                similar::ChangeTag::Delete => "-",
                similar::ChangeTag::Insert => "+",
                similar::ChangeTag::Equal => " ",
            };
            let (a_ln, b_ln) = match change.tag() {
                similar::ChangeTag::Delete => (change.old_index().map(|i| i + 1).unwrap_or(0), 0),
                similar::ChangeTag::Insert => (0, change.new_index().map(|i| i + 1).unwrap_or(0)),
                similar::ChangeTag::Equal => (
                    change.old_index().map(|i| i + 1).unwrap_or(0),
                    change.new_index().map(|i| i + 1).unwrap_or(0),
                ),
            };
            println!(
                "{sign} a{a_ln}:b{b_ln} {}",
                change.value().trim_end_matches('\n')
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_files_pass_l1() {
        let a = "x\nfoo\nbar\n";
        let b = "x\nfoo\nbar\n";
        assert_eq!(hash_lines(&split_lines(a)), hash_lines(&split_lines(b)));
        assert_eq!(merge_count(&split_lines(a), &split_lines(b)), (0, 0));
    }

    #[test]
    fn l1_hash_distinguishes_line_breaks() {
        // "a\nbc" vs "ab\nc" —— 若不分隔会碰撞
        let a = split_lines("a\nbc\n");
        let b = split_lines("ab\nc\n");
        assert_ne!(hash_lines(&a), hash_lines(&b));
    }

    #[test]
    fn merge_count_finds_missing_and_extra() {
        let a = split_lines("1\n2\n3\n");
        let b = split_lines("2\n3\n4\n");
        // 排序后 A={1,2,3} B={2,3,4} → missing=1 (1), extra=1 (4)
        assert_eq!(merge_count(&a, &b), (1, 1));
    }

    #[test]
    fn merge_count_handles_duplicates() {
        let a = split_lines("1\n1\n2\n");
        let b = split_lines("1\n2\n");
        // 排序后 A={1,1,2} B={1,2} → missing=1（多出的 1）
        assert_eq!(merge_count(&a, &b), (1, 0));
    }

    #[test]
    fn split_lines_no_trailing_empty() {
        assert_eq!(split_lines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(split_lines("a\nb"), vec!["a", "b"]);
    }
}
