use std::io::IsTerminal;
use std::path::PathBuf;
use std::process;

use anyhow::Result;

const GREEN: &str = "\x1b[1;32m";
const RED: &str = "\x1b[1;31m";
const YELLOW: &str = "\x1b[1;38;5;208m";
const RESET: &str = "\x1b[0m";

pub fn run(files: Vec<PathBuf>, write: bool, check: bool) -> Result<()> {
    if files.is_empty() {
        anyhow::bail!("no input files specified");
    }

    let color = std::io::stderr().is_terminal();

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_wfl::language())
        .map_err(|e| anyhow::anyhow!("failed to load WFL grammar: {e}"))?;

    let mut any_diff = false;

    for file in &files {
        let source = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", file.display()))?;

        // Parse with tree-sitter to validate syntax
        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| anyhow::anyhow!("failed to parse {}", file.display()))?;

        if tree.root_node().has_error() {
            if color {
                eprintln!(
                    "{RED}error{RESET}: syntax error in {}, skipping",
                    file.display()
                );
            } else {
                eprintln!("error: syntax error in {}, skipping", file.display());
            }
            any_diff = true;
            continue;
        }

        let formatted = format_source(&source);

        if check {
            if source != formatted {
                if color {
                    eprintln!("{YELLOW}{}{RESET}: not formatted", file.display());
                } else {
                    eprintln!("{}: not formatted", file.display());
                }
                any_diff = true;
            }
        } else if write {
            if source != formatted {
                std::fs::write(file, &formatted)
                    .map_err(|e| anyhow::anyhow!("writing {}: {e}", file.display()))?;
                if color {
                    eprintln!("{GREEN}formatted{RESET} {}", file.display());
                } else {
                    eprintln!("formatted {}", file.display());
                }
            }
        } else {
            print!("{formatted}");
        }
    }

    if check && any_diff {
        process::exit(1);
    }

    Ok(())
}

/// Format WFL source by normalizing indentation based on brace/paren nesting.
fn format_source(source: &str) -> String {
    let indent_str = "    ";
    let mut output = String::with_capacity(source.len());
    let mut indent: i32 = 0;
    let mut prev_blank = false;

    for line in source.lines() {
        let trimmed = line.trim();

        // Collapse multiple blank lines into one
        if trimmed.is_empty() {
            if !prev_blank && !output.is_empty() {
                output.push('\n');
            }
            prev_blank = true;
            continue;
        }
        prev_blank = false;

        // Count structural delimiters (skipping strings and comments)
        let (opens, closes) = count_delimiters(trimmed);

        // Lines starting with a closer are indented one less
        let this_indent = if trimmed.starts_with('}') || trimmed.starts_with(')') {
            (indent - 1).max(0) as usize
        } else {
            indent.max(0) as usize
        };

        // Write indented line (strip trailing whitespace)
        for _ in 0..this_indent {
            output.push_str(indent_str);
        }
        output.push_str(trimmed);
        output.push('\n');

        // Update indent for next line
        indent += opens as i32 - closes as i32;
    }

    // Ensure exactly one trailing newline
    while output.ends_with("\n\n") {
        output.pop();
    }
    if !output.ends_with('\n') {
        output.push('\n');
    }

    output
}

/// Count structural `{`/`(` (opens) and `}`/`)` (closes) in a line,
/// skipping characters inside string literals and `#` comments.
fn count_delimiters(line: &str) -> (usize, usize) {
    let mut opens = 0usize;
    let mut closes = 0usize;
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        match bytes[i] {
            b'#' => break, // rest is comment
            b'"' => {
                i += 1;
                while i < len && bytes[i] != b'"' {
                    i += 1;
                }
                if i < len {
                    i += 1;
                }
            }
            b'{' | b'(' => {
                opens += 1;
                i += 1;
            }
            b'}' | b')' => {
                closes += 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    (opens, closes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_simple_rule() {
        let input = r#"use "security.wfs"

rule brute_force {
events {
fail : auth_events && action == "failed"
}
match<sip:5m> {
on event {
fail | count >= 3;
}
} -> score(70.0)
entity(ip, fail.sip)
}
"#;
        let formatted = format_source(input);
        // Check indentation of events block
        assert!(formatted.contains("    events {"));
        assert!(formatted.contains("        fail : auth_events"));
        assert!(formatted.contains("    }"));
        // Check match block
        assert!(formatted.contains("    match<sip:5m> {"));
        assert!(formatted.contains("        on event {"));
        assert!(formatted.contains("            fail | count >= 3;"));
    }

    #[test]
    fn format_yield_multiline() {
        let input = r#"rule r {
events { e : w }
match<k:5m> {
on event { e | count >= 1; }
} -> score(1.0)
entity(ip, e.k)
yield out (
a = e.x,
b = e.y
)
}
"#;
        let formatted = format_source(input);
        assert!(formatted.contains("    yield out ("));
        assert!(formatted.contains("        a = e.x,"));
        assert!(formatted.contains("    )"));
    }

    #[test]
    fn format_preserves_comments() {
        let input = "# top comment\nrule r {\n# inner comment\nevents { e : w }\n}\n";
        let formatted = format_source(input);
        assert!(formatted.contains("# top comment"));
        assert!(formatted.contains("    # inner comment"));
    }

    #[test]
    fn format_braces_in_string_ignored() {
        let input = "rule r {\nevents { e : w && x == \"{test}\" }\n}\n";
        let formatted = format_source(input);
        // Braces inside string should not affect indentation
        assert!(formatted.contains("    events { e : w && x == \"{test}\" }"));
    }

    #[test]
    fn format_collapses_blank_lines() {
        let input = "use \"a.wfs\"\n\n\n\nrule r {\n}\n";
        let formatted = format_source(input);
        assert!(!formatted.contains("\n\n\n"));
    }

    #[test]
    fn format_idempotent() {
        let input = r#"use "security.wfs"

rule brute_force {
    events {
        fail : auth_events && action == "failed"
    }
    match<sip:5m> {
        on event {
            fail | count >= 3;
        }
    } -> score(70.0)
    entity(ip, fail.sip)
}
"#;
        let formatted = format_source(input);
        let formatted2 = format_source(&formatted);
        assert_eq!(formatted, formatted2, "formatting should be idempotent");
    }
}
