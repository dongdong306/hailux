//! 用量统计全屏页面：汇总卡 + 每日条形图 + 按模型/项目聚合 + 最近请求明细。
//! 数据来自 messages 表中带 usage 的 assistant 行（一次 LLM 请求一行）。
//! 交互：↑↓/PgUp/PgDn/滚轮 移动明细选中，←→ 切时间窗口，Tab 快速切换全部/当前项目，
//! p 打开项目选择器，r 刷新，Esc 返回。

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::storage::{
    DailyUsage, ModelUsage, ProjectUsage, UsageRecord, UsageSummary, WorkDirInfo,
};

/// 可切换的时间窗口（天；0 = 全部）
pub const STATS_WINDOWS: [u32; 4] = [7, 30, 90, 0];

/// 页面数据快照（打开/刷新时一次性查询）
#[derive(Debug, Default, Clone)]
pub struct StatsData {
    /// 当前窗口 + 范围内的汇总
    pub summary: UsageSummary,
    /// 同范围的全量汇总（对比展示）
    pub total: UsageSummary,
    pub daily: Vec<DailyUsage>,
    pub by_model: Vec<ModelUsage>,
    pub by_project: Vec<ProjectUsage>,
    pub recent: Vec<UsageRecord>,
    /// 统计窗口（天；0 = 全部）
    pub days: u32,
}

pub struct StatsViewer<'a> {
    pub data: &'a StatsData,
    pub selected_index: usize,
    /// 项目筛选；None = 全部项目
    pub work_dir: Option<&'a str>,
    /// 已知项目列表（选择器数据源）
    pub projects: &'a [WorkDirInfo],
    /// 选择器打开时的选中项
    pub picker_index: Option<usize>,
}

impl<'a> StatsViewer<'a> {
    pub fn render(self, area: Rect, buf: &mut Buffer) {
        // 全屏布局：标题(1) 汇总(3) 中部双栏(flex) 最近请求(flex) 帮助(1)
        let [
            header_area,
            summary_area,
            middle_area,
            recent_area,
            footer_area,
        ] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .areas(area);

        self.render_header(header_area, buf);
        self.render_summary(summary_area, buf);
        self.render_middle(middle_area, buf);
        self.render_recent(recent_area, buf);
        self.render_footer(footer_area, buf);

        if self.picker_index.is_some() {
            self.render_picker(area, buf);
        }
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let title = " Token 用量统计 ";
        let scope = match self.work_dir {
            None => "全部项目".to_string(),
            Some(w) => truncate_str(w, 48),
        };
        let window = match self.data.days {
            0 => "全部时间".to_string(),
            d => format!("近 {d} 天"),
        };
        let right = format!(" {scope} · {window} ");
        let title_w = UnicodeWidthStr::width(title);
        let right_w = UnicodeWidthStr::width(right.as_str());
        let pad = (area.width as usize).saturating_sub(title_w + right_w);
        Paragraph::new(Line::from(vec![
            Span::styled(
                title,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, Style::default().fg(Color::DarkGray)),
        ]))
        .render(area, buf);
    }

