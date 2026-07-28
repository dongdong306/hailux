use std::cmp::Ordering;
use std::path::Path;

pub fn compare_mtime(a: &Path, b: &Path) -> Ordering {
    let mtime_a = std::fs::metadata(a).ok().and_then(|m| m.modified().ok());
    let mtime_b = std::fs::metadata(b).ok().and_then(|m| m.modified().ok());
    match (mtime_a, mtime_b) {
        (Some(ta), Some(tb)) => tb.cmp(&ta),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

// ── 共享 Frontmatter 解析 ────────────────────────────────────

/// 去除 frontmatter 值两端的空白与成对引号。
pub fn strip_frontmatter_value(raw: &str) -> String {
    let v = raw.trim();
    if (v.starts_with('"') && v.ends_with('"') && v.len() >= 2)
        || (v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2)
    {
        v[1..v.len() - 1].trim().to_string()
    } else {
        v.to_string()
    }
}

/// 解析 frontmatter 结构，分离 frontmatter 原始文本与正文内容。
/// 支持 BOM 剥离、CRLF/LF 换行兼容。
/// 返回 `(frontmatter_raw, content)`；content 已 trim。
/// 若无合法 frontmatter 分隔块则返回 `None`。
pub fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let after = trimmed.strip_prefix("---")?;
    let after = after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after);

    let (marker_pos, marker_len) = if let Some(pos) = after.find("\r\n---") {
        (pos, 5)
    } else {
        let pos = after.find("\n---")?;
        (pos, 4)
    };

    let frontmatter = &after[..marker_pos];
    let mut content = &after[marker_pos + marker_len..];
    content = content
        .strip_prefix('\n')
        .or_else(|| content.strip_prefix("\r\n"))
        .unwrap_or(content);

    Some((frontmatter, content.trim()))
}
