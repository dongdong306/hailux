use ratatui::buffer::CellDiffOption;
use ratatui::prelude::*;
use std::collections::HashSet;

use super::{App, AppState};
use crate::tui::ask_user;
use crate::tui::chat_widget::ChatWidget;
use crate::tui::history_cell::{self, HistoryCell, SessionHeaderCell, TooltipCell};
use crate::tui::mcp_viewer::{self, render_mcp_viewer};
use crate::tui::model_picker::{render_add_model, render_model_picker};
use crate::tui::session_picker::SessionPicker;
use crate::tui::setup::render_setup;
use crate::tui::skills_viewer::{self, render_skills_viewer};
use crate::tui::tasks_viewer::{self, TasksViewer};

impl App {
    pub(super) fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let min_width: u16 = 30;

        if area.width < min_width || area.height < 5 {
            let msg = if area.width < min_width {
                "窗口太窄，请调整至更宽"
            } else {
                "窗口太矮，请调整至更高"
            };
            let col = area.x + area.width.saturating_sub(msg.len() as u16) / 2;
            let row = area.y + area.height / 2;
            if col < area.width && row < area.height {
                frame
                    .buffer_mut()
                    .set_string(col, row, msg, Style::default().fg(Color::Yellow));
            }
            return;
        }

        if let AppState::Setup(ref form) = self.state {
            let cursor_pos = render_setup(area, frame.buffer_mut(), form);
            if let Some(pos) = cursor_pos {
                frame.set_cursor_position(pos);
            }
            return;
        }

        let input_text = self.input.text().to_string();
        self.input.update_area_width(area.width.saturating_sub(2));
        let (total_visual_rows, cursor_visual_row, cursor_visual_col) =
            self.input.compute_visual_info();

        let visible_input_rows: u16 = total_visual_rows.clamp(1, 6);
        let input_area_height: u16 = visible_input_rows + 2;

        let scroll_row = self.input.input_scroll_row();
        let new_scroll = if cursor_visual_row < scroll_row {
            cursor_visual_row
        } else if cursor_visual_row >= scroll_row + visible_input_rows {
            cursor_visual_row - visible_input_rows + 1
        } else {
            scroll_row
        };
        let max_input_scroll = total_visual_rows.saturating_sub(visible_input_rows);
        self.input
            .set_input_scroll_row(new_scroll.min(max_input_scroll));