    fn render_summary(&self, area: Rect, buf: &mut Buffer) {
        let s = &self.data.summary;
        let miss = s.prompt_tokens.saturating_sub(s.cached_tokens);
        let total = s.prompt_tokens + s.completion_tokens;
        let hit_rate = if s.prompt_tokens > 0 {
            s.cached_tokens as f64 / s.prompt_tokens as f64 * 100.0
        } else {
            0.0
        };

        let labels = [
            ("请求", 10),
            ("输入", 12),
            ("缓存命中", 16),
            ("未命中", 12),
            ("输出", 12),
            ("总计", 12),
        ];
        let mut label_line = Vec::new();
        for (label, w) in labels {
            label_line.push(Span::styled(
                pad_display(label, w),
                Style::default().fg(Color::DarkGray),
            ));
        }

        let values = [
            (s.requests.to_string(), Color::White, 10),
            (format_tokens(s.prompt_tokens), Color::Green, 12),
            (
                format!("{} ({hit_rate:.1}%)", format_tokens(s.cached_tokens)),
                Color::Cyan,
                16,
            ),
            (format_tokens(miss), Color::Blue, 12),
            (format_tokens(s.completion_tokens), Color::Yellow, 12),
            (format_tokens(total), Color::Magenta, 12),
        ];
        let bold = Modifier::BOLD;
        let mut value_line = Vec::new();
        for (value, color, w) in values {
            value_line.push(Span::styled(
                pad_display(&value, w),
                Style::default().fg(color).add_modifier(bold),
            ));
        }

        let t = &self.data.total;
        Paragraph::new(vec![
            Line::from(label_line),
            Line::from(value_line),
            Line::from(Span::styled(
                format!(
                    " 同范围全部历史：请求 {} · 输入 {} · 缓存命中 {} · 输出 {}",
                    t.requests,
                    format_tokens(t.prompt_tokens),
                    format_tokens(t.cached_tokens),
                    format_tokens(t.completion_tokens),
                ),
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .render(area, buf);
    }

    fn render_middle(&self, area: Rect, buf: &mut Buffer) {
        let [daily_area, side_area] =
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                .areas(area);
        self.render_daily(daily_area, buf);
        self.render_side(side_area, buf);
    }

    /// 每日条形图：每根柱分三段 —— ▓ 缓存命中(cyan) + ░ 未命中输入(green) + █ 输出(yellow)
    fn render_daily(&self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            " 每日用量（▓ 缓存命中 · ░ 未命中输入 · █ 输出）",
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        )));

        let height = area.height as usize;
        let rows = height.saturating_sub(1);
        let tail: Vec<&DailyUsage> = self
            .data
            .daily
            .iter()
            .rev()
            .take(rows.min(14))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        if tail.is_empty() {
            lines.push(Line::from(Span::styled(
                " （暂无数据）",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            // 固定布局：日期(7) + 条形(BAR_W) + 总量(右对齐9) + 次数(右对齐8)
            let bar_width = (area.width as usize).saturating_sub(26).max(4);
            let max_total = tail
                .iter()
                .map(|d| d.prompt_tokens + d.completion_tokens)
                .max()
                .unwrap_or(1)
                .max(1);
            for d in &tail {
                let cached_len = (d.cached_tokens as usize * bar_width) / max_total as usize;
                let miss_len = (d.prompt_tokens.saturating_sub(d.cached_tokens) as usize
                    * bar_width)
                    / max_total as usize;
                let out_len = (d.completion_tokens as usize * bar_width) / max_total as usize;
                let total = d.prompt_tokens + d.completion_tokens;
                let date = &d.date[5.min(d.date.len())..];
                let bar_used = cached_len + miss_len + out_len;
                lines.push(Line::from(vec![
                    Span::styled(format!(" {date} "), Style::default().fg(Color::DarkGray)),
                    Span::styled("▓".repeat(cached_len), Style::default().fg(Color::Cyan)),
                    Span::styled("░".repeat(miss_len), Style::default().fg(Color::Green)),
                    Span::styled("█".repeat(out_len), Style::default().fg(Color::Yellow)),
                    Span::styled(
                        " ".repeat(bar_width.saturating_sub(bar_used)),
                        Style::default(),
                    ),
                    Span::styled(
                        format!(" {}  ", pad_display(&format_tokens(total), 8)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{}次", pad_display(&d.requests.to_string(), 5)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        Paragraph::new(lines).render(area, buf);
    }

    /// 右栏：按模型 + 按项目聚合
    fn render_side(&self, area: Rect, buf: &mut Buffer) {
        let height = area.height as usize;
        let mut lines: Vec<Line> = Vec::new();
        let section = Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD);

        // 按模型（预算：一半行数）
        lines.push(Line::from(Span::styled(" 按模型", section)));
        let model_budget = (height.saturating_sub(4) / 2).max(1);
        if self.data.by_model.is_empty() {
            lines.push(Line::from(Span::styled(
                " （暂无数据）",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for m in self.data.by_model.iter().take(model_budget) {
                let hit = if m.prompt_tokens > 0 {
                    m.cached_tokens as f64 / m.prompt_tokens as f64 * 100.0
                } else {
                    0.0
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {}", pad_right(&truncate_str(&m.model, 18), 19)),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled(
                        format!(
                            "{:>5}次 in {:>8} {} out {:>8}",
                            m.requests,
                            format_tokens(m.prompt_tokens),
                            hit_col(Some(hit)),
                            format_tokens(m.completion_tokens),
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            if self.data.by_model.len() > model_budget {
                lines.push(Line::from(Span::styled(
                    format!(" … 共 {} 个模型", self.data.by_model.len()),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        lines.push(Line::from(""));
        // 按项目（剩余行数）
        lines.push(Line::from(Span::styled(" 按项目", section)));
        let used = lines.len();
        let project_budget = height.saturating_sub(used + 1).max(1);
        if self.data.by_project.is_empty() {
            lines.push(Line::from(Span::styled(
                " （暂无数据）",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for p in self.data.by_project.iter().take(project_budget) {
                let name = tail_dir(&p.work_dir);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {}", pad_right(&truncate_str(&name, 18), 19)),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled(
                        format!(
                            "{:>5}次 in {:>8} {} out {:>8}",
                            p.requests,
                            format_tokens(p.prompt_tokens),
                            hit_col(None),
                            format_tokens(p.completion_tokens),
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            if self.data.by_project.len() > project_budget {
                lines.push(Line::from(Span::styled(
                    format!(" … 共 {} 个项目", self.data.by_project.len()),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        Paragraph::new(lines).render(area, buf);
    }

    /// 最近请求明细表：时间 / 模型 / 输入 / 命中 / 未命中 / 输出 / 项目
    /// 表头与数据行用同一组 pad 函数构造，保证列严格对齐。
    fn render_recent(&self, area: Rect, buf: &mut Buffer) {
        let dark = Style::default().fg(Color::DarkGray);
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!(" 最近请求（{} 条）", self.data.recent.len()),
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        )));
        // 表头：与数据行同构 —— 选择符(2) + 时间(12+2) + 模型(18+2) + 四个数字列(8+2)
        lines.push(Line::from(vec![
            Span::styled("   ", dark),
            Span::styled(pad_right("时间", 12), dark),
            Span::styled("  ", dark),
            Span::styled(pad_right("模型", 18), dark),
            Span::styled("  ", dark),
            Span::styled(pad_display("输入", 8), dark),
            Span::styled("  ", dark),
            Span::styled(pad_display("命中", 8), dark),
            Span::styled("  ", dark),
            Span::styled(pad_display("未命中", 8), dark),
            Span::styled("  ", dark),
            Span::styled(pad_display("输出", 8), dark),
            Span::styled("  ", dark),
            Span::styled("项目", dark),
        ]));

        let visible = (area.height as usize).saturating_sub(2);
        let len = self.data.recent.len();
        let first_visible = if visible > 0 {
            self.selected_index
                .saturating_sub(visible / 2)
                .min(len.saturating_sub(visible.min(len)))
        } else {
            0
        };

        let width = area.width as usize;
        for (i, r) in self.data.recent.iter().enumerate().skip(first_visible) {
            if lines.len() >= area.height as usize {
                break;
            }
            let is_selected = i == self.selected_index;
            let style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::Reset)
            };
            let dim = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let selector = if is_selected { "▸" } else { " " };
            let time = r
                .created_at
                .get(5..16)
                .unwrap_or(&r.created_at)
                .replace('T', " ");
            let miss = r.prompt_tokens.saturating_sub(r.cached_tokens);
            let project = tail_dir(&r.work_dir);

            let mut spans = vec![
                Span::styled(format!("{selector} "), dim),
                Span::styled(pad_right(&time, 12), style),
                Span::styled("  ", style),
                Span::styled(pad_right(&truncate_str(&r.model, 18), 18), style),
                Span::styled("  ", style),
                Span::styled(pad_display(&format_tokens(r.prompt_tokens), 8), style),
                Span::styled("  ", style),
                Span::styled(pad_display(&format_tokens(r.cached_tokens), 8), style),
                Span::styled("  ", style),
                Span::styled(pad_display(&format_tokens(miss), 8), style),
                Span::styled("  ", style),
                Span::styled(pad_display(&format_tokens(r.completion_tokens), 8), style),
                Span::styled("  ", style),
                Span::styled(truncate_str(&project, width.saturating_sub(78)), dim),
            ];
            if is_selected {
                spans.push(Span::styled(" ".repeat(width), style));
            }
            lines.push(Line::from(spans));
        }
        Paragraph::new(lines).render(area, buf);
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let help = if self.picker_index.is_some() {
            " ↑↓/PgUp/PgDn 选择项目  Enter 确认  Esc 取消"
        } else {
            " ↑↓ 选择  PgUp/PgDn 翻页  ←→ 时间窗口  Tab 全部/当前项目  p 选择项目  r 刷新  Esc 返回"
        };
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        )))
        .render(area, buf);
    }

    /// 项目选择器弹层：居中列表（全部项目 + 各项目名 + 会话数/最近活跃/路径）
    fn render_picker(&self, area: Rect, buf: &mut Buffer) {
        let Some(selected) = self.picker_index else {
            return;
        };
        let total = self.projects.len() + 1;

        let dialog_width = (area.width as usize).min(80);
        let list_rows = total.min(10);
        // 标题(1) + 列表(list_rows) + 空行(1) + 帮助(1) + 边框(2)
        let dialog_height = (list_rows + 5).min(area.height as usize);
        let dialog_x = (area.width as usize).saturating_sub(dialog_width) / 2;
        let dialog_y = (area.height as usize).saturating_sub(dialog_height) / 2;
        let dialog_area = Rect::new(
            dialog_x as u16,
            dialog_y as u16,
            dialog_width as u16,
            dialog_height as u16,
        );

        Clear.render(dialog_area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Gray))
            .border_type(BorderType::Rounded)
            .title(Span::styled(
                " 选择项目 ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center);
        let inner = block.inner(dialog_area);
        block.render(dialog_area, buf);

        let [content_area, help_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        let width = content_area.width as usize;
        let visible = content_area.height as usize;
        let first_visible = if visible > 0 {
            selected
                .saturating_sub(visible / 2)
                .min(total.saturating_sub(visible))
        } else {
            0
        };

        let mut lines: Vec<Line> = Vec::new();
        for i in 0..total {
            if i < first_visible || lines.len() >= visible {
                // 顶部/底部截断提示
                if i == first_visible && first_visible > 0 && lines.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "  ↑ …",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                continue;
            }
            let is_selected = i == selected;
            let sel_style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::Reset)
            };
            let dim = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let selector = if is_selected { "▸ " } else { "  " };

            let mut spans = vec![Span::styled(selector, dim)];
            if i == 0 {
                spans.push(Span::styled(
                    "全部项目（合并统计）",
                    Style::default()
                        .fg(if is_selected {
                            Color::Black
                        } else {
                            Color::White
                        })
                        .bg(if is_selected {
                            Color::Cyan
                        } else {
                            Color::Reset
                        })
                        .add_modifier(Modifier::BOLD),
                ));
                if is_selected {
                    spans.push(Span::styled(" ".repeat(width), sel_style));
                }
            } else {
                let p = &self.projects[i - 1];
                let name = tail_dir(&p.work_dir);
                let date = p.last_active.get(0..10).unwrap_or("").to_string();
                spans.push(Span::styled(
                    format!("{}  ", pad_right(&truncate_str(&name, 24), 24)),
                    sel_style,
                ));
                spans.push(Span::styled(
                    format!("{date}  {:>3}会话  ", p.sessions),
                    dim,
                ));
                spans.push(Span::styled(
                    truncate_str(&p.work_dir, width.saturating_sub(46)),
                    dim,
                ));
                if is_selected {
                    spans.push(Span::styled(" ".repeat(width), sel_style));
                }
            }
            lines.push(Line::from(spans));
        }
        if first_visible + visible < total {
            lines.push(Line::from(Span::styled(
                "  ↓ …",
                Style::default().fg(Color::DarkGray),
            )));
        }
        Paragraph::new(lines).render(content_area, buf);

        let help = " ↑↓/PgUp/PgDn 移动  Enter 确认  Esc 取消";
        Paragraph::new(Line::from(Span::styled(
            format!(" {} ", help),
            Style::default().fg(Color::DarkGray),
        )))
        .render(help_area, buf);
    }
}

/// 按模型/项目行共享的“命中”列段：Some = `命中 xx.x%`，None = 等宽空串占位，
/// 两行共用同一 format 模板，保证 out 列严格对齐。
fn hit_col(hit: Option<f64>) -> String {
    match hit {
        Some(h) => format!("命中 {:>5.1}%", h),
        None => " ".repeat(UnicodeWidthStr::width("命中 100.0%")),
    }
}

/// 按显示宽度右对齐填充（数字列）
fn pad_display(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat(width - w), s)
    }
}

/// 按显示宽度左对齐填充（名称列）
fn pad_right(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - w))
    }
}

/// 取路径最后一段作为项目名
fn tail_dir(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn format_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// 按显示宽度截断（CJK 占 2 列）：超出预算时截到 ≤ max_len-1 宽再补 `…`，
/// 保证结果宽度 ≤ max_len，与 pad_right/pad_display 的宽度语义一致。
fn truncate_str(s: &str, max_len: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_len {
        return s.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        // 预留 1 列给省略号
        if width + w > max_len.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += w;
    }
    if max_len > 0 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ascii() {
        assert_eq!(truncate_str("abc", 5), "abc");
        assert_eq!(truncate_str("abcdefgh", 5), "abcd…");
        assert_eq!(truncate_str("", 3), "");
        assert_eq!(truncate_str("abc", 0), "");
    }

    #[test]
    fn truncate_cjk_by_display_width() {
        // 9 个 CJK 字符 = 18 显示宽 > 预算 18？恰好等于，不截断
        assert_eq!(truncate_str("一二三四五六七八九", 18), "一二三四五六七八九");
        // 10 字符 = 20 宽 > 18：截 8 字符（16 宽）+ …（1）= 17 ≤ 18
        assert_eq!(
            truncate_str("一二三四五六七八九十", 18),
            "一二三四五六七八…"
        );
        // 奇数预算：截 8 字符 + … = 17
        assert_eq!(
            truncate_str("一二三四五六七八九十", 17),
            "一二三四五六七八…"
        );
        // 混合：ab(2) + 一(2) = 4 > 3，在 CJK 前断开
        assert_eq!(truncate_str("ab一二", 4), "ab…");
    }

    #[test]
    fn truncated_result_fits_pad_budget() {
        // 截断结果宽度必须 ≤ 预算，pad_right 才能正确补齐对齐列
        let long = "项目名".repeat(10);
        let t = truncate_str(&long, 18);
        assert!(UnicodeWidthStr::width(t.as_str()) <= 18);
    }
}
