use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use crate::tui::event::AppEvent;

impl App {
    /// skill 查看器的事件处理：↑/↓ 移动选中，Esc 返回聊天。
    pub(in crate::tui::app) fn handle_skills_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::Skills { selected_index } = &mut self.state else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };
        let total = self.skills.len();
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Chat;
            }
            KeyCode::Up => {
                if *selected_index > 0 {
                    *selected_index -= 1;
                }
            }
            KeyCode::Down => {
                if total > 0 && *selected_index + 1 < total {
                    *selected_index += 1;
                }
            }
            KeyCode::Enter if total > 0 => {
                self.state = AppState::SkillDetail {
                    skill_index: *selected_index,
                    scroll_offset: 0,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// skill 详情面板的事件处理：Esc 返回列表。
    pub(in crate::tui::app) fn handle_skill_detail_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::SkillDetail {
            skill_index,
            scroll_offset,
        } = &mut self.state
        else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };
        let total = self.skills.len();

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Skills {
                    selected_index: *skill_index,
                };
            }
            KeyCode::Up => {
                if *skill_index > 0 {
                    *skill_index -= 1;
                    *scroll_offset = 0;
                }
            }
            KeyCode::Down => {
                if total > 0 && *skill_index + 1 < total {
                    *skill_index += 1;
                    *scroll_offset = 0;
                }
            }
            KeyCode::PageUp => {
                *scroll_offset = scroll_offset.saturating_sub(10);
            }
            KeyCode::PageDown => {
                *scroll_offset = scroll_offset.saturating_add(10);
            }
            _ => {}
        }
        Ok(())
    }
}
