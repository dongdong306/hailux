use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use super::super::types::ModelPickerAction;
use crate::config::{self, DEFAULT_CONTEXT_WINDOW, DEFAULT_OUTPUT_TOKENS};
use crate::tui::event::AppEvent;
use crate::tui::model_picker::{AddModelForm, AddModelStep};
use crate::tui::setup::{SetupForm, SetupStep};

impl App {
    pub(in crate::tui::app) fn handle_model_picker_event(&mut self, event: AppEvent) -> Result<()> {
        let action = {
            let AppState::ModelPicker {
                models,
                selected_index,
            } = &mut self.state
            else {
                return Ok(());
            };

            // 总行数 = 模型数 + 1（"添加模型..." 项）
            let total_items = models.len() + 1;

            let AppEvent::InputKey(key) = event else {
                return Ok(());
            };

            match key.code {
                KeyCode::Esc => Some(ModelPickerAction::Close),
                KeyCode::Up => {
                    if *selected_index > 0 {
                        *selected_index -= 1;
                    }
                    None
                }
                KeyCode::Down => {
                    if *selected_index + 1 < total_items {
                        *selected_index += 1;
                    }
                    None
                }
                KeyCode::Enter => {
                    if *selected_index < models.len() {
                        Some(ModelPickerAction::Switch(models[*selected_index].clone()))
                    } else {
                        Some(ModelPickerAction::AddModel)
                    }
                }
                _ => None,
            }
        };

        match action {
            Some(ModelPickerAction::Close) => {
                self.state = AppState::Chat;
            }
            Some(ModelPickerAction::Switch(entry)) => {
                if entry.needs_setup {
                    // 未配置的预定义 provider，进入 API Key 设置（不提前修改 config）
                    let mut form = SetupForm::new();
                    form.step = SetupStep::PredefinedInputApiKey;
                    form.provider_index = config::PROVIDERS
                        .iter()
                        .position(|p| p.id == entry.provider_id)
                        .unwrap_or(0);
                    form.provider_id = entry.provider_id.clone();
                    form.append_only = true;
                    self.state = AppState::Setup(form);
                } else {
                    self.switch_model(&entry)?;
                }
            }
            Some(ModelPickerAction::AddModel) => {
                let providers = self.config.configured_providers();
                self.state = AppState::AddModel(AddModelForm::new(providers));
            }
            None => {}
        }
        Ok(())
    }

