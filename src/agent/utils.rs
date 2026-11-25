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

// ── Shared Frontmatter Parsing ────────────────────────────────

/// Strips surrounding whitespace and paired quotes from a frontmatter value.
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

/// Parses the frontmatter structure, separating raw frontmatter text from body content.
/// Supports BOM stripping and CRLF/LF line-ending tolerance.
/// Returns `(frontmatter_raw, content)`; content is trimmed.
/// Returns `None` if there is no valid frontmatter delimiter block.
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
