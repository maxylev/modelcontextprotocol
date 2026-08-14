/// Human-readable byte size, matching the reference server's `formatSize`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let i = (bytes as f64).log(1024.0).floor() as usize;
    if i == 0 {
        return format!("{bytes} B");
    }
    let unit_index = i.min(UNITS.len() - 1);
    let value = bytes as f64 / 1024f64.powi(unit_index as i32);
    format!("{value:.2} {}", UNITS[unit_index])
}

/// First `num_lines` lines of `content`.
pub fn head_lines(content: &str, num_lines: u32) -> String {
    content
        .lines()
        .take(num_lines as usize)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Last `num_lines` lines of `content`.
pub fn tail_lines(content: &str, num_lines: u32) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(num_lines as usize);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_like_reference() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(42), "42 B");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(5_242_880), "5.00 MB");
        assert_eq!(format_size(1_099_511_627_776), "1.00 TB");
    }

    #[test]
    fn head_and_tail_lines() {
        let content = "a\nb\nc\nd\ne\n";
        assert_eq!(head_lines(content, 2), "a\nb");
        assert_eq!(head_lines(content, 10), "a\nb\nc\nd\ne");
        assert_eq!(tail_lines(content, 2), "d\ne");
        assert_eq!(tail_lines(content, 10), "a\nb\nc\nd\ne");
        assert_eq!(tail_lines(content, 0), "");
    }
}
