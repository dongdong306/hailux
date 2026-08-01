use color_eyre::Result;
use crossterm::event::{KeyCode, KeyModifiers};

use super::{App, AppState};
use super::types::{ModelPickerAction, PickerAction};
use super::{DEFAULT_CONTEXT_WINDOW, DEFAULT_OUTPUT_TOKENS};
use crate::tui::event::AppEvent;
use crate::tui::model_picker::{AddModelForm, AddModelStep};
use crate::tui::setup::{SetupForm, SetupStep};
use crate::tui::tasks_viewer::{TaskEntry, TaskRunStatus};
use crate::config::{self, ModelEntry};
use crate::mcp::{McpConnection, McpToolBackend};
use crate::storage::SessionSummary;

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
                                        *sessions = self.storage.list_sessions(&work_dir).await?;
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

    pub(super) fn handle_model_picker_event(&mut self, event: AppEvent) -> Result<()> {
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

    pub(super) fn handle_add_model_event(&mut self, event: AppEvent) -> Result<()> {
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
                            resolved.max_tokens,
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

    pub(super) fn handle_setup_event(&mut self, event: AppEvent) -> Result<()> {
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
                                    resolved.max_tokens,
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
                                resolved.max_tokens,
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

    /// 合并内存 task_records 与数据库 subsession 记录，构建 TaskEntry 列表。
    pub(super) async fn build_task_entries(&self) -> Result<Vec<TaskEntry>> {
        let session_id = match &self.current_session_id {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };

        let subsessions = self.storage.list_subsessions(session_id).await?;

        // 以 task_records 为主，匹配 subsession
        let mut entries: Vec<TaskEntry> = Vec::new();

        for record in &self.tasks.records {
            let subsession = subsessions
                .iter()
                .find(|s| s.id == record.session_id)
                .cloned();
            entries.push(TaskEntry {
                record: Some(record.clone()),
                subsession,
                subagent_name: record.subagent_name.clone(),
                description: record.description.clone(),
                status: record.status,
            });
        }

        // 补充存在于数据库但不在内存 task_records 中的 subsession（如历史会话恢复后）
        for sub in &subsessions {
            let already = entries
                .iter()
                .any(|e| e.record.as_ref().is_some_and(|r| r.session_id == sub.id));
            if !already {
                // 从 subsession 的 title 解析 subagent 名称和 description
                // title 格式: "subagent_name|description" 或仅 "subagent_name"
                let (name, desc) = if sub.title.contains('|') {
                    let mut parts = sub.title.splitn(2, '|');
                    let n = parts.next().unwrap_or("subagent").to_string();
                    let d = parts.next().unwrap_or("").to_string();
                    (n, d)
                } else if !sub.title.is_empty() {
                    (sub.title.clone(), String::new())
                } else {
                    ("subagent".to_string(), String::new())
                };

                entries.push(TaskEntry {
                    record: None,
                    subsession: Some(sub.clone()),
                    subagent_name: name,
                    description: desc,
                    status: TaskRunStatus::Completed,
                });
            }
        }

        Ok(entries)
    }

    /// Tasks 面板的事件处理：↑/↓ 移动选中，Enter 查看详情，Esc 返回聊天。
    pub(super) async fn handle_tasks_event(&mut self, event: AppEvent) -> Result<()> {
        let (selected_index, entries_len) = {
            let AppState::Tasks {
                selected_index,
                entries,
            } = &self.state
            else {
                return Ok(());
            };
            (*selected_index, entries.len())
        };

        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Chat;
            }
            KeyCode::Up => {
                if selected_index > 0
                    && let AppState::Tasks { selected_index, .. } = &mut self.state
                {
                    *selected_index -= 1;
                }
            }
            KeyCode::Down => {
                if entries_len > 0
                    && selected_index + 1 < entries_len
                    && let AppState::Tasks { selected_index, .. } = &mut self.state
                {
                    *selected_index += 1;
                }
            }
            KeyCode::Enter if entries_len > 0 => {
                let (entry, idx, entries) = {
                    let AppState::Tasks {
                        entries,
                        selected_index,
                    } = &self.state
                    else {
                        return Ok(());
                    };
                    (
                        entries[*selected_index].clone(),
                        *selected_index,
                        entries.clone(),
                    )
                };

                let session_id = entry
                    .subsession
                    .as_ref()
                    .map(|s| s.id.clone())
                    .or_else(|| entry.record.as_ref().map(|r| r.session_id.clone()));

                let messages = if let Some(ref sid) = session_id {
                    self.storage.load_messages(sid).await.unwrap_or_default()
                } else {
                    Vec::new()
                };

                self.state = AppState::TaskDetail {
                    task_index: idx,
                    scroll_offset: usize::MAX,
                    messages,
                    entries,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// Task 详情面板的事件处理：↑/↓/PgUp/PgDn 滚动，Esc 返回列表。
    pub(super) async fn handle_task_detail_event(&mut self, event: AppEvent) -> Result<()> {
        let task_index = {
            let AppState::TaskDetail { task_index, .. } = &self.state else {
                return Ok(());
            };
            *task_index
        };

        // 鼠标滚轮事件
        match event {
            AppEvent::ScrollUp => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_sub(3);
                }
                return Ok(());
            }
            AppEvent::ScrollDown => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_add(3);
                }
                return Ok(());
            }
            _ => {}
        }

        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        match key.code {
            KeyCode::Esc => {
                let entries = self.build_task_entries().await?;
                let safe_index = task_index.min(entries.len().saturating_sub(1));
                self.state = AppState::Tasks {
                    selected_index: safe_index,
                    entries,
                };
            }
            KeyCode::Up => {
                if task_index > 0 {
                    let new_idx = task_index - 1;
                    let entries = self.build_task_entries().await?;
                    let session_id = entries.get(new_idx).and_then(|e| {
                        e.subsession
                            .as_ref()
                            .map(|s| s.id.clone())
                            .or_else(|| e.record.as_ref().map(|r| r.session_id.clone()))
                    });
                    let messages = if let Some(ref sid) = session_id {
                        self.storage.load_messages(sid).await.unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    self.state = AppState::TaskDetail {
                        task_index: new_idx,
                        scroll_offset: 0,
                        messages,
                        entries,
                    };
                }
            }
            KeyCode::Down => {
                let entries = self.build_task_entries().await?;
                let total = entries.len();
                if total > 0 && task_index + 1 < total {
                    let new_idx = task_index + 1;
                    let session_id = entries.get(new_idx).and_then(|e| {
                        e.subsession
                            .as_ref()
                            .map(|s| s.id.clone())
                            .or_else(|| e.record.as_ref().map(|r| r.session_id.clone()))
                    });
                    let messages = if let Some(ref sid) = session_id {
                        self.storage.load_messages(sid).await.unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    self.state = AppState::TaskDetail {
                        task_index: new_idx,
                        scroll_offset: 0,
                        messages,
                        entries,
                    };
                }
            }
            KeyCode::PageUp => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_add(10);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// MCP 面板的事件处理：↑/↓ 移动选中，Enter 查看详情，Esc 返回聊天。
    pub(super) fn handle_mcp_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::Mcp { selected_index } = &mut self.state else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };
        let total = self.mcp_servers.len();
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
            KeyCode::Enter if total > 0 && self.mcp_servers[*selected_index].connected => {
                let idx = *selected_index;
                self.state = AppState::McpDetail {
                    server_index: idx,
                    selected_index: 0,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// MCP 详情面板的事件处理：↑/↓ 移动选中，Esc 返回列表。
    pub(super) fn handle_mcp_detail_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::McpDetail {
            server_index,
            selected_index,
        } = &mut self.state
        else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        let server = match self.mcp_servers.get(*server_index) {
            Some(s) => s,
            None => {
                self.state = AppState::Mcp {
                    selected_index: *server_index,
                };
                return Ok(());
            }
        };

        let tools_len = server.tools.len();
        let resources_len = server.resources.len();
        let prompts_len = server.prompts.len();
        let total = tools_len + resources_len + prompts_len;

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Mcp {
                    selected_index: *server_index,
                };
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
                let si = *server_index;
                let ii = *selected_index;
                self.state = AppState::McpItemDetail {
                    server_index: si,
                    item_index: ii,
                    scroll_offset: 0,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// MCP 单项详情面板的事件处理：Esc 返回列表。
    pub(super) fn handle_mcp_item_detail_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::McpItemDetail {
            server_index,
            item_index,
            scroll_offset,
        } = &mut self.state
        else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        let server = match self.mcp_servers.get(*server_index) {
            Some(s) => s,
            None => {
                self.state = AppState::McpDetail {
                    server_index: *server_index,
                    selected_index: *item_index,
                };
                return Ok(());
            }
        };

        let total = server.tools.len() + server.resources.len() + server.prompts.len();

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::McpDetail {
                    server_index: *server_index,
                    selected_index: *item_index,
                };
            }
            KeyCode::Up => {
                if *item_index > 0 {
                    *item_index -= 1;
                    *scroll_offset = 0;
                }
            }
            KeyCode::Down => {
                if total > 0 && *item_index + 1 < total {
                    *item_index += 1;
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

    /// 处理后台 MCP 连接完成事件：更新 UI 状态，注册工具给 agent。
    pub(super) async fn handle_mcp_ready(&mut self, connections: Vec<McpConnection>) -> Result<()> {
        self.mcp_servers = connections.iter().map(|c| c.status.clone()).collect();
        let mut backends = self
            .shared
            .mcp_backends
            .lock()
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        backends.clear();
        for conn in &connections {
            if let Some(backend) = &conn.backend {
                let server_name = conn.status.name.clone();
                let backend_arc = backend.clone();
                let tools = conn.tools.clone();
                backends.push(McpToolBackend {
                    server_name,
                    backend: backend_arc,
                    tools,
                });
                for tool in &conn.tools {
                    self.agent.register_tool(Box::new(crate::mcp::McpTool::new(
                        &conn.status.name,
                        tool,
                        backend.clone(),
                    )));
                }
            }
        }
        Ok(())
    }

    /// skill 查看器的事件处理：↑/↓ 移动选中，Esc 返回聊天。
    pub(super) fn handle_skills_event(&mut self, event: AppEvent) -> Result<()> {
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
    pub(super) fn handle_skill_detail_event(&mut self, event: AppEvent) -> Result<()> {
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

    pub(super) fn switch_model(&mut self, entry: &ModelEntry) -> Result<()> {
        // 如果 provider 的模型还未写入配置，同步写入
        self.config.ensure_provider_models(&entry.provider_id);

        let resolved = self.config.resolve(&entry.display)?;
        self.agent.switch_model(
            resolved.config.clone(),
            &resolved.model_id,
            resolved.max_tokens,
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
