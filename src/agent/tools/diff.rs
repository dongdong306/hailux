/// 每个变更组保留的上下文行数
const DIFF_CONTEXT_LINES: usize = 3;

/// 序列化 edit/write 的 diff 展示数据。
///
/// 只存储预计算的 hunk（变更行 + 上下文行），不存储文件全文，
/// 避免大文件多次编辑导致 runtime_meta 落库膨胀。
pub(super) fn serialize_diff_data(old: Option<&str>, new: &str, file_path: &str) -> String {
    use similar::{ChangeTag, TextDiff};

    let old_lines: Vec<&str> = old.map(|s| s.lines().collect()).unwrap_or_default();
    let new_lines: Vec<&str> = new.lines().collect();

    let diff = TextDiff::from_slices(&old_lines, &new_lines);

    let mut additions = 0usize;
    let mut deletions = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            _ => {}
        }
    }

    let mut hunks = Vec::new();

    for group in diff.grouped_ops(DIFF_CONTEXT_LINES) {
        let mut old_start: Option<usize> = None;
        let mut new_start: Option<usize> = None;
        let mut changes: Vec<(char, String)> = Vec::new();

        for op in &group {
            for change in diff.iter_changes(op) {
                if old_start.is_none()
                    && let Some(i) = change.old_index()
                {
                    old_start = Some(i + 1);
                }
                if new_start.is_none()
                    && let Some(i) = change.new_index()
                {
                    new_start = Some(i + 1);
                }
                let sign = match change.tag() {
                    ChangeTag::Insert => '+',
                    ChangeTag::Delete => '-',
                    ChangeTag::Equal => ' ',
                };
                changes.push((sign, change.value().to_string()));
            }
        }

        if !changes.is_empty() {
            hunks.push(serde_json::json!({
                "old_start": old_start,
                "new_start": new_start,
                "changes": changes
                    .iter()
                    .map(|(sign, text)| serde_json::json!([sign.to_string(), text]))
                    .collect::<Vec<_>>(),
            }));
        }
    }

    serde_json::json!({
        "format": "diff",
        "path": file_path,
        "additions": additions,
        "deletions": deletions,
        "old_lines": old_lines.len(),
        "new_lines": new_lines.len(),
        "is_new_file": old.is_none(),
        "hunks": hunks,
    })
    .to_string()
}
