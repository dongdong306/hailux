use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use crate::agent::ReasoningEffort;
use crate::tui::event::AppEvent;

const EFFORT_LEVELS: [ReasoningEffort; 3] = [
    ReasoningEffort::Low,
    ReasoningEffort::High,
    ReasoningEffort::Max,
];

impl App {
    pub(in crate::tui::app) fn handle_effort_picker_event(
        &mut self,
        event: AppEvent,
    ) -> Result<()> {
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        let action = {
            let AppState::EffortPicker { selected_index } = &mut self.state else {
                return Ok(());
            };

            match key.code {
                KeyCode::Esc => Some(None),
                KeyCode::Up => {
                    if *selected_index > 0 {
                        *selected_index -= 1;
                    }
                    None
                }
                KeyCode::Down => {
                    if *selected_index + 1 < EFFORT_LEVELS.len() {
                        *selected_index += 1;
                    }
                    None
                }
                KeyCode::Enter => Some(Some(EFFORT_LEVELS[*selected_index])),
                _ => None,
            }
        };

        match action {
            // 未选择，关闭菜单
            Some(None) => self.state = AppState::Chat,
            // 确认选择：回 Chat 后应用（agent + config 持久化 + 共享配置 + 回显）
            Some(Some(effort)) => {
                self.state = AppState::Chat;
                self.apply_effort(effort);
            }
            None => {}
        }
        Ok(())
    }
}
