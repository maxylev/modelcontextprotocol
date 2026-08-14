use similar::TextDiff;

/// A single edit operation: find `old_text`, replace with `new_text`.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct EditOperation {
    /// Text to search for - must match exactly
    #[serde(rename = "oldText")]
    pub old_text: String,
    /// Text to replace with
    #[serde(rename = "newText")]
    pub new_text: String,
}

/// Normalize CRLF to LF so edits behave identically across platforms.
pub fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Apply a sequence of edits to `content`, mirroring the reference server:
///
/// 1. Try an exact substring match first.
/// 2. Otherwise match line-by-line, ignoring leading/trailing whitespace per
///    line, and preserve the indentation of the first matched line (keeping
///    relative indentation of the replacement).
///
/// Returns the modified content, or an error describing the first edit that
/// could not be applied.
pub fn apply_edits(content: &str, edits: &[EditOperation]) -> Result<String, String> {
    let content = normalize_line_endings(content);
    let mut modified = content;

    for edit in edits {
        let old = normalize_line_endings(&edit.old_text);
        let new = normalize_line_endings(&edit.new_text);

        if let Some(pos) = modified.find(&old) {
            let start = pos;
            let end = pos + old.len();
            modified.replace_range(start..end, &new);
            continue;
        }

        // Line-by-line matching with whitespace normalization.
        let old_lines: Vec<&str> = old.split('\n').collect();
        let content_lines: Vec<&str> = modified.split('\n').collect();
        let mut matched = false;

        if old_lines.len() > content_lines.len() {
            return Err(no_match_message(&edit.old_text));
        }

        'outer: for i in 0..=content_lines.len() - old_lines.len() {
            for (j, old_line) in old_lines.iter().enumerate() {
                let content_line = content_lines[i + j];
                if old_line.trim() != content_line.trim() {
                    continue 'outer;
                }
            }

            let original_indent = leading_whitespace(content_lines[i]);
            let mut new_lines: Vec<String> = Vec::with_capacity(old_lines.len());
            for (j, line) in new.split('\n').enumerate() {
                if j == 0 {
                    new_lines.push(format!("{original_indent}{}", line.trim_start()));
                } else {
                    let old_indent = leading_whitespace(old_lines.get(j).unwrap_or(&""));
                    let new_indent = leading_whitespace(line);
                    if !old_indent.is_empty() && !new_indent.is_empty() {
                        let relative = new_indent.len() as isize - old_indent.len() as isize;
                        let padding = (original_indent.len() as isize + relative).max(0) as usize;
                        new_lines.push(format!("{}{}", " ".repeat(padding), line.trim_start()));
                    } else {
                        new_lines.push(line.to_string());
                    }
                }
            }

            let mut out: Vec<String> = content_lines.iter().map(|s| s.to_string()).collect();
            out.splice(i..i + old_lines.len(), new_lines);
            modified = out.join("\n");
            matched = true;
            break;
        }

        if !matched {
            return Err(no_match_message(&edit.old_text));
        }
    }

    Ok(modified)
}

fn no_match_message(old_text: &str) -> String {
    format!("Could not find exact match for edit:\n{old_text}")
}

fn leading_whitespace(line: &str) -> &str {
    let end = line.len() - line.trim_start().len();
    &line[..end]
}

/// Render a unified diff between `original` and `modified` in git style.
pub fn render_diff(original: &str, modified: &str) -> String {
    let diff = TextDiff::from_lines(original, modified)
        .unified_diff()
        .context_radius(3)
        .header("original", "modified")
        .to_string();

    // Wrap in a fenced block, growing the backtick count if needed.
    let mut backticks = "```".to_string();
    while diff.contains(&backticks) {
        backticks.push('`');
    }
    format!("{backticks}diff\n{diff}{backticks}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old_text: &str, new_text: &str) -> EditOperation {
        EditOperation {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        }
    }

    #[test]
    fn exact_substring_replacement() {
        let content = "line one\nline two\nline three\n";
        let result = apply_edits(content, &[edit("line two", "changed")]).unwrap();
        assert_eq!(result, "line one\nchanged\nline three\n");
    }

    #[test]
    fn sequential_edits_apply_in_order() {
        let content = "a\nb\nc\n";
        let result = apply_edits(content, &[edit("a", "x"), edit("c", "z")]).unwrap();
        assert_eq!(result, "x\nb\nz\n");
    }

    #[test]
    fn whitespace_insensitive_line_match_preserves_indent() {
        let content = "    foo\n    bar\n";
        let result = apply_edits(content, &[edit("foo", "baz")]).unwrap();
        assert_eq!(result, "    baz\n    bar\n");
    }

    #[test]
    fn crlf_is_normalized() {
        let content = "a\r\nb\r\n";
        let result = apply_edits(content, &[edit("b", "c")]).unwrap();
        assert_eq!(result, "a\nc\n");
    }

    #[test]
    fn no_match_is_an_error() {
        let err = apply_edits("hello world", &[edit("nope", "x")]).unwrap_err();
        assert!(err.contains("Could not find exact match"));
        assert!(err.contains("nope"));
    }

    #[test]
    fn multiple_same_line_edits() {
        let content = "hello world hello\n";
        let result = apply_edits(content, &[edit("hello", "hi")]).unwrap();
        assert_eq!(result, "hi world hello\n");
    }

    #[test]
    fn diff_contains_expected_markers() {
        let diff = render_diff("a\nb\nc\n", "a\nB\nc\n");
        assert!(diff.contains("```diff"));
        assert!(diff.contains("--- original"));
        assert!(diff.contains("+++ modified"));
        assert!(diff.contains("@@"));
    }
}