        if self.render.dirty {
            let mut cells: Vec<Box<dyn HistoryCell>> = Vec::new();
            cells.push(Box::new(SessionHeaderCell {
                model_name: self.resolved.display.clone(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                directory: self.work_dir.clone(),
            }));
            cells.extend(history_cell::messages_to_cells(
                &self.messages,
                self.thinking_collapsed,
            ));
            if self.messages.is_empty() {
                cells.push(Box::new(TooltipCell {
                    text: "输入消息开始对话".to_string(),
                }));
            }
            let active_keys: HashSet<u64> = cells.iter().map(|c| c.cache_key()).collect();
            if self.render.cache.len() > active_keys.len() * 2 {
                self.render.cache.retain_keys(&active_keys);
            }
            self.render.cells = cells;
            self.render.dirty = false;
        }

        let widget = ChatWidget {
            messages: &self.messages,
            cells: &self.render.cells,
            input_buffer: &input_text,
            input_elements: self.input.element_info(),
            scroll_offset: self.scroll_offset,
            is_processing: self.is_processing,
            model_name: &self.resolved.display,
            input_scroll_row: self.input.input_scroll_row(),
            input_area_height,
            directory: &self.work_dir,
            plan_mode: self.plan_mode,
            yolo_mode: self.agent.permission().mode() == crate::permission::PermissionMode::Yolo,
            show_suggestions: self.cmd_suggestion.show,
            command_suggestions: &self.cmd_suggestion.items,
            selected_suggestion: self.cmd_suggestion.selected,
            esc_hint_active: self.esc_hint_active,
            context_tokens: self.context_prompt_tokens + self.context_completion_tokens,
            max_context_tokens: self.resolved.context_window,
            spinner_frame: self.spinner.frame,
            show_file_picker: self.file_picker.active,
            file_picker_results: &self.file_picker.results,
            file_picker_selected: self.file_picker.selected,
            user_msg_sent_at: self.timing.user_msg_sent_at,
            render_cache: &mut self.render.cache,
        };
        let result = widget.render(area, frame.buffer_mut());
        if self.scroll_offset > result.max_hide {
            self.scroll_offset = result.max_hide;
        }

        let show_cursor = match &self.state {
            AppState::Chat => !self.is_processing,
            AppState::SessionPicker { .. }
            | AppState::ModelPicker { .. }
            | AppState::Skills { .. }
            | AppState::SkillDetail { .. }
            | AppState::Mcp { .. }
            | AppState::McpDetail { .. }
            | AppState::McpItemDetail { .. }
            | AppState::Tasks { .. }
            | AppState::TaskDetail { .. }
            | AppState::Setup(_) => false,
            AppState::Permission { .. } => false,
            AppState::AskUser { editing_custom, .. } => *editing_custom,
            AppState::AddModel(_) => true,
        };

        if show_cursor {
            let gap_height: u16 = 0;
            let status_height: u16 = 1;
            let input_content_y = area.y
                + area
                    .height
                    .saturating_sub(input_area_height + gap_height + status_height)
                + 1;

            let display_row = cursor_visual_row.saturating_sub(self.input.input_scroll_row());
            let display_col = cursor_visual_col.min(area.width.saturating_sub(3));

            frame.set_cursor_position((
                area.x + 1 + display_col,
                input_content_y + display_row.min(visible_input_rows - 1),
            ));
        }

        if let AppState::SessionPicker {
            sessions,
            selected_index,
            search_query,
            filtered_indices,
        } = &self.state
        {
            let picker = SessionPicker {
                sessions,
                filtered_indices,
                selected_index: *selected_index,
                search_query,
                current_session_id: self.current_session_id.as_deref(),
            };
            picker.render(area, frame.buffer_mut());
        }

        if let AppState::ModelPicker {
            models,
            selected_index,
        } = &self.state
        {
            render_model_picker(
                area,
                frame.buffer_mut(),
                models,
                *selected_index,
                &self.resolved.display,
            );
        }

        if let AppState::AddModel(ref form) = self.state {
            let cursor_pos = render_add_model(area, frame.buffer_mut(), form);
            frame.set_cursor_position(cursor_pos);
        }

        if let AppState::Skills { selected_index } = &self.state {
            render_skills_viewer(
                area,
                frame.buffer_mut(),
                &self.skills,
                *selected_index,
                &self.home_dir,
            );
        }

        if let AppState::SkillDetail {
            skill_index,
            scroll_offset,
        } = &self.state
        {
            skills_viewer::render_skill_detail(
                area,
                frame.buffer_mut(),
                &self.skills,
                *skill_index,
                *scroll_offset,
                &self.home_dir,
            );
        }

        if let AppState::Mcp { selected_index } = &self.state {
            render_mcp_viewer(area, frame.buffer_mut(), &self.mcp_servers, *selected_index);
        }

        if let AppState::McpDetail {
            server_index,
            selected_index,
        } = &self.state
        {
            mcp_viewer::render_mcp_detail(
                area,
                frame.buffer_mut(),
                &self.mcp_servers,
                *server_index,
                *selected_index,
                &self.shared.mcp_backends,
            );
        }

        if let AppState::McpItemDetail {
            server_index,
            item_index,
            scroll_offset,
        } = &self.state
        {
            mcp_viewer::render_mcp_item_detail(
                area,
                frame.buffer_mut(),
                &self.mcp_servers,
                *server_index,
                *item_index,
                *scroll_offset,
                &self.shared.mcp_backends,
            );
        }

        if let AppState::Tasks {
            selected_index,
            entries,
        } = &self.state
        {
            let viewer = TasksViewer {
                entries,
                selected_index: *selected_index,
            };
            viewer.render(area, frame.buffer_mut());
        }

        if let AppState::TaskDetail {
            task_index,
            scroll_offset,
            messages,
            entries,
        } = &mut self.state
        {
            tasks_viewer::render_task_detail(
                area,
                frame.buffer_mut(),
                entries,
                *task_index,
                messages,
                scroll_offset,
            );
        }

        if let AppState::AskUser {
            questions,
            current_tab,
            selected,
            answers,
            custom_inputs,
            custom_cursor,
            editing_custom,
            ..
        } = &self.state
        {
            let cursor_pos = ask_user::render_ask_user(
                area,
                frame.buffer_mut(),
                questions,
                *current_tab,
                *selected,
                answers,
                custom_inputs,
                *custom_cursor,
                *editing_custom,
            );
            if let Some(pos) = cursor_pos {
                frame.set_cursor_position(pos);
            }
        }

        if let AppState::Permission {
            pending, selected, ..
        } = &self.state
            && let Some(first) = pending.first()
        {
            crate::tui::permission_dialog::render_permission_dialog(
                area,
                frame.buffer_mut(),
                &first.request,
                *selected,
                first.subagent_name.as_deref(),
            );
        }

        // JediTerm 下 ratatui 的增量 diff 会导致渲染错位，强制全量重绘
        if self.is_jediterm {
            for cell in frame.buffer_mut().content.iter_mut() {
                cell.diff_option = CellDiffOption::AlwaysUpdate;
            }
        }
    }
}
