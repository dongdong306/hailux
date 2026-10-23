use color_eyre::Result;
use crossterm::event::KeyCode;

use super::super::App;
use super::super::AppState;
use crate::config;
use crate::tui::event::AppEvent;
use crate::tui::setup::SetupStep;

impl App {
    pub(in crate::tui::app) fn handle_setup_event(&mut self, event: AppEvent) -> Result<()> {
        let key = match event {
            AppEvent::InputKey(k) => k,
            AppEvent::InputPaste(text) => {
                if let AppState::Setup(ref mut form) = self.state
                    && matches!(
                        form.step,
                        SetupStep::PredefinedInputApiKey
                            | SetupStep::CustomInputProviderName
                            | SetupStep::CustomInputBaseUrl
                            | SetupStep::CustomInputApiKey
                            | SetupStep::CustomInputModelName
                            | SetupStep::CustomInputContextWindow
                    )
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
            let mut form = match std::mem::replace(&mut self.state, AppState::Chat) {
                AppState::Setup(f) => f,
                other => {
                    self.state = other;
                    return Ok(());
                }
            };
            form.error_msg.clear();
            match form.step {
                SetupStep::Welcome => {
                    self.should_quit = true;
                }
                SetupStep::SelectProvider => {
                    form.step = SetupStep::Welcome;
                    form.buffer.clear();
                    form.cursor = 0;
                }
                SetupStep::PredefinedInputApiKey => {
                    form.step = SetupStep::SelectProvider;
                    form.selected_index = form.provider_index;
                    form.buffer.clear();
                    form.cursor = 0;
                }
                SetupStep::PredefinedSelectModel => {
                    form.step = SetupStep::PredefinedInputApiKey;
                    form.buffer = form.api_key.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::CustomInputProviderName => {
                    form.step = SetupStep::SelectProvider;
                    form.selected_index = config::PROVIDERS.len();
                    form.buffer.clear();
                    form.cursor = 0;
                }
                SetupStep::CustomInputBaseUrl => {
                    form.step = SetupStep::CustomInputProviderName;
                    form.buffer = form.provider_id.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::CustomInputApiKey => {
                    form.step = SetupStep::CustomInputBaseUrl;
                    form.buffer = form.base_url.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::CustomInputModelName => {
                    form.step = SetupStep::CustomInputApiKey;
                    form.buffer = form.api_key.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::CustomInputContextWindow => {
                    form.step = SetupStep::CustomInputModelName;
                    form.buffer = form.model_id.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::Done => {
                    form.step = SetupStep::SelectProvider;
                    form.selected_index = if form.is_custom {
                        config::PROVIDERS.len()
                    } else {
                        form.provider_index
                    };
                }
            }
            self.state = AppState::Setup(form);
            return Ok(());
        }

        let mut form = match std::mem::replace(&mut self.state, AppState::Chat) {
            AppState::Setup(f) => f,
            other => {
                self.state = other;
                return Ok(());
            }
        };

        form.error_msg.clear();

        match form.step {
            SetupStep::Welcome => {
                if key.code == KeyCode::Enter {
                    form.step = SetupStep::SelectProvider;
                    form.selected_index = 0;
                }
                self.state = AppState::Setup(form);
            }
            SetupStep::SelectProvider => {
                let total = config::PROVIDERS.len() + 1;
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
                        if form.selected_index < config::PROVIDERS.len() {
                            form.provider_index = form.selected_index;
                            form.is_custom = false;
                            form.step = SetupStep::PredefinedInputApiKey;
                        } else {
                            form.is_custom = true;
                            form.step = SetupStep::CustomInputProviderName;
                        }
                        form.buffer.clear();
                        form.cursor = 0;
                    }
                    _ => {}
                }
                self.state = AppState::Setup(form);
            }
            SetupStep::PredefinedSelectModel => {
                let provider = &config::PROVIDERS[form.provider_index];
                let total = provider.models.len();
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
                        let m = &provider.models[form.selected_index];
                        form.model_id = m.id.to_string();
                        form.step = SetupStep::Done;
                    }
                    _ => {}
                }
                self.state = AppState::Setup(form);
            }
            SetupStep::Done => {
                if key.code == KeyCode::Enter {
                    let cfg = match form.build_config() {
                        Ok(c) => c,
                        Err(e) => {
                            form.error_msg = format!("{}", e);
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                    };
                    if form.append_only {
                        // 从模型选择器进入：追加 provider 到现有 config
                        let provider_id = cfg
                            .main_model
                            .split_once('/')
                            .map(|(p, _)| p.to_string())
                            .unwrap_or_default();
                        let model_display = cfg.main_model.clone();
                        if let Some(entry) = cfg.providers.get(&provider_id) {
                            self.config
                                .add_predefined_provider(&provider_id, &entry.api_key);
                        }
                        self.config.main_model = model_display.clone();
                        if let Err(e) = self.config.save() {
                            form.error_msg = format!("保存失败: {}", e);
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                        // 同步到共享配置
                        if let Ok(mut shared) = self.shared.config.lock() {
                            *shared = self.config.clone();
                        }
                        match self.config.resolve(&model_display) {
                            Ok(resolved) => {
                                self.agent.switch_model(
                                    resolved.config.clone(),
                                    &resolved.model_id,
                                    &resolved.display,
                                    resolved.max_tokens,
                                    resolved.supports_vision,
                                );
                                self.resolved = resolved;
                                self.state = AppState::Chat;
                                return Ok(());
                            }
                            Err(e) => {
                                form.error_msg = format!("解析模型失败: {}", e);
                                self.state = AppState::Setup(form);
                                return Ok(());
                            }
                        }
                    }
                    if let Err(e) = cfg.save() {
                        form.error_msg = format!("保存失败: {}", e);
                        self.state = AppState::Setup(form);
                        return Ok(());
                    }
                    match cfg.resolve_default() {
                        Ok(resolved) => {
                            self.agent.switch_model(
                                resolved.config.clone(),
                                &resolved.model_id,
                                &resolved.display,
                                resolved.max_tokens,
                                resolved.supports_vision,
                            );
                            self.resolved = resolved;
                            self.config = cfg;
                            // 同步到共享配置
                            if let Ok(mut shared) = self.shared.config.lock() {
                                *shared = self.config.clone();
                            }
                            // state 已为 Chat（mem::replace 设置），保持
                            return Ok(());
                        }
                        Err(e) => {
                            form.error_msg = format!("解析模型失败: {}", e);
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                    }
                }
                self.state = AppState::Setup(form);
            }
            _ => {
                // 文本输入步骤
                let is_context_window = form.step == SetupStep::CustomInputContextWindow;
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
                            let next = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                            form.cursor = next;
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
                        if form.buffer.is_empty() && !is_context_window {
                            form.error_msg = match form.step {
                                SetupStep::PredefinedInputApiKey | SetupStep::CustomInputApiKey => {
                                    "API Key 不能为空".to_string()
                                }
                                SetupStep::CustomInputProviderName => {
                                    "服务商名称不能为空".to_string()
                                }
                                SetupStep::CustomInputBaseUrl => "API 地址不能为空".to_string(),
                                SetupStep::CustomInputModelName => "模型名称不能为空".to_string(),
                                _ => String::new(),
                            };
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                        if is_context_window
                            && !form.buffer.is_empty()
                            && form.buffer.parse::<u32>().is_err()
                        {
                            form.error_msg = "请输入有效的数字".to_string();
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                        match form.step {
                            SetupStep::PredefinedInputApiKey => {
                                form.api_key = form.buffer.clone();
                                form.step = SetupStep::PredefinedSelectModel;
                                form.selected_index = 0;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::CustomInputProviderName => {
                                form.provider_id = form.buffer.clone();
                                form.step = SetupStep::CustomInputBaseUrl;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::CustomInputBaseUrl => {
                                form.base_url = form.buffer.clone();
                                form.step = SetupStep::CustomInputApiKey;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::CustomInputApiKey => {
                                form.api_key = form.buffer.clone();
                                form.step = SetupStep::CustomInputModelName;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::CustomInputModelName => {
                                form.model_id = form.buffer.clone();
                                form.step = SetupStep::CustomInputContextWindow;
                                form.buffer = "131072".to_string();
                                form.cursor = form.buffer.len();
                            }
                            SetupStep::CustomInputContextWindow => {
                                form.context_window = form.buffer.clone();
                                form.step = SetupStep::Done;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                self.state = AppState::Setup(form);
            }
        }
        Ok(())
    }
}
