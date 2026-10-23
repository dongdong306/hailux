use color_eyre::Result;
use crossterm::event::{KeyCode, KeyModifiers};

use super::App;
use super::AppState;
use super::types::PickerAction;
use crate::config::ModelEntry;
use crate::storage::SessionSummary;
use crate::tui::event::AppEvent;

mod mcp;
mod model;
mod setup;
mod skills;
mod stats;
mod tasks;

impl App {
    pub(super) async fn handle_picker_event_inner(&mut self, event: AppEvent) -> Result<()> {
        let action = {
            let AppState::SessionPicker {
                sessions,
                selected_index,
                search_query,
                filtered_indices,
            } = &mut self.state
            else {
                return Ok(());
            };

            match event {
                AppEvent::InputKey(key) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        match key.code {
                            KeyCode::Char('c') => return Ok(()),
                            KeyCode::Char('n') => PickerAction::NewSession,
                            KeyCode::Char('d') => {
                                if !filtered_indices.is_empty()
                                    && *selected_index < filtered_indices.len()
                                {
                                    let idx = filtered_indices[*selected_index];
                                    if idx < sessions.len() {
                                        let session_id = sessions[idx].id.clone();
                                        self.storage.delete_session(&session_id).await?;
                                        let work_dir = Self::current_work_dir()?;
                                        *sessions =
                                            self.storage.list_top_level_sessions(&work_dir).await?;
                                        *filtered_indices =
                                            Self::filter_sessions(sessions, search_query);
                                        if *selected_index >= filtered_indices.len()
                                            && *selected_index > 0
                                        {
                                            *selected_index -= 1;
                                        }
                                    }
                                }
                                return Ok(());
                            }
                            _ => return Ok(()),
                        }
                    } else {
                        match key.code {
                            KeyCode::Esc => PickerAction::Close,
                            KeyCode::Up => {
                                if *selected_index > 0 {
                                    *selected_index -= 1;
                                }
                                PickerAction::None
                            }
                            KeyCode::Down => {
                                if *selected_index + 1 < filtered_indices.len() {
                                    *selected_index += 1;
                                }
                                PickerAction::None
                            }
                            KeyCode::Enter => {
                                if !filtered_indices.is_empty()
                                    && *selected_index < filtered_indices.len()
                                {
                                    let idx = filtered_indices[*selected_index];
                                    if idx < sessions.len() {
                                        PickerAction::Switch(sessions[idx].id.clone())
                                    } else {
                                        PickerAction::None
                                    }
                                } else {
                                    PickerAction::None
                                }
                            }
                            KeyCode::Backspace => {
                                if !search_query.is_empty() {
                                    search_query.pop();
                                    *filtered_indices =
                                        Self::filter_sessions(sessions, search_query);
                                    *selected_index = 0;
                                }
                                PickerAction::None
                            }
                            KeyCode::Char(c) => {
                                search_query.push(c);
                                *filtered_indices = Self::filter_sessions(sessions, search_query);
                                *selected_index = 0;
                                PickerAction::None
                            }
                            _ => PickerAction::None,
                        }
                    }
                }
                _ => PickerAction::None,
            }
        };

        match action {
            PickerAction::Close => {
                self.state = AppState::Chat;
            }
            PickerAction::Switch(session_id) => {
                self.switch_to_session(&session_id).await?;
            }
            PickerAction::NewSession => {
                self.create_new_session().await?;
            }
            PickerAction::None => {}
        }
        Ok(())
    }

    pub(super) fn filter_sessions(sessions: &[SessionSummary], query: &str) -> Vec<usize> {
        if query.is_empty() {
            (0..sessions.len()).collect()
        } else {
            let query_lower = query.to_lowercase();
            sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    s.title.to_lowercase().contains(&query_lower)
                        || s.model.to_lowercase().contains(&query_lower)
                })
                .map(|(i, _)| i)
                .collect()
        }
    }

    pub(super) fn open_model_picker(&mut self) {
        let models = self.config.available_models();
        if models.is_empty() {
            self.enter_setup();
            return;
        }
        // 默认选中当前模型
        let selected_index = models
            .iter()
            .position(|m| m.display == self.resolved.display)
            .unwrap_or(0);
        self.state = AppState::ModelPicker {
            models,
            selected_index,
        };
    }

    /// 打开 skill 查看器。始终打开（即使为空，也会显示目录说明）。
    pub(super) fn open_skills_viewer(&mut self) {
        self.state = AppState::Skills { selected_index: 0 };
    }

    /// 打开 MCP 服务器面板。
    pub(super) fn open_mcp_viewer(&mut self) {
        self.state = AppState::Mcp { selected_index: 0 };
    }

    /// 打开子代理任务面板。
    pub(super) async fn open_tasks_viewer(&mut self) -> Result<()> {
        let entries = self.build_task_entries().await?;
        self.state = AppState::Tasks {
            selected_index: 0,
            entries,
        };
        Ok(())
    }

    /// 打开 token 用量统计全屏页面（默认全部项目 · 近 30 天）。
    pub(super) async fn open_stats_viewer(&mut self) -> Result<()> {
        let data = self.load_stats_data(None, 30).await?;
        let projects = self.storage.list_work_dirs().await?;
        self.state = AppState::Stats {
            data,
            selected_index: 0,
            work_dir: None,
            projects,
            picker_index: None,
        };
        Ok(())
    }

    pub(super) fn switch_model(&mut self, entry: &ModelEntry) -> Result<()> {
        // 如果 provider 的模型还未写入配置，同步写入
        self.config.ensure_provider_models(&entry.provider_id);

        let resolved = self.config.resolve(&entry.display)?;
        self.agent.switch_model(
            resolved.config.clone(),
            &resolved.model_id,
            &resolved.display,
            resolved.max_tokens,
            resolved.supports_vision,
        );
        self.resolved = resolved;
        self.config.main_model = entry.display.clone();
        self.config.save()?;
        if let Ok(mut shared) = self.shared.config.lock() {
            *shared = self.config.clone();
        }
        self.state = AppState::Chat;
        Ok(())
    }
}