    pub(in crate::tui::app) fn handle_add_model_event(&mut self, event: AppEvent) -> Result<()> {
        let key = match event {
            AppEvent::InputKey(k) => k,
            AppEvent::InputPaste(text) => {
                if let AppState::AddModel(ref mut form) = self.state
                    && !matches!(form.step, AddModelStep::SelectProvider)
                {
                    let sanitized: String =
                        text.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
                    form.buffer.insert_str(form.cursor, &sanitized);
                    form.cursor += sanitized.len();
                    form.error_msg.clear();
                }
                return Ok(());
            }
            _ => return Ok(()),
        };

        if key.code == KeyCode::Esc {
            self.open_model_picker();
            return Ok(());
        }

        let mut form = match std::mem::replace(&mut self.state, AppState::Chat) {
            AppState::AddModel(f) => f,
            other => {
                self.state = other;
                return Ok(());
            }
        };

        form.error_msg.clear();

        match form.step {
            AddModelStep::SelectProvider => {
                let total = form.provider_options.len() + 1;
                match key.code {
                    KeyCode::Up => {
                        if form.selected_index > 0 {
                            form.selected_index -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if form.selected_index + 1 < total {
                            form.selected_index += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if form.selected_index < form.provider_options.len() {
                            let p = &form.provider_options[form.selected_index];
                            form.provider_id = p.id.clone();
                            form.base_url = p.base_url.clone();
                            form.api_key.clear();
                            form.step = AddModelStep::InputModelName;
                            form.buffer.clear();
                        } else {
                            form.step = AddModelStep::InputProviderName;
                            form.buffer.clear();
                        }
                    }
                    _ => {}
                }
                self.state = AppState::AddModel(form);
            }
            AddModelStep::InputContextWindow => {
                match key.code {
                    KeyCode::Left => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Right => {
                        if form.cursor < form.buffer.len() {
                            form.cursor = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                        }
                    }
                    KeyCode::Home => {
                        form.cursor = 0;
                    }
                    KeyCode::End => {
                        form.cursor = form.buffer.len();
                    }
                    KeyCode::Backspace => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.buffer.drain(prev..form.cursor);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Delete => {
                        if form.cursor < form.buffer.len() {
                            let next = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                            form.buffer.drain(form.cursor..next);
                        }
                    }
                    KeyCode::Enter => {
                        let context_window = if form.buffer.is_empty() {
                            DEFAULT_CONTEXT_WINDOW
                        } else {
                            match form.buffer.parse::<u32>() {
                                Ok(t) => t,
                                Err(_) => {
                                    form.error_msg = "请输入有效的数字".to_string();
                                    self.state = AppState::AddModel(form);
                                    return Ok(());
                                }
                            }
                        };
                        let is_new_provider = !form.api_key.is_empty();
                        self.config.add_custom_model(
                            &form.provider_id,
                            if is_new_provider {
                                Some(form.base_url.as_str())
                            } else {
                                None
                            },
                            if is_new_provider {
                                Some(form.api_key.as_str())
                            } else {
                                None
                            },
                            &form.model_name,
                            DEFAULT_OUTPUT_TOKENS,
                            context_window,
                        );
                        if let Err(e) = self.config.save() {
                            form.error_msg = format!("保存失败: {}", e);
                            self.state = AppState::AddModel(form);
                            return Ok(());
                        }
                        let display = format!("{}/{}", form.provider_id, form.model_name);
                        let resolved = self.config.resolve(&display)?;
                        self.agent.switch_model(
                            resolved.config.clone(),
                            &resolved.model_id,
                            &resolved.display,
                            resolved.max_tokens,
                            resolved.supports_vision,
                        );
                        self.resolved = resolved;
                        self.config.main_model = display;
                        if let Err(e) = self.config.save() {
                            form.error_msg = format!("保存失败: {}", e);
                            self.state = AppState::AddModel(form);
                            return Ok(());
                        }
                        // 同步到共享配置
                        if let Ok(mut shared) = self.shared.config.lock() {
                            *shared = self.config.clone();
                        }
                        // 不需要恢复 AddModel state，已经切到 Chat 了
                        return Ok(());
                    }
                    KeyCode::Char(c) => {
                        form.buffer.insert(form.cursor, c);
                        form.cursor += c.len_utf8();
                    }
                    _ => {}
                }
                self.state = AppState::AddModel(form);
            }
            _ => {
                // InputProviderName, InputBaseUrl, InputApiKey, InputModelName
                match key.code {
                    KeyCode::Left => {
                        if form.cursor > 0 {
                            // 向前找上一个 char 边界
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Right => {
                        if form.cursor < form.buffer.len() {
                            form.cursor = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                        }
                    }
                    KeyCode::Home => {
                        form.cursor = 0;
                    }
                    KeyCode::End => {
                        form.cursor = form.buffer.len();
                    }
                    KeyCode::Backspace => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.buffer.drain(prev..form.cursor);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Delete => {
                        if form.cursor < form.buffer.len() {
                            let next = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                            form.buffer.drain(form.cursor..next);
                        }
                    }
                    KeyCode::Char(c) => {
                        form.buffer.insert(form.cursor, c);
                        form.cursor += c.len_utf8();
                    }
                    KeyCode::Enter => {
                        let valid = match form.step {
                            AddModelStep::InputProviderName if form.buffer.is_empty() => {
                                form.error_msg = "服务商名称不能为空".to_string();
                                false
                            }
                            AddModelStep::InputBaseUrl if form.buffer.is_empty() => {
                                form.error_msg = "API 地址不能为空".to_string();
                                false
                            }
                            AddModelStep::InputApiKey if form.buffer.is_empty() => {
                                form.error_msg = "API Key 不能为空".to_string();
                                false
                            }
                            AddModelStep::InputModelName if form.buffer.is_empty() => {
                                form.error_msg = "模型名称不能为空".to_string();
                                false
                            }
                            _ => true,
                        };
                        if valid {
                            match form.step {
                                AddModelStep::InputProviderName => {
                                    form.provider_id = form.buffer.clone();
                                    form.step = AddModelStep::InputBaseUrl;
                                    form.buffer.clear();
                                    form.cursor = 0;
                                }
                                AddModelStep::InputBaseUrl => {
                                    form.base_url = form.buffer.clone();
                                    form.step = AddModelStep::InputApiKey;
                                    form.buffer.clear();
                                    form.cursor = 0;
                                }
                                AddModelStep::InputApiKey => {
                                    form.api_key = form.buffer.clone();
                                    form.step = AddModelStep::InputModelName;
                                    form.buffer.clear();
                                    form.cursor = 0;
                                }
                                AddModelStep::InputModelName => {
                                    form.model_name = form.buffer.clone();
                                    form.step = AddModelStep::InputContextWindow;
                                    form.buffer = "131072".to_string();
                                    form.cursor = form.buffer.len();
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                self.state = AppState::AddModel(form);
            }
        }
        Ok(())
    }
}
